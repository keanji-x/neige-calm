use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::card_role_cache::CardRoleCache;
use crate::codex_appserver::InputItem;
use crate::db::sqlite::{
    append_decision_event_in_tx, card_update_tx, card_with_codex_create_tx,
    session_bind_attribution_tx, session_projection_active_for_card_tx,
    session_projection_by_id_tx, session_set_status_tx,
};
use crate::db::{write_in_tx_typed, write_with_events_typed};
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, CardId, WaveId};
use crate::mcp_server::McpServer;
use crate::mcp_server::wiring::{
    card_mcp_env, card_mcp_thread_start_config, mint_and_persist_card_token,
};
use crate::model::{Card, CardRole, new_id, now_ms};
use crate::operation::worker_cleanup::{
    WorkerCleanupOutcome, compensate_worker_rows, worker_spawn_failure_preserved,
};
use crate::operation::workspace_lease::{
    WorkspaceLeaseTarget, acquire_workspace_lease_tx, prepare_workspace_lease_target_tx,
    provision_workspace_worktree, release_workspace_lease_by_id,
    remove_workspace_artifact_for_lease_by_id,
};
use crate::pending_codex_threads::{PendingEntry, PendingThreadStartRegistry};
use crate::routes::cards::card_scope;
use crate::routes::codex_cards::{
    await_shared_initial_turn_lifecycle, default_cwd, normalize_optional_css_color,
    shell_single_quote,
};
use crate::routes::settings::load_settings;
use crate::routes::theme::RequestTheme;
use crate::session_projection_repo::{
    AgentProvider, ThreadAttribution, WorkerSessionKind, WorkerSessionState,
};
use crate::shared_codex_appserver::{SharedCodexAppServer, SharedThreadStartParams};
use crate::state::{CodexClient, WriteContext};
use crate::terminal_sweeper::reap_terminal_artifacts_with_renderer;
use crate::wave_cove_cache::WaveCoveCache;
use calm_truth::decision_gate::PermissiveGate;

use super::{
    AppServerInteractKind, AppServerInteractOutcome, CompensationStateVersioned, CompensationStep,
    Operation, Phase, PhaseTag, ProviderAdapter, SpawnCtx, SpawnHandle, SpawnOutcome, Tx, TxOutput,
    checkpoint_app_server_interact_tx,
};

#[cfg(feature = "fixtures")]
use futures::future::BoxFuture;

#[cfg(feature = "fixtures")]
type SpawnHook = Arc<
    dyn Fn(String, String, String, Value) -> BoxFuture<'static, Result<SpawnHandle>> + Send + Sync,
>;

const CODEX_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::AppServerInteract,
    PhaseTag::SpawnStarted,
    PhaseTag::SpawnSucceeded,
    PhaseTag::Succeeded,
];

#[derive(Clone)]
pub struct CodexAdapter {
    repo: Arc<dyn crate::db::RouteRepo>,
    codex: Arc<CodexClient>,
    shared_codex_appserver: Arc<SharedCodexAppServer>,
    pending_codex_threads: Arc<PendingThreadStartRegistry>,
    pending_codex_threads_spawn_serial: Arc<Mutex<()>>,
    card_role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
    #[cfg(feature = "fixtures")]
    spawn_hook: Option<SpawnHook>,
}

#[derive(Clone)]
pub struct CodexWorkerAdapter {
    repo: Arc<dyn crate::db::RouteRepo>,
    codex: Arc<CodexClient>,
    shared_codex_appserver: Arc<SharedCodexAppServer>,
    mcp_server: Option<Arc<McpServer>>,
    card_role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
}

impl CodexAdapter {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        repo: Arc<dyn crate::db::RouteRepo>,
        codex: Arc<CodexClient>,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        pending_codex_threads: Arc<PendingThreadStartRegistry>,
        pending_codex_threads_spawn_serial: Arc<Mutex<()>>,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
    ) -> Self {
        Self {
            repo,
            codex,
            shared_codex_appserver,
            pending_codex_threads,
            pending_codex_threads_spawn_serial,
            card_role_cache,
            wave_cove_cache,
            #[cfg(feature = "fixtures")]
            spawn_hook: None,
        }
    }

    #[cfg(feature = "fixtures")]
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_spawn_hook(
        repo: Arc<dyn crate::db::RouteRepo>,
        codex: Arc<CodexClient>,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        pending_codex_threads: Arc<PendingThreadStartRegistry>,
        pending_codex_threads_spawn_serial: Arc<Mutex<()>>,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
        spawn_hook: SpawnHook,
    ) -> Self {
        Self {
            repo,
            codex,
            shared_codex_appserver,
            pending_codex_threads,
            pending_codex_threads_spawn_serial,
            card_role_cache,
            wave_cove_cache,
            spawn_hook: Some(spawn_hook),
        }
    }
}

impl CodexWorkerAdapter {
    pub fn new(
        repo: Arc<dyn crate::db::RouteRepo>,
        codex: Arc<CodexClient>,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        mcp_server: Option<Arc<McpServer>>,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
    ) -> Self {
        Self {
            repo,
            codex,
            shared_codex_appserver,
            mcp_server,
            card_role_cache,
            wave_cove_cache,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CodexCreateOperationPayload {
    pub actor: ActorId,
    #[serde(default)]
    pub runtime_id: Option<String>,
    pub request: NormalizedCodexCreateRequest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CodexWorkerOperationPayload {
    pub actor: ActorId,
    pub wave_id: String,
    pub idempotency_key: String,
    pub goal: String,
    /// Forward-compatible only. Scheduler-created Codex worker payloads keep
    /// this absent because the workspace lease path created in `prepare_tx`
    /// is the worker cwd.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub context: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_criteria: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CodexCreateRequestInput {
    pub wave_id: String,
    pub sort: Option<f64>,
    pub cwd: Option<String>,
    pub prompt: Option<String>,
    pub icon_bg: Option<String>,
    pub icon_fg: Option<String>,
    pub theme: RequestTheme,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NormalizedCodexCreateRequest {
    pub wave_id: String,
    #[serde(default)]
    pub sort: Option<f64>,
    pub cwd: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub icon_bg: Option<String>,
    #[serde(default)]
    pub icon_fg: Option<String>,
    pub theme: RequestTheme,
}

pub fn normalize_codex_create_request(
    input: CodexCreateRequestInput,
) -> Result<NormalizedCodexCreateRequest> {
    if let Some(raw) = input.cwd.as_deref()
        && raw.chars().any(|c| c.is_ascii_control())
    {
        return Err(CalmError::BadRequest(
            "cwd must not contain ASCII control characters".into(),
        ));
    }
    let cwd = input
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(default_cwd);
    let prompt = input
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    let icon_bg = normalize_optional_css_color(input.icon_bg.as_deref(), "icon_bg")?;
    let icon_fg = normalize_optional_css_color(input.icon_fg.as_deref(), "icon_fg")?;

    Ok(NormalizedCodexCreateRequest {
        wave_id: input.wave_id,
        sort: input.sort,
        cwd,
        prompt,
        icon_bg,
        icon_fg,
        theme: input.theme,
    })
}

#[async_trait]
impl ProviderAdapter for CodexAdapter {
    fn kind(&self) -> &'static str {
        "codex-create"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        CODEX_PHASES
    }

    fn app_server_interact_kind(
        &self,
        output: &TxOutput,
        _op: &Operation,
    ) -> Result<AppServerInteractKind> {
        if output_prompt(output)?.is_some() {
            Ok(AppServerInteractKind::MintAndAwait { thread_id: None })
        } else {
            Ok(AppServerInteractKind::RegisterPending { entry_id: None })
        }
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: CodexCreateOperationPayload = serde_json::from_value(input.clone())?;
        if payload.request.cwd.chars().any(|c| c.is_ascii_control()) {
            return Err(CalmError::BadRequest(
                "cwd must not contain ASCII control characters".into(),
            ));
        }
        validate_optional_color(payload.request.icon_bg.as_deref(), "icon_bg")?;
        validate_optional_color(payload.request.icon_fg.as_deref(), "icon_fg")?;
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
        if !self.shared_codex_appserver.is_running() {
            return Err(CalmError::Internal(
                "shared codex app-server is not running".into(),
            ));
        }
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        let payload: CodexCreateOperationPayload = serde_json::from_value(input.clone())?;
        let card_id = new_id();
        let runtime_id = payload.runtime_id.clone().unwrap_or_else(new_id);
        let wave_id = payload.request.wave_id.clone();
        let env = build_codex_env(self.repo.as_ref(), self.codex.as_ref(), &card_id).await?;
        let scope = card_scope(
            self.repo.as_ref(),
            CardId::from(card_id.clone()),
            WaveId::from(wave_id.clone()),
        )
        .await?;
        let (card, term, _token) = card_with_codex_create_tx(
            tx,
            card_id.clone(),
            &runtime_id,
            None,
            WaveId::from(wave_id.clone()),
            payload.request.sort,
            payload.request.cwd.clone(),
            env.clone(),
            payload.request.prompt.clone(),
            payload.request.icon_bg.clone(),
            payload.request.icon_fg.clone(),
            CardRole::Worker,
            true,
            &self.card_role_cache,
            payload.request.theme,
        )
        .await?;
        let projected_card =
            project_codex_runtime_fields_for_response(card.clone(), Some(&term.id), None, None);
        let event = Event::CardAdded(projected_card.clone());
        let runtime_event = Event::RuntimeStarted {
            runtime_id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Starting,
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
            "cwd": payload.request.cwd,
            "env": env,
            "prompt": payload.request.prompt,
        });
        output.post_commit_events.push(BroadcastEnvelope {
            id: event_id,
            event_version: SYNC_EVENT_VERSION,
            actor: payload.actor.clone(),
            scope: scope.clone(),
            event,
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
        output: &mut TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        let payload: CodexCreateOperationPayload = serde_json::from_value(op.payload.clone())?;
        let card_id = output.output_string("card_id", "codex")?;
        let cwd = output.output_string("cwd", "codex")?;

        if let Some(prompt_text) = output_prompt(output)? {
            let mut notifs = self.shared_codex_appserver.subscribe_notifications();
            let thread_id = match output
                .output_optional_string("codex_thread_id", "codex")?
                .or_else(|| phase_minted_thread_id(&op.phase))
            {
                Some(thread_id) => thread_id,
                None => {
                    if let Some(runtime) = self
                        .repo
                        .session_projection_active_for_card(&card_id)
                        .await?
                        && let Some(thread_id) =
                            TxOutput::non_empty_string(runtime.thread_id.as_deref())
                    {
                        thread_id
                    } else {
                        // CodexAdapter (kind="codex-create") is the user-initiated
                        // interactive card path: it has no McpServer and never mints a
                        // per-card MCP token, so there is no NEIGE_MCP_SOCKET/TOKEN to
                        // inject and no `neige` task-reporting contract to satisfy here.
                        // The worker path (CodexWorkerAdapter -> spawn_codex_worker_via_shared_daemon)
                        // is the one that needs shell_environment_policy.set (#836).
                        self.shared_codex_appserver
                            .thread_start_mint_for_card(
                                &card_id,
                                SharedThreadStartParams {
                                    cwd: cwd.clone(),
                                    approval_policy: "never".into(),
                                    sandbox_mode: "workspace-write".into(),
                                    developer_instructions: None,
                                    config: None,
                                },
                            )
                            .await?
                    }
                }
            };
            output.set_output_data("codex_thread_id", json!(thread_id.clone()), "codex")?;
            let updated = persist_prompt_thread(
                ctx,
                &self.card_role_cache,
                &self.wave_cove_cache,
                op,
                output,
                payload.actor.clone(),
                &card_id,
                &thread_id,
            )
            .await?;
            output.result = serde_json::to_value(&updated)?;
            let turn_started_at_ms = output_optional_i64(output, "turn_started_at_ms")?;
            if turn_started_at_ms.is_none() {
                self.shared_codex_appserver
                    .turn_start(&thread_id, vec![InputItem::text(prompt_text)])
                    .await?;
                output.set_output_data("turn_started_at_ms", json!(now_ms()), "codex")?;
                checkpoint_prompt_turn_started(ctx, op, output, &thread_id).await?;
            }
            match await_shared_initial_turn_lifecycle(&mut notifs, &thread_id).await {
                Ok(()) => {}
                Err(err) => return Err(err),
            }
            Ok(AppServerInteractOutcome::MintedAndAwaited { thread_id })
        } else {
            self.shared_codex_appserver
                .ensure_respawn_for_current_settings()
                .await?;
            output.set_output_data("pending_registered", json!(true), "codex")?;
            let updated = persist_pending_thread_status(
                ctx,
                &self.card_role_cache,
                &self.wave_cove_cache,
                op,
                output,
                payload.actor,
                &card_id,
            )
            .await?;
            output.result = serde_json::to_value(&updated)?;
            Ok(
                AppServerInteractOutcome::RegisteredPendingForLaterAttribution {
                    entry_id: card_id,
                },
            )
        }
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        let terminal_id = output.output_string("terminal_id", "codex")?;
        let card_id = output.output_string("card_id", "codex")?;
        let wave_id = output.output_string("wave_id", "codex")?;
        let runtime_id = output.output_string("runtime_id", "codex")?;
        let cwd = output.output_string("cwd", "codex")?;
        let env = output.data.get("env").cloned().unwrap_or_else(|| json!({}));
        ctx.repo.terminal_clear_exit_for_spawn(&terminal_id).await?;
        let term = ctx
            .repo
            .terminal_get(&terminal_id)
            .await?
            .ok_or_else(|| CalmError::Internal(format!("terminal {terminal_id} vanished")))?;
        let is_prompted = output_prompt(output)?.is_some();
        let command_line = if is_prompted {
            let thread_id = output.output_string("codex_thread_id", "codex")?;
            format!(
                "codex resume {} --remote {}",
                shell_single_quote(&thread_id),
                shell_single_quote(&self.shared_codex_appserver.remote_uri()),
            )
        } else {
            format!(
                "codex --remote {}",
                shell_single_quote(&self.shared_codex_appserver.remote_uri()),
            )
        };

        if !is_prompted {
            let _pending_spawn_serial_guard = self.pending_codex_threads_spawn_serial.lock().await;
            self.pending_codex_threads
                .register(PendingEntry::new(
                    card_id,
                    Some(wave_id),
                    terminal_id.clone(),
                    runtime_id.clone(),
                ))
                .await?;

            #[cfg(feature = "fixtures")]
            if let Some(hook) = &self.spawn_hook {
                return hook(terminal_id, command_line, cwd, env)
                    .await
                    .map(SpawnOutcome::Ready);
            }

            return ctx
                .spawn_terminal(&term, &command_line, &cwd, &env)
                .await
                .map(SpawnOutcome::Ready);
        }

        #[cfg(feature = "fixtures")]
        if let Some(hook) = &self.spawn_hook {
            return hook(terminal_id, command_line, cwd, env)
                .await
                .map(SpawnOutcome::Ready);
        }

        ctx.spawn_terminal(&term, &command_line, &cwd, &env)
            .await
            .map(SpawnOutcome::Ready)
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        _op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        let card_id = output.output_string("card_id", "codex")?;
        let terminal_id = output.output_string("terminal_id", "codex")?;
        let mut steps = vec![CompensationStep::new(
            "reap_terminal_pty",
            json!({ "terminal_id": terminal_id }),
        )];
        if output_prompt(output)?.is_some() {
            steps.push(CompensationStep::new(
                "interrupt_shared_codex_turn",
                json!({
                    "card_id": card_id.clone(),
                    "thread_id": output.output_optional_string("codex_thread_id", "codex")?,
                }),
            ));
            steps.push(CompensationStep::new(
                "session_projection_set_status_failed_for_card",
                json!({ "card_id": card_id }),
            ));
        } else {
            let runtime_id = output.output_string("runtime_id", "codex")?;
            steps.push(CompensationStep::new(
                "pending_codex_threads_remove_by_card",
                json!({ "card_id": card_id.clone(), "runtime_id": runtime_id }),
            ));
            steps.push(CompensationStep::new(
                "card_payload_clear_pending_status",
                json!({ "card_id": card_id }),
            ));
        }

        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.to_string(),
            steps,
        })
    }

    async fn compensate_step(
        &self,
        step: &CompensationStep,
        output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()> {
        if step.completed {
            return Ok(());
        }
        match step.op.as_str() {
            "reap_terminal_pty" => {
                let terminal_id = step.arg_string("terminal_id", "codex")?;
                if let Some(term) = ctx.repo.terminal_get(&terminal_id).await? {
                    reap_terminal_artifacts_with_renderer(
                        Some(ctx.terminal_renderer.as_ref()),
                        &term,
                    )
                    .await;
                }
                Ok(())
            }
            "interrupt_shared_codex_turn" => {
                let card_id = step.arg_string("card_id", "codex")?;
                let thread_id =
                    if let Some(thread_id) = step.args.get("thread_id").and_then(Value::as_str) {
                        Some(thread_id.to_string())
                    } else {
                        ctx.repo
                            .session_projection_active_for_card(&card_id)
                            .await?
                            .and_then(|runtime| {
                                TxOutput::non_empty_string(runtime.thread_id.as_deref())
                            })
                    };
                if let Some(thread_id) = thread_id
                    && let Err(e) = self
                        .shared_codex_appserver
                        .interrupt_active_turn(&thread_id)
                        .await
                {
                    tracing::warn!(
                        card_id = %card_id,
                        thread_id = %thread_id,
                        error = %e,
                        "prompted codex compensation could not interrupt active shared turn"
                    );
                }
                Ok(())
            }
            // Back-compat: operations that entered `compensating` under a pre-PR10-d
            // release persisted the legacy op string; accept it during recovery so
            // in-flight compensation states still drain. New states write the new name.
            "session_projection_set_status_failed_for_card"
            | "runtime_set_status_failed_for_card" => {
                let card_id = step.arg_string("card_id", "codex")?;
                ctx.repo
                    .session_projection_complete_for_card(&card_id, WorkerSessionState::Failed)
                    .await?;
                Ok(())
            }
            "pending_codex_threads_remove_by_card" => {
                let runtime_id = output.output_string("runtime_id", "codex")?;
                self.pending_codex_threads
                    .remove_by_runtime(&runtime_id)
                    .await;
                Ok(())
            }
            "card_payload_clear_pending_status" => {
                let card_id = step.arg_string("card_id", "codex")?;
                let runtime_id = output.output_string("runtime_id", "codex")?;
                match crate::pending_codex_threads::card_payload_clear_pending_status(
                    ctx.repo.as_ref(),
                    &ctx.events,
                    &card_id,
                    &runtime_id,
                )
                .await
                {
                    Ok(()) | Err(CalmError::NotFound(_)) => Ok(()),
                    Err(e) => Err(e),
                }
            }
            "delete_card_codex_thread" => {
                tracing::warn!(
                    target = "codex_adapter::compensation",
                    "skipping legacy delete_card_codex_thread step (table dropped post #552)"
                );
                Ok(())
            }
            other => Err(CalmError::Internal(format!(
                "unknown codex compensation op {other}"
            ))),
        }
    }
}

#[async_trait]
impl ProviderAdapter for CodexWorkerAdapter {
    fn kind(&self) -> &'static str {
        "codex-worker"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        &[
            PhaseTag::Pending,
            PhaseTag::TxCommitted,
            PhaseTag::AppServerInteract,
            PhaseTag::SpawnStarted,
            PhaseTag::SpawnSucceeded,
            PhaseTag::Succeeded,
        ]
    }

    fn app_server_interact_kind(
        &self,
        output: &TxOutput,
        _op: &Operation,
    ) -> Result<AppServerInteractKind> {
        Ok(AppServerInteractKind::RegisterPending {
            entry_id: Some(output.output_string("card_id", "codex-worker")?),
        })
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: CodexWorkerOperationPayload = serde_json::from_value(input.clone())?;
        if payload.idempotency_key.trim().is_empty() {
            return Err(CalmError::BadRequest(
                "codex worker idempotency_key must not be empty".into(),
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
        op: &Operation,
    ) -> Result<TxOutput> {
        let payload: CodexWorkerOperationPayload = serde_json::from_value(input.clone())?;
        let card_id = new_id();
        let runtime_id = new_id();
        let wave_id = WaveId::from(payload.wave_id.clone());
        // `payload.cwd` is forward-compatible only; the isolated lease path is
        // authoritative for codex-worker execution.
        let lease_target =
            prepare_workspace_lease_target_tx(tx, wave_id.as_str(), &card_id).await?;
        let cwd = lease_target.path_string();
        let env = build_codex_env(self.repo.as_ref(), self.codex.as_ref(), &card_id).await?;
        let rendered_prompt = render_worker_prompt(
            &payload.goal,
            &payload.context,
            payload.acceptance_criteria.as_deref(),
        );
        let scope = card_scope(
            self.repo.as_ref(),
            CardId::from(card_id.clone()),
            wave_id.clone(),
        )
        .await?;

        let (mut card, term, _mcp_token) = card_with_codex_create_tx(
            tx,
            card_id.clone(),
            &runtime_id,
            Some(op.id.as_str()),
            wave_id,
            None,
            cwd.clone(),
            env.clone(),
            None,
            None,
            None,
            CardRole::Worker,
            true,
            &self.card_role_cache,
            RequestTheme::default_dark(),
        )
        .await?;

        let (lease, lease_event) =
            acquire_workspace_lease_tx(tx, &card_id, card.wave_id.as_str(), &op.id, &lease_target)
                .await?;

        if let Some(existing_map) = card.payload.as_object() {
            let mut merged = existing_map.clone();
            merged.insert(
                "idempotency_key".into(),
                Value::String(payload.idempotency_key.clone()),
            );
            merged.insert("role_request".into(), Value::String("codex".into()));
            merged.insert("goal".into(), Value::String(payload.goal.clone()));
            merged.insert("context".into(), payload.context.clone());
            if let Some(ac) = payload.acceptance_criteria.as_ref() {
                merged.insert("acceptance_criteria".into(), Value::String(ac.clone()));
            }
            merged.insert("prompt".into(), Value::String(rendered_prompt.clone()));
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
            "cwd": cwd,
            "lease_id": lease.lease_id,
            "repo_root": lease_target.repo_root_string(),
            "slice_branch": lease_target.branch,
            "worktree_provisioned_event_persisted": false,
            "runtime_started_event_persisted": false,
            "env": env,
            "prompt": rendered_prompt,
            "scope": scope,
        });
        output.post_commit_events.push(lease_event);
        Ok(output)
    }

    async fn app_server_interact(
        &self,
        output: &mut TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        provision_codex_worker_workspace(
            ctx,
            &self.card_role_cache,
            &self.wave_cove_cache,
            op,
            output,
        )
        .await?;
        Ok(AppServerInteractOutcome::NotApplicable)
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        let card_id = output.output_string("card_id", "codex")?;
        let runtime_id = output.output_string("runtime_id", "codex")?;
        let terminal_id = output.output_string("terminal_id", "codex")?;
        let wave_id = WaveId::from(output.output_string("wave_id", "codex")?);
        let cwd = output.output_string("cwd", "codex")?;
        let rendered_prompt = output.output_string("prompt", "codex")?;
        let env = output.data.get("env").cloned().unwrap_or_else(|| json!({}));

        let term = ctx
            .repo
            .terminal_get(&terminal_id)
            .await?
            .ok_or_else(|| CalmError::Internal(format!("terminal {terminal_id} vanished")))?;
        if term.exit_code.is_some() || term.signal_killed {
            tracing::info!(
                card_id = %card_id,
                terminal_id = %terminal_id,
                exit_code = ?term.exit_code,
                signal_killed = term.signal_killed,
                "codex-worker recovery: worker already exited; skipping respawn",
            );
            log_worker_card_added(
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
                    "codex worker CardAdded append failed after recovery exit preservation; continuing"
                );
            });
            return Ok(SpawnOutcome::Ready(SpawnHandle::NoOp));
        }

        if !self.shared_codex_appserver.is_running() {
            return Err(CalmError::Internal(
                "shared codex app-server is not running".into(),
            ));
        }

        let card = ctx
            .repo
            .card_get(&card_id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
        let mcp_token = mint_card_mcp_token(ctx, &card_id, &runtime_id).await?;

        let handle = spawn_codex_worker_via_shared_daemon(CodexWorkerSpawnCtx {
            spawn_ctx: ctx,
            shared_codex_appserver: &self.shared_codex_appserver,
            mcp_server: self.mcp_server.as_deref(),
            card: &card,
            term: &term,
            runtime_id: &runtime_id,
            wave_id: &wave_id,
            mcp_token: Some(mcp_token.as_str()),
            rendered_prompt: &rendered_prompt,
            cwd: &cwd,
            legacy_env: &env,
        })
        .await?;

        log_worker_card_added(
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
                "codex worker CardAdded append failed after live spawn; continuing"
            );
        });
        Ok(SpawnOutcome::Ready(handle))
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        _op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        let mut steps = Vec::new();
        if let Some(lease_id) = output.output_optional_string("lease_id", "codex")? {
            steps.push(CompensationStep::new(
                "remove_workspace_artifact",
                json!({ "lease_id": lease_id.clone() }),
            ));
            steps.push(CompensationStep::new(
                "release_workspace_lease",
                json!({ "lease_id": lease_id }),
            ));
        }
        steps.push(CompensationStep::new(
            "cleanup_codex_worker",
            json!({
                "card_id": output.output_string("card_id", "codex")?,
                "terminal_id": output.output_string("terminal_id", "codex")?,
            }),
        ));
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.to_string(),
            steps,
        })
    }

    async fn compensate_step(
        &self,
        step: &CompensationStep,
        output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()> {
        if step.completed {
            return Ok(());
        }
        if step.op == "remove_workspace_artifact" {
            let lease_id = step.arg_string("lease_id", "codex")?;
            let pool = ctx.operation_repo.sqlite_pool();
            remove_workspace_artifact_for_lease_by_id(&pool, &ctx.events, &lease_id).await?;
            return Ok(());
        }
        if step.op == "release_workspace_lease" {
            let lease_id = step.arg_string("lease_id", "codex")?;
            let pool = ctx.operation_repo.sqlite_pool();
            release_workspace_lease_by_id(&pool, &ctx.events, &lease_id).await?;
            return Ok(());
        }
        if step.op != "cleanup_codex_worker" {
            return Err(CalmError::Internal(format!(
                "unknown codex worker compensation op {}",
                step.op
            )));
        }
        let card_id = step.arg_string("card_id", "codex")?;
        let terminal_id = step.arg_string("terminal_id", "codex")?;
        let runtime_id = output.output_string("runtime_id", "codex")?;
        let runtime_turn = ctx
            .repo
            .session_projection_by_id(&runtime_id)
            .await?
            .and_then(|runtime| {
                TxOutput::non_empty_string(runtime.thread_id.as_deref()).map(|thread_id| {
                    (
                        thread_id,
                        TxOutput::non_empty_string(runtime.active_turn_id.as_deref()),
                    )
                })
            });
        let outcome = compensate_worker_rows(
            ctx.repo.as_ref(),
            ctx.terminal_renderer.as_ref(),
            &self.card_role_cache,
            &card_id,
            &terminal_id,
        )
        .await;
        if outcome == WorkerCleanupOutcome::Deleted
            && let Some((thread_id, persisted_turn)) = runtime_turn
        {
            let cached_turn = self
                .shared_codex_appserver
                .active_turn_id_for_thread(&thread_id);
            if let Err(e) = self
                .shared_codex_appserver
                .interrupt_active_turn(&thread_id)
                .await
            {
                let turn_id = cached_turn
                    .as_deref()
                    .or(persisted_turn.as_deref())
                    .unwrap_or("");
                tracing::warn!(
                    runtime_id = %runtime_id,
                    thread_id = %thread_id,
                    turn_id = %turn_id,
                    error = %e,
                    "worker compensation replay thread interrupt failed"
                );
            }
            if cached_turn.is_none()
                && let Some(persisted_turn) = persisted_turn.as_deref()
                && let Err(e) = self
                    .shared_codex_appserver
                    .turn_interrupt(&thread_id, persisted_turn)
                    .await
            {
                tracing::warn!(
                    runtime_id = %runtime_id,
                    thread_id = %thread_id,
                    turn_id = persisted_turn,
                    error = %e,
                    "worker compensation replay persisted-turn interrupt failed"
                );
            }
        }
        Ok(())
    }
}

pub(crate) struct CodexWorkerSpawnCtx<'a> {
    pub(crate) spawn_ctx: &'a SpawnCtx,
    pub(crate) shared_codex_appserver: &'a Arc<SharedCodexAppServer>,
    pub(crate) mcp_server: Option<&'a McpServer>,
    pub(crate) card: &'a Card,
    pub(crate) term: &'a crate::model::Terminal,
    pub(crate) runtime_id: &'a str,
    pub(crate) wave_id: &'a WaveId,
    pub(crate) mcp_token: Option<&'a str>,
    pub(crate) rendered_prompt: &'a str,
    pub(crate) cwd: &'a str,
    pub(crate) legacy_env: &'a Value,
}

pub(crate) async fn spawn_codex_worker_via_shared_daemon(
    ctx: CodexWorkerSpawnCtx<'_>,
) -> Result<SpawnHandle> {
    let mut notifications = ctx.shared_codex_appserver.subscribe_notifications();
    let remote_uri = ctx.shared_codex_appserver.remote_uri();
    let card_id = ctx.card.id.as_str();
    let runtime_id = ctx.runtime_id.to_string();
    let runtime = ctx
        .spawn_ctx
        .repo
        .session_projection_by_id(&runtime_id)
        .await?
        .ok_or_else(|| CalmError::Internal(format!("worker runtime {runtime_id} vanished")))?;
    let persisted_turn_id = TxOutput::non_empty_string(runtime.active_turn_id.as_deref());
    let worker_instructions = crate::spec_card::render_system_prompt(
        crate::spec_card::SeededCardRole::Worker.prompt_template(),
        ctx.wave_id.as_str(),
    );
    let thread_id =
        if let Some(thread_id) = TxOutput::non_empty_string(runtime.thread_id.as_deref()) {
            thread_id
        } else {
            // The worker's AI exec-shells only receive NEIGE_MCP_SOCKET /
            // NEIGE_MCP_TOKEN via the per-thread `shell_environment_policy.set`
            // config — codex does NOT inherit the daemon process env into
            // exec-shells, and the daemon `env_remove`s NEIGE_MCP_TOKEN from
            // itself. Without this, the mandated `neige task-completed` CLI (and
            // every `neige` read) fails and the worker can never report
            // task.complete (#836). Use the SAME shim socket already used below
            // for the terminal viewer env so the daemon's shim socket matches.
            let config = match (ctx.mcp_token, ctx.mcp_server) {
                (Some(token), Some(server)) => Some(card_mcp_thread_start_config(
                    &server.shim_config.socket_path,
                    token,
                )),
                _ => None,
            };
            let thread_id = ctx
                .shared_codex_appserver
                .thread_start_mint_for_card(
                    card_id,
                    SharedThreadStartParams {
                        cwd: ctx.cwd.to_string(),
                        approval_policy: "never".into(),
                        sandbox_mode: "workspace-write".into(),
                        developer_instructions: Some(worker_instructions),
                        config,
                    },
                )
                .await?;
            tracing::info!(
                target: "shared_codex_daemon::worker",
                card_id,
                wave_id = %ctx.wave_id,
                thread_id = %thread_id,
                "thread_start_succeeded"
            );
            thread_id
        };

    persist_shared_worker_runtime_fields(
        ctx.spawn_ctx,
        ctx.card,
        ctx.runtime_id,
        &thread_id,
        &remote_uri,
        persisted_turn_id.as_deref(),
    )
    .await?;

    if persisted_turn_id.is_none() {
        let initial_turn_result = async {
            let turn_id = ctx
                .shared_codex_appserver
                .turn_start(
                    &thread_id,
                    vec![InputItem::text(ctx.rendered_prompt.trim())],
                )
                .await?;
            persist_shared_worker_runtime_fields(
                ctx.spawn_ctx,
                ctx.card,
                ctx.runtime_id,
                &thread_id,
                &remote_uri,
                Some(&turn_id),
            )
            .await?;
            await_shared_worker_initial_turn_started(&mut notifications, &thread_id).await?;
            Ok::<(), CalmError>(())
        }
        .await;
        if let Err(e) = initial_turn_result {
            tracing::warn!(
                target: "shared_codex_daemon::worker",
                card_id,
                wave_id = %ctx.wave_id,
                thread_id = %thread_id,
                error = %e,
                "turn_start_failed"
            );
            return Err(e);
        }
    }

    let mut env_for_spawn = ctx.legacy_env.clone();
    if let Some(map) = env_for_spawn.as_object_mut() {
        map.insert(
            "CODEX_HOME".into(),
            Value::String(ctx.shared_codex_appserver.status_snapshot().codex_home),
        );
        if let (Some(token), Some(server)) = (ctx.mcp_token, ctx.mcp_server) {
            for (key, value) in card_mcp_env(&server.shim_config.socket_path, token) {
                map.insert(key.into(), Value::String(value));
            }
        }
    }

    let command_line = format!(
        "codex resume {} --remote {}",
        shell_single_quote(&thread_id),
        shell_single_quote(&remote_uri)
    );
    match ctx
        .spawn_ctx
        .spawn_terminal(ctx.term, &command_line, ctx.cwd, &env_for_spawn)
        .await
    {
        Ok(handle) => {
            tracing::info!(
                target: "shared_codex_daemon::worker",
                card_id,
                wave_id = %ctx.wave_id,
                terminal_id = %ctx.term.id,
                thread_id = %thread_id,
                "worker_spawn_succeeded"
            );
            Ok(handle)
        }
        Err(e)
            if worker_spawn_failure_preserved(ctx.spawn_ctx.repo.as_ref(), &ctx.term.id)
                .await? =>
        {
            tracing::info!(
                target: "shared_codex_daemon::worker",
                card_id,
                wave_id = %ctx.wave_id,
                terminal_id = %ctx.term.id,
                thread_id = %thread_id,
                spawn_err = %e,
                "worker shared TUI fast-exit; preserving card + terminal"
            );
            Ok(SpawnHandle::NoOp)
        }
        Err(e) => Err(e),
    }
}

async fn mint_card_mcp_token(ctx: &SpawnCtx, card_id: &str, runtime_id: &str) -> Result<String> {
    let card_id = card_id.to_string();
    let runtime_id = runtime_id.to_string();
    write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        let card_id = card_id.clone();
        let runtime_id = runtime_id.clone();
        Box::pin(async move { mint_and_persist_card_token(tx, &card_id, &runtime_id).await })
    })
    .await
}

async fn log_worker_card_added(
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

async fn await_shared_worker_initial_turn_started(
    rx: &mut tokio::sync::broadcast::Receiver<crate::codex_appserver::Notification>,
    thread_id: &str,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            tracing::warn!(
                target: "shared_codex_daemon::worker",
                thread_id,
                "timed out awaiting initial turn/started; continuing best-effort"
            );
            return Ok(());
        }
        match tokio::time::timeout(deadline - now, rx.recv()).await {
            Ok(Ok(n)) => {
                if n.thread_id() == Some(thread_id)
                    && matches!(n, crate::codex_appserver::Notification::TurnStarted { .. })
                {
                    return Ok(());
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped))) => {
                tracing::warn!(
                    target: "shared_codex_daemon::worker",
                    skipped,
                    thread_id,
                    "shared worker initial lifecycle subscriber lagged"
                );
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                return Err(CalmError::CodexAppServer(format!(
                    "shared app-server notification channel closed before initial lifecycle for {thread_id}"
                )));
            }
            Err(_) => {
                tracing::warn!(
                    target: "shared_codex_daemon::worker",
                    thread_id,
                    "timed out awaiting initial turn/started; continuing best-effort"
                );
                return Ok(());
            }
        }
    }
}

async fn persist_shared_worker_runtime_fields(
    ctx: &SpawnCtx,
    card: &Card,
    runtime_id: &str,
    thread_id: &str,
    remote_uri: &str,
    active_turn_id: Option<&str>,
) -> Result<()> {
    let card_id_for_tx = card.id.to_string();
    let runtime_id_for_tx = runtime_id.to_string();
    let thread_id_for_tx = thread_id.to_string();
    let active_turn_id_for_tx = active_turn_id.map(ToOwned::to_owned);
    let remote_uri_for_tx = remote_uri.to_string();
    write_in_tx_typed::<Card, _>(ctx.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let mut payload = card_payload_get_tx(tx, &card_id_for_tx).await?;
            let Some(map) = payload.as_object_mut() else {
                return Err(CalmError::Internal(format!(
                    "worker card {card_id_for_tx} payload is not a JSON object; cannot persist shared codex runtime fields"
                )));
            };
            map.insert("appserver_sock".into(), Value::String(remote_uri_for_tx));
            map.remove("appserver_pgid");
            let updated = card_update_tx(
                tx,
                &card_id_for_tx,
                crate::model::CardPatch {
                    kind: None,
                    sort: None,
                    payload: Some(payload),
                    deletable: None,
                },
            )
            .await?;
            let runtime = session_projection_by_id_tx(tx, &runtime_id_for_tx)
                .await?
                .ok_or_else(|| {
                    CalmError::Internal(format!(
                        "worker runtime {runtime_id_for_tx} vanished before shared codex bind"
                    ))
                })?;
            let old_status = runtime.status;
            session_bind_attribution_tx(
                tx,
                &runtime.id,
                ThreadAttribution {
                    runtime_id: runtime.id.clone(),
                    provider: AgentProvider::Codex,
                    thread_id: Some(thread_id_for_tx),
                    session_id: None,
                    active_turn_id: active_turn_id_for_tx,
                },
            )
            .await?;
            if old_status != WorkerSessionState::Running {
                session_set_status_tx(tx, &runtime.id, WorkerSessionState::Running).await?;
            }
            Ok(updated)
        })
    })
    .await?;
    Ok(())
}

async fn card_payload_get_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card_id: &str,
) -> Result<Value> {
    let row: Option<(String,)> = sqlx::query_as("SELECT payload FROM cards WHERE id = ?1")
        .bind(card_id)
        .fetch_optional(&mut **tx)
        .await?;
    let payload_text = row
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?
        .0;
    serde_json::from_str(&payload_text)
        .map_err(|e| CalmError::Internal(format!("card {card_id} payload is not valid JSON: {e}")))
}

pub(crate) fn render_worker_prompt(
    goal: &str,
    context: &Value,
    acceptance_criteria: Option<&str>,
) -> String {
    let mut out = String::new();
    out.push_str("Goal:\n");
    out.push_str(goal);

    let context_str = match context {
        Value::Null => String::new(),
        Value::String(s) if s.trim().is_empty() => String::new(),
        Value::Object(m) if m.is_empty() => String::new(),
        Value::Array(a) if a.is_empty() => String::new(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    };
    if !context_str.is_empty() {
        out.push_str("\n\nContext:\n");
        out.push_str(&context_str);
    }

    if let Some(ac) = acceptance_criteria.map(str::trim).filter(|s| !s.is_empty()) {
        out.push_str("\n\nAcceptance criteria:\n");
        out.push_str(ac);
    }
    out
}

async fn build_codex_env(
    repo: &dyn crate::db::RouteRepo,
    codex: &CodexClient,
    card_id: &str,
) -> Result<Value> {
    let settings = load_settings(repo).await?;
    let mut env_map = serde_json::Map::new();
    env_map.insert(
        "CODEX_HOME".to_string(),
        Value::String(codex.codex_home_dir().to_string_lossy().to_string()),
    );
    env_map.insert(
        "NEIGE_CARD_ID".to_string(),
        Value::String(card_id.to_string()),
    );
    env_map.insert(
        "NEIGE_CALM_BASE_URL".to_string(),
        Value::String(codex.ingest_url.clone()),
    );
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

async fn provision_codex_worker_workspace(
    ctx: &SpawnCtx,
    card_role_cache: &CardRoleCache,
    wave_cove_cache: &WaveCoveCache,
    op: &Operation,
    output: &mut TxOutput,
) -> Result<()> {
    let card_id = output.output_string("card_id", "codex-worker")?;
    let wave_id = output.output_string("wave_id", "codex-worker")?;
    let runtime_id = output.output_string("runtime_id", "codex-worker")?;
    let cwd = output.output_string("cwd", "codex-worker")?;
    let repo_root = output.output_optional_string("repo_root", "codex-worker")?;
    let branch = output.output_optional_string("slice_branch", "codex-worker")?;
    let Some((repo_root, branch)) = repo_root.zip(branch) else {
        tracing::info!(
            operation_id = %op.id,
            card_id = %card_id,
            wave_id = %wave_id,
            "codex-worker legacy tx_output has no worktree target; skipping workspace provisioning"
        );
        return Ok(());
    };
    let target = WorkspaceLeaseTarget {
        repo_root: PathBuf::from(repo_root),
        path: PathBuf::from(cwd.clone()),
        branch,
    };
    provision_workspace_worktree(&target)?;

    let provisioned_persisted = output
        .data
        .get("worktree_provisioned_event_persisted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let runtime_started_persisted = output
        .data
        .get("runtime_started_event_persisted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if provisioned_persisted && runtime_started_persisted {
        return Ok(());
    }

    let scope = card_scope(
        ctx.repo.as_ref(),
        CardId::from(card_id.clone()),
        WaveId::from(wave_id.clone()),
    )
    .await?;
    let mut checkpoint_output = output.clone();
    checkpoint_output.set_output_data(
        "worktree_provisioned_event_persisted",
        json!(true),
        "codex-worker",
    )?;
    checkpoint_output.set_output_data(
        "runtime_started_event_persisted",
        json!(true),
        "codex-worker",
    )?;

    let card_id_for_tx = card_id.clone();
    let wave_id_for_tx = wave_id.clone();
    let runtime_id_for_tx = runtime_id.clone();
    let cwd_for_tx = cwd.clone();
    let op_for_tx = op.clone();
    let output_for_tx = checkpoint_output.clone();
    let write = WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone());
    write_with_events_typed(
        ctx.repo.as_ref(),
        ActorId::KernelDispatcher,
        None,
        &ctx.events,
        &write,
        move |tx| {
            Box::pin(async move {
                checkpoint_app_server_interact_tx(
                    tx,
                    &op_for_tx,
                    AppServerInteractKind::RegisterPending {
                        entry_id: Some(card_id_for_tx.clone()),
                    },
                    &output_for_tx,
                )
                .await?;
                let mut events = Vec::new();
                if !provisioned_persisted {
                    events.push((
                        scope.clone(),
                        Event::WorktreeProvisioned {
                            wave_id: WaveId::from(wave_id_for_tx.clone()),
                            card_id: CardId::from(card_id_for_tx.clone()),
                            path: cwd_for_tx.clone(),
                        },
                    ));
                }
                if !runtime_started_persisted {
                    events.push((
                        scope,
                        Event::RuntimeStarted {
                            runtime_id: runtime_id_for_tx,
                            card_id: card_id_for_tx,
                            kind: WorkerSessionKind::CodexCard,
                            agent_provider: Some(AgentProvider::Codex),
                            status: WorkerSessionState::Starting,
                        },
                    ));
                }
                Ok(((), events))
            })
        },
    )
    .await?;
    *output = checkpoint_output;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn persist_prompt_thread(
    ctx: &SpawnCtx,
    card_role_cache: &CardRoleCache,
    wave_cove_cache: &WaveCoveCache,
    op: &Operation,
    output: &TxOutput,
    actor: ActorId,
    card_id: &str,
    thread_id: &str,
) -> Result<Card> {
    let card = ctx
        .repo
        .card_get(card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;

    let scope = card_scope(ctx.repo.as_ref(), card.id.clone(), card.wave_id.clone()).await?;
    let card_id_for_tx = card_id.to_string();
    let thread_id_for_tx = thread_id.to_string();
    let card_for_event = card;
    let op_for_tx = op.clone();
    let output_for_tx = output.clone();
    let write = WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone());
    let (updated, _ids) = write_with_events_typed(
        ctx.repo.as_ref(),
        actor,
        None,
        &ctx.events,
        &write,
        move |tx| {
            Box::pin(async move {
                let runtime = session_projection_active_for_card_tx(tx, &card_id_for_tx)
                    .await?
                    .ok_or_else(|| {
                        CalmError::Internal(format!(
                            "codex card {card_id_for_tx} has no active runtime to bind"
                        ))
                    })?;
                let terminal_id_for_projection = runtime.terminal_run_id.clone();
                session_bind_attribution_tx(
                    tx,
                    &runtime.id,
                    ThreadAttribution {
                        runtime_id: runtime.id.clone(),
                        provider: AgentProvider::Codex,
                        thread_id: Some(thread_id_for_tx.clone()),
                        session_id: None,
                        active_turn_id: None,
                    },
                )
                .await?;
                let old_status = runtime.status;
                let runtime_id = runtime.id.clone();
                if runtime.status != WorkerSessionState::Running {
                    session_set_status_tx(tx, &runtime.id, WorkerSessionState::Running).await?;
                }
                let card = project_codex_runtime_fields_for_response(
                    card_for_event,
                    terminal_id_for_projection.as_deref(),
                    Some(&thread_id_for_tx),
                    Some("started"),
                );
                let mut checkpoint_output = output_for_tx.clone();
                checkpoint_output.result = serde_json::to_value(&card)?;
                checkpoint_app_server_interact_tx(
                    tx,
                    &op_for_tx,
                    AppServerInteractKind::MintAndAwait {
                        thread_id: Some(thread_id_for_tx.clone()),
                    },
                    &checkpoint_output,
                )
                .await?;
                let mut events = vec![(scope.clone(), Event::CardUpdated(card.clone()))];
                if old_status != WorkerSessionState::Running {
                    events.push((
                        scope,
                        Event::RuntimeStatusChanged {
                            runtime_id,
                            card_id: card_id_for_tx,
                            old_status,
                            new_status: WorkerSessionState::Running,
                        },
                    ));
                }
                Ok((card, events))
            })
        },
    )
    .await?;
    Ok(updated)
}

async fn checkpoint_prompt_turn_started(
    ctx: &SpawnCtx,
    op: &Operation,
    output: &TxOutput,
    thread_id: &str,
) -> Result<()> {
    let op_for_tx = op.clone();
    let output_for_tx = output.clone();
    let thread_id_for_tx = thread_id.to_string();
    write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        Box::pin(async move {
            checkpoint_app_server_interact_tx(
                tx,
                &op_for_tx,
                AppServerInteractKind::MintAndAwait {
                    thread_id: Some(thread_id_for_tx),
                },
                &output_for_tx,
            )
            .await
        })
    })
    .await
}

async fn persist_pending_thread_status(
    ctx: &SpawnCtx,
    card_role_cache: &CardRoleCache,
    wave_cove_cache: &WaveCoveCache,
    op: &Operation,
    output: &TxOutput,
    actor: ActorId,
    card_id: &str,
) -> Result<Card> {
    let card = ctx
        .repo
        .card_get(card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;

    let scope = card_scope(ctx.repo.as_ref(), card.id.clone(), card.wave_id.clone()).await?;
    let card_id_for_tx = card_id.to_string();
    let card_for_event = card;
    let op_for_tx = op.clone();
    let output_for_tx = output.clone();
    let write = WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone());
    let (updated, _ids) = write_with_events_typed(
        ctx.repo.as_ref(),
        actor,
        None,
        &ctx.events,
        &write,
        move |tx| {
            Box::pin(async move {
                let runtime = session_projection_active_for_card_tx(tx, &card_id_for_tx)
                    .await?
                    .ok_or_else(|| {
                        CalmError::Internal(format!(
                            "codex card {card_id_for_tx} has no active runtime to mark pending"
                        ))
                    })?;
                let terminal_id_for_projection = runtime.terminal_run_id.clone();
                let old_status = runtime.status;
                let runtime_id = runtime.id.clone();
                if old_status != WorkerSessionState::TurnPending {
                    session_set_status_tx(tx, &runtime.id, WorkerSessionState::TurnPending).await?;
                }
                let card = project_codex_runtime_fields_for_response(
                    card_for_event,
                    terminal_id_for_projection.as_deref(),
                    None,
                    Some("pending_thread_start"),
                );
                let mut checkpoint_output = output_for_tx.clone();
                checkpoint_output.result = serde_json::to_value(&card)?;
                checkpoint_app_server_interact_tx(
                    tx,
                    &op_for_tx,
                    AppServerInteractKind::RegisterPending {
                        entry_id: Some(card_id_for_tx.clone()),
                    },
                    &checkpoint_output,
                )
                .await?;
                let mut events = vec![(scope.clone(), Event::CardUpdated(card.clone()))];
                if old_status != WorkerSessionState::TurnPending {
                    events.push((
                        scope,
                        Event::RuntimeStatusChanged {
                            runtime_id,
                            card_id: card_id_for_tx,
                            old_status,
                            new_status: WorkerSessionState::TurnPending,
                        },
                    ));
                }
                Ok((card, events))
            })
        },
    )
    .await?;
    Ok(updated)
}

fn project_codex_runtime_fields_for_response(
    mut card: Card,
    terminal_id: Option<&str>,
    thread_id: Option<&str>,
    thread_status: Option<&str>,
) -> Card {
    if let Some(map) = card.payload.as_object_mut() {
        if let Some(terminal_id) = terminal_id {
            insert_payload_string(map, "terminal_id", terminal_id);
        }
        if let Some(thread_id) = thread_id {
            insert_payload_string(map, "codex_thread_id", thread_id);
        }
        if let Some(thread_status) = thread_status {
            insert_payload_string(map, "codex_thread_status", thread_status);
        }
    }
    card
}

fn insert_payload_string(map: &mut serde_json::Map<String, Value>, key: &str, value: &str) {
    map.entry(key.to_string())
        .or_insert_with(|| Value::String(value.to_string()));
}

fn validate_optional_color(value: Option<&str>, field: &str) -> Result<()> {
    if let Some(value) = value {
        normalize_optional_css_color(Some(value), field)?;
    }
    Ok(())
}

fn output_prompt(output: &TxOutput) -> Result<Option<String>> {
    match output.data.get("prompt") {
        Some(Value::String(prompt)) => Ok(Some(prompt.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(CalmError::Internal(
            "codex tx_output prompt must be string or null".into(),
        )),
    }
}

fn output_optional_i64(output: &TxOutput, key: &str) -> Result<Option<i64>> {
    match output.data.get(key) {
        Some(Value::Number(value)) => value.as_i64().map(Some).ok_or_else(|| {
            CalmError::Internal(format!("codex tx_output {key} must be a signed integer"))
        }),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(CalmError::Internal(format!(
            "codex tx_output {key} must be a signed integer or null"
        ))),
    }
}

fn phase_minted_thread_id(phase: &Phase) -> Option<String> {
    match phase {
        Phase::AppServerInteract {
            kind:
                AppServerInteractKind::MintAndAwait {
                    thread_id: Some(thread_id),
                },
        } => Some(thread_id.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::begin_immediate_tx;
    use crate::event::EventBus;
    use crate::operation::workspace_lease::release_workspace_lease_for_card_repo;
    use crate::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
    use sqlx::Row;
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;

    struct WorkerLeaseHarness {
        repo: Arc<crate::db::sqlite::SqlxRepo>,
        adapter: CodexWorkerAdapter,
        wave_id: String,
        events: EventBus,
        repo_root: tempfile::TempDir,
    }

    async fn worker_lease_harness() -> WorkerLeaseHarness {
        let repo_root = tempfile::tempdir().unwrap();
        init_git_repo(repo_root.path());
        let repo = Arc::new(
            crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
                .await
                .unwrap(),
        );
        let cove = crate::db::RepoSyncDomainRaw::cove_create(
            repo.as_ref(),
            crate::model::NewCove {
                name: "workspace leases".into(),
                color: "#101010".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
        let wave = crate::db::RepoSyncDomainRaw::wave_create(
            repo.as_ref(),
            crate::model::NewWave {
                cove_id: cove.id,
                title: "workspace leases".into(),
                sort: None,
                cwd: repo_root.path().display().to_string(),
                workflow_id: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
        )
        .await
        .unwrap();
        let route_repo: Arc<dyn crate::db::RouteRepo> = repo.clone();
        let full_repo: Arc<dyn crate::db::Repo> = repo.clone();
        WorkerLeaseHarness {
            adapter: CodexWorkerAdapter::new(
                route_repo,
                Arc::new(CodexClient::new_stub()),
                SharedCodexAppServer::new_stub(full_repo),
                None,
                CardRoleCache::new(),
                WaveCoveCache::new(),
            ),
            repo,
            wave_id: wave.id.to_string(),
            events: EventBus::new(),
            repo_root,
        }
    }

    fn worker_payload(wave_id: &str, key: &str) -> Value {
        serde_json::to_value(CodexWorkerOperationPayload {
            actor: ActorId::KernelDispatcher,
            wave_id: wave_id.to_string(),
            idempotency_key: format!("{wave_id}:{key}"),
            goal: format!("do {key}"),
            cwd: None,
            context: Value::Null,
            acceptance_criteria: None,
        })
        .unwrap()
    }

    #[test]
    fn codex_worker_payload_omits_none_cwd_for_hash_stability() {
        let payload = CodexWorkerOperationPayload {
            actor: ActorId::KernelDispatcher,
            wave_id: "wave-hash".into(),
            idempotency_key: "wave-hash:task-a".into(),
            goal: "do task-a".into(),
            cwd: None,
            context: json!({ "from": "legacy" }),
            acceptance_criteria: None,
        };
        let serialized = serde_json::to_value(&payload).unwrap();
        assert!(
            !serialized.as_object().unwrap().contains_key("cwd"),
            "None cwd must serialize as absent for pre-upgrade hash parity"
        );

        let legacy_without_cwd = json!({
            "actor": serde_json::to_value(ActorId::KernelDispatcher).unwrap(),
            "wave_id": "wave-hash",
            "idempotency_key": "wave-hash:task-a",
            "goal": "do task-a",
            "context": { "from": "legacy" },
        });
        assert_eq!(
            crate::routes::terminal_cards::stable_payload_hash(&payload).unwrap(),
            crate::routes::terminal_cards::stable_payload_hash(&legacy_without_cwd).unwrap()
        );

        let task_with_cwd = crate::model::Task {
            id: "wave-hash:task-a".into(),
            wave_id: "wave-hash".into(),
            key: "task-a".into(),
            kind: crate::model::TaskKind::Codex,
            goal: "do task-a".into(),
            context_json: json!({ "from": "legacy" }).to_string(),
            acceptance_criteria: None,
            cwd: Some("/repo/from-plan-upsert".into()),
            depends_on_json: "[]".into(),
            priority: 0,
            gate_json: None,
            status: crate::model::TaskStatus::Pending,
            status_detail: None,
            worker_card_id: None,
            gate_result_json: None,
            gate_attempt: 0,
            gate_pid: None,
            gate_pid_starttime: None,
            gate_pid_boot_id: None,
            running_deadline_ms: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            finished_at_ms: None,
        };
        let (kind, built) = crate::scheduler::build_worker_payload(&task_with_cwd).unwrap();
        assert_eq!(kind, "codex-worker");
        assert!(
            !built.as_object().unwrap().contains_key("cwd"),
            "build_worker_payload must not leak task.cwd into codex op identity"
        );
        assert_eq!(
            crate::routes::terminal_cards::stable_payload_hash(&built).unwrap(),
            crate::routes::terminal_cards::stable_payload_hash(&legacy_without_cwd).unwrap()
        );
    }

    fn worker_op(id: &str, payload: Value) -> Operation {
        Operation {
            id: id.to_string(),
            operation_key: format!("op-key-{id}"),
            kind: "codex-worker".into(),
            idempotency_key: Some(id.to_string()),
            payload_hash: "hash".into(),
            target_type: "unknown".into(),
            target_id: None,
            target: json!({ "type": "unknown", "id": null }),
            payload,
            tx_output: None,
            phase: Phase::Pending,
            phase_detail: None,
            attempt: 0,
            last_error: None,
            compensation_state: None,
            lease_owner: None,
            lease_until_ms: None,
            spawn_artifacts: None,
            parked_at_ms: None,
            parked_deadline_ms: None,
        }
    }

    async fn prepare_worker(
        harness: &WorkerLeaseHarness,
        key: &str,
    ) -> (TxOutput, Vec<BroadcastEnvelope>) {
        let payload = worker_payload(&harness.wave_id, key);
        let op_repo = SqlxOperationRepo::new(harness.repo.pool().clone());
        let op_id = op_repo
            .insert_operation(
                "codex-worker",
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: Some(format!("op-{key}")),
                    payload_hash: format!("hash-{key}"),
                },
                payload.clone(),
            )
            .await
            .unwrap();
        let op = op_repo
            .claim_drive_batch(1)
            .await
            .unwrap()
            .into_iter()
            .find(|op| op.id == op_id)
            .unwrap();
        let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
        let output = harness
            .adapter
            .prepare_tx(&mut tx, &payload, &op)
            .await
            .unwrap();
        let events = output.post_commit_events.clone();
        tx.commit().await.unwrap();
        (output, events)
    }

    #[test]
    fn render_worker_prompt_goal_only() {
        let out = render_worker_prompt("fix the bug", &Value::Null, None);
        assert_eq!(out, "Goal:\nfix the bug");
    }

    #[test]
    fn render_worker_prompt_goal_plus_context() {
        let ctx = serde_json::json!({ "issue": 42, "title": "x" });
        let out = render_worker_prompt("fix it", &ctx, None);
        assert!(out.starts_with("Goal:\nfix it"));
        assert!(out.contains("\n\nContext:\n"));
        assert!(out.contains("\"issue\": 42"));
        assert!(out.contains("\"title\": \"x\""));
        assert!(!out.contains("Acceptance criteria"));
    }

    #[test]
    fn render_worker_prompt_goal_plus_context_plus_ac() {
        let ctx = serde_json::json!({ "pr": 7 });
        let out = render_worker_prompt("ship", &ctx, Some("tests pass"));
        assert!(out.contains("Goal:\nship"));
        assert!(out.contains("\n\nContext:\n"));
        assert!(out.contains("\"pr\": 7"));
        assert!(out.ends_with("Acceptance criteria:\ntests pass"));
    }

    #[test]
    fn render_worker_prompt_skips_empty_context_object() {
        let out = render_worker_prompt("g", &serde_json::json!({}), Some("ac"));
        assert!(
            !out.contains("Context"),
            "empty {{}} should be skipped: {out}"
        );
        assert!(out.contains("Acceptance criteria:\nac"));
    }

    #[test]
    fn render_worker_prompt_skips_blank_ac() {
        let out = render_worker_prompt("g", &Value::Null, Some("   "));
        assert_eq!(out, "Goal:\ng");
    }

    #[tokio::test]
    async fn codex_worker_prepare_acquires_held_workspace_lease_cwd() {
        let harness = worker_lease_harness().await;
        let (output, events) = prepare_worker(&harness, "a").await;
        let card_id = output.output_string("card_id", "test").unwrap();
        let lease_id = output.output_string("lease_id", "test").unwrap();
        let cwd = output.output_string("cwd", "test").unwrap();

        let cwd_path = std::path::Path::new(&cwd);
        assert!(cwd_path.is_absolute());
        assert!(cwd_path.starts_with(harness.repo_root.path()));
        assert!(
            cwd_path.parent().unwrap().is_dir(),
            "leased cwd parent exists"
        );
        assert!(
            !cwd_path.exists(),
            "leased cwd leaf is left for git worktree add"
        );
        let row = sqlx::query(
            "SELECT state, path, card_id, wave_id FROM workspace_leases WHERE lease_id = ?1",
        )
        .bind(&lease_id)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap();
        assert_eq!(row.get::<String, _>("state"), "held");
        assert_eq!(row.get::<String, _>("path"), cwd);
        assert_eq!(row.get::<String, _>("card_id"), card_id);
        assert_eq!(row.get::<String, _>("wave_id"), harness.wave_id);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0].event, Event::WorkspaceLeased { .. }));

        assert!(
            release_workspace_lease_for_card_repo(harness.repo.as_ref(), &harness.events, &card_id)
                .await
                .unwrap()
        );
        assert!(
            !std::path::Path::new(&cwd).exists(),
            "lease acquisition leaves cwd leaf absent until provisioning"
        );
    }

    #[tokio::test]
    async fn codex_worker_budget_parallelism_gets_disjoint_lease_paths() {
        let harness = worker_lease_harness().await;
        let (first, _) = prepare_worker(&harness, "a").await;
        let (second, _) = prepare_worker(&harness, "b").await;
        let first_card = first.output_string("card_id", "test").unwrap();
        let second_card = second.output_string("card_id", "test").unwrap();
        let first_cwd = first.output_string("cwd", "test").unwrap();
        let second_cwd = second.output_string("cwd", "test").unwrap();

        assert_ne!(first_card, second_card);
        assert_ne!(first_cwd, second_cwd);
        assert!(std::path::Path::new(&first_cwd).is_absolute());
        assert!(std::path::Path::new(&second_cwd).is_absolute());

        let held: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM workspace_leases WHERE state = 'held'")
                .fetch_one(harness.repo.pool())
                .await
                .unwrap();
        assert_eq!(held, 2);

        release_workspace_lease_for_card_repo(harness.repo.as_ref(), &harness.events, &first_card)
            .await
            .unwrap();
        release_workspace_lease_for_card_repo(harness.repo.as_ref(), &harness.events, &second_card)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn workspace_lease_release_flips_row_and_persists_event() {
        let harness = worker_lease_harness().await;
        let (output, _) = prepare_worker(&harness, "a").await;
        let card_id = output.output_string("card_id", "test").unwrap();
        let lease_id = output.output_string("lease_id", "test").unwrap();

        assert!(
            release_workspace_lease_for_card_repo(harness.repo.as_ref(), &harness.events, &card_id)
                .await
                .unwrap()
        );
        let state: String =
            sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
                .bind(&lease_id)
                .fetch_one(harness.repo.pool())
                .await
                .unwrap();
        assert_eq!(state, "released");
        let released_events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'workspace.released'")
                .fetch_one(harness.repo.pool())
                .await
                .unwrap();
        assert_eq!(released_events, 1);
        let removed_events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'worktree.removed'")
                .fetch_one(harness.repo.pool())
                .await
                .unwrap();
        assert_eq!(removed_events, 0);

        assert!(
            !release_workspace_lease_for_card_repo(
                harness.repo.as_ref(),
                &harness.events,
                &card_id
            )
            .await
            .unwrap(),
            "release is idempotent after the row is released"
        );
    }

    #[tokio::test]
    async fn codex_worker_compensation_removes_workspace_before_row_release() {
        let harness = worker_lease_harness().await;
        let (output, _) = prepare_worker(&harness, "a").await;
        let op = worker_op("op-a", Value::Null);
        let state = harness
            .adapter
            .plan_compensation(PhaseTag::SpawnStarted, "boom", &output, &op)
            .await
            .unwrap();

        assert_eq!(state.steps[0].op, "remove_workspace_artifact");
        assert_eq!(state.steps[1].op, "release_workspace_lease");
        assert_eq!(state.steps[2].op, "cleanup_codex_worker");
        let lease_id = output.output_string("lease_id", "test").unwrap();
        assert_eq!(
            state.steps[1].arg_string("lease_id", "test").unwrap(),
            lease_id
        );
    }

    fn init_git_repo(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        run_git(path, ["init"]);
        run_git(path, ["config", "user.email", "codex-worker@example.test"]);
        run_git(path, ["config", "user.name", "Codex Worker Test"]);
        std::fs::write(path.join("README.md"), "initial\n").unwrap();
        run_git(path, ["add", "README.md"]);
        run_git(path, ["commit", "-m", "initial"]);
    }

    fn run_git<const N: usize>(repo: &Path, args: [&str; N]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
            args,
            repo.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
