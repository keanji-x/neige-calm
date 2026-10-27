use crate::shell_command::sh_single_quote;
use provider::control::NativeInterruptOutcome;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::card_role_cache::CardRoleCache;
use crate::codex_appserver::TurnApprovals;
use crate::db::{write_in_tx_typed, write_with_events_typed};
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, SYNC_EVENT_VERSION};
use crate::git_candidate::delivery::AttemptOutcome;
use crate::ids::{ActorId, CardId, TrackId};
use crate::mcp_server::McpServer;
use crate::mcp_server::wiring::{card_mcp_env, mint_and_persist_card_token};
use crate::model::{Card, CardRole, new_id, now_ms};
use crate::operation::task_gate_run::GateRunWait;
use crate::operation::task_prompt::render_task_worker_prompt_tx;
use crate::operation::worker_cleanup::{WorkerCleanupOutcome, compensate_worker_rows};
use crate::operation::workspace_lease::{
    ReleaseDelivery, acquire_workspace_lease_tx,
    release::release_workspace_lease_by_id,
    worker::{record_catch_up, record_declared_head, verify_recorded_head, verify_worker_checkout},
};
use crate::pending_codex_threads::{PendingEntry, PendingThreadStartRegistry};
use crate::routes::cards::card_scope;
use crate::routes::codex_cards::{
    INITIAL_TURN_LIFECYCLE_TIMEOUT, await_shared_initial_turn_lifecycle, default_cwd,
    normalize_optional_css_color,
};
use crate::routes::settings::load_settings;
use crate::routes::theme::RequestTheme;
use crate::session_projection_repo::{
    AgentProvider, ThreadAttribution, WorkerSessionKind, WorkerSessionState,
};
use crate::shared_codex_appserver::{
    MintedThread, SharedCodexAppServer, SharedThreadStartParams, ThreadConfig,
};
use crate::state::{CodexClient, WriteContext};
use crate::terminal_sweeper::try_reap_terminal_artifacts_with_renderer;
use crate::track_area_cache::TrackAreaCache;
use calm_truth::db::sqlite::{
    append_decision_event_in_tx, card_update_tx, card_with_codex_create_tx,
    session_bind_attribution_tx, session_projection_active_for_card_tx,
    session_projection_by_id_tx, session_set_status_tx,
};
use provider::InputItem;
use provider::TurnModelSelection;

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
    track_area_cache: TrackAreaCache,
    initial_turn_lifecycle_timeout: Duration,
    #[cfg(feature = "fixtures")]
    spawn_hook: Option<SpawnHook>,
}

#[derive(Clone)]
pub struct CodexWorkerAdapter {
    repo: Arc<dyn crate::db::RouteRepo>,
    codex: Arc<CodexClient>,
    codex_backends: Arc<crate::agent_backend_pool::codex::CodexBackends>,
    mcp_server: Option<Arc<McpServer>>,
    card_role_cache: CardRoleCache,
    track_area_cache: TrackAreaCache,
    /// The managed workspace root: taking a lease re-runs materialization for a managed track. Boot-frozen config, threaded rather than read from a global.
    workspace_root: std::path::PathBuf,
    /// The wait of one gate-run call its prompt states (#2464 D3).
    gate_run_wait: GateRunWait,
    #[cfg(feature = "fixtures")]
    preparation_hook: Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
    #[cfg(test)]
    viewer_preparation_hook: Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
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
        track_area_cache: TrackAreaCache,
    ) -> Self {
        Self {
            repo,
            codex,
            shared_codex_appserver,
            pending_codex_threads,
            pending_codex_threads_spawn_serial,
            card_role_cache,
            track_area_cache,
            initial_turn_lifecycle_timeout: INITIAL_TURN_LIFECYCLE_TIMEOUT,
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
        track_area_cache: TrackAreaCache,
        spawn_hook: SpawnHook,
    ) -> Self {
        Self {
            repo,
            codex,
            shared_codex_appserver,
            pending_codex_threads,
            pending_codex_threads_spawn_serial,
            card_role_cache,
            track_area_cache,
            initial_turn_lifecycle_timeout: INITIAL_TURN_LIFECYCLE_TIMEOUT,
            spawn_hook: Some(spawn_hook),
        }
    }

    #[cfg(feature = "fixtures")]
    pub fn with_initial_turn_lifecycle_timeout(mut self, timeout: Duration) -> Self {
        self.initial_turn_lifecycle_timeout = timeout;
        self
    }
}

impl CodexWorkerAdapter {
    /// The configured wait of one gate-run call (`GateRunWait::DEFAULT` until boot sets it).
    pub fn with_gate_run_wait(mut self, wait: GateRunWait) -> Self {
        self.gate_run_wait = wait;
        self
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        repo: Arc<dyn crate::db::RouteRepo>,
        codex: Arc<CodexClient>,
        codex_backends: Arc<crate::agent_backend_pool::codex::CodexBackends>,
        mcp_server: Option<Arc<McpServer>>,
        card_role_cache: CardRoleCache,
        track_area_cache: TrackAreaCache,
        workspace_root: std::path::PathBuf,
    ) -> Self {
        Self {
            repo,
            codex,
            codex_backends,
            mcp_server,
            card_role_cache,
            track_area_cache,
            workspace_root,
            gate_run_wait: GateRunWait::DEFAULT,
            #[cfg(feature = "fixtures")]
            preparation_hook: None,
            #[cfg(test)]
            viewer_preparation_hook: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CodexCreateOperationPayload {
    pub actor: ActorId,
    #[serde(default)]
    /// Wire key frozen as `runtime_id`: stored payloads keep the old key, and without the `rename` a parked row with a real id
    /// would deserialize to `None` and `unwrap_or_else(new_id)` below would mint a FRESH session id for it.
    #[serde(rename = "runtime_id")]
    pub worker_session_id: Option<String>,
    pub request: NormalizedCodexCreateRequest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CodexWorkerOperationPayload {
    pub actor: ActorId,
    pub track_id: String,
    pub idempotency_key: String,
    pub goal: String,
    /// Forward-compatible only; scheduler-created payloads keep this absent because the workspace lease path is the worker cwd.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub context: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_criteria: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CodexCreateRequestInput {
    pub track_id: String,
    pub title: Option<String>,
    pub sort: Option<f64>,
    pub cwd: Option<String>,
    pub prompt: Option<String>,
    pub icon_bg: Option<String>,
    pub icon_fg: Option<String>,
    pub theme: RequestTheme,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NormalizedCodexCreateRequest {
    pub track_id: String,
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
        track_id: input.track_id,
        title: input.title,
        sort: input.sort,
        cwd,
        prompt,
        icon_bg,
        icon_fg,
        theme: input.theme,
    })
}

/// The payload as the transaction sees it, so an owned-keys merge writes back over current data instead of a pre-tx snapshot.
pub(crate) async fn card_payload_get_tx(
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

async fn build_codex_env(
    repo: &dyn crate::db::RouteRepo,
    codex: &CodexClient,
    card_id: &str,
    home: &std::path::Path,
) -> Result<Value> {
    let settings = load_settings(repo).await?;
    let mut env_map = serde_json::Map::new();
    env_map.insert(
        "CODEX_HOME".to_string(),
        Value::String(home.to_string_lossy().to_string()),
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
mod tests;

mod worker_preparation;

mod interactive;
mod worker;
mod worker_spawn;
use worker_spawn::*;
