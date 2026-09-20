//! Plugin host — the kernel's side of the plugin protocol.

pub mod auth;
pub mod callbacks;
pub mod child_process;
pub mod cli_query;
pub mod config;
pub mod connector;
pub mod error;
pub mod events;
mod glob;
pub mod http_headers;
pub mod http_mcp;
pub mod lifecycle;
pub mod managed;
pub mod manifest;
pub mod mcp;
pub mod mcp_setup;
pub mod perms;
pub mod process;
pub mod registry;
pub mod resources;
pub mod template_input;
pub mod version;

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
pub use version::{KERNEL_VERSION, KernelTooOld, check_min_kernel_version};

use tokio::sync::{Mutex, mpsc};

use crate::db::RouteRepo;
use crate::event::{Event, EventBus, EventScope};
use crate::forge_trust::trusted_forge_plugin;
use crate::ids::ActorId;
use crate::model::Plugin;
use crate::state::WriteContext;

use callbacks::{CallbackCtx, SubscriptionRecord};

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
struct AdmissionGuard {
    host: Arc<PluginHost>,
    id: String,
    armed: bool,
}

impl AdmissionGuard {
    fn new(host: Arc<PluginHost>, id: String) -> Self {
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

impl Drop for AdmissionGuard {
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
pub trait PluginListDb: Send + Sync {
    async fn plugins_list_all(&self) -> Result<Vec<Plugin>, crate::error::CalmError>;
}

struct RepoPluginListDb {
    repo: Arc<dyn RouteRepo>,
}

#[async_trait]
impl PluginListDb for RepoPluginListDb {
    async fn plugins_list_all(&self) -> Result<Vec<Plugin>, crate::error::CalmError> {
        self.repo.plugins_list_all().await.map_err(Into::into)
    }
}

pub struct PluginHost {
    registry: Arc<PluginRegistry>,
    /// `RouteRepo`, not `Repo`: raw sync-domain writes are unreachable so the host cannot bypass the audit log.
    pub(crate) repo: Arc<dyn RouteRepo>,
    /// Narrow read port used only by boot autospawn's initial enumeration.
    plugin_list_db: Arc<dyn PluginListDb>,
    /// Wall-clock fence for the initial plugin enumeration.
    plugin_list_wall: Duration,
    /// Resolved per-plugin mutable-state root from `Config::plugins_data_dir_resolved`.
    pub plugins_data_dir: PathBuf,
    /// Plugin install root; fallback when the registry has no install_path.
    pub plugins_dir: PathBuf,
    /// Plugin ids the operator has explicitly disabled via config.
    plugins_disabled: Vec<String>,
    /// Live broadcaster for `Event::PluginState`. Kept as an `Option` so test
    /// shims can leave it `None` and skip emissions.
    events: Option<EventBus>,
    /// Same bus as an `Arc` for the router; a private bus when `events` is `None` so dispatch keeps working.
    events_arc: Arc<EventBus>,
    /// Write-surface caches shared with REST/worker paths.
    write: WriteContext,
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
    lifecycle_db: Arc<dyn lifecycle::LifecycleDb>,
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
    #[cfg(test)]
    pub(crate) fn for_test(id: &str) -> Self {
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
    if let Err(e) = client.initialize().await {
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

#[allow(deprecated)]
impl PluginHost {
    /// Real boot-time constructor.
    #[allow(clippy::too_many_arguments)]
    pub fn new_full(
        registry: Arc<PluginRegistry>,
        repo: Arc<dyn RouteRepo>,
        plugins_dir: PathBuf,
        plugins_data_dir: PathBuf,
        plugins_disabled: Vec<String>,
        events: EventBus,
        write: WriteContext,
    ) -> Self {
        let events_arc = Arc::new(events.clone());
        let lifecycle_db: Arc<dyn lifecycle::LifecycleDb> =
            Arc::new(lifecycle::RepoLifecycleDb::new(Arc::clone(&repo)));
        let plugin_list_db: Arc<dyn PluginListDb> = Arc::new(RepoPluginListDb {
            repo: Arc::clone(&repo),
        });
        Self {
            registry,
            repo,
            plugin_list_db,
            plugin_list_wall: PLUGIN_LIST_WALL,
            plugins_dir,
            plugins_data_dir,
            plugins_disabled,
            events: Some(events),
            events_arc,
            write,
            processes: std::sync::Mutex::new(ProcessTable::default()),
            spawn_order: std::sync::Mutex::new(HashMap::new()),
            config_gate: std::sync::Mutex::new(std::collections::HashSet::new()),
            config_gate_breaches: std::sync::Mutex::new(HashMap::new()),
            lifecycle: std::sync::Mutex::new(HashMap::new()),
            run_epoch_seq: std::sync::atomic::AtomicU64::new(1),
            lifecycle_db,
            app_autospawn_wall: APP_AUTOSPAWN_WALL,
            backoff: BackoffConfig::default(),
        }
    }

    /// Post-construction override of the narrow `LifecycleDb` port; production never calls this.
    #[must_use]
    pub fn with_lifecycle_db(mut self, db: Arc<dyn lifecycle::LifecycleDb>) -> Self {
        self.lifecycle_db = db;
        self
    }

    /// Post-construction override of the plugin-list read port; production never calls this.
    /// Overrides enumeration only; later reads and writes still use `self.repo`.
    #[must_use]
    pub fn with_plugin_list_db(mut self, db: Arc<dyn PluginListDb>) -> Self {
        self.plugin_list_db = db;
        self
    }

    /// Post-construction override of `PLUGIN_LIST_WALL`; production never calls this.
    #[must_use]
    pub fn with_plugin_list_wall(mut self, wall: Duration) -> Self {
        self.plugin_list_wall = wall;
        self
    }

    /// Post-construction override of the crash-window / respawn backoff tunables. `schedule_ms` must be non-empty.
    #[must_use]
    pub fn with_backoff_schedule(
        mut self,
        schedule_ms: Vec<u64>,
        crash_window: Duration,
        crash_window_limit: u32,
    ) -> Self {
        assert!(
            !schedule_ms.is_empty(),
            "backoff schedule must have at least one entry"
        );
        self.backoff = BackoffConfig {
            schedule_ms,
            crash_window,
            crash_window_limit,
        };
        self
    }

    /// Post-construction override of `APP_AUTOSPAWN_WALL`.
    #[must_use]
    pub fn with_app_autospawn_wall(mut self, wall: Duration) -> Self {
        self.app_autospawn_wall = wall;
        self
    }

    /// Read-only view of the registry: the mutators are module-private and take a `LifecycleGuard`, so this handle grants no write capability outside `plugin_host`.
    pub fn registry(&self) -> &Arc<PluginRegistry> {
        &self.registry
    }

    /// The `Arc<Mutex>` for `id`, creating the map entry on first use.
    fn lifecycle_cell(&self, id: &str) -> Arc<Mutex<()>> {
        let mut map = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(
            &map.entry(id.to_string())
                .or_insert_with(|| {
                    Arc::new(LifecycleCell {
                        lock: Arc::new(Mutex::new(())),
                    })
                })
                .lock,
        )
    }

    /// **External** acquisition: non-blocking, returns `LifecycleBusy` having done nothing, so the refusal is safely retryable.
    pub fn try_lock_lifecycle(&self, id: &str) -> Result<LifecycleGuard, HostError> {
        let cell = self.lifecycle_cell(id);
        match cell.try_lock_owned() {
            Ok(held) => Ok(LifecycleGuard {
                id: id.to_string(),
                _held: held,
            }),
            Err(_) => Err(HostError::LifecycleBusy(id.to_string())),
        }
    }

    /// **Internal** acquisition: waits. For callers with nobody to answer (crash supervisor, boot reconciliation); each must re-decide everything afterwards. No wait budget on purpose.
    async fn await_lifecycle(&self, id: &str) -> LifecycleGuard {
        let cell = self.lifecycle_cell(id);
        let held = cell.lock_owned().await;
        LifecycleGuard {
            id: id.to_string(),
            _held: held,
        }
    }

    /// Allocate the next [`RunningPlugin::run_epoch`].
    fn next_run_epoch(&self) -> u64 {
        self.run_epoch_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// Runtime registry write, routed through the host so it requires the lifecycle guard.
    pub fn registry_insert(
        &self,
        guard: &LifecycleGuard,
        manifest: manifest::Manifest,
        install_path: Option<PathBuf>,
    ) {
        self.registry.insert(guard, manifest, install_path);
    }

    /// Runtime registry removal.
    pub fn registry_remove(&self, guard: &LifecycleGuard) -> Option<manifest::Manifest> {
        self.registry.remove(guard)
    }

    /// Lock the process table, recovering from poison so `AdmissionGuard::drop` can release during a panic unwind.
    fn lock_table(&self) -> std::sync::MutexGuard<'_, ProcessTable> {
        self.processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn write(&self) -> &WriteContext {
        &self.write
    }

    /// Always returns a real bus, even when no bus is configured.
    fn events_arc(&self) -> Arc<EventBus> {
        Arc::clone(&self.events_arc)
    }

    /// The effective configuration for a spawn plus the row's `enabled` bit (`None` = no row).
    /// A missing row is `{}`; a DB failure fails the spawn, since defaults could mask an operator's real configuration.
    /// Returns a bare `Err(String)` so the caller must publish `Unavailable` rather than `?` it away.
    async fn effective_config_for_spawn(
        &self,
        id: &str,
        manifest: &Manifest,
    ) -> Result<(Option<bool>, serde_json::Map<String, serde_json::Value>), String> {
        let (enabled, user_config) = match self.repo.plugin_get_by_id(id).await {
            Ok(Some(row)) => (Some(row.enabled), row.user_config),
            Ok(None) => (None, serde_json::Value::Object(Default::default())),
            Err(e) => return Err(e.to_string()),
        };
        Ok((enabled, config::effective_config(manifest, &user_config)))
    }

    /// **The** spawn-time configuration gate for every kind: read the effective configuration, refuse if unreadable or a `required` key is missing, and hand the admission reservation back on success.
    /// The `app` path calls it before `emit_state(Spawning)`; the connector paths call it after, so those emit `Spawning` then `Unavailable`.
    /// The guard is taken by value: both failure arms consume it to publish the terminal entry.
    async fn config_for_spawn_or_unavailable(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        manifest: &Manifest,
        guard: AdmissionGuard,
    ) -> Result<(serde_json::Map<String, serde_json::Value>, AdmissionGuard), HostError> {
        let id = lifecycle.id();
        // Stamped first, before either refusal arm: the claim recorded is 'the gate ran', not 'the gate passed'.
        self.config_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.to_string());
        let (enabled, effective) = match self.effective_config_for_spawn(id, manifest).await {
            Ok(pair) => pair,
            Err(detail) => {
                // A `?` here would drop `guard` un-disarmed with no live entry and no event, so `status` would answer `None`.
                let reason = format!("could not read stored configuration: {detail}");
                let _ = self
                    .publish_unavailable_under(lifecycle, Some(guard), reason.clone())
                    .await;
                return Err(HostError::ConfigUnreadable {
                    plugin_id: id.to_string(),
                    reason,
                });
            }
        };

        // The operator's `enabled` bit is spawn admission, enforced here because every spawn of every kind goes through this door.
        // A row that says `enabled = false` refuses; no row at all proceeds. No `Unavailable` entry: the operator turned it off, and dropping `guard` releases the reservation.
        if enabled == Some(false) {
            drop(guard);
            return Err(HostError::OperatorDisabled(id.to_string()));
        }

        let missing = config::missing_required(manifest, &effective);
        if !missing.is_empty() {
            let reason = config::missing_required_reason(&missing);
            // `Unavailable`, not `Crashed`: `Crashed` cannot carry the `last_error` via `status`, and nothing crashed — no child was spawned and no supervisor is installed.
            let _ = self
                .publish_unavailable_under(lifecycle, Some(guard), reason.clone())
                .await;
            return Err(HostError::MissingRequiredConfig {
                plugin_id: id.to_string(),
                reason,
            });
        }

        Ok((effective, guard))
    }

    /// Mint + persist a fresh process token; only the hash is kept, so a kernel restart forces every plugin to re-handshake with a fresh credential.
    /// Takes the guard because `uninstall` deletes `plugin_tokens` and the write must be inside the same critical section as the spawn.
    pub async fn ensure_plugin_token(&self, guard: &LifecycleGuard) -> Result<String, HostError> {
        let id = guard.id();
        let raw = PluginToken::generate();
        let hashed = hash_token(raw.as_str());
        self.repo
            .plugin_token_set(id, &hashed, i64::MAX)
            .await
            .map_err(|e| HostError::BadState(format!("plugin_token_set({id}): {e}")))?;
        Ok(raw.into_inner())
    }

    /// Forced rotation: clear the token slot, then restart (the spawn mints the new token), or — when `enabled = false` — stop and delete without restarting.
    pub async fn rotate_plugin_token(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        // Reject-only pre-lock probe; the same check `rotate_plugin_token_under` opens with.
        self.rotate_admission_check(id)?;
        let guard = self.try_lock_lifecycle(id)?;
        self.rotate_plugin_token_under(&guard).await
    }

    /// `rotate_plugin_token`'s opening checks, shared by the pre-lock probe and the in-guard decision. Reject-only: the returned `Manifest` is authoritative only under the guard.
    fn rotate_admission_check(&self, id: &str) -> Result<Manifest, HostError> {
        let Some(manifest) = self.registry.get(id) else {
            return Err(HostError::NotFound(id.to_string()));
        };
        if !manifest.kind.is_app() {
            return Err(HostError::UnsupportedForKind {
                plugin_id: id.to_string(),
                kind: manifest.kind.wire_name(),
                operation: "token rotation (connectors are never issued a plugin token)",
            });
        }
        Ok(manifest)
    }

    async fn rotate_plugin_token_under(
        self: &Arc<Self>,
        guard: &LifecycleGuard,
    ) -> Result<(), HostError> {
        let id = guard.id();
        // Refuse for connectors BEFORE the delete and the restart: they never had a token, so rotation would be a no-op delete plus a real stop+respawn. Fail closed on an unknown id.
        let _manifest = self.rotate_admission_check(id)?;
        // Read the `enabled` bit inside the guard, then branch; a read failure fails closed and is inert. An absent row is not a disabled row.
        let enabled = match self.repo.plugin_get_by_id(id).await {
            Ok(Some(row)) => row.enabled,
            Ok(None) => true,
            Err(e) => {
                return Err(HostError::BadState(format!(
                    "rotate `{id}`: the plugin row could not be read, so the \
                     `enabled` bit could not be honoured; nothing was deleted \
                     and nothing was restarted: {e}"
                )));
            }
        };

        if !enabled {
            // The restart is rotation's side effect, not its request: on a disabled plugin, stop (reconciling any orphaned `Running` process) and only then delete, so a plugin that cannot be stopped keeps its token row.
            // `NotFound` from `stop_under` is benign only if verified below: a prior failed stop leaves a `stopping` entry that answers `NotFound` while still `Running`.
            match self.stop_under(guard).await {
                Ok(()) | Err(HostError::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
            // `Running` and only `Running`: `status` also answers `Some` for `Crashed`/`Unavailable`/`Spawning`, none of which has a live process behind it; refusing on those would lock the token out forever.
            if let Some(PluginRuntimeStatus::Running) = self.status(id).await.map(|s| s.status) {
                return Err(HostError::BadState(format!(
                    "rotate `{id}`: the plugin is disabled but is still \
                     `Running` after the stop; the token was NOT deleted, \
                     because clearing it while that process keeps running \
                     would leave a live plugin the kernel can no longer \
                     account for"
                )));
            }
            // On this branch the delete IS the whole rotation (nothing follows that rewrites the hash), so a failed delete must not answer `Ok`.
            self.repo.plugin_token_delete(id).await.map_err(|e| {
                HostError::BadState(format!("rotate `{id}`: plugin_token_delete: {e}"))
            })?;
            tracing::info!(
                plugin_id = %id,
                "token rotated for a disabled plugin; not restarted, and any \
                 live process was stopped — `enable` will mint a fresh token \
                 on its next spawn"
            );
            return Ok(());
        }

        // Best-effort: the restart's `ensure_plugin_token` UPSERTs the hash whether or not this DELETE landed, so propagating would refuse a rotation that was going to happen.
        if let Err(e) = self.repo.plugin_token_delete(id).await {
            tracing::warn!(
                plugin_id = %id,
                error = %e,
                "could not delete the plugin's token row; continuing to the \
                 restart — if it reaches `ensure_plugin_token` the UPSERT \
                 overwrites the hash and the rotation completes without this \
                 delete, and if it does not, the old hash outlives it"
            );
        }
        self.restart_under(guard).await
    }

    /// Auto-spawn every enabled plugin known to the repo. Per-plugin failures are logged and swallowed.
    /// Connector bring-up stays inline and serial: the boot audit loop in `AppState::new` reads `exposes_tools` after this returns, so a background task would race the materialization.
    pub async fn autospawn_enabled(self: &Arc<Self>) {
        self.autospawn_enabled_within(CONNECTOR_AUTOSPAWN_BUDGET)
            .await;
    }

    /// `autospawn_enabled` with the connector budget supplied, so a test can drive the real loop against a small budget.
    pub async fn autospawn_enabled_within(self: &Arc<Self>, connector_budget: Duration) {
        // Enumeration needs its own wall: neither per-app nor connector-loop fences can start until rows exist.
        let rows = match tokio::time::timeout(
            self.plugin_list_wall,
            self.plugin_list_db.plugins_list_all(),
        )
        .await
        {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                tracing::warn!(
                    error = %e,
                    reason = "plugin enumeration failed",
                    "plugin autospawn skipped: plugin enumeration failed"
                );
                return;
            }
            Err(_elapsed) => {
                // No plugin id is known yet, and this arm may not await an event-store write: the same wedged repo may be what exhausted the fence.
                tracing::warn!(
                    wall_ms = self.plugin_list_wall.as_millis(),
                    reason = "plugin enumeration timed out",
                    "plugin autospawn skipped: plugin enumeration timed out"
                );
                return;
            }
        };
        // The budget must never be smaller than a single connector's own cap, or a lone connector is cut off by the LOOP bound at boot but comes up fine through `POST /enable`.
        // Bounded: `connector_bringup_budget` is capped at manifest parse time.
        let widest = rows
            .iter()
            .filter(|p| p.enabled)
            .filter_map(|p| self.registry.get(&p.id))
            .filter(|m| !m.kind.is_app())
            .map(|m| connector_bringup_budget(&m))
            .max()
            .unwrap_or_default();
        let connector_budget = widened_connector_budget(connector_budget, widest);

        // CONNECTOR-only elapsed time: a slow `app` child ahead of a connector must not consume this budget.
        let ceiling = connector_phase_ceiling(connector_budget);
        let mut connector_elapsed = Duration::ZERO;
        for plug in rows {
            if !plug.enabled {
                continue;
            }
            // `app` plugins spawn a local child and are not network-bound, so
            // they are outside this budget — it exists for the remote half.
            let is_connector = self
                .registry
                .get(&plug.id)
                .is_some_and(|m| !m.kind.is_app());
            if !is_connector {
                // The fence wraps the whole iteration (lock wait, spawn, every emission), not a named step inside it.
                match tokio::time::timeout(self.app_autospawn_wall, self.autospawn_one(&plug.id))
                    .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::warn!(plugin_id = %plug.id, error = %e, "plugin autospawn failed");
                    }
                    Err(_elapsed) => {
                        let reason = format!(
                            "plugin `{}` was cut off at boot: it did not finish starting \
                             within {} ms (a wedged lifecycle lock, a stuck child, or a \
                             slow event store). Enable it again once that is fixed.",
                            plug.id,
                            self.app_autospawn_wall.as_millis()
                        );
                        tracing::warn!(plugin_id = %plug.id, "{reason}");
                        // Terminal + observable. `try`, not `await`: the likely holder is the guard we just gave up waiting for.
                        // `mark_unavailable_under`, not `publish_`: this arm runs outside the fence, and the event-store write could hang boot.
                        match self.try_lock_lifecycle(&plug.id) {
                            Ok(g) => {
                                self.mark_unavailable_under(&g, None, reason);
                            }
                            Err(_) => tracing::warn!(
                                plugin_id = %plug.id,
                                "app autospawn was cut off and the lifecycle lock is \
                                 still held; leaving the runtime state to its holder"
                            ),
                        }
                    }
                }
                continue;
            }

            // One fence over the WHOLE per-connector iteration — bring-up, reconciliation, every persisted emission — so a slow event store cannot hold boot.
            let started = Instant::now();
            let spawn_deadline = started + connector_budget.saturating_sub(connector_elapsed);
            let phase_deadline = started + ceiling.saturating_sub(connector_elapsed);
            let fenced = tokio::time::timeout_at(
                tokio::time::Instant::from_std(phase_deadline),
                self.autospawn_one_connector(&plug.id, spawn_deadline, connector_budget),
            )
            .await;
            // Only connector iterations are charged.
            connector_elapsed += started.elapsed();
            if fenced.is_err() {
                // Out of wall clock: leave a terminal entry with NO await, because an await is what ran us out.
                let reason = format!(
                    "connector `{}` was cut off at boot: the connector phase's {} ms \
                     wall-clock ceiling was reached (a slow or unreachable connector, \
                     or a slow event store). Re-enable it once that is fixed.",
                    plug.id,
                    ceiling.as_millis()
                );
                tracing::warn!(plugin_id = %plug.id, "{reason}");
                // Take the lifecycle lock synchronously: an unlocked table write could be the last runtime write of a concurrent `stop`, resurrecting a plugin with no matching event.
                match self.try_lock_lifecycle(&plug.id) {
                    Ok(g) => {
                        self.mark_unavailable_under(&g, None, reason);
                    }
                    Err(_) => tracing::warn!(
                        plugin_id = %plug.id,
                        "boot budget elapsed and the lifecycle lock was held; \
                         leaving the runtime state to its holder"
                    ),
                }
            }
        }
    }

    /// One non-connector autospawn attempt. `Busy` waits rather than guessing; the wait is unbounded here and both callers fence it.
    async fn autospawn_one(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        match self.try_lock_lifecycle(id) {
            Ok(g) => self.spawn_under(&g, None).await,
            Err(HostError::LifecycleBusy(_)) => {
                tracing::info!(
                    plugin_id = %id,
                    "autospawn found the lifecycle lock held; waiting for it"
                );
                let g = self.await_lifecycle(id).await;
                self.spawn_under(&g, None).await
            }
            Err(other) => Err(other),
        }
    }

    /// One connector's boot iteration: bring it up within `spawn_deadline`, then reconcile. Every await here is inside the caller's phase fence.
    async fn autospawn_one_connector(
        self: &Arc<Self>,
        id: &str,
        spawn_deadline: Instant,
        connector_budget: Duration,
    ) {
        let remaining = spawn_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let reason = format!(
                "connector `{id}` was not brought up at boot: the {} ms budget for \
                 starting all connectors was already spent by earlier ones. \
                 Re-enable it once the slow/unreachable connectors are fixed.",
                connector_budget.as_millis()
            );
            tracing::warn!(plugin_id = %id, "{reason}");
            // Terminal + observable; never regress an id that is already live and `Running`.
            let _ = self.publish_unavailable(id, reason).await;
            return;
        }
        let outcome = tokio::time::timeout_at(
            tokio::time::Instant::from_std(spawn_deadline),
            self.autospawn_one(id),
        )
        .await;
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!(plugin_id = %id, error = %e, "plugin autospawn failed");
            }
            Err(_elapsed) => {
                // The dropped `spawn` future released its reservation but emitted no terminal state.
                // This arm must RECONCILE: the timeout may have elapsed after the live insert but before `emit_state(Running)`, so `publish_unavailable` refuses to regress a `Running` entry.
                let reason = format!(
                    "connector `{id}` did not finish starting within the remaining \
                     {} ms of the {} ms boot budget for all connectors",
                    remaining.as_millis(),
                    connector_budget.as_millis()
                );
                if self.publish_unavailable(id, reason.clone()).await {
                    tracing::warn!(plugin_id = %id, "{reason}");
                } else {
                    tracing::info!(
                        plugin_id = %id,
                        "connector boot budget elapsed after it had already come up; \
                         keeping it Running"
                    );
                    // The dropped future never reached its own `emit_state(Running)`. Not `emit_state` directly: `reaffirm_running` re-decides and emits as one serialized step so a concurrent `stop()` cannot be overwritten.
                    self.reaffirm_running(id).await;
                }
            }
        }
    }

    /// Spawn a plugin by id. Returns `Ok(())` once `initialize` has handshaken and the supervisor is wired.
    pub async fn spawn(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        self.spawn_admission_check(id)?;
        let guard = self.try_lock_lifecycle(id)?;
        self.spawn_under(&guard, None).await
    }

    /// `spawn_under`'s opening pair — config-disabled, then registered — shared with the pre-lock probe in `spawn` / `restart` so an unknown id cannot mint a lock cell.
    /// Reject-only for the pre-lock callers: the `Manifest` is authoritative only under the guard.
    fn spawn_admission_check(&self, id: &str) -> Result<Manifest, HostError> {
        if self.plugins_disabled.iter().any(|d| d == id) {
            return Err(HostError::Disabled(id.to_string()));
        }
        self.registry
            .get(id)
            .ok_or_else(|| HostError::NotFound(id.to_string()))
    }

    /// `spawn` for a caller that already holds the guard. `inherit` carries the crash-window counters across a supervisor respawn; `None` reads the live entry, so an explicit spawn after a crash does not zero them.
    async fn spawn_under(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        inherit: Option<CrashWindow>,
    ) -> Result<(), HostError> {
        let id = lifecycle.id();
        let manifest = self.spawn_admission_check(id)?;

        // Refuse to spawn plugins that demand a newer kernel; parse failures were already caught by `Manifest::validate`.
        let required = semver::Version::parse(&manifest.min_kernel_version).map_err(|e| {
            HostError::BadState(format!(
                "plugin `{id}` has an unparseable min_kernel_version `{}` \
                 (should have been rejected at manifest load): {e}",
                manifest.min_kernel_version
            ))
        })?;
        if let Err(err) = check_min_kernel_version(&KERNEL_VERSION, &required) {
            tracing::warn!(
                plugin_id = %id,
                required = %err.required,
                actual = %err.actual,
                "plugin '{id}' requires kernel >= {}, this kernel is {} — refusing to load",
                err.required,
                err.actual,
            );
            return Err(HostError::KernelTooOld(err));
        }

        // Atomic admission under ONE table lock: refuse already running/admitted, check template-id uniqueness against running ∧ admitted holders, then reserve.
        // Ordered before the token mint so a refusal has zero side effects.
        let admission = {
            let mut table = self.lock_table();
            if table.spawning.contains(id) {
                return Err(HostError::AlreadyRunning(id.to_string()));
            }
            if let Some(rp) = table.live.get(id)
                && matches!(
                    rp.status,
                    PluginRuntimeStatus::Running | PluginRuntimeStatus::Spawning
                )
            {
                // Crashed→spawn is the recovery path; the supervisor cleared
                // its handle, so we treat that as "go ahead".
                return Err(HostError::AlreadyRunning(id.to_string()));
            }
            match find_template_conflict(
                &manifest,
                self.registry.list(),
                &table.template_holder_ids(),
                &trusted_forge_plugin,
            ) {
                Some(conflict) => Err(conflict),
                None => {
                    table.spawning.insert(id.to_string());
                    // The reservation's lifetime is owned by the RAII guard; only the success path's atomic swap disarms.
                    Ok(AdmissionGuard::new(Arc::clone(self), id.to_string()))
                }
            }
        };
        let admitted = match admission {
            Ok(guard) => guard,
            Err(conflict) => {
                tracing::warn!(
                    plugin_id = %id,
                    error = %conflict,
                    "refusing to spawn plugin with a conflicting template id"
                );
                // Surface the refusal as a failed `PluginState` event so operators see why the plugin isn't running.
                self.emit_crashed_under(lifecycle, &conflict.to_string())
                    .await;
                return Err(conflict);
            }
        };

        self.spawn_admitted(lifecycle, &manifest, admitted, inherit)
            .await
    }

    /// Everything downstream of a successful admission reservation. Owns the `AdmissionGuard`: every failure exit drops it (releasing the reservation); the success swap disarms it.
    /// Clears the config-gate witness, runs the kind arm, and asserts the witness on success.
    async fn spawn_admitted(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        manifest: &Manifest,
        guard: AdmissionGuard,
        inherit: Option<CrashWindow>,
    ) -> Result<(), HostError> {
        self.config_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(lifecycle.id());
        let outcome = self
            .spawn_admitted_inner(lifecycle, manifest, guard, inherit)
            .await;
        if outcome.is_ok() {
            self.assert_config_gate_ran(lifecycle.id(), manifest.kind);
        }
        outcome
    }

    /// A spawn that reached `Running` without passing `config_for_spawn_or_unavailable`. Quantifies over every successful spawn of every kind, so there is no list to keep up to date.
    /// `debug_assert!` in test builds; a `tracing::error!` plus a count in `config_gate_breaches` in release, since tearing down a healthy process over bookkeeping would be worse.
    pub fn assert_config_gate_ran(&self, id: &str, kind: ConnectorKind) {
        let ran = self
            .config_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(id);
        if ran {
            return;
        }
        *self
            .config_gate_breaches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(id.to_string())
            .or_insert(0) += 1;
        let msg = format!(
            "#1284 §4.7: plugin `{id}` (kind `{}`) completed a spawn without passing \
             `config_for_spawn_or_unavailable` — its effective configuration and its \
             `required` verdict came from somewhere else, or from nowhere",
            kind.wire_name()
        );
        tracing::error!("{msg}");
        debug_assert!(false, "{msg}");
    }

    /// Whether `id`'s most recent spawn attempt passed the shared configuration gate — NOT whether it succeeded or is running now.
    pub fn config_gate_ran(&self, id: &str) -> bool {
        self.config_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(id)
    }

    /// How many times `id` completed a spawn without the witness; zero on any healthy build.
    pub fn config_gate_breaches(&self, id: &str) -> u64 {
        self.config_gate_breaches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .copied()
            .unwrap_or(0)
    }

    async fn spawn_admitted_inner(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        manifest: &Manifest,
        guard: AdmissionGuard,
        inherit: Option<CrashWindow>,
    ) -> Result<(), HostError> {
        let id = lifecycle.id();
        let install_path = self
            .registry
            .install_path(id)
            .unwrap_or_else(|| self.plugins_dir.join(id));

        // Branch by kind BEFORE `ensure_plugin_token()`: a token for a connector would be a `plugin_tokens` row nobody ever presents.
        match manifest.kind {
            ConnectorKind::App => {}
            ConnectorKind::McpHttp => {
                return self
                    .spawn_mcp_http(lifecycle, manifest, &install_path, guard, inherit)
                    .await;
            }
            ConnectorKind::CliQuery => {
                return self
                    .spawn_cli_query(lifecycle, manifest, &install_path, guard, inherit)
                    .await;
            }
        }

        // Read before the token mint and the exec so a refusal leaves nothing behind.
        let (effective, guard) = self
            .config_for_spawn_or_unavailable(lifecycle, manifest, guard)
            .await?;

        // Every spawn mints fresh; the raw value is passed via env and must be echoed back in `initialize`.
        let token = self.ensure_plugin_token(lifecycle).await?;

        self.emit_state_under(lifecycle, &PluginRuntimeStatus::Spawning)
            .await;

        // Spawn the process. On failure we propagate without touching the
        // live map (the caller releases the admission reservation).
        let process = Arc::new(
            PluginProcess::spawn(manifest, &install_path, &self.plugins_data_dir, &token)
                .map_err(HostError::from)?,
        );

        // Hand stdio over to the MCP client. The supervisor task picks the
        // `Child` up below for `wait()`.
        let (stdin, stdout) = process
            .take_stdio()
            .ok_or_else(|| HostError::Mcp(McpError::TransportClosed("stdio not piped".into())))?;
        let mcp = match McpClient::connect_with_auth(
            stdout,
            stdin,
            InitializeMeta {
                expected_echo: Some(token.as_str()),
                // `Some` even when empty: 'configuration delivered, none set' and 'kernel predates configuration delivery' are different facts.
                config: Some(&effective),
            },
        )
        .await
        {
            Ok(c) => c,
            Err(e) => {
                // Failed handshake — try to clean up the child before bailing.
                if let Some(mut child) = process.take_child() {
                    let _ = child.start_kill();
                }
                // An auth-mismatch is a security event, not a transient crash: no supervisor is installed so no respawn fires.
                if matches!(&e, McpError::Framing(m) if m == "auth mismatch") {
                    let reason = "auth handshake failed";
                    // Drop any stale live entry so `status` doesn't report a stale `Running`.
                    let _ = self.lock_table().live.remove(id);
                    self.emit_crashed_under(lifecycle, reason).await;
                    return Err(HostError::AuthMismatch(id.to_string()));
                }
                self.emit_crashed_under(lifecycle, &format!("initialize failed: {e}"))
                    .await;
                return Err(HostError::InitializeRejected(e.to_string()));
            }
        };

        // Install the real `neige.*` router iff the plugin declared the kernel-callbacks capability; the bounded mpsc must always be drained.
        let inbound = match mcp.take_inbound_requests() {
            Some(rx) => rx,
            None => {
                // Somebody else already took it; use a channel that closes immediately so the router exits cleanly.
                let (_tx, rx) = mpsc::channel::<InboundRequest>(1);
                rx
            }
        };
        let inbound_notifs = mcp.take_inbound_notifications();
        let subscriptions: Arc<Mutex<Vec<SubscriptionRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let router = if mcp.has_kernel_callbacks_capability(id) {
            spawn_neige_router(
                id.to_string(),
                Arc::clone(&self.repo),
                self.events_arc(),
                Arc::clone(&self.registry),
                Arc::clone(&mcp),
                Arc::clone(&subscriptions),
                inbound,
                inbound_notifs,
                self.write.clone(),
            )
        } else {
            tracing::info!(
                plugin_id = %id,
                "plugin did not declare experimental.dev.neige/kernel-callbacks; \
                 installing MethodNotFound drainer (neige.* calls will fail)"
            );
            spawn_methodnotfound_drainer(id.to_string(), inbound, inbound_notifs)
        };

        // Supervisor task: waits for the child, restarts on unexpected exit.
        let child_handle = process.take_child().ok_or_else(|| {
            HostError::BadState("PluginProcess lost its Child before supervision".into())
        })?;
        // The epoch is allocated BEFORE the supervisor task and the live insert, and handed to the supervisor by value.
        let run_epoch = self.next_run_epoch();
        let supervisor = {
            let host = Arc::clone(self);
            let plugin_id = id.to_string();
            tokio::spawn(async move {
                host.supervise(plugin_id, run_epoch, child_handle).await;
            })
        };

        // Swap the reservation for the live entry and disarm the guard under the SAME lock, so no interleaving sees 'neither reserved nor running' and a later drop cannot release a newer reservation.
        {
            let mut table = self.lock_table();
            let (crashes_in_window, window_started) = inherited_window(&table, id, inherit);
            table.spawning.remove(id);
            guard.disarm();
            table.live.insert(
                id.to_string(),
                RunningPlugin {
                    process: Some(process.clone()),
                    // App plugins are ALWAYS `Stdio`; `mcp_client()`'s `(Running, Stdio)` match depends on it.
                    mcp: Some(ConnectorClient::Stdio(mcp.clone())),
                    status: PluginRuntimeStatus::Running,
                    stopping: false,
                    crashes_in_window,
                    window_started,
                    run_epoch,
                    crash_attempt: 0,
                    supervisor: Some(supervisor),
                    router: Some(router),
                    subscriptions,
                },
            );
        }

        self.emit_state_under(lifecycle, &PluginRuntimeStatus::Running)
            .await;
        tracing::info!(plugin_id = %id, "plugin running");

        Ok(())
    }

    /// `kind: mcp-http` spawn arm. No token, no process, no router, no supervisor.
    /// Materialize-before-publish is load-bearing: `running_plugin_ids` gates tool discovery and the boot audit loop, both of which read `exposes_tools`.
    async fn spawn_mcp_http(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        manifest: &Manifest,
        install_path: &std::path::Path,
        guard: AdmissionGuard,
        inherit: Option<CrashWindow>,
    ) -> Result<(), HostError> {
        let id = lifecycle.id();
        let block = manifest.mcp_http.as_ref().ok_or_else(|| {
            HostError::BadState(format!(
                "plugin `{id}` is kind mcp-http but has no mcp_http block"
            ))
        })?;

        // Reset the ordering witness so a stale half from a failed previous attempt cannot pair with this one.
        self.spawn_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);

        self.emit_state_under(lifecycle, &PluginRuntimeStatus::Spawning)
            .await;

        // ONE outer wall-clock bound over the WHOLE bring-up; `request_timeout_ms` is the `tools/call` budget and is NOT consulted.
        // Config is read through the same gate every other kind uses, after `emit_state(Spawning)` to match `spawn_cli_query`.
        let (effective, guard) = self
            .config_for_spawn_or_unavailable(lifecycle, manifest, guard)
            .await?;

        // Render the url's `{{config.*}}` slots and re-validate; a failure is the connector's normal terminal state, not a kernel error.
        let url = match manifest::resolve_mcp_http_url(block, &effective) {
            Ok(url) => url,
            Err(reason) => return self.connector_unavailable(lifecycle, guard, reason).await,
        };

        let per_request = Duration::from_millis(block.bringup_timeout_ms());
        // One formula, one place: `autospawn_enabled_within` widens the loop budget by this same value.
        let budget = connector_bringup_budget(manifest);
        let outcome = tokio::time::timeout(
            budget,
            connect_mcp_http(id, block, &url, install_path.to_path_buf()),
        )
        .await;

        let (client, upstream) = match outcome {
            Err(_elapsed) => {
                return self
                    .connector_unavailable(
                        lifecycle,
                        guard,
                        format!(
                            "connector bring-up timed out after {} ms \
                             ({} × mcp_http.bringup_timeout_ms = {} ms, plus {} ms slack)",
                            budget.as_millis(),
                            MCP_HTTP_ROUND_TRIPS,
                            per_request.as_millis(),
                            CONNECTOR_BRINGUP_SLACK.as_millis(),
                        ),
                    )
                    .await;
            }
            Ok(Err(reason)) => {
                return self.connector_unavailable(lifecycle, guard, reason).await;
            }
            Ok(Ok(v)) => v,
        };

        let tools = connector::materialize_http_tools(id, block, &upstream);
        let tool_count = tools.len();

        // Materialization, then the live insert: the ORDER is the invariant, and each block stamps the tick as its LAST action.

        // Field-level mutation, and a NO-OP if the id is not in the registry; abandoning the spawn is the right answer to 'the registry does not know this id' however we got there.
        if !self.registry.set_exposes_tools(lifecycle, tools) {
            let reason = format!(
                "plugin `{id}` left the registry while its connector was starting \
                 (uninstalled or reloaded mid-spawn); abandoning spawn"
            );
            tracing::warn!(plugin_id = %id, "{reason}");
            drop(guard);
            // Without this the event log would sit at `spawning` forever. No live entry on purpose: a runtime row would resurrect an uninstalled id.
            self.emit_state_under(lifecycle, &PluginRuntimeStatus::Unavailable { reason })
                .await;
            return Err(HostError::NotFound(id.to_string()));
        }
        self.stamp_spawn_order(id, SpawnOrderStep::Materialized);

        {
            let mut table = self.lock_table();
            let (crashes_in_window, window_started) = inherited_window(&table, id, inherit);
            table.spawning.remove(id);
            guard.disarm();
            table.live.insert(
                id.to_string(),
                RunningPlugin {
                    process: None,
                    mcp: Some(ConnectorClient::Http(Arc::clone(&client))),
                    status: PluginRuntimeStatus::Running,
                    stopping: false,
                    crashes_in_window,
                    window_started,
                    // Connectors have no supervisor; still allocated so `live` has one meaning of 'run instance'.
                    run_epoch: self.next_run_epoch(),
                    crash_attempt: 0,
                    supervisor: None,
                    router: None,
                    subscriptions: Arc::new(Mutex::new(Vec::new())),
                },
            );
            drop(table);
            self.stamp_spawn_order(id, SpawnOrderStep::LiveInserted);
        }

        self.emit_state_under(lifecycle, &PluginRuntimeStatus::Running)
            .await;
        tracing::info!(
            plugin_id = %id,
            target = %client.log_target(),
            tool_count,
            "mcp-http connector running"
        );
        Ok(())
    }

    /// Bring up a `kind: cli-query` connector: same ordering invariants and failure channel as `spawn_mcp_http`, with a local resolve/probe in place of the network bring-up.
    /// No token, no supervised process, no router: each `tools/call` forks a fresh child.
    async fn spawn_cli_query(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        manifest: &Manifest,
        install_path: &std::path::Path,
        guard: AdmissionGuard,
        inherit: Option<CrashWindow>,
    ) -> Result<(), HostError> {
        let id = lifecycle.id();
        let block = manifest.cli_query.as_ref().ok_or_else(|| {
            HostError::BadState(format!(
                "plugin `{id}` is kind cli-query but has no cli_query block"
            ))
        })?;

        // Reset the ordering witness so a stale half cannot pair with this attempt.
        self.spawn_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);

        self.emit_state_under(lifecycle, &PluginRuntimeStatus::Spawning)
            .await;

        // Read once, here: the child environment and argv config slots are built at bring-up and cached.
        let (effective, guard) = self
            .config_for_spawn_or_unavailable(lifecycle, manifest, guard)
            .await?;

        // ONE outer wall-clock bound: `AppState::new` awaits this inline. `cli_query.timeout_ms` is the `tools/call` budget and is NOT consulted.
        let budget = connector_bringup_budget(manifest);
        let outcome = tokio::time::timeout(
            budget,
            cli_query::bring_up(id, block, install_path, &effective),
        )
        .await;

        let runtime = match outcome {
            Err(_elapsed) => {
                return self
                    .connector_unavailable(
                        lifecycle,
                        guard,
                        format!(
                            "cli-query connector bring-up timed out after {} ms \
                             (command resolution plus the `--version` fingerprint probe)",
                            budget.as_millis(),
                        ),
                    )
                    .await;
            }
            Ok(Err(reason)) => {
                return self.connector_unavailable(lifecycle, guard, reason).await;
            }
            Ok(Ok(rt)) => Arc::new(rt),
        };

        let tools = connector::materialize_cli_tools(id, block);
        let tool_count = tools.len();

        // Materialization, then the live insert: the ORDER is the invariant.
        if !self.registry.set_exposes_tools(lifecycle, tools) {
            let reason = format!(
                "plugin `{id}` left the registry while its connector was starting \
                 (uninstalled or reloaded mid-spawn); abandoning spawn"
            );
            tracing::warn!(plugin_id = %id, "{reason}");
            drop(guard);
            self.emit_state_under(lifecycle, &PluginRuntimeStatus::Unavailable { reason })
                .await;
            return Err(HostError::NotFound(id.to_string()));
        }
        self.stamp_spawn_order(id, SpawnOrderStep::Materialized);

        {
            let mut table = self.lock_table();
            let (crashes_in_window, window_started) = inherited_window(&table, id, inherit);
            table.spawning.remove(id);
            guard.disarm();
            table.live.insert(
                id.to_string(),
                RunningPlugin {
                    // No supervised child: the process exists only for the
                    // duration of one `tools/call`.
                    process: None,
                    mcp: Some(ConnectorClient::Cli(Arc::clone(&runtime))),
                    status: PluginRuntimeStatus::Running,
                    stopping: false,
                    crashes_in_window,
                    window_started,
                    run_epoch: self.next_run_epoch(),
                    crash_attempt: 0,
                    supervisor: None,
                    router: None,
                    subscriptions: Arc::new(Mutex::new(Vec::new())),
                },
            );
            drop(table);
            self.stamp_spawn_order(id, SpawnOrderStep::LiveInserted);
        }

        self.emit_state_under(lifecycle, &PluginRuntimeStatus::Running)
            .await;
        tracing::info!(
            plugin_id = %id,
            program = %runtime.program().display(),
            fingerprint = %runtime.fingerprint(),
            tool_count,
            "cli-query connector running"
        );
        Ok(())
    }

    /// Record one half of the ordering pair; kept in the production path because a `cfg(test)`-only probe cannot witness production ordering.
    fn stamp_spawn_order(&self, id: &str, step: SpawnOrderStep) {
        let tick = SPAWN_ORDER_TICK.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut map = self
            .spawn_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = map.entry(id.to_string()).or_default();
        match step {
            SpawnOrderStep::Materialized => entry.materialized_at = Some(tick),
            SpawnOrderStep::LiveInserted => entry.live_inserted_at = Some(tick),
        }
    }

    /// The recorded ordering for `id`'s most recent connector spawn; `materialized_at < live_inserted_at` is the invariant.
    pub fn connector_spawn_order(&self, id: &str) -> Option<ConnectorSpawnOrder> {
        self.spawn_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .copied()
    }

    /// Shared connector failure exit: swap the reservation for a live `Unavailable` entry, emit it, return a typed error.
    /// The live entry is what makes the failure observable; it does not block a later re-enable.
    async fn connector_unavailable(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        guard: AdmissionGuard,
        reason: String,
    ) -> Result<(), HostError> {
        // Discarded on purpose: this path runs strictly before the live insert, so the 'already Running' arm is unreachable.
        let _ = self
            .publish_unavailable_under(lifecycle, Some(guard), reason.clone())
            .await;
        Err(HostError::ConnectorUnavailable {
            plugin_id: lifecycle.id().to_string(),
            reason,
        })
    }

    /// The state half of `connector_unavailable`, usable without an admission guard (the boot-budget arm, whose `spawn` future was dropped).
    /// Will not regress a live `Running` entry and returns `false` when it declines: the boot timeout can elapse after the live insert.
    /// Waits for the lifecycle lock: a `Busy` folded into the failure arm would report a healthy connector as `Unavailable`.
    async fn publish_unavailable(self: &Arc<Self>, id: &str, reason: String) -> bool {
        let guard = self.await_lifecycle(id).await;
        self.publish_unavailable_under(&guard, None, reason).await
    }

    async fn publish_unavailable_under(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        guard: Option<AdmissionGuard>,
        reason: String,
    ) -> bool {
        if !self.mark_unavailable_under(lifecycle, guard, reason.clone()) {
            return false;
        }
        self.emit_state_under(lifecycle, &PluginRuntimeStatus::Unavailable { reason })
            .await;
        true
    }

    /// The **synchronous** half of `publish_unavailable`: table write only, no await, so the boot fence can give up the event half but never this one.
    /// Returns `false` without touching anything when the id is live and `Running`.
    fn mark_unavailable_under(
        &self,
        lifecycle: &LifecycleGuard,
        guard: Option<AdmissionGuard>,
        reason: String,
    ) -> bool {
        let id = lifecycle.id();
        {
            // One lock: release the reservation and publish the terminal entry together.
            let mut table = self.lock_table();
            let (crashes_in_window, window_started) = match table.live.get(id) {
                Some(prev) => (prev.crashes_in_window, prev.window_started),
                None => (0, Instant::now()),
            };
            table.spawning.remove(id);
            if let Some(guard) = guard {
                guard.disarm();
            }
            if matches!(
                table.live.get(id).map(|rp| &rp.status),
                Some(PluginRuntimeStatus::Running)
            ) {
                return false;
            }
            table.live.insert(
                id.to_string(),
                RunningPlugin {
                    process: None,
                    mcp: None,
                    status: PluginRuntimeStatus::Unavailable {
                        reason: reason.clone(),
                    },
                    stopping: false,
                    crashes_in_window,
                    window_started,
                    // Must be distinct from the run it replaces so a stale supervisor cannot match it.
                    run_epoch: self.next_run_epoch(),
                    crash_attempt: 0,
                    supervisor: None,
                    router: None,
                    subscriptions: Arc::new(Mutex::new(Vec::new())),
                },
            );
        }
        tracing::warn!(plugin_id = %id, reason = %reason, "plugin unavailable");
        true
    }

    /// Re-emit `Running` for a connector whose own `emit_state(Running)` never ran, but only if it is still running when the emission happens; returns whether it emitted.
    /// The lifecycle guard is held across the check and the emission, and `stopping` is checked as well as `Running`, so a concurrent `stop` cannot be overwritten by a stale `Running`.
    pub async fn reaffirm_running(self: &Arc<Self>, id: &str) -> bool {
        // Boot reconciliation waits: a `false` on a busy lock would be indistinguishable from 'not running' and leave the log stuck at `spawning`.
        let serialized = self.await_lifecycle(id).await;
        self.reaffirm_running_under(&serialized).await
    }

    async fn reaffirm_running_under(self: &Arc<Self>, guard: &LifecycleGuard) -> bool {
        {
            let table = self.lock_table();
            match table.live.get(guard.id()) {
                Some(rp) if matches!(rp.status, PluginRuntimeStatus::Running) && !rp.stopping => {}
                _ => return false,
            }
        }
        self.emit_state_under(guard, &PluginRuntimeStatus::Running)
            .await;
        true
    }

    /// Gracefully stop a plugin: sets `stopping=true` so the supervisor won't respawn, SIGTERMs, awaits exit.
    /// Stopping a never-successfully-enabled connector is `Ok` and clears its `Unavailable` entry; the reason stays in the event log.
    pub async fn stop(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        // Reject-only pre-lock probe so an unknown id cannot mint a `lifecycle` map entry.
        // `spawning` must be part of it: a mid-spawn id must answer `Busy`, not 404.
        {
            let table = self.lock_table();
            if !table.live.contains_key(id) && !table.spawning.contains(id) {
                return Err(HostError::NotFound(id.to_string()));
            }
        }
        let guard = self.try_lock_lifecycle(id)?;
        self.stop_under(&guard).await
    }

    /// `stop` for a caller that already holds the guard. An in-flight spawn holds the same guard, so by now it has either landed or unwound.
    async fn stop_under(self: &Arc<Self>, guard: &LifecycleGuard) -> Result<(), HostError> {
        let id = guard.id();
        let (process, supervisor, subs) = {
            let mut table = self.lock_table();
            let rp = table
                .live
                .get_mut(id)
                .ok_or_else(|| HostError::NotFound(id.to_string()))?;
            // Unreachable while the guard is held; kept as a debug assertion so an unlocked stop path trips in tests.
            debug_assert!(
                !rp.stopping,
                "{id} was already stopping while its lifecycle guard was held"
            );
            if rp.stopping {
                return Err(HostError::NotFound(id.to_string()));
            }
            rp.stopping = true;
            let process = rp.process.clone();
            let supervisor = rp.supervisor.take();
            let subs = Arc::clone(&rp.subscriptions);
            // Abort the router so it doesn't race the channel-close on mcp drop; abort() is idempotent. Connectors have no router.
            if let Some(router) = rp.router.as_ref() {
                router.abort();
            }
            (process, supervisor, subs)
        };

        // Abort every active `neige.event.subscribe` bridge task. Holding
        // these past process exit would leak event-bus subscribers.
        {
            let mut s = subs.lock().await;
            for rec in s.drain(..) {
                rec.task.abort();
            }
        }

        // Abort the supervisor *before* we kill the process so it doesn't
        // race us into a respawn attempt.
        if let Some(h) = supervisor {
            h.abort();
        }
        // A connector has no child to signal: dropping the last `ConnectorClient` clone closes the HTTP client / releases the CLI runtime.
        if let Some(process) = process {
            match process.stop(STOP_GRACE).await {
                Ok(_status) => {}
                Err(ProcessError::AlreadyDead) => {
                    // Supervisor was already going to react to this. Fine.
                }
                Err(e) => {
                    return Err(HostError::Spawn(e));
                }
            }
        }

        // Removing the entry and announcing `Disabled` is ONE serialized step, or `reaffirm_running` can slip a stale `Running` between them.
        // Locked after the slow child teardown so it never spans a SIGTERM grace.
        {
            let mut table = self.lock_table();
            table.live.remove(id);
        }
        self.emit_state_under(guard, &PluginRuntimeStatus::Disabled)
            .await;
        Ok(())
    }

    /// Stop then spawn, as ONE critical section: a concurrent `uninstall` in the gap would have the respawn resurrect a deleted plugin.
    pub async fn restart(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        // Reject-only; the same pair `spawn_under` opens with, moved before a map cell is minted.
        self.spawn_admission_check(id)?;
        let guard = self.try_lock_lifecycle(id)?;
        self.restart_under(&guard).await
    }

    async fn restart_under(self: &Arc<Self>, guard: &LifecycleGuard) -> Result<(), HostError> {
        // Stop is best-effort: if it returns NotFound (e.g. already crashed
        // and cleaned up), we proceed to spawn.
        match self.stop_under(guard).await {
            Ok(()) | Err(HostError::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
        self.spawn_under(guard, None).await
    }

    /// Snapshot current status for one plugin. An admission-reserved id
    /// (mid-spawn, no live entry yet) reports as `Spawning` with no pid.
    pub async fn status(&self, id: &str) -> Option<PluginHostStatus> {
        let table = self.lock_table();
        if table.spawning.contains(id) {
            return Some(PluginHostStatus {
                id: id.to_string(),
                status: PluginRuntimeStatus::Spawning,
                pid: None,
            });
        }
        table.live.get(id).map(|rp| PluginHostStatus {
            id: id.to_string(),
            status: rp.status.clone(),
            pid: rp.process.as_ref().and_then(|p| p.pid()),
        })
    }

    /// Snapshot the full table. Admission-reserved ids report as `Spawning` and shadow any stale crashed live entry.
    pub async fn list_running(&self) -> Vec<PluginHostStatus> {
        let table = self.lock_table();
        let mut out: Vec<PluginHostStatus> = table
            .spawning
            .iter()
            .map(|id| PluginHostStatus {
                id: id.clone(),
                status: PluginRuntimeStatus::Spawning,
                pid: None,
            })
            .collect();
        out.extend(
            table
                .live
                .iter()
                .filter(|(id, _)| !table.spawning.contains(*id))
                .map(|(id, rp)| PluginHostStatus {
                    id: id.clone(),
                    status: rp.status.clone(),
                    pid: rp.process.as_ref().and_then(|p| p.pid()),
                }),
        );
        out
    }

    /// Snapshot ids that are currently running. Admission-reserved
    /// (`Spawning`) ids are deliberately NOT included: tool visibility and
    /// dispatch must not expose a plugin before its handshake completed.
    pub async fn running_plugin_ids(&self) -> BTreeSet<String> {
        let table = self.lock_table();
        table
            .live
            .iter()
            .filter(|(_, rp)| matches!(rp.status, PluginRuntimeStatus::Running))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Most-recent stderr lines, oldest → newest. A connector has no stderr ring and reports an empty tail rather than `None`.
    pub async fn stderr_tail(&self, id: &str, n: usize) -> Option<Vec<String>> {
        let table = self.lock_table();
        table.live.get(id).map(|rp| {
            rp.process
                .as_ref()
                .map(|p| p.stderr_tail(n))
                .unwrap_or_default()
        })
    }

    /// Borrow the live **stdio** MCP client; `None` for connectors. Callers that structurally need an `app` plugin use this; ordinary dispatch uses `connector_client`.
    pub async fn mcp_client(&self, id: &str) -> Option<Arc<McpClient>> {
        let table = self.lock_table();
        table
            .live
            .get(id)
            .filter(|rp| matches!(rp.status, PluginRuntimeStatus::Running))
            .and_then(|rp| rp.mcp.as_ref()?.as_stdio().cloned())
    }

    /// Borrow the live client of a running plugin whatever its kind.
    /// The clone happens under the sync table mutex, which is why every `ConnectorClient` variant is `Arc`-wrapped.
    pub async fn connector_client(&self, id: &str) -> Option<ConnectorClient> {
        let table = self.lock_table();
        table
            .live
            .get(id)
            .filter(|rp| matches!(rp.status, PluginRuntimeStatus::Running))
            .and_then(|rp| rp.mcp.clone())
    }

    /// Dispatch a `neige.*` callback against the in-kernel handler, using the same `CallbackCtx` the plugin's inbound router builds; the plugin process is never asked.
    /// `call_id` lands in `events.correlation` as `user_tool_call:<call_id>`. Returns `RpcError::Custom(-32002, ...)` if the plugin isn't running.
    pub async fn dispatch_neige_callback(
        &self,
        plugin_id: &str,
        method: &str,
        params: serde_json::Value,
        call_id: Option<&str>,
    ) -> Result<serde_json::Value, RpcError> {
        let (mcp, subscriptions) = {
            let table = self.lock_table();
            let rp = table
                .live
                .get(plugin_id)
                .ok_or_else(|| RpcError::custom(-32002, "plugin not running"))?;
            if !matches!(rp.status, PluginRuntimeStatus::Running) {
                return Err(RpcError::custom(-32002, "plugin not running"));
            }
            // The `neige.*` channel does not exist for connectors, so a non-`Stdio` client is refused here.
            let Some(stdio) = rp.mcp.as_ref().and_then(|c| c.as_stdio()) else {
                let detail = match rp.mcp.as_ref() {
                    Some(client) => format!("is a `{}` connector", client.variant_name()),
                    None => {
                        "has no MCP client (it is running without a live transport)".to_string()
                    }
                };
                return Err(RpcError::custom(
                    -32002,
                    format!(
                        "plugin `{plugin_id}` {detail}; \
                         neige.* callbacks are only available to app plugins"
                    ),
                ));
            };
            (Arc::clone(stdio), Arc::clone(&rp.subscriptions))
        };

        let ctx = CallbackCtx {
            plugin_id,
            repo: Arc::clone(&self.repo),
            event_bus: self.events_arc(),
            registry: Arc::clone(&self.registry),
            mcp,
            subscriptions,
            call_id,
            write: self.write.clone(),
        };
        callbacks::dispatch(&ctx, method, params).await
    }

    /// Persist a `plugin.state` event and broadcast it; the bus broadcast fires only after commit succeeds.
    /// The ONLY emitter, and it demands the guard, so an emission outside its decision's critical section is not expressible in this module.
    async fn emit_state_under(&self, guard: &LifecycleGuard, status: &PluginRuntimeStatus) {
        let id = guard.id();
        if let Some(bus) = &self.events {
            let event = Event::PluginState {
                id: id.to_string(),
                state: status.wire_name().to_string(),
                last_error: status.last_error().map(String::from),
            };
            // `EventScope::System`: a server-lifecycle signal with no entity scope.
            if let Err(e) = self
                .repo
                .log_pure_event(
                    ActorId::Plugin(id.to_string()),
                    EventScope::System,
                    None,
                    bus,
                    self.write.role_cache(),
                    self.write.area_cache(),
                    event,
                )
                .await
            {
                tracing::warn!(plugin_id = %id, error = %e, "plugin_state event log failed");
            }
        }
    }

    async fn emit_crashed_under(&self, guard: &LifecycleGuard, reason: &str) {
        let status = PluginRuntimeStatus::Crashed {
            reason: reason.to_string(),
        };
        self.emit_state_under(guard, &status).await;
    }

    /// Supervisor loop for one plugin. Boxed because `supervise` ↔ `spawn` form a mutual recursion through `tokio::spawn`, which auto-Send inference can't see through.
    fn supervise(
        self: Arc<Self>,
        id: String,
        run_epoch: u64,
        child: tokio::process::Child,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(self.supervise_inner(id, run_epoch, child))
    }

    /// The supervisor in three segments: [lock held] account the crash and emit `crashed`; [no lock] sleep out the backoff; [lock re-taken] re-decide everything and respawn.
    async fn supervise_inner(
        self: Arc<Self>,
        id: String,
        run_epoch: u64,
        mut child: tokio::process::Child,
    ) {
        let exit_result = child.wait().await;

        // `await_lifecycle`, not `try`: the task was created under the spawn's own guard, so a child that dies in that window collides with it by construction.
        let (attempt, delay_ms) = {
            let guard = self.await_lifecycle(&id).await;

            // Is this still MY run instance? An old supervisor that only now got the lock must not mark a NEW entry crashed.
            let observed = {
                let table = self.lock_table();
                table.live.get(&id).map(|rp| {
                    (
                        rp.run_epoch,
                        rp.stopping,
                        matches!(rp.status, PluginRuntimeStatus::Running),
                    )
                })
            };
            match observed {
                // Removed by `stop`/`uninstall`, or replaced by a newer run:
                // not ours to report.
                None => {
                    tracing::info!(plugin_id = %id, "plugin exited; its live entry is gone");
                    return;
                }
                Some((epoch, _, _)) if epoch != run_epoch => {
                    tracing::info!(
                        plugin_id = %id,
                        "plugin exited; a newer run instance owns the entry"
                    );
                    return;
                }
                Some((_, true, _)) => {
                    tracing::info!(plugin_id = %id, "plugin exited gracefully");
                    return;
                }
                Some((_, false, false)) => {
                    // Somebody already wrote a terminal state for this exact
                    // run (`Crashed` from the auth-mismatch path, `Unavailable`
                    // from a boot fence). Not a second crash.
                    tracing::info!(
                        plugin_id = %id,
                        "plugin exited but its run instance is no longer Running"
                    );
                    return;
                }
                Some((_, false, true)) => {}
            }

            let reason = match exit_result {
                Ok(status) => format!("exited with {status}"),
                Err(e) => format!("wait failed: {e}"),
            };
            tracing::warn!(plugin_id = %id, reason = %reason, "plugin exited unexpectedly");

            // Snapshot stderr tail so the crash event carries useful detail.
            let tail = {
                let table = self.lock_table();
                table
                    .live
                    .get(&id)
                    .and_then(|rp| rp.process.as_ref())
                    .map(|p| p.stderr_tail(10).join("\n"))
                    .unwrap_or_default()
            };
            let combined_reason = if tail.is_empty() {
                reason
            } else {
                format!("{reason}\nstderr tail:\n{tail}")
            };

            // Crash-window bookkeeping.
            let (attempts, attempt, exceeded) = {
                let mut table = self.lock_table();
                let Some(entry) = table.live.get_mut(&id) else {
                    return;
                };
                if entry.window_started.elapsed() > self.backoff.crash_window {
                    entry.window_started = Instant::now();
                    entry.crashes_in_window = 0;
                }
                entry.crashes_in_window += 1;
                entry.crash_attempt += 1;
                entry.status = PluginRuntimeStatus::Crashed {
                    reason: combined_reason.clone(),
                };
                (
                    entry.crashes_in_window,
                    entry.crash_attempt,
                    entry.crashes_in_window >= self.backoff.crash_window_limit,
                )
            };

            self.emit_crashed_under(&guard, &combined_reason).await;

            if exceeded {
                tracing::error!(
                    plugin_id = %id,
                    attempts,
                    "plugin exceeded crash-window limit; not respawning",
                );
                // Leave the Crashed entry so `status()` returns it; drop the supervisor handle so it gets reaped.
                // Epoch-checked so a raced `stop`+`spawn`'s NEW entry keeps its handle.
                let mut table = self.lock_table();
                if let Some(rp) = table.live.get_mut(&id)
                    && rp.run_epoch == run_epoch
                {
                    rp.supervisor = None;
                }
                return;
            }

            // Backoff then respawn. Index by (attempts - 1) clamped to the table.
            let idx = (attempts as usize).saturating_sub(1);
            let delay_ms = self
                .backoff
                .schedule_ms
                .get(idx)
                .copied()
                .unwrap_or_else(|| *self.backoff.schedule_ms.last().expect("non-empty schedule"));
            tracing::info!(
                plugin_id = %id,
                delay_ms,
                attempts,
                "scheduling plugin respawn",
            );
            (attempt, delay_ms)
        };

        // Lock NOT held: a `disable` during a crash loop must not block for the whole backoff.
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;

        self.respawn_after_backoff(&id, run_epoch, attempt).await;
    }

    /// Segment 3 of the supervisor: re-derive everything after the sleep. The manifest is NOT carried over: a `reload` during the backoff may have replaced it.
    async fn respawn_after_backoff(self: &Arc<Self>, id: &str, run_epoch: u64, attempt: u64) {
        for retry in 0..LIFECYCLE_DB_READ_RETRIES {
            let guard = self.await_lifecycle(id).await;

            // (a) still our run instance, still the Crashed state WE wrote,
            //     not being stopped, and no further crash has happened.
            let ok = {
                let table = self.lock_table();
                match table.live.get(id) {
                    Some(rp) => {
                        rp.run_epoch == run_epoch
                            && rp.crash_attempt == attempt
                            && !rp.stopping
                            && matches!(rp.status, PluginRuntimeStatus::Crashed { .. })
                    }
                    None => false,
                }
            };
            if !ok {
                tracing::info!(
                    plugin_id = %id,
                    "backoff elapsed but the run instance is gone or has moved on; not respawning"
                );
                return;
            }

            // (b) still installed.
            if self.registry.get(id).is_none() {
                tracing::info!(
                    plugin_id = %id,
                    "backoff elapsed but the plugin left the registry; not respawning"
                );
                return;
            }

            // Still enabled, per the DB. Fail closed: the route layer can write the plugin row without the host, so a read failure must not be treated as 'probably still enabled'.
            match self.lifecycle_db.enabled_row(id).await {
                Ok(Some(true)) => {}
                Ok(Some(false)) | Ok(None) => {
                    tracing::info!(
                        plugin_id = %id,
                        "backoff elapsed but the plugin is no longer enabled; not respawning"
                    );
                    return;
                }
                Err(e) => {
                    drop(guard);
                    tracing::warn!(
                        plugin_id = %id,
                        error = %e,
                        retry,
                        "could not read the plugin row after backoff; keeping Crashed and retrying"
                    );
                    tokio::time::sleep(LIFECYCLE_DB_READ_RETRY_DELAY).await;
                    continue;
                }
            }

            // Carry the crash window ACROSS the remove; otherwise every respawn starts from zero.
            let carried = {
                let mut table = self.lock_table();
                let carried = table.live.get(id).map(|rp| CrashWindow {
                    crashes: rp.crashes_in_window,
                    started: rp.window_started,
                });
                table.live.remove(id);
                carried
            };
            if let Err(e) = self.spawn_under(&guard, carried).await {
                tracing::error!(plugin_id = %id, error = %e, "respawn failed");
                self.emit_crashed_under(&guard, &format!("respawn failed: {e}"))
                    .await;
            }
            return;
        }
        // The exhausted terminal must be explicit and observable, not only a log line.
        // Epoch-checked: if the world moved while we slept, whoever moved it owns the terminal.
        let guard = self.await_lifecycle(id).await;
        let still_ours = {
            let table = self.lock_table();
            match table.live.get(id) {
                Some(rp) => {
                    rp.run_epoch == run_epoch
                        && rp.crash_attempt == attempt
                        && !rp.stopping
                        && matches!(rp.status, PluginRuntimeStatus::Crashed { .. })
                }
                None => false,
            }
        };
        if still_ours {
            let reason = format!(
                "plugin `{id}` crashed and the kernel gave up respawning it: its \
                 database row could not be read {LIFECYCLE_DB_READ_RETRIES} times in a \
                 row, so whether it is still enabled is unknown and a respawn would \
                 be a guess. Nothing will retry automatically — enable (or spawn) it \
                 explicitly once the database is readable again."
            );
            self.publish_unavailable_under(&guard, None, reason).await;
        }
        tracing::error!(
            plugin_id = %id,
            republished = still_ours,
            "gave up respawning after {LIFECYCLE_DB_READ_RETRIES} failed plugin-row reads"
        );
    }
}

/// The crash-window counters a new live entry starts from: `inherit` wins, else the entry being replaced (an explicit spawn after a crash must not zero them).
fn inherited_window(
    table: &ProcessTable,
    id: &str,
    inherit: Option<CrashWindow>,
) -> (u32, Instant) {
    match inherit {
        Some(w) => (w.crashes, w.started),
        None => match table.live.get(id) {
            Some(prev) => (prev.crashes_in_window, prev.window_started),
            None => (0, Instant::now()),
        },
    }
}

/// Pure core of the template-id uniqueness check: only trusted plugins participate, only holders (running + admission-reserved) count, and the plugin's own registry entry is skipped.
/// The trust predicate is injected to keep this testable without process env.
fn find_template_conflict(
    manifest: &Manifest,
    candidates: impl IntoIterator<Item = Manifest>,
    holder_ids: &BTreeSet<String>,
    is_trusted: &dyn Fn(&str) -> bool,
) -> Option<HostError> {
    if !is_trusted(&manifest.id) {
        return None;
    }
    for other in candidates {
        if other.id == manifest.id || !holder_ids.contains(&other.id) || !is_trusted(&other.id) {
            continue;
        }
        for template in &manifest.templates {
            if other.templates.iter().any(|held| held.id == template.id) {
                return Some(HostError::TemplateConflict {
                    plugin_id: manifest.id.clone(),
                    template_id: template.id.clone(),
                    held_by: other.id.clone(),
                });
            }
        }
    }
    None
}

/// Router task: drains inbound MCP requests into `callbacks::dispatch`; notifications are logged and dropped. Ends when both channels close.
#[allow(clippy::too_many_arguments)]
fn spawn_neige_router(
    plugin_id: String,
    repo: Arc<dyn RouteRepo>,
    event_bus: Arc<EventBus>,
    registry: Arc<PluginRegistry>,
    mcp: Arc<McpClient>,
    subscriptions: Arc<Mutex<Vec<SubscriptionRecord>>>,
    mut inbound: mpsc::Receiver<InboundRequest>,
    inbound_notifs: Option<mpsc::Receiver<InboundNotification>>,
    write: WriteContext,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Notifications are lossy by specification; logged for debugging only.
        if let Some(mut notif_rx) = inbound_notifs {
            let plugin_id_n = plugin_id.clone();
            tokio::spawn(async move {
                while let Some(notif) = notif_rx.recv().await {
                    tracing::debug!(
                        plugin_id = %plugin_id_n,
                        method = %notif.method,
                        "inbound plugin notification (currently logged + ignored)"
                    );
                }
            });
        }

        while let Some(req) = inbound.recv().await {
            let ctx = CallbackCtx {
                plugin_id: &plugin_id,
                repo: Arc::clone(&repo),
                event_bus: Arc::clone(&event_bus),
                registry: Arc::clone(&registry),
                mcp: Arc::clone(&mcp),
                subscriptions: Arc::clone(&subscriptions),
                // Plugin-initiated requests have no caller-side tracing id; event rows get `correlation = NULL`.
                call_id: None,
                write: write.clone(),
            };
            let outcome = callbacks::dispatch(&ctx, &req.method, req.params).await;
            // If the responder is gone (plugin disconnected mid-call), drop
            // silently — the mcp reader already cleans up the wire.
            let _ = req.responder.send(outcome);
        }
        tracing::debug!(plugin_id = %plugin_id, "inbound request channel closed");
    })
}

/// Drainer installed when a plugin omits the `experimental.dev.neige/kernel-callbacks` capability: every inbound request gets `MethodNotFound` instead of a hang.
fn spawn_methodnotfound_drainer(
    plugin_id: String,
    mut inbound: mpsc::Receiver<InboundRequest>,
    inbound_notifs: Option<mpsc::Receiver<InboundNotification>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Some(mut notif_rx) = inbound_notifs {
            let plugin_id_n = plugin_id.clone();
            tokio::spawn(async move {
                while let Some(notif) = notif_rx.recv().await {
                    tracing::debug!(
                        plugin_id = %plugin_id_n,
                        method = %notif.method,
                        "inbound plugin notification (no-callbacks plugin; logged + ignored)"
                    );
                }
            });
        }
        while let Some(req) = inbound.recv().await {
            let outcome = Err(RpcError::method_not_found(&req.method));
            let _ = req.responder.send(outcome);
        }
        tracing::debug!(plugin_id = %plugin_id, "inbound request channel closed (no-callbacks)");
    })
}

#[cfg(test)]
mod template_conflict_tests {
    use super::*;

    fn manifest_with_template(id: &str, template_id: &str) -> Manifest {
        let json = serde_json::json!({
            "manifest_version": 2,
            "id": id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Template Conflict Stub",
            "entrypoint": { "command": "bin/stub" },
            "templates": [
                { "id": template_id }
            ],
            "permissions": {}
        });
        Manifest::parse(&json.to_string()).expect("manifest parses")
    }

    fn running(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn duplicate_template_on_running_trusted_plugin_conflicts() {
        let incoming = manifest_with_template("dev.second", "issue-development");
        let holder = manifest_with_template("dev.first", "issue-development");
        let trusted = |_: &str| true;
        let conflict =
            find_template_conflict(&incoming, [holder], &running(&["dev.first"]), &trusted)
                .expect("duplicate template id must conflict");
        match conflict {
            HostError::TemplateConflict {
                plugin_id,
                template_id,
                held_by,
            } => {
                assert_eq!(plugin_id, "dev.second");
                assert_eq!(template_id, "issue-development");
                assert_eq!(held_by, "dev.first");
            }
            other => panic!("expected TemplateConflict, got {other:?}"),
        }
    }

    #[test]
    fn stopped_holder_does_not_squat_on_template_id() {
        let incoming = manifest_with_template("dev.second", "issue-development");
        let holder = manifest_with_template("dev.first", "issue-development");
        let trusted = |_: &str| true;
        assert!(
            find_template_conflict(&incoming, [holder], &running(&[]), &trusted).is_none(),
            "a stopped plugin must not hold the template id"
        );
    }

    #[test]
    fn untrusted_duplicates_are_tolerated() {
        let incoming = manifest_with_template("dev.second", "issue-development");
        let holder = manifest_with_template("dev.first", "issue-development");
        let running_ids = running(&["dev.first"]);

        // Untrusted spawner: never enters the resolution set — no conflict.
        let only_first_trusted = |id: &str| id == "dev.first";
        assert!(
            find_template_conflict(
                &incoming,
                [holder.clone()],
                &running_ids,
                &only_first_trusted
            )
            .is_none()
        );

        // Untrusted holder: its templates are unresolvable — no conflict.
        let only_second_trusted = |id: &str| id == "dev.second";
        assert!(
            find_template_conflict(&incoming, [holder], &running_ids, &only_second_trusted)
                .is_none()
        );
    }

    #[test]
    fn respawn_skips_own_registry_entry_and_distinct_ids_pass() {
        let incoming = manifest_with_template("dev.first", "issue-development");
        let own_entry = manifest_with_template("dev.first", "issue-development");
        let trusted = |_: &str| true;
        assert!(
            find_template_conflict(&incoming, [own_entry], &running(&["dev.first"]), &trusted)
                .is_none(),
            "respawn must not conflict with the plugin's own registry entry"
        );

        let other = manifest_with_template("dev.other", "different-template");
        assert!(
            find_template_conflict(&incoming, [other], &running(&["dev.other"]), &trusted)
                .is_none(),
            "distinct template ids must not conflict"
        );
    }
}
