//! Plugin host — the kernel's side of the plugin protocol.

pub use crate::{
    auth, cli_query, config, error, forge_caller, http_headers, http_mcp, manifest, mcp, perms,
    process,
};
mod builtin;
pub mod connector;
pub mod events;
pub mod lifecycle;
pub mod managed;
pub mod mcp_setup;
pub mod ports;
pub mod registry;
pub mod resources;
pub use crate::template_input;
pub use crate::version;
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
pub use auth::{PluginToken, hash_token, verify_token};
pub use cli_query::{CLI_QUERY_BRINGUP_BUDGET, CliQueryRuntime};
pub use config::{effective_config, missing_required};
pub use connector::{ConnectorClient, SecretsError, read_secrets};
pub use error::{HostError, McpError, ProcessError};
pub use http_mcp::{HttpCredential, HttpMcpClient};
pub use managed::ConnectorInstall;
pub use manifest::{CONFIG_SCHEMA_KEY, ConnectorKind, Manifest};
pub use mcp::{
    CallToolResult, ContentBlock, InboundNotification, InboundRequest, InitializeMeta, McpClient,
    RequestId, ResourceContent, ResourceContents, RpcError,
};
pub use process::PluginProcess;
pub use registry::{PluginRegistry, PluginRegistryBuilder};
pub use resources::{ResourceError, read_ui_resource};
pub use version::{KernelTooOld, check_min_kernel_version};

use tokio::sync::{Mutex, mpsc};

use crate::ports::ErrorFactory;
use ports::{
    Backends, CallbackInvocation, Callbacks, PluginRecord as Plugin, StateSink, Store,
    SubscriptionRecord,
};

/// SIGTERM → SIGKILL grace.
const STOP_GRACE: Duration = Duration::from_secs(2);

/// Crash-loop window: this many crashes within it disables the plugin until an explicit `spawn(id)`.
const CRASH_WINDOW: Duration = Duration::from_secs(300);
const CRASH_WINDOW_LIMIT: u32 = 5;

/// Exponential-backoff schedule for respawn: 1, 2, 4, 8, 30, 30, ...
const BACKOFF_SCHEDULE_MS: &[u64] = &[1_000, 2_000, 4_000, 8_000, 30_000];

/// How many times the supervisor re-reads the plugin row before leaving the plugin `Crashed`.
const LIFECYCLE_DB_READ_RETRIES: u32 = 5;

const LIFECYCLE_DB_READ_RETRY_DELAY: Duration = Duration::from_millis(200);

/// Whether a spawn refused for a template or minted-name conflict publishes `crashed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConflictReport {
    /// The plugin stays enabled and is not running; the live state says why.
    Publish,
    /// The caller undoes its own enable on this refusal, so the request changes nothing at all.
    Silent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginRuntimeStatus {
    Installing,
    Spawning,
    Running,
    /// Crash-looped or otherwise unrecoverable. Carries the latest error.
    Crashed {
        reason: String,
    },
    /// No process was started and no supervisor is watching: terminal until an operator re-enables.
    Unavailable {
        reason: String,
    },
    Disabled,
}

impl PluginRuntimeStatus {
    pub fn wire_name(&self) -> &'static str {
        match self {
            Self::Installing => "installing",
            Self::Spawning => "spawning",
            Self::Running => "running",
            Self::Crashed { .. } => "crashed",
            Self::Unavailable { .. } => "unavailable",
            Self::Disabled => "disabled",
        }
    }

    pub fn last_error(&self) -> Option<&str> {
        match self {
            Self::Crashed { reason } | Self::Unavailable { reason } => Some(reason.as_str()),
            _ => None,
        }
    }
}

struct RunningPlugin {
    /// `None` for connectors: they have no kernel-supervised child.
    process: Option<Arc<PluginProcess>>,
    /// `None` means the entry exists only to make a terminal `Unavailable` observable; there is nothing to call.
    mcp: Option<ConnectorClient>,
    status: PluginRuntimeStatus,
    /// Set by `stop()` so the supervisor does NOT respawn.
    stopping: bool,
    /// Cumulative crash count within the current rolling window.
    crashes_in_window: u32,
    window_started: Instant,
    /// Identity of THIS run instance; the supervisor uses it to check the entry is still the one it was supervising.
    run_epoch: u64,
    /// Monotonic crash count for the life of the entry; unlike `crashes_in_window` it is never reset.
    crash_attempt: u64,
    /// Supervisor task handle. Aborted on graceful stop so we don't leak.
    supervisor: Option<tokio::task::JoinHandle<()>>,
    /// Router task draining inbound MCP requests; `None` for connectors.
    router: Option<tokio::task::JoinHandle<()>>,
    /// `neige.event.subscribe` bridge tasks; `stop()` aborts them all before killing the process.
    subscriptions: Arc<Mutex<Vec<SubscriptionRecord>>>,
}

/// All plugin runtime state under ONE std mutex so admission is atomic; never held across an `.await`.
/// `spawning` is the admission set: an id counts as a template-id holder from admission until swapped for a `live` entry or released.
#[derive(Default)]
struct ProcessTable {
    live: HashMap<String, RunningPlugin>,
    spawning: BTreeSet<String>,
}

/// RAII admission reservation, held across the whole `spawn_admitted` future: the success swap disarms it under the table lock; every other exit (`Err`, abort, panic) releases the reservation via `Drop`.
/// `Drop` only locks when still armed, and no code path drops an armed guard while holding the table lock.
struct AdmissionGuard<E: ErrorFactory> {
    host: Arc<PluginHost<E>>,
    id: String,
    armed: bool,
}

impl<E: ErrorFactory> AdmissionGuard<E> {
    fn new(host: Arc<PluginHost<E>>, id: String) -> Self {
        Self {
            host,
            id,
            armed: true,
        }
    }

    /// Consume without releasing: only for the success path's reservation→live swap, which removes the reservation itself.
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl<E: ErrorFactory> Drop for AdmissionGuard<E> {
    fn drop(&mut self) {
        if self.armed {
            self.host.lock_table().spawning.remove(&self.id);
        }
    }
}

impl ProcessTable {
    /// Ids that hold their manifests' template ids: `Running` live plugins plus admission-reserved ids.
    fn template_holder_ids(&self) -> BTreeSet<String> {
        let mut ids: BTreeSet<String> = self
            .live
            .iter()
            .filter(|(_, rp)| matches!(rp.status, PluginRuntimeStatus::Running))
            .map(|(id, _)| id.clone())
            .collect();
        ids.extend(self.spawning.iter().cloned());
        ids
    }
}

/// Per-plugin runtime view exposed to callers.
#[derive(Debug, Clone)]
pub struct PluginHostStatus {
    pub id: String,
    pub status: PluginRuntimeStatus,
    pub pid: Option<u32>,
}

/// Crash-window / respawn-backoff knobs; `Default` reproduces the module constants.
#[derive(Debug, Clone)]
pub struct BackoffConfig {
    /// Respawn delays indexed by `attempts - 1`, clamped to the last entry.
    pub schedule_ms: Vec<u64>,
    /// Sliding window over which crashes are counted.
    pub crash_window: Duration,
    /// Crashes within `crash_window` that stop the respawn loop.
    pub crash_window_limit: u32,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            schedule_ms: BACKOFF_SCHEDULE_MS.to_vec(),
            crash_window: CRASH_WINDOW,
            crash_window_limit: CRASH_WINDOW_LIMIT,
        }
    }
}

/// Crash-window counters carried explicitly across the supervisor's `live.remove` into the respawn.
#[derive(Debug, Clone, Copy)]
struct CrashWindow {
    crashes: u32,
    started: Instant,
}

/// Narrow port for the plugin-row read that starts boot autospawn.
/// Implementors must stay cooperative: the boot fence can only preempt at await points.
#[async_trait]
pub trait PluginListDb<E: ErrorFactory>: Send + Sync {
    async fn plugins_list_all(&self) -> Result<Vec<Plugin>, E>;
}

struct RepoPluginListDb<E: ErrorFactory> {
    repo: Arc<dyn Store<E>>,
}
#[async_trait]
impl<E: ErrorFactory> PluginListDb<E> for RepoPluginListDb<E> {
    async fn plugins_list_all(&self) -> Result<Vec<Plugin>, E> {
        self.repo.plugins_list_all().await
    }
}
pub struct HostInputs<E: ErrorFactory> {
    pub registry: Arc<PluginRegistry>,
    pub store: Arc<dyn Store<E>>,
    pub plugins_dir: PathBuf,
    pub plugins_data_dir: PathBuf,
    pub plugins_disabled: Vec<String>,
    pub callbacks: Arc<dyn Callbacks>,
    pub state_sink: Arc<dyn StateSink>,
    pub backends: Arc<dyn Backends>,
    pub kernel_version: semver::Version,
}

pub struct PluginHost<E: ErrorFactory> {
    registry: Arc<PluginRegistry>,
    /// `RouteRepo`, not `Repo`: raw sync-domain writes are unreachable so the host cannot bypass the audit log.
    pub(crate) repo: Arc<dyn Store<E>>,
    /// Narrow read port used only by boot autospawn's initial enumeration.
    plugin_list_db: Arc<dyn PluginListDb<E>>,
    /// Wall-clock fence for the initial plugin enumeration.
    plugin_list_wall: Duration,
    /// Resolved per-plugin mutable-state root from `Config::plugins_data_dir_resolved`.
    pub plugins_data_dir: PathBuf,
    /// Plugin install root; fallback when the registry has no install_path.
    pub plugins_dir: PathBuf,
    /// Plugin ids the operator has explicitly disabled via config.
    plugins_disabled: Vec<String>,
    callbacks: Arc<dyn Callbacks>,
    state_sink: Arc<dyn StateSink>,
    backends: Arc<dyn Backends>,
    kernel_version: semver::Version,
    processes: std::sync::Mutex<ProcessTable>,
    /// Recorded order of the two connector-spawn steps whose relative order is the invariant.
    spawn_order: std::sync::Mutex<HashMap<String, ConnectorSpawnOrder>>,
    /// Ids whose CURRENT spawn passed through `config_for_spawn_or_unavailable`; cleared per attempt.
    config_gate: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Spawns that reached `Running` without passing the config gate, per plugin id (a release build's record of the breach).
    config_gate_breaches: std::sync::Mutex<HashMap<String, u64>>,
    /// **The** per-plugin-id lifecycle lock: every lifecycle operation and every `plugin.state` emission it produces run inside one `LifecycleGuard`.
    /// Lock order is one-way: `lifecycle` (async) → `processes` (sync) → registry (leaf).
    /// Entries are never removed: a fresh mutex handed to the next caller while an old guard is alive would break mutual exclusion.
    lifecycle: std::sync::Mutex<HashMap<String, Arc<LifecycleCell>>>,
    /// Allocator for per-run-instance identity.
    run_epoch_seq: std::sync::atomic::AtomicU64,
    /// Narrow DB port for the supervisor's third segment and the enable/disable pair.
    lifecycle_db: Arc<dyn lifecycle::LifecycleDb<E>>,
    /// The `app` half of boot's wall-clock fence.
    app_autospawn_wall: Duration,
    /// Crash-loop / respawn-backoff tunables.
    backoff: BackoffConfig,
}

/// Per-id lifecycle lock; created on first use and never removed.
struct LifecycleCell {
    lock: Arc<Mutex<()>>,
}

/// Proof that the caller holds the lifecycle lock for `id`; only the two acquisition functions construct one.
pub struct LifecycleGuard {
    id: String,
    _held: tokio::sync::OwnedMutexGuard<()>,
}

impl LifecycleGuard {
    /// The plugin id this guard is held for.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// A guard over a throwaway mutex, for the lib's own unit tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(id: &str) -> Self {
        let lock = Arc::new(Mutex::new(()));
        let held = lock.try_lock_owned().expect("fresh mutex");
        Self {
            id: id.to_string(),
            _held: held,
        }
    }
}

pub use calm_types::boot_budget::{
    APP_AUTOSPAWN_WALL, CONNECTOR_AUTOSPAWN_BUDGET, CONNECTOR_LOOP_WIDENING_MARGIN,
    MAX_CONNECTOR_AUTOSPAWN_WALL, MAX_CONNECTOR_BRINGUP_BUDGET, PLUGIN_LIST_WALL,
    boot_autospawn_ceiling, connector_phase_ceiling, widened_connector_budget,
};
pub(crate) use calm_types::boot_budget::{CONNECTOR_BRINGUP_SLACK, MCP_HTTP_ROUND_TRIPS};

/// The wall-clock cap on ONE connector's bring-up. Reads `bringup_timeout_ms`, never `request_timeout_ms` (the uncapped `tools/call` budget).
pub fn connector_bringup_budget(manifest: &Manifest) -> Duration {
    if let Some(block) = manifest.mcp_http.as_ref() {
        return Duration::from_millis(block.bringup_timeout_ms()) * MCP_HTTP_ROUND_TRIPS
            + CONNECTOR_BRINGUP_SLACK;
    }
    if manifest.cli_query.is_some() {
        return CLI_QUERY_BRINGUP_BUDGET;
    }
    CONNECTOR_BRINGUP_SLACK
}

/// Process-global monotonic tick behind `ConnectorSpawnOrder`; global so two hosts over one plugin dir still compare.
static SPAWN_ORDER_TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

enum SpawnOrderStep {
    Materialized,
    LiveInserted,
}

/// When each half of the materialize/live-insert pair happened. The two steps have no `.await` between them, so the ticks are the only observable witness of their order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConnectorSpawnOrder {
    /// Tick at which the tool catalog reached the registry.
    pub materialized_at: Option<u64>,
    /// Tick at which the id became visible as `Running`.
    pub live_inserted_at: Option<u64>,
}

impl ConnectorSpawnOrder {
    /// `true` iff materialization is recorded strictly before the live insert; a missing half is not ok.
    pub fn materialized_before_live_insert(&self) -> bool {
        match (self.materialized_at, self.live_inserted_at) {
            (Some(m), Some(l)) => m < l,
            _ => false,
        }
    }
}

/// The network half of `spawn_mcp_http`, factored out so ONE timeout can bound all of it.
async fn connect_mcp_http(
    id: &str,
    block: &manifest::McpHttpBlock,
    url: &manifest::ResolvedMcpUrl,
    install_path: PathBuf,
    kernel_version: &semver::Version,
) -> Result<(Arc<HttpMcpClient>, Vec<serde_json::Value>), String> {
    // A wrongly-permissioned secrets file refuses the enable outright.
    let secrets = connector::read_secrets(&install_path)
        .await
        .map_err(|e| format!("secrets.json rejected: {e}"))?
        .unwrap_or_default();

    // The point at which a secret becomes an HTTP credential; `HttpCredential` carries the redaction constraints.
    let api_key = match block.api_key_secret.as_deref() {
        Some(name) => {
            let raw = secrets.get(name).cloned().ok_or_else(|| {
                format!(
                    "mcp_http.api_key_secret names `{name}`, which is absent from {}",
                    install_path.join(connector::SECRETS_FILENAME).display()
                )
            })?;
            // The reason names the rule, never the value: it is persisted and
            // broadcast as `PluginState.last_error`.
            Some(HttpCredential::parse(&raw).map_err(|why| {
                format!(
                    "the credential in {} named by mcp_http.api_key_secret (`{name}`) {why}",
                    install_path.join(connector::SECRETS_FILENAME).display()
                )
            })?)
        }
        None => None,
    };

    let headers = http_headers::HttpHeaders::parse(
        block
            .header_secrets
            .iter()
            .map(|(header, key)| {
                secrets
                    .get(key)
                    .cloned()
                    .map(|value| (header.clone(), value))
                    .ok_or_else(|| {
                        "a configured HTTP header is missing from secrets.json".to_string()
                    })
            })
            .collect::<Result<_, _>>()?,
    )?;
    let client =
        Arc::new(HttpMcpClient::new(id, url, block, api_key.as_ref()).with_headers(headers));

    // Best-effort: an `initialize` failure is informational; it shares the caller's budget.
    if let Err(e) = client.initialize(kernel_version.to_string().as_str()).await {
        tracing::info!(
            plugin_id = %id,
            target = %client.log_target(),
            error = %e,
            "mcp-http connector did not answer initialize; continuing to tools/list"
        );
    }

    // `tools_list` drains pagination. Every page shares the existing outer
    // `connector_bringup_budget`; discovery never widens boot time.
    let upstream = client
        .tools_list()
        .await
        .map_err(|e| format!("tools/list against {} failed: {e}", client.log_target()))?;
    Ok((client, upstream))
}

mod admission;
mod boot;
mod configuration;
mod connectors;
mod core;
mod spawn_app;
mod state;
mod supervision;
mod support;
use support::*;
