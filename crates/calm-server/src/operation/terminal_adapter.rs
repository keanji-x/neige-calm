use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::{
    append_decision_event_in_tx, card_update_tx, card_with_terminal_create_tx,
    session_projection_active_for_card_tx, session_set_status_tx,
};
use crate::db::write_with_events_typed;
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, CardId, TrackId};
use crate::model::{CardRole, new_id};
use crate::operation::worker_cleanup::{compensate_worker_rows, worker_spawn_failure_preserved};
use crate::routes::cards::card_scope;
use crate::routes::settings::load_settings;
use crate::routes::theme::RequestTheme;
use crate::session_projection_repo::{WorkerSessionKind, WorkerSessionState};
use crate::state::WriteContext;
use crate::terminal_sweeper::reap_terminal_artifacts_with_renderer;
use crate::track_area_cache::TrackAreaCache;
use calm_truth::decision_gate::PermissiveGate;

use super::{
    AppServerInteractOutcome, CompensationStateVersioned, CompensationStep, Operation, PhaseTag,
    ProviderAdapter, SpawnCtx, SpawnHandle, SpawnOutcome, Tx, TxOutput,
};

pub type SpawnHook = Arc<
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
    track_area_cache: TrackAreaCache,
    spawn_hook: Option<SpawnHook>,
}

#[derive(Clone)]
pub struct TerminalWorkerAdapter {
    repo: Arc<dyn crate::db::RouteRepo>,
    card_role_cache: CardRoleCache,
    track_area_cache: TrackAreaCache,
    spawn_hook: Option<SpawnHook>,
}

impl TerminalAdapter {
    pub fn new(
        repo: Arc<dyn crate::db::RouteRepo>,
        card_role_cache: CardRoleCache,
        track_area_cache: TrackAreaCache,
    ) -> Self {
        Self {
            repo,
            card_role_cache,
            track_area_cache,
            spawn_hook: None,
        }
    }

    pub fn new_with_spawn_hook(
        repo: Arc<dyn crate::db::RouteRepo>,
        card_role_cache: CardRoleCache,
        track_area_cache: TrackAreaCache,
        spawn_hook: SpawnHook,
    ) -> Self {
        Self {
            repo,
            card_role_cache,
            track_area_cache,
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
        track_area_cache: TrackAreaCache,
    ) -> Self {
        Self {
            repo,
            card_role_cache,
            track_area_cache,
            spawn_hook: None,
        }
    }

    pub fn new_with_spawn_hook(
        repo: Arc<dyn crate::db::RouteRepo>,
        card_role_cache: CardRoleCache,
        track_area_cache: TrackAreaCache,
        spawn_hook: SpawnHook,
    ) -> Self {
        Self {
            repo,
            card_role_cache,
            track_area_cache,
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
    pub track_id: String,
    #[serde(default)]
    pub title: Option<String>,
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
    pub track_id: String,
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

/// The cwd the caller actually named, or `None` when it named none — an absent
/// field and a blank string are the same request.
///
/// #1147 S6 — this used to fall back to `$HOME` here. The default is no longer
/// a process-environment constant: it is the track's workspace, and only
/// [`terminal_cwd_or_track_workspace`] can resolve it, because that needs the
/// transaction.
pub(crate) fn explicit_terminal_cwd(cwd: Option<String>) -> Option<String> {
    cwd.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

/// #1147 S6 — resolve a terminal card's working directory: whatever the caller
/// named, else **the track's workspace**.
///
/// This is the slice's whole point. Design §产品契约 says every track owns a
/// repository; before S6 nothing made that true at a terminal prompt, because
/// both terminal paths fell back to `$HOME` and never read
/// `tracks.workspace_path`. A user who opened a terminal in a track landed
/// outside the track's repository — the same "the track is not where you think it
/// is" defect #1147 was opened on, one layer up.
///
/// Read inside the transaction that is about to write the terminal row, so the
/// path cannot move between the read and the write (the row creation freezes
/// the workspace in that same transaction).
///
/// An empty stored path is refused rather than silently falling back. Every
/// track has had a materialized workspace since S2, so an empty one means the
/// row is broken; inheriting the server's cwd instead is exactly how #1147's
/// original `spawn-failed` reached a user with no explanation.
pub(crate) async fn terminal_cwd_or_track_workspace(
    tx: &mut Tx<'_>,
    track_id: &str,
    requested: Option<String>,
) -> Result<String> {
    if let Some(cwd) = requested {
        return Ok(cwd);
    }
    let workspace = crate::db::sqlite::track_workspace_read_tx(tx, track_id).await?;
    if workspace.path.trim().is_empty() {
        return Err(CalmError::Internal(format!(
            "track {track_id} has no workspace path; refusing to open a terminal in the server's cwd"
        )));
    }
    Ok(workspace.path)
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
            .track_get(&payload.request.track_id)
            .await?
            .is_none()
        {
            return Err(CalmError::NotFound(format!(
                "track {}",
                payload.request.track_id
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
        let env = payload.request.env.clone();
        let card_id = new_id();
        let runtime_id = payload.runtime_id.clone().unwrap_or_else(new_id);
        let track_id = payload.request.track_id.clone();
        // #1147 S6 — an empty request `cwd` means "the track's workspace". It is
        // resolved here rather than in `normalize_terminal_create_request` for
        // the same reason the dispatcher keeps `cwd: None` on the terminal-worker
        // payload (see `scheduler::build_*_payload`): materializing a default
        // into the operation payload puts it into `stable_payload_hash`.
        let cwd = terminal_cwd_or_track_workspace(
            tx,
            &track_id,
            explicit_terminal_cwd(Some(payload.request.cwd.clone())),
        )
        .await?;
        let scope = card_scope(
            self.repo.as_ref(),
            CardId::from(card_id.clone()),
            TrackId::from(track_id.clone()),
        )
        .await?;
        let (card, term) = card_with_terminal_create_tx(
            tx,
            card_id,
            &runtime_id,
            None,
            TrackId::from(track_id),
            payload.request.title.clone(),
            payload.request.sort,
            program.clone(),
            cwd.clone(),
            env.clone(),
            CardRole::Worker,
            true,
            &self.card_role_cache,
            payload.request.theme,
        )
        .await?;
        let event = Event::CardAdded(card.clone());
        let runtime_event = Event::RuntimeStarted {
            runtime_id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::Terminal,
            agent_provider: None,
            status: WorkerSessionState::Starting,
        };
        if let Err(violation) = crate::role_gate::enforce_role(
            &payload.actor,
            &event,
            &scope,
            &self.card_role_cache,
            &self.track_area_cache,
        ) {
            return Err(CalmError::Forbidden(violation.to_string()));
        }
        if let Err(violation) = crate::role_gate::enforce_role(
            &payload.actor,
            &runtime_event,
            &scope,
            &self.card_role_cache,
            &self.track_area_cache,
        ) {
            return Err(CalmError::Forbidden(violation.to_string()));
        }
        let event_id =
            append_decision_event_in_tx(tx, &PermissiveGate, &payload.actor, &scope, None, &event)
                .await?;
        let runtime_event_id = append_decision_event_in_tx(
            tx,
            &PermissiveGate,
            &payload.actor,
            &scope,
            None,
            &runtime_event,
        )
        .await?;

        let projected_card = project_terminal_id_for_response(&card, &term.id);
        let mut output = TxOutput::new(
            "runtime",
            Some(runtime_id.clone()),
            serde_json::to_value(&projected_card)?,
        );
        output.data = json!({
            "card_id": card.id,
            "runtime_id": runtime_id,
            "track_id": card.track_id,
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
    ) -> Result<SpawnOutcome> {
        let card_id = output_card_id(output)?;
        let terminal_id = output.output_string("terminal_id", "terminal")?;
        let program = output.output_string("program", "terminal")?;
        let cwd = output.output_string("cwd", "terminal")?;
        let env = output.data.get("env").cloned().unwrap_or_else(|| json!({}));

        match self
            .spawn_terminal_from_output(terminal_id.clone(), program, cwd, env, ctx)
            .await
        {
            Ok(handle) => {
                let status_result: Result<()> = async {
                    let existing = ctx.repo.session_projection_active_for_card(&card_id).await?;
                    let needs_status_write = existing
                        .as_ref()
                        .map(|runtime| runtime.status != WorkerSessionState::Running)
                        .unwrap_or(true);
                    if !needs_status_write {
                        return Ok(());
                    }

                    let track_id = if let Some(track_id) =
                        output.data.get("track_id").and_then(Value::as_str)
                    {
                        TrackId::from(track_id.to_string())
                    } else {
                        ctx.repo
                            .card_get(&card_id)
                            .await?
                            .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?
                            .track_id
                    };
                    let scope =
                        card_scope(ctx.repo.as_ref(), CardId::from(card_id.clone()), track_id)
                            .await?;
                    let write = WriteContext::new(
                        self.card_role_cache.clone(),
                        self.track_area_cache.clone(),
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
                                    session_projection_active_for_card_tx(tx, &card_id_for_tx)
                                        .await?
                                        .ok_or_else(|| {
                                            CalmError::Internal(format!(
                                                "terminal card {card_id_for_tx} has no active runtime to mark running"
                                            ))
                                        })?;
                                let old_status = runtime.status;
                                let runtime_id = runtime.id.clone();
                                session_set_status_tx(tx, &runtime.id, WorkerSessionState::Running)
                                    .await?;
                                Ok((
                                    (),
                                    vec![(
                                        scope,
                                        Event::RuntimeStatusChanged {
                                            runtime_id,
                                            card_id: card_id_for_tx,
                                            old_status,
                                            new_status: WorkerSessionState::Running,
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
                Ok(SpawnOutcome::Ready(handle))
            }
            Err(e) => {
                if let Err(mark_err) = ctx
                    .repo
                    .session_projection_complete_for_card(&card_id, WorkerSessionState::Failed)
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
                    "card_id": output.output_string("card_id", "terminal")?,
                    "terminal_id": output.output_string("terminal_id", "terminal")?,
                    "track_id": output_track_id(output)?,
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
        let track_id = step
            .args
            .get("track_id")
            .and_then(Value::as_str)
            .ok_or_else(|| CalmError::Internal("rollback step missing track_id".into()))?
            .to_string();
        let card = CardId::from(card_id.clone());
        let track = TrackId::from(track_id);
        let scope = card_scope(ctx.repo.as_ref(), card.clone(), track.clone()).await?;
        if let Some(term) = ctx.repo.terminal_get(&terminal_id).await? {
            reap_terminal_artifacts_with_renderer(Some(ctx.terminal_renderer.as_ref()), &term)
                .await;
        }
        let cache = self.card_role_cache.clone();
        let write = crate::state::WriteContext::new(
            self.card_role_cache.clone(),
            self.track_area_cache.clone(),
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
                    let event_track = track.clone();
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
                            track_id: event_track,
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
        if self.repo.track_get(&payload.track_id).await?.is_none() {
            return Err(CalmError::NotFound(format!("track {}", payload.track_id)));
        }
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> Result<TxOutput> {
        let payload: TerminalWorkerOperationPayload = serde_json::from_value(input.clone())?;
        super::refuse_if_context_stale(tx, Some(&payload.idempotency_key)).await?;
        // #1149 — title the worker card after its task key. Derived from
        // the `tasks` row inside this tx (never carried on the payload,
        // which would move `stable_payload_hash`), and fail-soft: `None`
        // just leaves the card untitled.
        let card_title = super::task_key_for_card_title(tx, &payload.idempotency_key).await;
        let card_id = new_id();
        let runtime_id = new_id();
        let track_id = TrackId::from(payload.track_id.clone());
        // #1147 S6 — the task row's cwd if it named one, else the track's
        // workspace (was `$HOME`).
        let cwd = terminal_cwd_or_track_workspace(
            tx,
            &payload.track_id,
            explicit_terminal_cwd(payload.cwd.clone()),
        )
        .await?;
        let env = terminal_worker_env(self.repo.as_ref()).await?;
        let scope = card_scope(
            self.repo.as_ref(),
            CardId::from(card_id.clone()),
            track_id.clone(),
        )
        .await?;
        let (mut card, term) = card_with_terminal_create_tx(
            tx,
            card_id,
            &runtime_id,
            Some(op.id.as_str()),
            track_id,
            None,
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
                    title: card_title,
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
            "track_id": card.track_id,
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
    ) -> Result<SpawnOutcome> {
        let card_id = output_card_id(output)?;
        let terminal_id = output.output_string("terminal_id", "terminal")?;
        let track_id = TrackId::from(output.output_string("track_id", "terminal")?);
        let cmd = output.output_string("cmd", "terminal")?;
        let cwd = output.output_string("cwd", "terminal")?;
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
                &self.track_area_cache,
                &card_id,
                &track_id,
            )
            .await
            .unwrap_or_else(|e| {
                tracing::error!(
                    card_id = %card_id,
                    track_id = %track_id,
                    error = %e,
                    "terminal worker CardAdded append failed after recovery exit preservation; continuing"
                );
            });
            return Ok(SpawnOutcome::Ready(SpawnHandle::NoOp));
        }
        ctx.repo.terminal_clear_exit_for_spawn(&terminal_id).await?;
        let term = ctx
            .repo
            .terminal_get(&terminal_id)
            .await?
            .ok_or_else(|| CalmError::Internal(format!("terminal {terminal_id} vanished")))?;

        let spawn_result = if let Some(hook) = &self.spawn_hook {
            hook(terminal_id.clone(), cmd.clone(), cwd.clone(), env.clone()).await
        } else {
            ctx.spawn_terminal(&term, &cmd, &cwd, &env).await
        };

        match spawn_result {
            Ok(handle) => {
                if let Err(e) = ctx
                    .repo
                    .session_projection_set_status_for_card(
                        card_id.as_ref(),
                        WorkerSessionState::Running,
                    )
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
                    &self.track_area_cache,
                    &card_id,
                    &track_id,
                )
                .await
                .unwrap_or_else(|e| {
                    tracing::error!(
                        card_id = %card_id,
                        track_id = %track_id,
                        error = %e,
                        "terminal worker CardAdded append failed after live spawn; continuing"
                    );
                });
                Ok(SpawnOutcome::Ready(handle))
            }
            Err(e) if worker_spawn_failure_preserved(ctx.repo.as_ref(), &terminal_id).await? => {
                tracing::info!(
                    card_id = %card_id,
                    track_id = %track_id,
                    terminal_id = %terminal_id,
                    spawn_err = %e,
                    "worker terminal fast-exit (sidecar present); preserving card + terminal",
                );
                log_terminal_worker_card_added(
                    ctx,
                    &self.card_role_cache,
                    &self.track_area_cache,
                    &card_id,
                    &track_id,
                )
                .await
                .unwrap_or_else(|e| {
                    tracing::error!(
                        card_id = %card_id,
                        track_id = %track_id,
                        error = %e,
                        "terminal worker CardAdded append failed after fast-exit preservation; continuing"
                    );
                });
                Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
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
                    "card_id": output.output_string("card_id", "terminal")?,
                    "terminal_id": output.output_string("terminal_id", "terminal")?,
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

fn output_track_id(output: &TxOutput) -> Result<&str> {
    output
        .result
        .get("track_id")
        .and_then(Value::as_str)
        .ok_or_else(|| CalmError::Internal("terminal tx_output missing track_id".into()))
}

pub(crate) async fn terminal_worker_env(repo: &dyn crate::db::RouteRepo) -> Result<Value> {
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
    track_area_cache: &TrackAreaCache,
    card_id: &str,
    track_id: &TrackId,
) -> Result<()> {
    let card = ctx
        .repo
        .card_get(card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    let scope = card_scope(
        ctx.repo.as_ref(),
        CardId::from(card_id.to_string()),
        track_id.clone(),
    )
    .await?;
    ctx.repo
        .log_pure_event(
            ActorId::KernelDispatcher,
            scope,
            None,
            &ctx.events,
            card_role_cache,
            track_area_cache,
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

fn normalize_program(program: String) -> String {
    let program = program.trim();
    if program.is_empty() {
        default_program()
    } else {
        program.to_string()
    }
}

/// #1147 S6 — trim only. An empty cwd stays empty all the way into the
/// operation payload and is resolved to the track's workspace inside
/// `prepare_tx` (`terminal_cwd_or_track_workspace`). Filling `$HOME` in here
/// would bake the server's environment into `stable_payload_hash`.
fn normalize_cwd(cwd: String) -> String {
    cwd.trim().to_string()
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

#[cfg(test)]
mod tests;
