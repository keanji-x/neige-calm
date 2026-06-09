use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::{
    card_update_tx, card_with_terminal_create_tx, event_append_for_operation_tx,
    runtime_get_active_for_card_tx, runtime_set_status_tx,
};
use crate::db::write_with_events_typed;
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, CardId, WaveId};
use crate::model::{CardRole, new_id};
use crate::operation::worker_cleanup::{compensate_worker_rows, worker_spawn_failure_preserved};
use crate::routes::cards::card_scope;
use crate::routes::settings::load_settings;
use crate::routes::theme::RequestTheme;
use crate::runtime_repo::{RunStatus, RuntimeKind};
use crate::state::WriteContext;
use crate::terminal_sweeper::reap_terminal_artifacts_with_renderer;
use crate::wave_cove_cache::WaveCoveCache;

use super::{
    AppServerInteractOutcome, CompensationStateVersioned, CompensationStep, Operation, PhaseTag,
    ProviderAdapter, SpawnCtx, SpawnHandle, Tx, TxOutput,
};

#[cfg(feature = "fixtures")]
use futures::future::BoxFuture;

#[cfg(feature = "fixtures")]
type SpawnHook = Arc<
    dyn Fn(String, String, String, Value) -> BoxFuture<'static, Result<SpawnHandle>> + Send + Sync,
>;

const TERMINAL_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::SpawnStarted,
    PhaseTag::SpawnSucceeded,
    PhaseTag::Succeeded,
];

#[derive(Clone)]
pub struct TerminalAdapter {
    repo: Arc<dyn crate::db::RouteRepo>,
    card_role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
    #[cfg(feature = "fixtures")]
    spawn_hook: Option<SpawnHook>,
}

#[derive(Clone)]
pub struct TerminalWorkerAdapter {
    repo: Arc<dyn crate::db::RouteRepo>,
    card_role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
    #[cfg(feature = "fixtures")]
    spawn_hook: Option<SpawnHook>,
}

impl TerminalAdapter {
    pub fn new(
        repo: Arc<dyn crate::db::RouteRepo>,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
    ) -> Self {
        Self {
            repo,
            card_role_cache,
            wave_cove_cache,
            #[cfg(feature = "fixtures")]
            spawn_hook: None,
        }
    }

    #[cfg(feature = "fixtures")]
    pub fn new_with_spawn_hook(
        repo: Arc<dyn crate::db::RouteRepo>,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
        spawn_hook: SpawnHook,
    ) -> Self {
        Self {
            repo,
            card_role_cache,
            wave_cove_cache,
            spawn_hook: Some(spawn_hook),
        }
    }

    async fn spawn_terminal_from_output(
        &self,
        terminal_id: String,
        program: String,
        cwd: String,
        env: Value,
        ctx: &SpawnCtx,
    ) -> Result<SpawnHandle> {
        ctx.repo.terminal_clear_exit_for_spawn(&terminal_id).await?;
        let term = ctx
            .repo
            .terminal_get(&terminal_id)
            .await?
            .ok_or_else(|| CalmError::Internal(format!("terminal {terminal_id} vanished")))?;

        #[cfg(feature = "fixtures")]
        if let Some(hook) = &self.spawn_hook {
            return hook(terminal_id, program, cwd, env).await;
        }

        ctx.spawn_terminal(&term, &program, &cwd, &env).await
    }
}

impl TerminalWorkerAdapter {
    pub fn new(
        repo: Arc<dyn crate::db::RouteRepo>,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
    ) -> Self {
        Self {
            repo,
            card_role_cache,
            wave_cove_cache,
            #[cfg(feature = "fixtures")]
            spawn_hook: None,
        }
    }

    #[cfg(feature = "fixtures")]
    pub fn new_with_spawn_hook(
        repo: Arc<dyn crate::db::RouteRepo>,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
        spawn_hook: SpawnHook,
    ) -> Self {
        Self {
            repo,
            card_role_cache,
            wave_cove_cache,
            spawn_hook: Some(spawn_hook),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TerminalCreateOperationPayload {
    pub actor: ActorId,
    #[serde(default)]
    pub runtime_id: Option<String>,
    #[serde(flatten)]
    pub request: TerminalCreateRequestPayload,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TerminalCreateRequestPayload {
    pub wave_id: String,
    #[serde(default)]
    pub sort: Option<f64>,
    #[serde(default)]
    pub program: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub env: Value,
    pub theme: RequestTheme,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TerminalWorkerOperationPayload {
    pub actor: ActorId,
    pub wave_id: String,
    pub idempotency_key: String,
    pub cmd: String,
    #[serde(default)]
    pub cwd: Option<String>,
}

pub fn normalize_terminal_create_request(
    mut request: TerminalCreateRequestPayload,
) -> TerminalCreateRequestPayload {
    request.program = normalize_program(request.program);
    request.cwd = normalize_cwd(request.cwd);
    request.env = normalize_env(request.env);
    request
}

pub(crate) fn normalize_terminal_worker_cwd(cwd: Option<String>) -> String {
    cwd.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(default_cwd)
}

#[async_trait]
impl ProviderAdapter for TerminalAdapter {
    fn kind(&self) -> &'static str {
        "terminal-create"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        TERMINAL_PHASES
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: TerminalCreateOperationPayload = serde_json::from_value(input.clone())?;
        if self
            .repo
            .wave_get(&payload.request.wave_id)
            .await?
            .is_none()
        {
            return Err(CalmError::NotFound(format!(
                "wave {}",
                payload.request.wave_id
            )));
        }
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        let payload: TerminalCreateOperationPayload = serde_json::from_value(input.clone())?;
        let program = payload.request.program.clone();
        let cwd = payload.request.cwd.clone();
        let env = payload.request.env.clone();
        let card_id = new_id();
        let runtime_id = payload.runtime_id.clone().unwrap_or_else(new_id);
        let wave_id = payload.request.wave_id.clone();
        let scope = card_scope(
            self.repo.as_ref(),
            CardId::from(card_id.clone()),
            WaveId::from(wave_id.clone()),
        )
        .await?;
        let (card, term) = card_with_terminal_create_tx(
            tx,
            card_id,
            &runtime_id,
            WaveId::from(wave_id),
            payload.request.sort,
            program.clone(),
            cwd.clone(),
            env.clone(),
            CardRole::Plain,
            true,
            &self.card_role_cache,
            payload.request.theme,
        )
        .await?;
        let event = Event::CardAdded(card.clone());
        let runtime_event = Event::RuntimeStarted {
            runtime_id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: RuntimeKind::Terminal,
            agent_provider: None,
            status: RunStatus::Starting,
        };
        if let Err(violation) = crate::role_gate::enforce_role(
            &payload.actor,
            &event,
            &scope,
            &self.card_role_cache,
            &self.wave_cove_cache,
        ) {
            return Err(CalmError::Forbidden(violation.to_string()));
        }
        if let Err(violation) = crate::role_gate::enforce_role(
            &payload.actor,
            &runtime_event,
            &scope,
            &self.card_role_cache,
            &self.wave_cove_cache,
        ) {
            return Err(CalmError::Forbidden(violation.to_string()));
        }
        let event_id =
            event_append_for_operation_tx(tx, &payload.actor, &scope, None, &event).await?;
        let runtime_event_id =
            event_append_for_operation_tx(tx, &payload.actor, &scope, None, &runtime_event).await?;

        let projected_card = project_terminal_id_for_response(&card, &term.id);
        let mut output = TxOutput::new(
            "runtime",
            Some(runtime_id.clone()),
            serde_json::to_value(&projected_card)?,
        );
        output.data = json!({
            "card_id": card.id,
            "runtime_id": runtime_id,
            "wave_id": card.wave_id,
            "terminal_id": term.id,
            "program": program,
            "cwd": cwd,
            "env": env,
        });
        output.post_commit_events.push(BroadcastEnvelope {
            id: event_id,
            event_version: SYNC_EVENT_VERSION,
            actor: payload.actor.clone(),
            scope: scope.clone(),
            event: Event::CardAdded(projected_card),
        });
        output.post_commit_events.push(BroadcastEnvelope {
            id: runtime_event_id,
            event_version: SYNC_EVENT_VERSION,
            actor: payload.actor,
            scope,
            event: runtime_event,
        });
        Ok(output)
    }

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnHandle> {
        let card_id = output_card_id(output)?;
        let terminal_id = output_string(output, "terminal_id")?;
        let program = output_string(output, "program")?;
        let cwd = output_string(output, "cwd")?;
        let env = output.data.get("env").cloned().unwrap_or_else(|| json!({}));

        match self
            .spawn_terminal_from_output(terminal_id.clone(), program, cwd, env, ctx)
            .await
        {
            Ok(handle) => {
                let status_result: Result<()> = async {
                    let existing = ctx.repo.runtime_get_active_for_card(&card_id).await?;
                    let needs_status_write = existing
                        .as_ref()
                        .map(|runtime| runtime.status != RunStatus::Running)
                        .unwrap_or(true);
                    if !needs_status_write {
                        return Ok(());
                    }

                    let wave_id = if let Some(wave_id) =
                        output.data.get("wave_id").and_then(Value::as_str)
                    {
                        WaveId::from(wave_id.to_string())
                    } else {
                        ctx.repo
                            .card_get(&card_id)
                            .await?
                            .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?
                            .wave_id
                    };
                    let scope =
                        card_scope(ctx.repo.as_ref(), CardId::from(card_id.clone()), wave_id)
                            .await?;
                    let write = WriteContext::new(
                        self.card_role_cache.clone(),
                        self.wave_cove_cache.clone(),
                    );
                    let card_id_for_tx = card_id.clone();
                    let (_unit, _ids) = write_with_events_typed(
                        ctx.repo.as_ref(),
                        ActorId::Kernel,
                        None,
                        &ctx.events,
                        &write,
                        move |tx| {
                            Box::pin(async move {
                                let runtime =
                                    runtime_get_active_for_card_tx(tx, &card_id_for_tx)
                                        .await?
                                        .ok_or_else(|| {
                                            CalmError::Internal(format!(
                                                "terminal card {card_id_for_tx} has no active runtime to mark running"
                                            ))
                                        })?;
                                let old_status = runtime.status.clone();
                                let runtime_id = runtime.id.clone();
                                runtime_set_status_tx(tx, &runtime.id, RunStatus::Running)
                                    .await?;
                                Ok((
                                    (),
                                    vec![(
                                        scope,
                                        Event::RuntimeStatusChanged {
                                            runtime_id,
                                            card_id: card_id_for_tx,
                                            old_status,
                                            new_status: RunStatus::Running,
                                        },
                                    )],
                                ))
                            })
                        },
                    )
                    .await?;
                    Ok(())
                }
                .await;
                if let Err(e) = status_result {
                    tracing::warn!(
                        target: "operation::terminal_adapter::runtime_running_mark_failed",
                        card_id = %card_id,
                        terminal_id = %terminal_id,
                        error = %e,
                        "failed to mark terminal runtime running after spawn; continuing operation"
                    );
                }
                Ok(handle)
            }
            Err(e) => {
                if let Err(mark_err) = ctx
                    .repo
                    .runtime_complete_for_card(&card_id, RunStatus::Failed)
                    .await
                {
                    tracing::warn!(
                        card_id = %card_id,
                        terminal_id = %terminal_id,
                        error = %mark_err,
                        "failed to mark terminal runtime failed after spawn error"
                    );
                }
                Err(e)
            }
        }
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        _op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.to_string(),
            steps: vec![CompensationStep {
                op: "rollback_terminal_card".into(),
                args: json!({
                    "card_id": output_string(output, "card_id")?,
                    "terminal_id": output_string(output, "terminal_id")?,
                    "wave_id": output_wave_id(output)?,
                }),
                completed: false,
                attempts: 0,
                last_error: None,
            }],
        })
    }

    async fn compensate_step(
        &self,
        step: &CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()> {
        if step.completed {
            return Ok(());
        }
        if step.op != "rollback_terminal_card" {
            return Err(CalmError::Internal(format!(
                "unknown terminal compensation op {}",
                step.op
            )));
        }
        let card_id = step
            .args
            .get("card_id")
            .and_then(Value::as_str)
            .ok_or_else(|| CalmError::Internal("rollback step missing card_id".into()))?
            .to_string();
        let terminal_id = step
            .args
            .get("terminal_id")
            .and_then(Value::as_str)
            .ok_or_else(|| CalmError::Internal("rollback step missing terminal_id".into()))?
            .to_string();
        let wave_id = step
            .args
            .get("wave_id")
            .and_then(Value::as_str)
            .ok_or_else(|| CalmError::Internal("rollback step missing wave_id".into()))?
            .to_string();
        let card = CardId::from(card_id.clone());
        let wave = WaveId::from(wave_id);
        let scope = card_scope(ctx.repo.as_ref(), card.clone(), wave.clone()).await?;
        if let Some(term) = ctx.repo.terminal_get(&terminal_id).await? {
            reap_terminal_artifacts_with_renderer(Some(ctx.terminal_renderer.as_ref()), &term)
                .await;
        }
        let cache = self.card_role_cache.clone();
        let write = crate::state::WriteContext::new(
            self.card_role_cache.clone(),
            self.wave_cove_cache.clone(),
        );
        ctx.repo
            .write_with_event(
                ActorId::Kernel,
                scope,
                None,
                &ctx.events,
                &write,
                Box::new(move |tx| {
                    let event_card = card.clone();
                    let event_wave = wave.clone();
                    let card_id = card_id.clone();
                    let terminal_id = terminal_id.clone();
                    let cache = cache.clone();
                    Box::pin(async move {
                        crate::dispatcher::card_with_terminal_rollback_tx(
                            tx,
                            &card_id,
                            &terminal_id,
                            &cache,
                        )
                        .await?;
                        Ok(Event::CardDeleted {
                            id: event_card,
                            wave_id: event_wave,
                        })
                    })
                }),
            )
            .await?;
        Ok(())
    }
}

#[async_trait]
impl ProviderAdapter for TerminalWorkerAdapter {
    fn kind(&self) -> &'static str {
        "terminal-worker"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        TERMINAL_PHASES
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: TerminalWorkerOperationPayload = serde_json::from_value(input.clone())?;
        if payload.idempotency_key.trim().is_empty() {
            return Err(CalmError::BadRequest(
                "terminal worker idempotency_key must not be empty".into(),
            ));
        }
        if self.repo.wave_get(&payload.wave_id).await?.is_none() {
            return Err(CalmError::NotFound(format!("wave {}", payload.wave_id)));
        }
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        let payload: TerminalWorkerOperationPayload = serde_json::from_value(input.clone())?;
        let card_id = new_id();
        let runtime_id = new_id();
        let wave_id = WaveId::from(payload.wave_id.clone());
        let cwd = normalize_terminal_worker_cwd(payload.cwd.clone());
        let env = terminal_worker_env(self.repo.as_ref()).await?;
        let scope = card_scope(
            self.repo.as_ref(),
            CardId::from(card_id.clone()),
            wave_id.clone(),
        )
        .await?;
        let (mut card, term) = card_with_terminal_create_tx(
            tx,
            card_id,
            &runtime_id,
            wave_id,
            None,
            payload.cmd.clone(),
            cwd.clone(),
            env.clone(),
            CardRole::Worker,
            true,
            &self.card_role_cache,
            RequestTheme::default_dark(),
        )
        .await?;

        if let Some(existing_map) = card.payload.as_object() {
            let mut merged = existing_map.clone();
            merged.insert(
                "idempotency_key".into(),
                Value::String(payload.idempotency_key.clone()),
            );
            merged.insert("role_request".into(), Value::String("terminal".into()));
            merged.insert("cmd".into(), Value::String(payload.cmd.clone()));
            merged.insert("cwd".into(), Value::String(cwd.clone()));
            card = card_update_tx(
                tx,
                card.id.as_ref(),
                crate::model::CardPatch {
                    kind: None,
                    sort: None,
                    payload: Some(Value::Object(merged)),
                    deletable: None,
                },
            )
            .await?;
        }

        let mut output = TxOutput::new(
            "card",
            Some(card.id.to_string()),
            serde_json::to_value(&card)?,
        );
        output.data = json!({
            "card_id": card.id,
            "runtime_id": runtime_id,
            "wave_id": card.wave_id,
            "terminal_id": term.id,
            "cmd": payload.cmd,
            "cwd": cwd,
            "env": env,
            "scope": scope,
        });
        Ok(output)
    }

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnHandle> {
        let card_id = output_card_id(output)?;
        let terminal_id = output_string(output, "terminal_id")?;
        let wave_id = WaveId::from(output_string(output, "wave_id")?);
        let cmd = output_string(output, "cmd")?;
        let cwd = output_string(output, "cwd")?;
        let env = output.data.get("env").cloned().unwrap_or_else(|| json!({}));
        let existing_term = ctx
            .repo
            .terminal_get(&terminal_id)
            .await?
            .ok_or_else(|| CalmError::Internal(format!("terminal {terminal_id} vanished")))?;
        if existing_term.exit_code.is_some() || existing_term.signal_killed {
            tracing::info!(
                card_id = %card_id,
                terminal_id = %terminal_id,
                exit_code = ?existing_term.exit_code,
                signal_killed = existing_term.signal_killed,
                "terminal-worker recovery: worker already exited; skipping respawn",
            );
            log_terminal_worker_card_added(
                ctx,
                &self.card_role_cache,
                &self.wave_cove_cache,
                &card_id,
                &wave_id,
            )
            .await
            .unwrap_or_else(|e| {
                tracing::error!(
                    card_id = %card_id,
                    wave_id = %wave_id,
                    error = %e,
                    "terminal worker CardAdded append failed after recovery exit preservation; continuing"
                );
            });
            return Ok(SpawnHandle::NoOp);
        }
        ctx.repo.terminal_clear_exit_for_spawn(&terminal_id).await?;
        let term = ctx
            .repo
            .terminal_get(&terminal_id)
            .await?
            .ok_or_else(|| CalmError::Internal(format!("terminal {terminal_id} vanished")))?;

        #[cfg(feature = "fixtures")]
        let spawn_result = if let Some(hook) = &self.spawn_hook {
            hook(terminal_id.clone(), cmd.clone(), cwd.clone(), env.clone()).await
        } else {
            ctx.spawn_terminal(&term, &cmd, &cwd, &env).await
        };
        #[cfg(not(feature = "fixtures"))]
        let spawn_result = ctx.spawn_terminal(&term, &cmd, &cwd, &env).await;

        match spawn_result {
            Ok(handle) => {
                if let Err(e) = ctx
                    .repo
                    .runtime_set_status_for_card(card_id.as_ref(), RunStatus::Running)
                    .await
                {
                    tracing::warn!(
                        target: "operation::terminal_worker_adapter::runtime_running_mark_failed",
                        card_id = %card_id,
                        terminal_id = %terminal_id,
                        error = %e,
                        "failed to mark terminal worker runtime running after spawn; CardAdded still broadcasting",
                    );
                }
                log_terminal_worker_card_added(
                    ctx,
                    &self.card_role_cache,
                    &self.wave_cove_cache,
                    &card_id,
                    &wave_id,
                )
                .await
                .unwrap_or_else(|e| {
                    tracing::error!(
                        card_id = %card_id,
                        wave_id = %wave_id,
                        error = %e,
                        "terminal worker CardAdded append failed after live spawn; continuing"
                    );
                });
                Ok(handle)
            }
            Err(e) if worker_spawn_failure_preserved(ctx.repo.as_ref(), &terminal_id).await? => {
                tracing::info!(
                    card_id = %card_id,
                    wave_id = %wave_id,
                    terminal_id = %terminal_id,
                    spawn_err = %e,
                    "worker terminal fast-exit (sidecar present); preserving card + terminal",
                );
                log_terminal_worker_card_added(
                    ctx,
                    &self.card_role_cache,
                    &self.wave_cove_cache,
                    &card_id,
                    &wave_id,
                )
                .await
                .unwrap_or_else(|e| {
                    tracing::error!(
                        card_id = %card_id,
                        wave_id = %wave_id,
                        error = %e,
                        "terminal worker CardAdded append failed after fast-exit preservation; continuing"
                    );
                });
                Ok(SpawnHandle::NoOp)
            }
            Err(e) => Err(e),
        }
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        _op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.to_string(),
            steps: vec![CompensationStep {
                op: "cleanup_terminal_worker".into(),
                args: json!({
                    "card_id": output_string(output, "card_id")?,
                    "terminal_id": output_string(output, "terminal_id")?,
                }),
                completed: false,
                attempts: 0,
                last_error: None,
            }],
        })
    }

    async fn compensate_step(
        &self,
        step: &CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()> {
        if step.completed {
            return Ok(());
        }
        if step.op != "cleanup_terminal_worker" {
            return Err(CalmError::Internal(format!(
                "unknown terminal worker compensation op {}",
                step.op
            )));
        }
        let card_id = step
            .args
            .get("card_id")
            .and_then(Value::as_str)
            .ok_or_else(|| CalmError::Internal("terminal worker cleanup missing card_id".into()))?;
        let terminal_id = step
            .args
            .get("terminal_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CalmError::Internal("terminal worker cleanup missing terminal_id".into())
            })?;
        compensate_worker_rows(
            ctx.repo.as_ref(),
            ctx.terminal_renderer.as_ref(),
            &self.card_role_cache,
            card_id,
            terminal_id,
        )
        .await;
        Ok(())
    }
}

fn output_wave_id(output: &TxOutput) -> Result<&str> {
    output
        .result
        .get("wave_id")
        .and_then(Value::as_str)
        .ok_or_else(|| CalmError::Internal("terminal tx_output missing wave_id".into()))
}

async fn terminal_worker_env(repo: &dyn crate::db::RouteRepo) -> Result<Value> {
    let settings = load_settings(repo).await?;
    let mut env_map = serde_json::Map::new();
    if let Some(p) = settings.http_proxy.as_deref().filter(|s| !s.is_empty()) {
        env_map.insert("HTTP_PROXY".to_string(), Value::String(p.to_string()));
        env_map.insert("http_proxy".to_string(), Value::String(p.to_string()));
    }
    if let Some(p) = settings.https_proxy.as_deref().filter(|s| !s.is_empty()) {
        env_map.insert("HTTPS_PROXY".to_string(), Value::String(p.to_string()));
        env_map.insert("https_proxy".to_string(), Value::String(p.to_string()));
    }
    Ok(Value::Object(env_map))
}

async fn log_terminal_worker_card_added(
    ctx: &SpawnCtx,
    card_role_cache: &CardRoleCache,
    wave_cove_cache: &WaveCoveCache,
    card_id: &str,
    wave_id: &WaveId,
) -> Result<()> {
    let card = ctx
        .repo
        .card_get(card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    let scope = card_scope(
        ctx.repo.as_ref(),
        CardId::from(card_id.to_string()),
        wave_id.clone(),
    )
    .await?;
    ctx.repo
        .log_pure_event(
            ActorId::KernelDispatcher,
            scope,
            None,
            &ctx.events,
            card_role_cache,
            wave_cove_cache,
            Event::CardAdded(card),
        )
        .await?;
    Ok(())
}

fn output_card_id(output: &TxOutput) -> Result<String> {
    if let Some(card_id) = output.data.get("card_id").and_then(Value::as_str) {
        return Ok(card_id.to_string());
    }
    if output.target_type == "card" {
        return output
            .target_id
            .clone()
            .ok_or_else(|| CalmError::Internal("terminal tx_output missing card_id".into()));
    }
    Err(CalmError::Internal(
        "terminal tx_output missing card_id".into(),
    ))
}

fn output_string(output: &TxOutput, key: &str) -> Result<String> {
    output
        .data
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| CalmError::Internal(format!("terminal tx_output missing {key}")))
}

fn normalize_program(program: String) -> String {
    let program = program.trim();
    if program.is_empty() {
        default_program()
    } else {
        program.to_string()
    }
}

fn normalize_cwd(cwd: String) -> String {
    let cwd = cwd.trim();
    if cwd.is_empty() {
        default_cwd()
    } else {
        cwd.to_string()
    }
}

fn normalize_env(env: Value) -> Value {
    if env.is_null() { json!({}) } else { env }
}

fn project_terminal_id_for_response(
    card: &crate::model::Card,
    terminal_id: &str,
) -> crate::model::Card {
    let mut card = card.clone();
    if let Some(map) = card.payload.as_object_mut() {
        map.entry("terminal_id")
            .or_insert_with(|| Value::String(terminal_id.to_string()));
    }
    card
}

fn default_program() -> String {
    let s = std::env::var("SHELL").unwrap_or_default();
    if s.is_empty() {
        "/bin/sh".to_string()
    } else {
        s
    }
}

fn default_cwd() -> String {
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return home;
    }
    std::env::current_dir()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}
