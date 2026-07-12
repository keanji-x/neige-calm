use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::{
    append_decision_event_in_tx, card_update_tx, card_with_claude_create_tx,
    card_with_claude_worker_create_tx, session_projection_active_for_card_tx,
    session_set_status_tx,
};
use crate::db::{write_in_tx_typed, write_with_events_typed};
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, CardId, WaveId};
use crate::mcp_server::McpServer;
use crate::mcp_server::wiring::{card_mcp_env, mint_and_persist_card_token};
use crate::model::{Card, CardRole, new_id};
use crate::operation::codex_adapter::render_worker_prompt;
use crate::operation::worker_cleanup::{compensate_worker_rows, worker_spawn_failure_preserved};
use crate::operation::workspace_lease::{
    acquire_plain_workspace_lease_tx, plain_workspace_lease_path_for,
    release_workspace_lease_by_id, remove_workspace_artifact_for_lease_by_id,
};
use crate::routes::cards::card_scope;
use crate::routes::claude_cards::{
    build_claude_settings_json, build_claude_worker_settings_json, claude_hook_command,
};
use crate::routes::codex_cards::{default_cwd, normalize_optional_css_color, shell_single_quote};
use crate::routes::settings::load_settings;
use crate::routes::theme::RequestTheme;
use crate::session_projection_repo::{AgentProvider, WorkerSessionKind, WorkerSessionState};
use crate::state::{CodexClient, WriteContext};
use crate::terminal_sweeper::reap_terminal_artifacts_with_renderer;
use crate::wave_cove_cache::WaveCoveCache;
use calm_truth::decision_gate::PermissiveGate;

use super::{
    AppServerInteractOutcome, CompensationStateVersioned, CompensationStep, Operation, PhaseTag,
    ProviderAdapter, SpawnCtx, SpawnHandle, SpawnOutcome, Tx, TxOutput,
};

#[cfg(feature = "fixtures")]
use futures::future::BoxFuture;

#[cfg(feature = "fixtures")]
type SpawnHook = Arc<
    dyn Fn(String, String, String, Value) -> BoxFuture<'static, Result<SpawnHandle>> + Send + Sync,
>;

pub(super) const CLAUDE_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::SpawnStarted,
    PhaseTag::SpawnSucceeded,
    PhaseTag::Succeeded,
];

#[derive(Clone)]
pub struct ClaudeAdapter {
    repo: Arc<dyn crate::db::RouteRepo>,
    codex: Arc<CodexClient>,
    card_role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
    #[cfg(feature = "fixtures")]
    spawn_hook: Option<SpawnHook>,
}

#[derive(Clone)]
pub struct ClaudeWorkerAdapter {
    repo: Arc<dyn crate::db::RouteRepo>,
    codex: Arc<CodexClient>,
    mcp_server: Option<Arc<McpServer>>,
    card_role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
    #[cfg(feature = "fixtures")]
    spawn_hook: Option<SpawnHook>,
}

impl ClaudeAdapter {
    pub fn new(
        repo: Arc<dyn crate::db::RouteRepo>,
        codex: Arc<CodexClient>,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
    ) -> Self {
        Self {
            repo,
            codex,
            card_role_cache,
            wave_cove_cache,
            #[cfg(feature = "fixtures")]
            spawn_hook: None,
        }
    }

    #[cfg(feature = "fixtures")]
    pub fn new_with_spawn_hook(
        repo: Arc<dyn crate::db::RouteRepo>,
        codex: Arc<CodexClient>,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
        spawn_hook: SpawnHook,
    ) -> Self {
        Self {
            repo,
            codex,
            card_role_cache,
            wave_cove_cache,
            spawn_hook: Some(spawn_hook),
        }
    }
}

impl ClaudeWorkerAdapter {
    pub fn new(
        repo: Arc<dyn crate::db::RouteRepo>,
        codex: Arc<CodexClient>,
        mcp_server: Option<Arc<McpServer>>,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
    ) -> Self {
        Self {
            repo,
            codex,
            mcp_server,
            card_role_cache,
            wave_cove_cache,
            #[cfg(feature = "fixtures")]
            spawn_hook: None,
        }
    }

    #[cfg(feature = "fixtures")]
    pub fn new_with_spawn_hook(
        repo: Arc<dyn crate::db::RouteRepo>,
        codex: Arc<CodexClient>,
        mcp_server: Option<Arc<McpServer>>,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
        spawn_hook: SpawnHook,
    ) -> Self {
        Self {
            repo,
            codex,
            mcp_server,
            card_role_cache,
            wave_cove_cache,
            spawn_hook: Some(spawn_hook),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClaudeCreateOperationPayload {
    pub actor: ActorId,
    #[serde(default)]
    pub runtime_id: Option<String>,
    pub request: PreparedClaudeCreateRequest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClaudeWorkerOperationPayload {
    pub actor: ActorId,
    pub wave_id: String,
    pub idempotency_key: String,
    pub goal: String,
    /// Forward-compatible only. Scheduler-created Claude worker payloads keep
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
pub struct ClaudeCreateRequestInput {
    pub wave_id: String,
    pub title: Option<String>,
    pub sort: Option<f64>,
    pub cwd: Option<String>,
    pub prompt: Option<String>,
    pub icon_bg: Option<String>,
    pub icon_fg: Option<String>,
    pub theme: RequestTheme,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NormalizedClaudeCreateRequest {
    pub wave_id: String,
    #[serde(default)]
    pub title: Option<String>,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreparedClaudeCreateRequest {
    pub wave_id: String,
    #[serde(default)]
    pub title: Option<String>,
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
    pub card_id: String,
    pub claude_session_id: String,
    pub settings_path: String,
    pub command_line: String,
    pub env: Value,
}

pub fn normalize_claude_create_request(
    input: ClaudeCreateRequestInput,
) -> Result<NormalizedClaudeCreateRequest> {
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

    Ok(NormalizedClaudeCreateRequest {
        wave_id: input.wave_id,
        title: input.title,
        sort: input.sort,
        cwd,
        prompt,
        icon_bg,
        icon_fg,
        theme: input.theme,
    })
}

pub async fn prepare_claude_create_request(
    repo: &dyn crate::db::RouteRepo,
    codex: &CodexClient,
    request: NormalizedClaudeCreateRequest,
) -> Result<PreparedClaudeCreateRequest> {
    let card_id = new_id();
    let claude_session_id = uuid::Uuid::new_v4().to_string();
    let settings_path = codex
        .claude_settings_dir
        .join(&card_id)
        .join("settings.json")
        .to_string_lossy()
        .to_string();
    let env = build_claude_env(repo, codex, &card_id).await?;
    let mut command_line = format!(
        "{} --allow-dangerously-skip-permissions --settings {} --session-id {}",
        shell_single_quote(&codex.claude_bin),
        shell_single_quote(&settings_path),
        shell_single_quote(&claude_session_id),
    );
    if let Some(prompt) = request.prompt.as_deref() {
        command_line.push_str(" -- ");
        command_line.push_str(&shell_single_quote(prompt));
    }

    Ok(PreparedClaudeCreateRequest {
        wave_id: request.wave_id,
        title: request.title,
        sort: request.sort,
        cwd: request.cwd,
        prompt: request.prompt,
        icon_bg: request.icon_bg,
        icon_fg: request.icon_fg,
        theme: request.theme,
        card_id,
        claude_session_id,
        settings_path,
        command_line,
        env,
    })
}

pub async fn build_claude_env(
    repo: &dyn crate::db::RouteRepo,
    codex: &CodexClient,
    card_id: &str,
) -> Result<Value> {
    let settings = load_settings(repo).await?;
    let mut env_map = serde_json::Map::new();
    env_map.insert(
        "NEIGE_CARD_ID".to_string(),
        Value::String(card_id.to_string()),
    );
    env_map.insert(
        "NEIGE_CALM_BASE_URL".to_string(),
        Value::String(codex.ingest_url.clone()),
    );
    env_map.insert(
        "NEIGE_HOOK_PROVIDER".to_string(),
        Value::String("claude".into()),
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

fn build_claude_worker_command_line(
    claude_bin: &str,
    settings_path: &Path,
    claude_session_id: &str,
    wave_id: &str,
    prompt: &str,
) -> String {
    let worker_system_prompt = crate::spec_card::render_system_prompt(
        crate::spec_card::SeededCardRole::Worker.prompt_template(),
        wave_id,
    );
    let mut command_line = format!(
        "{} --allow-dangerously-skip-permissions --settings {} --session-id {} --append-system-prompt {}",
        shell_single_quote(claude_bin),
        shell_single_quote(&settings_path.to_string_lossy()),
        shell_single_quote(claude_session_id),
        shell_single_quote(&worker_system_prompt),
    );
    command_line.push_str(" -- ");
    command_line.push_str(&shell_single_quote(prompt));
    command_line
}

async fn mint_claude_worker_mcp_token(
    ctx: &SpawnCtx,
    card_id: &str,
    runtime_id: &str,
) -> Result<String> {
    let card_id = card_id.to_string();
    let runtime_id = runtime_id.to_string();
    write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        let card_id = card_id.clone();
        let runtime_id = runtime_id.clone();
        Box::pin(async move { mint_and_persist_card_token(tx, &card_id, &runtime_id).await })
    })
    .await
}

#[async_trait]
impl ProviderAdapter for ClaudeAdapter {
    fn kind(&self) -> &'static str {
        "claude-create"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        CLAUDE_PHASES
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: ClaudeCreateOperationPayload = serde_json::from_value(input.clone())?;
        if payload.request.cwd.chars().any(|c| c.is_ascii_control()) {
            return Err(CalmError::BadRequest(
                "cwd must not contain ASCII control characters".into(),
            ));
        }
        normalize_optional_css_color(payload.request.icon_bg.as_deref(), "icon_bg")?;
        normalize_optional_css_color(payload.request.icon_fg.as_deref(), "icon_fg")?;
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
        if !payload.request.env.is_object() {
            return Err(CalmError::BadRequest("env must be an object".into()));
        }
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        let payload: ClaudeCreateOperationPayload = serde_json::from_value(input.clone())?;
        let runtime_id = payload.runtime_id.clone().unwrap_or_else(new_id);
        let request = payload.request;
        let card_id = request.card_id.clone();
        let wave_id = request.wave_id.clone();
        let scope = card_scope(
            self.repo.as_ref(),
            CardId::from(card_id.clone()),
            WaveId::from(wave_id.clone()),
        )
        .await?;
        let (card, term) = card_with_claude_create_tx(
            tx,
            card_id,
            &runtime_id,
            WaveId::from(wave_id),
            request.title.clone(),
            request.sort,
            request.command_line.clone(),
            request.cwd.clone(),
            request.env.clone(),
            request.prompt.clone(),
            request.icon_bg.clone(),
            request.icon_fg.clone(),
            request.settings_path.clone(),
            request.claude_session_id.clone(),
            CardRole::Worker,
            true,
            &self.card_role_cache,
            request.theme,
        )
        .await?;
        let projected_card = project_claude_runtime_fields_for_response(
            card.clone(),
            &term.id,
            &request.claude_session_id,
        );
        let event = Event::CardAdded(projected_card.clone());
        let runtime_event = Event::RuntimeStarted {
            runtime_id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::ClaudeCard,
            agent_provider: Some(AgentProvider::Claude),
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
            "settings_path": request.settings_path,
            "claude_session_id": request.claude_session_id,
            "command_line": request.command_line,
            "cwd": request.cwd,
            "env": request.env,
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
        let card_id = output.output_string("card_id", "claude")?;
        let terminal_id = output.output_string("terminal_id", "claude")?;
        let settings_path = PathBuf::from(output.output_string("settings_path", "claude")?);
        let settings_dir = settings_path_parent(&settings_path)?;
        let command_line = output.output_string("command_line", "claude")?;
        let cwd = output.output_string("cwd", "claude")?;
        let env = output.data.get("env").cloned().unwrap_or_else(|| json!({}));

        ctx.repo.terminal_clear_exit_for_spawn(&terminal_id).await?;
        let term = ctx
            .repo
            .terminal_get(&terminal_id)
            .await?
            .ok_or_else(|| CalmError::Internal(format!("terminal {terminal_id} vanished")))?;
        std::fs::create_dir_all(&settings_dir).map_err(|e| {
            CalmError::Internal(format!(
                "mkdir claude settings dir {}: {e}",
                settings_dir.display()
            ))
        })?;
        let hook_command = claude_hook_command(
            &self.codex.bridge_bin.to_string_lossy(),
            &card_id,
            &self.codex.ingest_url,
        );
        std::fs::write(&settings_path, build_claude_settings_json(&hook_command))
            .map_err(|e| CalmError::Internal(format!("write claude settings.json: {e}")))?;

        #[cfg(feature = "fixtures")]
        let handle = if let Some(hook) = &self.spawn_hook {
            hook(terminal_id.clone(), command_line, cwd, env).await
        } else {
            ctx.spawn_terminal(&term, &command_line, &cwd, &env).await
        };

        #[cfg(not(feature = "fixtures"))]
        let handle = ctx.spawn_terminal(&term, &command_line, &cwd, &env).await;

        match handle {
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
                                    session_projection_active_for_card_tx(tx, &card_id_for_tx)
                                        .await?
                                        .ok_or_else(|| {
                                            CalmError::Internal(format!(
                                                "claude card {card_id_for_tx} has no active runtime to mark running"
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
                        target: "operation::claude_adapter::runtime_running_mark_failed",
                        card_id = %card_id,
                        terminal_id = %terminal_id,
                        error = %e,
                        "failed to mark claude runtime running after spawn; continuing operation"
                    );
                }
                Ok(SpawnOutcome::Ready(handle))
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
        let terminal_id = output.output_string("terminal_id", "claude")?;
        let card_id = output.output_string("card_id", "claude")?;
        let settings_path = output.output_string("settings_path", "claude")?;
        let settings_dir = settings_path_parent(Path::new(&settings_path))?
            .to_string_lossy()
            .to_string();
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.to_string(),
            steps: vec![
                CompensationStep::new("reap_terminal_pty", json!({ "terminal_id": terminal_id })),
                CompensationStep::new(
                    "delete_claude_settings_dir",
                    json!({ "settings_dir": settings_dir }),
                ),
                CompensationStep::new(
                    "session_projection_set_status_failed_for_card",
                    json!({ "card_id": card_id }),
                ),
            ],
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
            "delete_claude_settings_dir" => {
                let settings_dir = step_arg_string(step, "settings_dir")?;
                remove_dir_all_idempotent(Path::new(&settings_dir))
            }
            // Back-compat: operations that entered `compensating` under a pre-PR10-d
            // release persisted the legacy op string; accept it during recovery so
            // in-flight compensation states still drain. New states write the new name.
            "session_projection_set_status_failed_for_card"
            | "runtime_set_status_failed_for_card" => {
                let card_id = step_arg_string(step, "card_id")?;
                ctx.repo
                    .session_projection_complete_for_card(&card_id, WorkerSessionState::Failed)
                    .await?;
                Ok(())
            }
            other => Err(CalmError::Internal(format!(
                "unknown claude compensation op {other}"
            ))),
        }
    }
}

#[async_trait]
impl ProviderAdapter for ClaudeWorkerAdapter {
    fn kind(&self) -> &'static str {
        "claude-worker"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        CLAUDE_PHASES
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: ClaudeWorkerOperationPayload = serde_json::from_value(input.clone())?;
        if payload.idempotency_key.trim().is_empty() {
            return Err(CalmError::BadRequest(
                "claude worker idempotency_key must not be empty".into(),
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
        let payload: ClaudeWorkerOperationPayload = serde_json::from_value(input.clone())?;
        let card_id = new_id();
        let runtime_id = new_id();
        let claude_session_id = uuid::Uuid::new_v4().to_string();
        let wave_id = WaveId::from(payload.wave_id.clone());
        let cwd_path = plain_workspace_lease_path_for(wave_id.as_str(), &card_id)?;
        let cwd = cwd_path.to_string_lossy().to_string();
        let settings_path = self
            .codex
            .claude_settings_dir
            .join(&card_id)
            .join("settings.json");
        let settings_dir = settings_path_parent(&settings_path)?;
        let rendered_prompt = render_worker_prompt(
            &payload.goal,
            &payload.context,
            payload.acceptance_criteria.as_deref(),
        );
        let command_line = build_claude_worker_command_line(
            &self.codex.claude_bin,
            &settings_path,
            &claude_session_id,
            wave_id.as_str(),
            &rendered_prompt,
        );
        let env = build_claude_env(self.repo.as_ref(), &self.codex, &card_id).await?;
        let scope = card_scope(
            self.repo.as_ref(),
            CardId::from(card_id.clone()),
            wave_id.clone(),
        )
        .await?;

        let (mut card, term) = card_with_claude_worker_create_tx(
            tx,
            card_id.clone(),
            &runtime_id,
            Some(op.id.as_str()),
            wave_id,
            None,
            None,
            command_line.clone(),
            cwd.clone(),
            env.clone(),
            Some(rendered_prompt.clone()),
            None,
            None,
            settings_path.to_string_lossy().to_string(),
            claude_session_id.clone(),
            &self.card_role_cache,
            RequestTheme::default_dark(),
        )
        .await?;

        let (lease, lease_event) = acquire_plain_workspace_lease_tx(
            tx,
            &card_id,
            card.wave_id.as_str(),
            &op.id,
            &cwd_path,
        )
        .await?;

        if let Some(existing_map) = card.payload.as_object() {
            let mut merged = existing_map.clone();
            merged.insert(
                "idempotency_key".into(),
                Value::String(payload.idempotency_key.clone()),
            );
            merged.insert("role_request".into(), Value::String("claude".into()));
            merged.insert("goal".into(), Value::String(payload.goal.clone()));
            merged.insert("context".into(), payload.context.clone());
            if let Some(ac) = payload.acceptance_criteria.as_ref() {
                merged.insert("acceptance_criteria".into(), Value::String(ac.clone()));
            }
            merged.insert("prompt".into(), Value::String(rendered_prompt.clone()));
            merged.insert(
                "claude_session_id".into(),
                Value::String(claude_session_id.clone()),
            );
            card = card_update_tx(
                tx,
                card.id.as_ref(),
                crate::model::CardPatch {
                    title: None,
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
            "settings_path": settings_path,
            "settings_dir": settings_dir,
            "claude_session_id": claude_session_id,
            "command_line": command_line,
            "cwd": cwd,
            "lease_id": lease.lease_id,
            "env": env,
            "prompt": rendered_prompt,
            "scope": scope,
        });
        output.post_commit_events.push(lease_event);
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
        let card_id = output.output_string("card_id", "claude worker")?;
        let runtime_id = output.output_string("runtime_id", "claude worker")?;
        let terminal_id = output.output_string("terminal_id", "claude worker")?;
        let wave_id = WaveId::from(output.output_string("wave_id", "claude worker")?);
        let settings_path = PathBuf::from(output.output_string("settings_path", "claude worker")?);
        let settings_dir = settings_path_parent(&settings_path)?;
        let command_line = output.output_string("command_line", "claude worker")?;
        let cwd = output.output_string("cwd", "claude worker")?;
        let mut env = output.data.get("env").cloned().unwrap_or_else(|| json!({}));

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
                "claude-worker recovery: worker already exited; skipping respawn",
            );
            mark_claude_worker_running(
                ctx,
                &self.card_role_cache,
                &self.wave_cove_cache,
                &card_id,
                &terminal_id,
                &wave_id,
            )
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    target: "operation::claude_worker_adapter::runtime_running_mark_failed",
                    card_id = %card_id,
                    terminal_id = %terminal_id,
                    error = %e,
                    "failed to mark claude worker runtime running after spawn; continuing operation"
                );
            });
            log_claude_worker_card_added(
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
                    "claude worker CardAdded append failed after recovery exit preservation; continuing"
                );
            });
            return Ok(SpawnOutcome::Ready(SpawnHandle::NoOp));
        }
        if term.pid.is_some() && term.exit_code.is_none() && !term.signal_killed {
            tracing::info!(
                card_id = %card_id,
                terminal_id = %terminal_id,
                pid = ?term.pid,
                "claude-worker recovery: worker already live; skipping token rotation and respawn",
            );
            mark_claude_worker_running(
                ctx,
                &self.card_role_cache,
                &self.wave_cove_cache,
                &card_id,
                &terminal_id,
                &wave_id,
            )
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    target: "operation::claude_worker_adapter::runtime_running_mark_failed",
                    card_id = %card_id,
                    terminal_id = %terminal_id,
                    error = %e,
                    "failed to mark claude worker runtime running after spawn; continuing operation"
                );
            });
            log_claude_worker_card_added(
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
                    "claude worker CardAdded append failed after live recovery preservation; continuing"
                );
            });
            return Ok(SpawnOutcome::Ready(SpawnHandle::NoOp));
        }

        let mcp_server = self.mcp_server.as_ref().ok_or_else(|| {
            CalmError::Internal("MCP server is not running; claude worker cannot report".into())
        })?;
        let raw_token = mint_claude_worker_mcp_token(ctx, &card_id, &runtime_id).await?;
        let env_map = env.as_object_mut().ok_or_else(|| {
            CalmError::Internal("claude worker env must be an object before spawn".into())
        })?;
        for (key, value) in card_mcp_env(&mcp_server.shim_config.socket_path, raw_token.as_str()) {
            env_map.insert(key.into(), Value::String(value));
        }

        fs::create_dir_all(&settings_dir).map_err(|e| {
            CalmError::Internal(format!(
                "mkdir claude worker settings dir {}: {e}",
                settings_dir.display()
            ))
        })?;
        let hook_command = claude_hook_command(
            &self.codex.bridge_bin.to_string_lossy(),
            &card_id,
            &self.codex.ingest_url,
        );
        fs::write(
            &settings_path,
            build_claude_worker_settings_json(&hook_command),
        )
        .map_err(|e| CalmError::Internal(format!("write claude worker settings.json: {e}")))?;

        #[cfg(feature = "fixtures")]
        let handle = if let Some(hook) = &self.spawn_hook {
            hook(terminal_id.clone(), command_line, cwd, env).await
        } else {
            ctx.spawn_terminal(&term, &command_line, &cwd, &env).await
        };

        #[cfg(not(feature = "fixtures"))]
        let handle = ctx.spawn_terminal(&term, &command_line, &cwd, &env).await;

        match handle {
            Ok(handle) => {
                mark_claude_worker_running(
                    ctx,
                    &self.card_role_cache,
                    &self.wave_cove_cache,
                    &card_id,
                    &terminal_id,
                    &wave_id,
                )
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        target: "operation::claude_worker_adapter::runtime_running_mark_failed",
                        card_id = %card_id,
                        terminal_id = %terminal_id,
                        error = %e,
                        "failed to mark claude worker runtime running after spawn; continuing operation"
                    );
                });
                log_claude_worker_card_added(
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
                        "claude worker CardAdded append failed after live spawn; continuing"
                    );
                });
                Ok(SpawnOutcome::Ready(handle))
            }
            Err(e) if worker_spawn_failure_preserved(ctx.repo.as_ref(), &terminal_id).await? => {
                tracing::info!(
                    card_id = %card_id,
                    wave_id = %wave_id,
                    terminal_id = %terminal_id,
                    spawn_err = %e,
                    "claude worker TUI fast-exit; preserving card + terminal"
                );
                mark_claude_worker_running(
                    ctx,
                    &self.card_role_cache,
                    &self.wave_cove_cache,
                    &card_id,
                    &terminal_id,
                    &wave_id,
                )
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        target: "operation::claude_worker_adapter::runtime_running_mark_failed",
                        card_id = %card_id,
                        terminal_id = %terminal_id,
                        error = %e,
                        "failed to mark claude worker runtime running after spawn; continuing operation"
                    );
                });
                log_claude_worker_card_added(
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
                        "claude worker CardAdded append failed after fast-exit preservation; continuing"
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
        let mut steps = Vec::new();
        if let Some(lease_id) = output.output_optional_string("lease_id", "claude worker")? {
            steps.push(CompensationStep::new(
                "remove_workspace_artifact",
                json!({ "lease_id": lease_id.clone() }),
            ));
            steps.push(CompensationStep::new(
                "release_workspace_lease",
                json!({ "lease_id": lease_id }),
            ));
        }
        let card_id = output.output_string("card_id", "claude worker")?;
        let terminal_id = output.output_string("terminal_id", "claude worker")?;
        let settings_path = output.output_string("settings_path", "claude worker")?;
        let settings_dir = settings_path_parent(Path::new(&settings_path))?
            .to_string_lossy()
            .to_string();
        steps.push(CompensationStep::new(
            "cleanup_claude_worker",
            json!({
                "card_id": card_id,
                "terminal_id": terminal_id,
            }),
        ));
        steps.push(CompensationStep::new(
            "delete_claude_settings_dir",
            json!({ "settings_dir": settings_dir }),
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
        _output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()> {
        if step.completed {
            return Ok(());
        }
        match step.op.as_str() {
            "remove_workspace_artifact" => {
                let lease_id = step_arg_string(step, "lease_id")?;
                let pool = ctx.operation_repo.sqlite_pool();
                remove_workspace_artifact_for_lease_by_id(&pool, &ctx.events, &lease_id).await?;
                Ok(())
            }
            "release_workspace_lease" => {
                let lease_id = step_arg_string(step, "lease_id")?;
                let pool = ctx.operation_repo.sqlite_pool();
                release_workspace_lease_by_id(&pool, &ctx.events, &lease_id).await?;
                Ok(())
            }
            "cleanup_claude_worker" => {
                let card_id = step_arg_string(step, "card_id")?;
                let terminal_id = step_arg_string(step, "terminal_id")?;
                compensate_worker_rows(
                    ctx.repo.as_ref(),
                    ctx.terminal_renderer.as_ref(),
                    &self.card_role_cache,
                    &card_id,
                    &terminal_id,
                )
                .await;
                Ok(())
            }
            "delete_claude_settings_dir" => {
                let settings_dir = step_arg_string(step, "settings_dir")?;
                remove_dir_all_idempotent(Path::new(&settings_dir))
            }
            other => Err(CalmError::Internal(format!(
                "unknown claude worker compensation op {other}"
            ))),
        }
    }
}

async fn mark_claude_worker_running(
    ctx: &SpawnCtx,
    card_role_cache: &CardRoleCache,
    wave_cove_cache: &WaveCoveCache,
    card_id: &str,
    terminal_id: &str,
    wave_id: &WaveId,
) -> Result<()> {
    let card_id_string = card_id.to_string();
    let existing = ctx
        .repo
        .session_projection_active_for_card(&card_id_string)
        .await?;
    let needs_status_write = existing
        .as_ref()
        .map(|runtime| runtime.status != WorkerSessionState::Running)
        .unwrap_or(true);
    if !needs_status_write {
        return Ok(());
    }

    let scope = card_scope(
        ctx.repo.as_ref(),
        CardId::from(card_id.to_string()),
        wave_id.clone(),
    )
    .await?;
    let write = WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone());
    let card_id_for_tx = card_id.to_string();
    let (_unit, _ids) = write_with_events_typed(
        ctx.repo.as_ref(),
        ActorId::Kernel,
        None,
        &ctx.events,
        &write,
        move |tx| {
            Box::pin(async move {
                let runtime = session_projection_active_for_card_tx(tx, &card_id_for_tx)
                    .await?
                    .ok_or_else(|| {
                        CalmError::Internal(format!(
                            "claude worker card {card_id_for_tx} has no active runtime to mark running"
                        ))
                    })?;
                let old_status = runtime.status;
                let runtime_id = runtime.id.clone();
                session_set_status_tx(tx, &runtime.id, WorkerSessionState::Running).await?;
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
    tracing::debug!(
        card_id,
        terminal_id,
        "claude worker runtime marked running after spawn"
    );
    Ok(())
}

async fn log_claude_worker_card_added(
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

fn project_claude_runtime_fields_for_response(
    mut card: Card,
    terminal_id: &str,
    claude_session_id: &str,
) -> Card {
    if let Some(map) = card.payload.as_object_mut() {
        map.entry("terminal_id".to_string())
            .or_insert_with(|| Value::String(terminal_id.to_string()));
        map.entry("claude_session_id".to_string())
            .or_insert_with(|| Value::String(claude_session_id.to_string()));
    }
    card
}

fn settings_path_parent(path: &Path) -> Result<PathBuf> {
    path.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| CalmError::Internal("claude settings_path has no parent".into()))
}

fn remove_dir_all_idempotent(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(CalmError::Internal(format!(
            "delete claude settings dir {}: {e}",
            path.display()
        ))),
    }
}

fn step_arg_string(step: &CompensationStep, key: &str) -> Result<String> {
    step.args
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| CalmError::Internal(format!("claude compensation step missing {key} arg")))
}

#[cfg(test)]
mod tests;
