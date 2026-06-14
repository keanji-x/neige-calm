use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::card_role_cache::CardRoleCache;
use crate::codex_appserver::InputItem;
use crate::db::sqlite::{
    append_decision_event_in_tx, card_mcp_token_set_tx, card_update_tx, card_with_codex_create_tx,
    runtime_bind_attribution_tx, runtime_get_active_for_card_tx, runtime_get_by_id_tx,
    runtime_set_status_tx, session_mcp_token_set_tx,
};
use crate::db::{write_in_tx_typed, write_with_events_typed};
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, CardId, WaveId};
use crate::mcp_server::McpServer;
use crate::model::{Card, CardRole, new_id, now_ms};
use crate::operation::worker_cleanup::{
    WorkerCleanupOutcome, compensate_worker_rows, worker_spawn_failure_preserved,
};
use crate::pending_codex_threads::{PendingEntry, PendingThreadStartRegistry};
use crate::routes::cards::card_scope;
use crate::routes::codex_cards::{
    await_shared_initial_turn_lifecycle, default_cwd, normalize_optional_css_color,
    shell_single_quote,
};
use crate::routes::settings::load_settings;
use crate::routes::theme::RequestTheme;
use crate::runtime_repo::{AgentProvider, RunStatus, RuntimeKind, ThreadAttribution};
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
            kind: RuntimeKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
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
        let card_id = output_string(output, "card_id")?;
        let cwd = output_string(output, "cwd")?;

        if let Some(prompt_text) = output_prompt(output)? {
            let mut notifs = self.shared_codex_appserver.subscribe_notifications();
            let thread_id = match output_optional_string(output, "codex_thread_id")?
                .or_else(|| phase_minted_thread_id(&op.phase))
            {
                Some(thread_id) => thread_id,
                None => {
                    if let Some(runtime) = self.repo.runtime_get_active_for_card(&card_id).await?
                        && let Some(thread_id) = non_empty_string(runtime.thread_id.as_deref())
                    {
                        thread_id
                    } else {
                        // Worker daemons inherit NEIGE_MCP_TOKEN from the per-card spawn env, so no per-thread override is needed.
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
            set_output_data(output, "codex_thread_id", json!(thread_id.clone()))?;
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
                set_output_data(output, "turn_started_at_ms", json!(now_ms()))?;
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
            set_output_data(output, "pending_registered", json!(true))?;
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
        let terminal_id = output_string(output, "terminal_id")?;
        let card_id = output_string(output, "card_id")?;
        let wave_id = output_string(output, "wave_id")?;
        let runtime_id = output_string(output, "runtime_id")?;
        let cwd = output_string(output, "cwd")?;
        let env = output.data.get("env").cloned().unwrap_or_else(|| json!({}));
        ctx.repo.terminal_clear_exit_for_spawn(&terminal_id).await?;
        let term = ctx
            .repo
            .terminal_get(&terminal_id)
            .await?
            .ok_or_else(|| CalmError::Internal(format!("terminal {terminal_id} vanished")))?;
        let is_prompted = output_prompt(output)?.is_some();
        let command_line = if is_prompted {
            let thread_id = output_string(output, "codex_thread_id")?;
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
        let card_id = output_string(output, "card_id")?;
        let terminal_id = output_string(output, "terminal_id")?;
        let mut steps = vec![step(
            "reap_terminal_pty",
            json!({ "terminal_id": terminal_id }),
        )];
        if output_prompt(output)?.is_some() {
            steps.push(step(
                "interrupt_shared_codex_turn",
                json!({
                    "card_id": card_id.clone(),
                    "thread_id": output_optional_string(output, "codex_thread_id")?,
                }),
            ));
            steps.push(step(
                "runtime_set_status_failed_for_card",
                json!({ "card_id": card_id }),
            ));
        } else {
            steps.push(step(
                "pending_codex_threads_remove_by_card",
                json!({ "card_id": card_id.clone() }),
            ));
            steps.push(step(
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
                let terminal_id = step_arg_string(step, "terminal_id")?;
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
                let card_id = step_arg_string(step, "card_id")?;
                let thread_id =
                    if let Some(thread_id) = step.args.get("thread_id").and_then(Value::as_str) {
                        Some(thread_id.to_string())
                    } else {
                        ctx.repo
                            .runtime_get_active_for_card(&card_id)
                            .await?
                            .and_then(|runtime| non_empty_string(runtime.thread_id.as_deref()))
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
            "runtime_set_status_failed_for_card" => {
                let card_id = step_arg_string(step, "card_id")?;
                ctx.repo
                    .runtime_complete_for_card(&card_id, RunStatus::Failed)
                    .await?;
                Ok(())
            }
            "pending_codex_threads_remove_by_card" => {
                let card_id = step_arg_string(step, "card_id")?;
                self.pending_codex_threads.remove_by_card(&card_id).await;
                Ok(())
            }
            "card_payload_clear_pending_status" => {
                let card_id = step_arg_string(step, "card_id")?;
                let runtime_id = output_string(output, "runtime_id")?;
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
            PhaseTag::SpawnStarted,
            PhaseTag::SpawnSucceeded,
            PhaseTag::Succeeded,
        ]
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
        _op: &Operation,
    ) -> Result<TxOutput> {
        let payload: CodexWorkerOperationPayload = serde_json::from_value(input.clone())?;
        let card_id = new_id();
        let runtime_id = new_id();
        let wave_id = WaveId::from(payload.wave_id.clone());
        let cwd = default_cwd();
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
            "env": env,
            "prompt": rendered_prompt,
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
        let card_id = output_string(output, "card_id")?;
        let runtime_id = output_string(output, "runtime_id")?;
        let terminal_id = output_string(output, "terminal_id")?;
        let wave_id = WaveId::from(output_string(output, "wave_id")?);
        let cwd = output_string(output, "cwd")?;
        let rendered_prompt = output_string(output, "prompt")?;
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
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.to_string(),
            steps: vec![step(
                "cleanup_codex_worker",
                json!({
                    "card_id": output_string(output, "card_id")?,
                    "terminal_id": output_string(output, "terminal_id")?,
                }),
            )],
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
        if step.op != "cleanup_codex_worker" {
            return Err(CalmError::Internal(format!(
                "unknown codex worker compensation op {}",
                step.op
            )));
        }
        let card_id = step_arg_string(step, "card_id")?;
        let terminal_id = step_arg_string(step, "terminal_id")?;
        let runtime_id = output_string(output, "runtime_id")?;
        let runtime_turn = ctx
            .repo
            .runtime_get_by_id(&runtime_id)
            .await?
            .and_then(|runtime| {
                non_empty_string(runtime.thread_id.as_deref()).map(|thread_id| {
                    (
                        thread_id,
                        non_empty_string(runtime.active_turn_id.as_deref()),
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
        .runtime_get_by_id(&runtime_id)
        .await?
        .ok_or_else(|| CalmError::Internal(format!("worker runtime {runtime_id} vanished")))?;
    let persisted_turn_id = non_empty_string(runtime.active_turn_id.as_deref());
    let worker_instructions = crate::spec_card::render_system_prompt(
        crate::spec_card::SeededCardRole::Worker.prompt_template(),
        ctx.wave_id.as_str(),
    );
    let thread_id = if let Some(thread_id) = non_empty_string(runtime.thread_id.as_deref()) {
        thread_id
    } else {
        let thread_id = ctx
            .shared_codex_appserver
            .thread_start_mint_for_card(
                card_id,
                SharedThreadStartParams {
                    cwd: ctx.cwd.to_string(),
                    approval_policy: "never".into(),
                    sandbox_mode: "workspace-write".into(),
                    developer_instructions: Some(worker_instructions),
                    config: None,
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
        if let Some(token) = ctx.mcp_token {
            map.insert("NEIGE_MCP_TOKEN".into(), Value::String(token.to_string()));
        }
        if let Some(server) = ctx.mcp_server {
            map.insert(
                "NEIGE_MCP_SOCKET".into(),
                Value::String(server.shim_config.socket_path.to_string_lossy().to_string()),
            );
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
    let token = crate::mcp_server::auth::CardMcpToken::generate();
    let hashed = crate::mcp_server::auth::hash_token(token.as_str());
    let card_id = card_id.to_string();
    let runtime_id = runtime_id.to_string();
    write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        let card_id = card_id.clone();
        let runtime_id = runtime_id.clone();
        let hashed = hashed.clone();
        Box::pin(async move {
            card_mcp_token_set_tx(tx, &card_id, &hashed)
                .await
                .map_err(CalmError::from)?;
            session_mcp_token_set_tx(tx, &runtime_id, &hashed)
                .await
                .map_err(CalmError::from)?;
            Ok(())
        })
    })
    .await?;
    Ok(token.into_inner())
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
            let runtime = runtime_get_by_id_tx(tx, &runtime_id_for_tx)
                .await?
                .ok_or_else(|| {
                    CalmError::Internal(format!(
                        "worker runtime {runtime_id_for_tx} vanished before shared codex bind"
                    ))
                })?;
            let old_status = runtime.status.clone();
            runtime_bind_attribution_tx(
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
            if old_status != RunStatus::Running {
                runtime_set_status_tx(tx, &runtime.id, RunStatus::Running).await?;
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
                let runtime = runtime_get_active_for_card_tx(tx, &card_id_for_tx)
                    .await?
                    .ok_or_else(|| {
                        CalmError::Internal(format!(
                            "codex card {card_id_for_tx} has no active runtime to bind"
                        ))
                    })?;
                let terminal_id_for_projection = runtime.terminal_run_id.clone();
                runtime_bind_attribution_tx(
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
                let old_status = runtime.status.clone();
                let runtime_id = runtime.id.clone();
                if runtime.status != RunStatus::Running {
                    runtime_set_status_tx(tx, &runtime.id, RunStatus::Running).await?;
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
                if old_status != RunStatus::Running {
                    events.push((
                        scope,
                        Event::RuntimeStatusChanged {
                            runtime_id,
                            card_id: card_id_for_tx,
                            old_status,
                            new_status: RunStatus::Running,
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
                let runtime = runtime_get_active_for_card_tx(tx, &card_id_for_tx)
                    .await?
                    .ok_or_else(|| {
                        CalmError::Internal(format!(
                            "codex card {card_id_for_tx} has no active runtime to mark pending"
                        ))
                    })?;
                let terminal_id_for_projection = runtime.terminal_run_id.clone();
                let old_status = runtime.status.clone();
                let runtime_id = runtime.id.clone();
                if old_status != RunStatus::TurnPending {
                    runtime_set_status_tx(tx, &runtime.id, RunStatus::TurnPending).await?;
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
                if old_status != RunStatus::TurnPending {
                    events.push((
                        scope,
                        Event::RuntimeStatusChanged {
                            runtime_id,
                            card_id: card_id_for_tx,
                            old_status,
                            new_status: RunStatus::TurnPending,
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

fn non_empty_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
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

fn output_string(output: &TxOutput, key: &str) -> Result<String> {
    output
        .data
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| CalmError::Internal(format!("codex tx_output missing {key}")))
}

fn output_optional_string(output: &TxOutput, key: &str) -> Result<Option<String>> {
    match output.data.get(key) {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(CalmError::Internal(format!(
            "codex tx_output {key} must be string or null"
        ))),
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

fn set_output_data(output: &mut TxOutput, key: &str, value: Value) -> Result<()> {
    let data = output
        .data
        .as_object_mut()
        .ok_or_else(|| CalmError::Internal("codex tx_output data is not an object".into()))?;
    data.insert(key.to_string(), value);
    Ok(())
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

fn step(op: &str, args: Value) -> CompensationStep {
    CompensationStep {
        op: op.to_string(),
        args,
        completed: false,
        attempts: 0,
        last_error: None,
    }
}

fn step_arg_string(step: &CompensationStep, key: &str) -> Result<String> {
    step.args
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            CalmError::Internal(format!("codex compensation step {} missing {key}", step.op))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
