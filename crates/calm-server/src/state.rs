//! Shared app state passed to every handler. `Clone` is cheap — everything inside is `Arc`.

use crate::card_kind::CardKindRegistry;
use crate::card_role_cache::CardRoleCache;
use crate::config::Config;
use crate::db::{Repo, RouteRepo};
use crate::dispatcher::Dispatcher;
use crate::event::{Event, EventBus, EventScope};

use crate::harness::HarnessRegistry;
use crate::ids::ActorId;
use crate::isolated_codex::config::{Backend as IsolatedCodexBackend, IsolatedCodexConfig};
use crate::mcp_server::McpServer;
use crate::operation::child_track_adapter::ChildTrackAdapter;
use crate::operation::claude_adapter::{ClaudeAdapter, ClaudeWorkerAdapter};
use crate::operation::claude_restart_adapter::ClaudeRestartAdapter;
use crate::operation::codex_adapter::{CodexAdapter, CodexWorkerAdapter};
use crate::operation::forge_action_adapter::ForgeActionAdapter;
use crate::operation::planner_harness_interrupt_adapter::PlannerHarnessInterruptAdapter;
use crate::operation::planner_harness_shutdown_adapter::PlannerHarnessShutdownAdapter;
use crate::operation::planner_harness_start_adapter::PlannerHarnessStartAdapter;
use crate::operation::task_verify_adapter::TaskVerifyAdapter;
use crate::operation::terminal_adapter::{SpawnHook, TerminalAdapter, TerminalWorkerAdapter};
use crate::operation::{
    OperationCompletionBus, OperationRuntime, ProviderAdapter, SpawnCtx, SqlxOperationRepo,
};
use crate::pending_codex_threads::{PendingThreadStartRegistry, spawn_periodic_expire_task};
use crate::plugin_host::{PluginHost, PluginRegistry};
use crate::shared_codex_appserver::SharedCodexAppServer;
use crate::state_clients::resolve_mcp_stdio_shim_bin;
use crate::terminal_renderer::TerminalRendererRegistry;
use crate::track_area_cache::TrackAreaCache;
use crate::worker_flow::WorkerFlowDriver;
use axum::extract::FromRef;
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::sync::Mutex;

const HOOK_INGEST_CACHE_CAPACITY: usize = 4096;

pub use crate::state_clients::{CodexClient, DaemonClient};

/// Fixed-size FIFO cache for hook ingest idempotency keys. Process-local: after a
/// restart the first re-posted hook can emit again.
#[derive(Debug)]
pub(crate) struct HookIngestCache {
    capacity: usize,
    order: VecDeque<String>,
    keys: HashSet<String>,
}

impl HookIngestCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            order: VecDeque::with_capacity(capacity),
            keys: HashSet::with_capacity(capacity),
        }
    }

    pub(crate) fn contains(&self, key: &str) -> bool {
        self.keys.contains(key)
    }

    pub(crate) fn insert(&mut self, key: String) {
        if !self.keys.insert(key.clone()) {
            return;
        }
        while self.order.len() >= self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.keys.remove(&evicted);
            } else {
                break;
            }
        }
        self.order.push_back(key);
    }
}

pub use calm_truth::state::WriteContext;

#[derive(Clone)]
pub struct RouteState {
    pub repo: Arc<dyn RouteRepo>,
    /// Root for server-managed track workspaces (`<root>/<area_id>/<track_id>`).
    /// Resolved once at boot; never read from env at request time.
    pub workspace_root: PathBuf,
    /// Server-resolved default used by both scheduler admission and report
    /// diagnostics. Read once at boot; request handlers never consult env.
    pub task_budget_default: i64,
    pub events: EventBus,
    pub plugin: Arc<PluginHost>,
    pub db_instance_id: Arc<String>,
    pub database_id: Arc<String>,
    pub write: WriteContext,
    pub operation_runtime: Arc<OperationRuntime>,
    pub harness: HarnessRegistry,
    /// The context the kernel's MCP tools run in, shared with the HTTP layer.
    pub mcp_context: Arc<crate::mcp_server::registry::AppContext>,
    /// The template roster every route reads. `&'static` because entries are borrowed
    /// across a create transaction and the admitted key's bytes are what `tracks.template_id` stores.
    pub templates: &'static crate::templates::TemplateRoster,
    pub terminal_renderer: Arc<TerminalRendererRegistry>,
    pub(crate) hook_ingest_cache: Arc<StdMutex<HookIngestCache>>,
    /// Per-card lock for lazy planner harness recovery. Lock order:
    /// `conversation_first_message_locks` → `planner_recovery_locks`, never the reverse.
    pub(crate) planner_recovery_locks: crate::per_card_lock::PerCardLocks,
    /// Per-card claim for the Today bootstrap's first-message send. A SEPARATE map from
    /// `planner_recovery_locks`: the claim is held across a call that takes that lock and
    /// `tokio::sync::Mutex` is not reentrant. In-process only — one calm-server per data directory.
    pub(crate) conversation_first_message_locks: crate::per_card_lock::PerCardLocks,
    /// `None` in production; armed by the cross-instance primary-key race test.
    pub(crate) track_create_mint_rendezvous: crate::routes::tracks::TrackCreateMintRendezvous,
    /// Per-track fence between deletion and the direct runtime recovery/reattach paths that
    /// bypass `OperationRuntime`; the guard transfers to the deletion saga and survives request cancellation.
    pub(crate) track_delete_locks: crate::per_card_lock::KeyedLocks,
    /// One planner attachment upload per card at a time. Takes no other lock.
    pub(crate) planner_attachment_locks: crate::per_card_lock::PerCardLocks,
    /// Serializes a user-area delete with the ordinary track-create route.
    /// The creator holds it through workspace materialization and planner
    /// startup; deletion therefore snapshots a closed member set.
    pub(crate) area_delete_locks: crate::per_card_lock::KeyedLocks,
}

#[derive(Clone)]
pub struct WorkerState {
    pub repo: Arc<dyn Repo>,
    pub daemon: Arc<DaemonClient>,
    pub dispatcher: Arc<Dispatcher>,
    pub mcp_server: Option<Arc<McpServer>>,
    pub harness: HarnessRegistry,
    pub terminal_renderer: Arc<TerminalRendererRegistry>,
    pub write: WriteContext,
}

#[derive(Clone)]
pub struct CodexShellState {
    pub codex: Arc<CodexClient>,
    pub shared_codex_appserver: Arc<SharedCodexAppServer>,
    pub pending_codex_threads: Arc<PendingThreadStartRegistry>,
    pub pending_codex_threads_spawn_serial: Arc<Mutex<()>>,
    pub plugin: Arc<PluginHost>,
}

pub struct BootState {
    pub repo: Arc<dyn Repo>,
    pub workspace_root: PathBuf,
    /// Owns the auto-allocated workspace root of a test/replay state; `None` in production.
    pub workspace_root_guard: Option<Arc<tempfile::TempDir>>,
    pub task_budget_default: i64,
    pub events: EventBus,
    pub daemon: Arc<DaemonClient>,
    pub terminal_renderer: Arc<TerminalRendererRegistry>,
    pub plugin: Arc<PluginHost>,
    pub codex: Arc<CodexClient>,
    pub db_instance_id: Arc<String>,
    pub database_id: Arc<String>,
    pub templates: &'static crate::templates::TemplateRoster,
    pub card_role_cache: CardRoleCache,
    pub track_area_cache: TrackAreaCache,
    pub card_kind_registry: Arc<CardKindRegistry>,
    pub dispatcher: Arc<Dispatcher>,
    pub mcp_server: Option<Arc<McpServer>>,
    pub mcp_context: Arc<crate::mcp_server::registry::AppContext>,
    pub harness: HarnessRegistry,
    pub shared_codex_appserver: Arc<SharedCodexAppServer>,
    pub pending_codex_threads: Arc<PendingThreadStartRegistry>,
    pub pending_codex_threads_spawn_serial: Arc<Mutex<()>>,
    pub operation_runtime: Arc<OperationRuntime>,
    pub worker_flow: Arc<WorkerFlowDriver>,
    pub isolated_codex_backend: Option<Arc<IsolatedCodexBackend>>,
}

impl BootState {
    pub fn into_app_state(self) -> AppState {
        let route_repo: Arc<dyn RouteRepo> = self.repo.clone();
        let write = WriteContext::new(self.card_role_cache.clone(), self.track_area_cache.clone());
        let hook_ingest_cache = Arc::new(StdMutex::new(HookIngestCache::new(
            HOOK_INGEST_CACHE_CAPACITY,
        )));
        let route = RouteState {
            terminal_renderer: self.terminal_renderer.clone(),
            repo: route_repo.clone(),
            workspace_root: self.workspace_root.clone(),
            task_budget_default: self.task_budget_default,
            events: self.events.clone(),
            plugin: self.plugin.clone(),
            db_instance_id: self.db_instance_id.clone(),
            database_id: self.database_id.clone(),
            write: write.clone(),
            operation_runtime: self.operation_runtime.clone(),
            harness: self.harness.clone(),
            mcp_context: self.mcp_context.clone(),
            templates: self.templates,
            hook_ingest_cache,
            planner_recovery_locks: crate::per_card_lock::new_per_card_locks(),
            conversation_first_message_locks: crate::per_card_lock::new_per_card_locks(),
            track_create_mint_rendezvous: None,
            planner_attachment_locks: crate::per_card_lock::new_per_card_locks(),
            track_delete_locks: crate::per_card_lock::new_keyed_locks(),
            area_delete_locks: crate::per_card_lock::new_keyed_locks(),
        };
        let worker = WorkerState {
            repo: self.repo.clone(),
            daemon: self.daemon.clone(),
            dispatcher: self.dispatcher.clone(),
            mcp_server: self.mcp_server.clone(),
            harness: self.harness.clone(),
            terminal_renderer: self.terminal_renderer.clone(),
            write,
        };
        let codex_shell = CodexShellState {
            codex: self.codex.clone(),
            shared_codex_appserver: self.shared_codex_appserver.clone(),
            pending_codex_threads: self.pending_codex_threads.clone(),
            pending_codex_threads_spawn_serial: self.pending_codex_threads_spawn_serial.clone(),
            plugin: self.plugin.clone(),
        };

        AppState {
            repo: route_repo,
            events: self.events,
            system_area_mint: Arc::new(crate::routes::today::SystemAreaMintCounters::default()),
            system_area_mint_rendezvous: None,
            today_summary_create: Arc::new(
                crate::routes::today_summary::TodaySummaryCreateCounters::default(),
            ),
            today_summary_create_rendezvous: None,
            today_summary_bootstrap_rendezvous: None,
            daemon: self.daemon,
            terminal_renderer: self.terminal_renderer,
            plugin: self.plugin,
            codex: self.codex,
            db_instance_id: self.db_instance_id,
            database_id: self.database_id,
            ws_replay_cap: crate::ws::events::ws_replay_max_events_from_env(),
            card_role_cache: self.card_role_cache,
            track_area_cache: self.track_area_cache,
            card_kind_registry: self.card_kind_registry,
            dispatcher: self.dispatcher,
            mcp_server: self.mcp_server,
            harness: self.harness,
            shared_codex_appserver: self.shared_codex_appserver,
            pending_codex_threads: self.pending_codex_threads,
            pending_codex_threads_spawn_serial: self.pending_codex_threads_spawn_serial,
            operation_runtime: self.operation_runtime,
            worker_flow: self.worker_flow,
            isolated_codex_backend: self.isolated_codex_backend,
            raw: self.repo,
            workspace_root_guard: self.workspace_root_guard,
            route,
            worker,
            codex_shell,
        }
    }
}

/// Route-facing handle: excludes `RepoSyncDomainRaw`. Tests reach `&dyn Repo` via the
/// `fixtures`-gated [`AppState::raw_repo`].
#[derive(Clone)]
pub struct AppState {
    /// Sync-domain raw writes are unreachable from this handle — handlers must funnel
    /// them through `db::write_with_event_typed`.
    pub repo: Arc<dyn RouteRepo>,
    pub events: EventBus,
    pub system_area_mint: Arc<crate::routes::today::SystemAreaMintCounters>,
    /// `None` in production; armed by the system-area concurrency test.
    pub system_area_mint_rendezvous: crate::routes::today::SystemAreaMintRendezvous,
    pub today_summary_create: Arc<crate::routes::today_summary::TodaySummaryCreateCounters>,
    /// `None` in production; armed by the create-race test.
    pub today_summary_create_rendezvous: crate::routes::today_summary::TodaySummaryCreateRendezvous,
    /// `None` in production; armed by the first-message race test.
    pub today_summary_bootstrap_rendezvous:
        crate::routes::today_summary::TodaySummaryBootstrapRendezvous,
    pub daemon: Arc<DaemonClient>,
    pub terminal_renderer: Arc<TerminalRendererRegistry>,
    pub plugin: Arc<PluginHost>,
    pub codex: Arc<CodexClient>,
    /// UUID v4 minted once per server-process boot, surfaced on `/api/version` as
    /// `dbInstanceId` so the client can bust its caches when the DB was recreated. Never persisted.
    pub db_instance_id: Arc<String>,
    /// Stable identity of the database itself (`databaseId` on `/api/version`), minted once
    /// into `database_identity` and read back by every later open.
    pub database_id: Arc<String>,
    /// Ceiling on rows a single WS replay may stream. Resolved once at construction so tests
    /// can inject a cap without mutating process-global env.
    pub ws_replay_cap: i64,
    /// `CardId -> CardRole` cache used by `role_gate::enforce_role`; threaded into every
    /// `_tx` card helper so it stays write-through inside the surrounding transaction.
    pub card_role_cache: CardRoleCache,
    /// `TrackId -> AreaId` cache the role gate consults to cross-check `scope.area`
    /// against a Worker card's home area.
    pub track_area_cache: TrackAreaCache,
    /// Registry of kernel-owned card kind handlers; unknown card kinds stay opaque.
    pub card_kind_registry: Arc<CardKindRegistry>,
    /// Dropping the `AppState` doesn't abort the dispatcher task — closure happens when
    /// the event bus's `tx` drops too.
    pub dispatcher: Arc<Dispatcher>,
    /// Kernel-as-MCP-server handle. `None` when `from_parts` (replay / unit tests) skips the listener boot.
    pub mcp_server: Option<Arc<McpServer>>,
    pub harness: HarnessRegistry,
    pub shared_codex_appserver: Arc<SharedCodexAppServer>,
    /// FIFO attribution registry for empty cards that fresh-start a thread
    /// through the shared daemon's TUI.
    pub pending_codex_threads: Arc<PendingThreadStartRegistry>,
    /// Serializes the shared empty-card `(pending register, PTY spawn)` pair
    /// so FIFO pending attribution matches actual TUI fresh-start order.
    pub pending_codex_threads_spawn_serial: Arc<Mutex<()>>,
    pub operation_runtime: Arc<OperationRuntime>,
    pub worker_flow: Arc<WorkerFlowDriver>,
    /// Explicit boot configuration, retained across fixture registry rebuilds.
    #[cfg_attr(not(feature = "fixtures"), allow(dead_code))]
    isolated_codex_backend: Option<Arc<IsolatedCodexBackend>>,
    /// Full-capability handle, kept private so the gate at `AppState::repo` survives;
    /// reachable only through the `fixtures`-gated [`AppState::raw_repo`].
    #[allow(dead_code)]
    raw: Arc<dyn Repo>,
    /// RAII guard: never read, only dropped (the drop removes the per-`AppState` sandbox).
    #[allow(dead_code)]
    workspace_root_guard: Option<Arc<tempfile::TempDir>>,
    route: RouteState,
    worker: WorkerState,
    codex_shell: CodexShellState,
}

struct OperationAdapterInputs {
    isolated_codex_backend: Option<Arc<IsolatedCodexBackend>>,
    route_repo: Arc<dyn RouteRepo>,
    repo: Arc<dyn Repo>,
    plugin: Arc<PluginHost>,
    codex: Arc<CodexClient>,
    shared_codex_appserver: Arc<SharedCodexAppServer>,
    pending_codex_threads: Arc<PendingThreadStartRegistry>,
    pending_codex_threads_spawn_serial: Arc<Mutex<()>>,
    card_role_cache: CardRoleCache,
    track_area_cache: TrackAreaCache,
    terminal_spawn_hook: Option<SpawnHook>,
    harness: HarnessRegistry,
    mcp_server: Option<Arc<McpServer>>,
    gate_logs_dir: PathBuf,
    workspace_root: PathBuf,
}

fn terminal_hook_settings(codex: &CodexClient) -> crate::terminal_hooks::TerminalHookSettings {
    crate::terminal_hooks::TerminalHookSettings {
        bridge_bin: codex.bridge_bin.clone(),
        base_url: codex.ingest_url.clone(),
        settings_dir: codex.terminal_hook_settings_dir.clone(),
    }
}

fn build_operation_adapters(input: OperationAdapterInputs) -> Vec<Arc<dyn ProviderAdapter>> {
    let isolated_codex_adapter: Arc<dyn ProviderAdapter> =
        Arc::new(crate::isolated_codex::adapter::IsolatedCodexAdapter::new(
            input.isolated_codex_backend,
            input.route_repo.clone(),
            input
                .mcp_server
                .as_ref()
                .map(|server| server.shim_config.socket_path.clone()),
            WriteContext::new(
                input.card_role_cache.clone(),
                input.track_area_cache.clone(),
            ),
        ));
    let hook_settings = Some(terminal_hook_settings(&input.codex));
    let terminal_adapter: Arc<dyn ProviderAdapter> =
        if let Some(spawn_hook) = input.terminal_spawn_hook.clone() {
            Arc::new(
                TerminalAdapter::new_with_spawn_hook(
                    input.route_repo.clone(),
                    input.card_role_cache.clone(),
                    input.track_area_cache.clone(),
                    spawn_hook,
                )
                .with_hook_settings(hook_settings),
            )
        } else {
            Arc::new(
                TerminalAdapter::new(
                    input.route_repo.clone(),
                    input.card_role_cache.clone(),
                    input.track_area_cache.clone(),
                )
                .with_hook_settings(hook_settings),
            )
        };
    let terminal_worker_adapter: Arc<dyn ProviderAdapter> =
        if let Some(spawn_hook) = input.terminal_spawn_hook {
            Arc::new(TerminalWorkerAdapter::new_with_spawn_hook(
                input.route_repo.clone(),
                input.card_role_cache.clone(),
                input.track_area_cache.clone(),
                spawn_hook,
            ))
        } else {
            Arc::new(TerminalWorkerAdapter::new(
                input.route_repo.clone(),
                input.card_role_cache.clone(),
                input.track_area_cache.clone(),
            ))
        };
    let codex_adapter: Arc<dyn ProviderAdapter> = Arc::new(CodexAdapter::new(
        input.route_repo.clone(),
        input.codex.clone(),
        input.shared_codex_appserver.clone(),
        input.pending_codex_threads.clone(),
        input.pending_codex_threads_spawn_serial.clone(),
        input.card_role_cache.clone(),
        input.track_area_cache.clone(),
    ));
    let codex_worker_adapter: Arc<dyn ProviderAdapter> = Arc::new(CodexWorkerAdapter::new(
        input.route_repo.clone(),
        input.codex.clone(),
        input.shared_codex_appserver.clone(),
        input.mcp_server.clone(),
        input.card_role_cache.clone(),
        input.track_area_cache.clone(),
        input.workspace_root.clone(),
    ));
    let claude_adapter: Arc<dyn ProviderAdapter> = Arc::new(ClaudeAdapter::new(
        input.route_repo.clone(),
        input.codex.clone(),
        input.card_role_cache.clone(),
        input.track_area_cache.clone(),
    ));
    let claude_worker_adapter: Arc<dyn ProviderAdapter> = Arc::new(ClaudeWorkerAdapter::new(
        input.route_repo.clone(),
        input.codex.clone(),
        input.mcp_server.clone(),
        input.card_role_cache.clone(),
        input.track_area_cache.clone(),
        input.workspace_root.clone(),
    ));
    let claude_restart_adapter: Arc<dyn ProviderAdapter> = Arc::new(ClaudeRestartAdapter::new(
        input.route_repo.clone(),
        input.codex.clone(),
        input.card_role_cache.clone(),
        input.track_area_cache.clone(),
    ));
    let planner_harness_start_adapter: Arc<dyn ProviderAdapter> =
        Arc::new(PlannerHarnessStartAdapter::new(
            input.repo.clone(),
            input.shared_codex_appserver.clone(),
            input.harness.clone(),
            input.plugin.clone(),
            input.card_role_cache.clone(),
            input.track_area_cache.clone(),
            input
                .mcp_server
                .as_ref()
                .map(|server| server.shim_config.socket_path.clone()),
        ));
    let planner_harness_interrupt_adapter: Arc<dyn ProviderAdapter> =
        Arc::new(PlannerHarnessInterruptAdapter::new(input.harness.clone()));
    let planner_harness_shutdown_adapter: Arc<dyn ProviderAdapter> = Arc::new(
        PlannerHarnessShutdownAdapter::new(input.harness, input.shared_codex_appserver, input.repo),
    );
    let task_verify_adapter: Arc<dyn ProviderAdapter> =
        Arc::new(TaskVerifyAdapter::new(input.gate_logs_dir));
    let forge_action_adapter: Arc<dyn ProviderAdapter> = Arc::new(ForgeActionAdapter::new());
    let child_track_adapter: Arc<dyn ProviderAdapter> = Arc::new(ChildTrackAdapter::new(
        input.card_role_cache.clone(),
        input.track_area_cache.clone(),
        input.workspace_root.clone(),
    ));

    vec![
        terminal_adapter,
        terminal_worker_adapter,
        codex_adapter,
        codex_worker_adapter,
        isolated_codex_adapter,
        Arc::new(crate::file_delivery::adapter::FilePublicationAdapter::new(
            input.route_repo.clone(),
        )),
        Arc::new(
            crate::file_delivery::candidate_verify::CandidateVerifyAdapter::new(
                input.route_repo.clone(),
            ),
        ),
        claude_adapter,
        claude_worker_adapter,
        claude_restart_adapter,
        planner_harness_start_adapter,
        planner_harness_interrupt_adapter,
        planner_harness_shutdown_adapter,
        task_verify_adapter,
        forge_action_adapter,
        child_track_adapter,
    ]
}

impl AppState {
    pub(crate) fn isolated_tasks_available(&self) -> bool {
        self.isolated_codex_backend.is_some()
    }

    /// Bypass the sync-domain gate. For test-fixture seeding only — production code MUST
    /// go through `write_with_event_typed` / `log_pure_event`.
    #[cfg(feature = "fixtures")]
    pub fn raw_repo(&self) -> &dyn Repo {
        self.raw.as_ref()
    }

    pub(crate) fn sqlite_pool(&self) -> Option<sqlx::SqlitePool> {
        self.raw.sqlite_pool()
    }

    /// Test seam: pin a small replay cap without mutating the process-global env var.
    pub fn with_ws_replay_cap(mut self, cap: i64) -> Self {
        self.ws_replay_cap = cap;
        self
    }

    /// The managed workspace root this process was booted with.
    pub fn workspace_root(&self) -> &std::path::Path {
        &self.route.workspace_root
    }

    pub(crate) fn track_delete_locks(&self) -> &crate::per_card_lock::KeyedLocks {
        &self.route.track_delete_locks
    }

    /// Test seam — pin the managed workspace root. Rebuilds the operation runtime because
    /// the codex-worker adapter carries its own copy of the root.
    #[cfg(feature = "fixtures")]
    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.route.workspace_root = root;
        // Release the auto-allocated sandbox; the caller supplied its own root.
        self.workspace_root_guard = None;
        self.rebuild_operation_runtime();
        self
    }

    /// Arm the system-area mint rendezvous so the concurrency test can create that race.
    #[cfg(feature = "fixtures")]
    #[doc(hidden)]
    pub fn with_system_area_mint_rendezvous(
        mut self,
        barrier: std::sync::Arc<tokio::sync::Barrier>,
    ) -> Self {
        self.system_area_mint_rendezvous = Some(barrier);
        self
    }

    /// Arm the create-race rendezvous.
    #[cfg(feature = "fixtures")]
    #[doc(hidden)]
    pub fn with_today_summary_create_rendezvous(
        mut self,
        barrier: std::sync::Arc<tokio::sync::Barrier>,
    ) -> Self {
        self.today_summary_create_rendezvous = Some(barrier);
        self
    }

    /// Arm the first-message race rendezvous.
    #[cfg(feature = "fixtures")]
    #[doc(hidden)]
    pub fn with_today_summary_bootstrap_rendezvous(
        mut self,
        barrier: std::sync::Arc<tokio::sync::Barrier>,
    ) -> Self {
        self.today_summary_bootstrap_rendezvous = Some(barrier);
        self
    }

    pub async fn recover_harnesses_on_boot(&self) -> crate::error::Result<usize> {
        crate::harness::recover_harnesses_on_boot(
            self.raw.clone(),
            self.events.clone(),
            self.card_role_cache.clone(),
            self.track_area_cache.clone(),
            self.shared_codex_appserver.clone(),
            &self.harness,
            &self.route.track_delete_locks,
        )
        .await
    }

    /// Arm the deferred (post-heal) planner harness recovery task; called from boot only
    /// when the daemon spawn failed. The caller detaches the returned handle.
    pub fn arm_deferred_harness_recovery(&self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(crate::harness::recover_harnesses_deferred(
            crate::harness::DeferredRecoveryParams {
                repo: self.raw.clone(),
                events: self.events.clone(),
                card_role_cache: self.card_role_cache.clone(),
                track_area_cache: self.track_area_cache.clone(),
                daemon: self.shared_codex_appserver.clone(),
                registry: self.harness.clone(),
                track_delete_locks: self.route.track_delete_locks.clone(),
                #[cfg(feature = "fixtures")]
                post_eligibility_hook: None,
            },
        ))
    }

    /// Test / replay-lib hatch: build an `AppState` from already-constructed pieces, skipping
    /// the boot-time plugin registry load and background task spawn. `None` caches default to empty.
    pub fn from_parts(
        repo: Arc<dyn Repo>,
        events: EventBus,
        daemon: Arc<DaemonClient>,
        plugin: Arc<PluginHost>,
        codex: Arc<CodexClient>,
        card_role_cache: Option<CardRoleCache>,
        track_area_cache: Option<TrackAreaCache>,
    ) -> Self {
        Self::from_parts_inner(
            repo,
            events,
            daemon,
            plugin,
            codex,
            card_role_cache,
            track_area_cache,
            None,
        )
    }

    /// Replay-lib hatch: the dispatcher is spawned from the same runtime, so replay worker
    /// requests cannot fall back to the real process supervisor.
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts_with_terminal_spawn_hook(
        repo: Arc<dyn Repo>,
        events: EventBus,
        daemon: Arc<DaemonClient>,
        plugin: Arc<PluginHost>,
        codex: Arc<CodexClient>,
        card_role_cache: Option<CardRoleCache>,
        track_area_cache: Option<TrackAreaCache>,
        terminal_spawn_hook: SpawnHook,
    ) -> Self {
        Self::from_parts_inner(
            repo,
            events,
            daemon,
            plugin,
            codex,
            card_role_cache,
            track_area_cache,
            Some(terminal_spawn_hook),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_parts_inner(
        repo: Arc<dyn Repo>,
        events: EventBus,
        daemon: Arc<DaemonClient>,
        plugin: Arc<PluginHost>,
        codex: Arc<CodexClient>,
        card_role_cache: Option<CardRoleCache>,
        track_area_cache: Option<TrackAreaCache>,
        terminal_spawn_hook: Option<SpawnHook>,
    ) -> Self {
        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
        terminal_renderer.set_hook_settings_dir(codex.terminal_hook_settings_dir.clone());
        let card_role_cache = card_role_cache.unwrap_or_default();
        let track_area_cache = track_area_cache.unwrap_or_default();
        let harness = HarnessRegistry::new();
        let pending_codex_threads = Arc::new(PendingThreadStartRegistry::new(
            repo.clone(),
            events.clone(),
        ));
        let pending_codex_threads_spawn_serial = Arc::new(Mutex::new(()));
        let shared_codex_appserver = SharedCodexAppServer::new_stub(repo.clone());
        // Allocated before the adapters so the codex-worker adapter and the routes agree on one root.
        let workspace_root_sandbox = Arc::new(
            tempfile::Builder::new()
                .prefix("neige-calm-test-workspaces-")
                .tempdir()
                .expect("allocate a managed-workspace sandbox for AppState::from_parts"),
        );
        let operation_repo = Arc::new(SqlxOperationRepo::new(
            repo.sqlite_pool()
                .expect("AppState::from_parts requires a sqlite-backed Repo"),
        ));
        let adapters = build_operation_adapters(OperationAdapterInputs {
            isolated_codex_backend: None,
            route_repo: route_repo.clone(),
            repo: repo.clone(),
            plugin: plugin.clone(),
            codex: codex.clone(),
            shared_codex_appserver: shared_codex_appserver.clone(),
            pending_codex_threads: pending_codex_threads.clone(),
            pending_codex_threads_spawn_serial: pending_codex_threads_spawn_serial.clone(),
            card_role_cache: card_role_cache.clone(),
            track_area_cache: track_area_cache.clone(),
            terminal_spawn_hook,
            harness: harness.clone(),
            mcp_server: None,
            gate_logs_dir: TaskVerifyAdapter::default_gate_logs_dir(),
            workspace_root: workspace_root_sandbox.path().to_path_buf(),
        });
        let completion = OperationCompletionBus::new();
        let operation_runtime = Arc::new(OperationRuntime::new_unchecked(
            operation_repo.clone(),
            adapters,
            events.clone(),
            completion.clone(),
            SpawnCtx::new(
                route_repo.clone(),
                operation_repo,
                daemon.clone(),
                terminal_renderer.clone(),
                events.clone(),
                completion,
            )
            .with_shared_codex_appserver(shared_codex_appserver.clone()),
        ));
        let card_kind_registry = Arc::new(CardKindRegistry::builtins());
        let write = WriteContext::new(card_role_cache.clone(), track_area_cache.clone());
        let task_budget_default = crate::scheduler::Scheduler::budget_from_env(
            crate::scheduler::DEFAULT_TRACK_TASK_BUDGET,
        );
        // The operation-runtime cell is left empty on purpose: the runtime built below is
        // replaced by the fixture builders, so a value set here could go stale.
        let plugin_host_cell = Arc::new(tokio::sync::OnceCell::new());
        let _ = plugin_host_cell.set(plugin.clone());
        let mcp_context = crate::mcp_server::registry::AppContext::new(
            repo.clone(),
            events.clone(),
            write.clone(),
            None,
            plugin_host_cell,
            Arc::new(tokio::sync::OnceCell::new()),
            TaskVerifyAdapter::default_gate_logs_dir(),
            task_budget_default,
        );
        let dispatcher = Arc::new(
            Dispatcher::spawn_with_terminal_renderer_and_harness_and_operation_runtime(
                repo.clone(),
                events.clone(),
                write.clone(),
                codex.clone(),
                daemon.clone(),
                terminal_renderer.clone(),
                None,
                harness.clone(),
                shared_codex_appserver.clone(),
                operation_runtime.clone(),
                Dispatcher::permits_from_env(8),
                task_budget_default,
            ),
        );
        let worker_flow = WorkerFlowDriver::from_state_parts(
            repo.clone(),
            shared_codex_appserver.clone(),
            events.clone(),
        );
        let database_id = repo.database_id();
        BootState {
            repo,
            workspace_root: workspace_root_sandbox.path().to_path_buf(),
            workspace_root_guard: Some(workspace_root_sandbox),
            task_budget_default,
            events,
            daemon,
            terminal_renderer,
            plugin,
            codex,
            db_instance_id: Arc::new(uuid::Uuid::new_v4().to_string()),
            database_id,
            templates: crate::templates::TemplateRoster::builtin(),
            card_role_cache,
            track_area_cache,
            card_kind_registry,
            dispatcher,
            mcp_server: None,
            mcp_context,
            harness,
            shared_codex_appserver,
            pending_codex_threads,
            pending_codex_threads_spawn_serial,
            operation_runtime,
            worker_flow,
            isolated_codex_backend: None,
        }
        .into_app_state()
    }

    #[cfg(feature = "fixtures")]
    pub fn with_operation_runtime(mut self, runtime: Arc<OperationRuntime>) -> Self {
        self.operation_runtime = runtime.clone();
        self.route.operation_runtime = runtime;
        self
    }

    #[cfg(feature = "fixtures")]
    pub fn with_shared_codex_appserver(mut self, shared: Arc<SharedCodexAppServer>) -> Self {
        self.shared_codex_appserver = shared.clone();
        self.codex_shell.shared_codex_appserver = shared;
        self.worker_flow = WorkerFlowDriver::from_state_parts(
            self.raw.clone(),
            self.shared_codex_appserver.clone(),
            self.events.clone(),
        );
        self.rebuild_operation_runtime();
        self
    }

    /// Arm the track-create mint rendezvous so the cross-instance primary-key race is
    /// constructed rather than hoped for.
    #[cfg(feature = "fixtures")]
    #[doc(hidden)]
    pub fn with_track_create_mint_rendezvous(
        mut self,
        gate: Arc<crate::routes::tracks::TrackCreateMintGate>,
    ) -> Self {
        self.route.track_create_mint_rendezvous = Some(gate);
        self
    }

    /// Fixture assembly only: configure before authoring or dispatching work.
    #[cfg(feature = "fixtures")]
    pub fn with_isolated_codex_backend(mut self, backend: Arc<IsolatedCodexBackend>) -> Self {
        self.isolated_codex_backend = Some(backend);
        self.rebuild_operation_runtime();
        self
    }

    /// Route the series read through a test's own `AppContext`.
    #[cfg(feature = "fixtures")]
    pub fn with_mcp_context(
        mut self,
        mcp_context: Arc<crate::mcp_server::registry::AppContext>,
    ) -> Self {
        self.route.mcp_context = mcp_context;
        self
    }

    #[cfg(feature = "fixtures")]
    pub fn with_mcp_server(mut self, mcp_server: Arc<McpServer>) -> Self {
        self.mcp_server = Some(mcp_server);
        self.rebuild_operation_runtime();
        self
    }

    #[cfg(feature = "fixtures")]
    pub fn with_pending_codex_threads(mut self, pending: Arc<PendingThreadStartRegistry>) -> Self {
        self.pending_codex_threads = pending.clone();
        self.codex_shell.pending_codex_threads = pending;
        self.rebuild_operation_runtime();
        self
    }

    /// Test seam — the roster `--templates-dir <dir>` would give this process, through the
    /// same `for_boot` the production boot uses. Panics on a directory that does not load.
    #[cfg(feature = "fixtures")]
    pub fn with_templates_dir(mut self, dir: &std::path::Path) -> Self {
        self.route.templates = crate::templates::TemplateRoster::for_boot(Some(dir))
            .unwrap_or_else(|error| panic!("with_templates_dir({}): {error}", dir.display()));
        self
    }

    #[cfg(feature = "fixtures")]
    fn rebuild_operation_runtime(&mut self) {
        let route_repo: Arc<dyn RouteRepo> = self.raw.clone();
        let operation_repo =
            Arc::new(SqlxOperationRepo::new(self.raw.sqlite_pool().expect(
                "OperationRuntime rebuild requires a sqlite-backed Repo",
            )));
        let adapters = build_operation_adapters(OperationAdapterInputs {
            isolated_codex_backend: self.isolated_codex_backend.clone(),
            route_repo: route_repo.clone(),
            repo: self.raw.clone(),
            plugin: self.plugin.clone(),
            codex: self.codex.clone(),
            shared_codex_appserver: self.shared_codex_appserver.clone(),
            pending_codex_threads: self.pending_codex_threads.clone(),
            pending_codex_threads_spawn_serial: self.pending_codex_threads_spawn_serial.clone(),
            card_role_cache: self.card_role_cache.clone(),
            track_area_cache: self.track_area_cache.clone(),
            terminal_spawn_hook: None,
            harness: self.harness.clone(),
            mcp_server: self.mcp_server.clone(),
            gate_logs_dir: TaskVerifyAdapter::default_gate_logs_dir(),
            workspace_root: self.route.workspace_root.clone(),
        });
        let completion = OperationCompletionBus::new();
        let runtime = Arc::new(OperationRuntime::new_unchecked(
            operation_repo.clone(),
            adapters,
            self.events.clone(),
            completion.clone(),
            SpawnCtx::new(
                route_repo,
                operation_repo,
                self.daemon.clone(),
                self.terminal_renderer.clone(),
                self.events.clone(),
                completion,
            )
            .with_shared_codex_appserver(self.shared_codex_appserver.clone()),
        ));
        self.operation_runtime = runtime.clone();
        self.route.operation_runtime = runtime.clone();
        if self.isolated_codex_backend.is_some() {
            // Assembly must replace every consumer of the old registry; otherwise
            // REST sees the configured backend while the scheduler still sees None.
            self.dispatcher.stop_background_for_fixture_rebuild();
            let dispatcher = Arc::new(
                Dispatcher::spawn_with_terminal_renderer_and_harness_and_operation_runtime(
                    self.raw.clone(),
                    self.events.clone(),
                    self.route.write.clone(),
                    self.codex.clone(),
                    self.daemon.clone(),
                    self.terminal_renderer.clone(),
                    self.mcp_server.clone(),
                    self.harness.clone(),
                    self.shared_codex_appserver.clone(),
                    runtime,
                    self.dispatcher.permits(),
                    self.route.task_budget_default,
                ),
            );
            self.worker.dispatcher = dispatcher.clone();
            self.worker.mcp_server = self.mcp_server.clone();
            self.dispatcher = dispatcher;
        }
    }

    pub fn card_kind_registry(&self) -> &CardKindRegistry {
        &self.card_kind_registry
    }

    pub fn write(&self) -> &WriteContext {
        &self.route.write
    }

    /// The production boot: template roster first (fail-closed, before storage exists),
    /// then storage, then [`Self::new`].
    pub async fn boot(cfg: &Config) -> anyhow::Result<Self> {
        let templates = crate::templates::TemplateRoster::for_boot(cfg.templates_dir.as_deref())
            .map_err(|error| anyhow::anyhow!("template roster: {error}"))?;
        let repo: Arc<dyn Repo> = if cfg.db_url == "mock" {
            tracing::warn!(
                "calm-server starting with in-memory SqlxRepo (sqlite::memory:, non-durable)"
            );
            Arc::new(crate::db::sqlite::SqlxRepo::open("sqlite::memory:").await?)
        } else {
            Arc::new(crate::db::sqlite::SqlxRepo::open(&cfg.db_url).await?)
        };
        Self::new(cfg, repo, templates).await
    }

    /// Boot-time constructor. Per-plugin load failures are downgraded to warnings so one
    /// broken plugin can't block boot. `templates` is a parameter because the roster is
    /// validated before `repo` exists.
    pub async fn new(
        cfg: &Config,
        repo: Arc<dyn Repo>,
        templates: &'static crate::templates::TemplateRoster,
    ) -> anyhow::Result<Self> {
        let isolated_codex_backend = match &cfg.isolated_codex_config {
            Some(path) => {
                let config: IsolatedCodexConfig = serde_json::from_slice(&std::fs::read(path)?)?;
                Some(Arc::new(IsolatedCodexBackend::new(config)?))
            }
            None => None,
        };
        let plugins_dir = cfg.plugins_dir_resolved();
        if !plugins_dir.exists() {
            // Fresh-install path: a missing dir is normal on first boot.
            tracing::info!(
                plugins_dir = %plugins_dir.display(),
                "creating plugins dir"
            );
            std::fs::create_dir_all(&plugins_dir)?;
        }
        let (registry, report) = PluginRegistry::load_from_dir(&plugins_dir)?;
        tracing::info!(
            loaded = report.loaded.len(),
            skipped = report.skipped.len(),
            "plugin registry loaded"
        );

        // Created at boot so the first track create is not what discovers the parent is unwritable.
        let workspace_root = cfg.workspace_root_resolved();
        if !workspace_root.exists() {
            tracing::info!(
                workspace_root = %workspace_root.display(),
                "creating managed workspace root"
            );
            std::fs::create_dir_all(&workspace_root)?;
        }
        // Canonicalize once, at boot: every downstream prefix comparison is only sound
        // against a canonical root.
        let workspace_root = std::fs::canonicalize(&workspace_root).map_err(|error| {
            anyhow::anyhow!(
                "canonicalize managed workspace root {}: {error}",
                workspace_root.display()
            )
        })?;

        let plugins_data_dir = cfg.plugins_data_dir_resolved();
        if !plugins_data_dir.exists() {
            tracing::info!(
                plugins_data_dir = %plugins_data_dir.display(),
                "creating plugins data dir"
            );
            std::fs::create_dir_all(&plugins_data_dir)?;
        }

        let events = EventBus::new();
        let task_budget_default = crate::scheduler::Scheduler::budget_from_env(
            crate::scheduler::DEFAULT_TRACK_TASK_BUDGET,
        );

        // Seed after migrations and before any background task is spawned, so every task
        // sees the same cache state the first REST write will.
        let card_role_cache = CardRoleCache::new();
        repo.seed_card_role_cache(&card_role_cache).await?;
        // Same seed-then-spawn order as the role cache.
        let track_area_cache = TrackAreaCache::new();
        repo.seed_track_area_cache(&track_area_cache).await?;
        let card_kind_registry = Arc::new(CardKindRegistry::builtins());
        let write = WriteContext::new(card_role_cache.clone(), track_area_cache.clone());

        crate::card_fsm::spawn(repo.clone(), events.clone(), write.clone());

        let daemon = Arc::new(DaemonClient::new(cfg));
        let codex = Arc::new(CodexClient::new(cfg));
        if let Err(e) = codex.shared_codex_home.seed() {
            tracing::warn!(
                error = %e,
                "shared CODEX_HOME seed failed; continuing; legacy per-card homes still functional"
            );
        }
        // One-time boot repair for historically-seeded homes. On failure the launch-time
        // guard refuses the shared daemon; calm-server itself stays up.
        match codex
            .shared_codex_home
            .sanitize_unexpected_mcp_servers(crate::shared_codex_home::EXPECTED_MCP_SERVERS)
        {
            Ok(removed) if !removed.is_empty() => {
                tracing::warn!(
                    ?removed,
                    "sanitized shared CODEX_HOME: removed unexpected executable-vector entries"
                );
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "shared CODEX_HOME sanitize failed; shared daemon boot guard may refuse launch"
                );
            }
        }

        // Boot failure is a hard error: no MCP server means planner / worker cards can't
        // emit events, which would silently break the track FSM.
        let mcp_socket_path =
            crate::mcp_server::transport::default_socket_path(&cfg.data_dir_resolved());
        let mcp_shim_bin = resolve_mcp_stdio_shim_bin(cfg);
        let mcp_registry = crate::mcp_server::build_default_registry();
        let daemon_mcp_token =
            crate::mcp_server::auth::get_or_generate_daemon_token(&cfg.data_dir_resolved())?;
        let daemon_mcp_token_hash = crate::mcp_server::auth::hash_token(&daemon_mcp_token);
        let plugin_host_cell = Arc::new(tokio::sync::OnceCell::new());
        let operation_runtime_cell = Arc::new(tokio::sync::OnceCell::new());
        // One resolution of the gate-logs dir, shared by the gate runner and the MCP
        // `gate.log` view, so writer and reader cannot split.
        let gate_logs_dir = cfg.data_dir_resolved().join("gate-logs");
        // One context for both readers: the MCP listener and `RouteState::mcp_context` hold the same `Arc`.
        let mcp_context = crate::mcp_server::registry::AppContext::new(
            repo.clone(),
            events.clone(),
            write.clone(),
            Some(daemon_mcp_token_hash),
            plugin_host_cell.clone(),
            operation_runtime_cell.clone(),
            gate_logs_dir.clone(),
            task_budget_default,
        );
        let mcp_server = crate::mcp_server::McpServer::spawn_with_context(
            mcp_context.clone(),
            mcp_socket_path,
            mcp_shim_bin,
            mcp_registry,
        )
        .await?;
        if let Err(e) = codex
            .shared_codex_home
            .ensure_daemon_mcp_config(&mcp_server.shim_config, &daemon_mcp_token)
        {
            tracing::warn!(
                error = %e,
                "shared CODEX_HOME daemon MCP config write failed; shared prompt cards may not reach kernel MCP"
            );
        }

        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
        terminal_renderer.set_hook_settings_dir(codex.terminal_hook_settings_dir.clone());
        mcp_server
            .terminal_interaction
            .set(Arc::new(
                crate::terminal_interaction::TerminalInteraction::new(
                    route_repo.clone(),
                    terminal_renderer.clone(),
                ),
            ))
            .map_err(|_| anyhow::anyhow!("terminal interaction already initialized"))?;
        let harness = HarnessRegistry::new();
        crate::track_activity::spawn(repo.clone(), events.clone(), write.clone(), harness.clone());
        let pending_codex_threads = Arc::new(PendingThreadStartRegistry::new(
            repo.clone(),
            events.clone(),
        ));
        spawn_periodic_expire_task(
            pending_codex_threads.clone(),
            Duration::from_secs(60),
            Duration::from_secs(60 * 60 * 6),
        );
        let pending_codex_threads_spawn_serial = Arc::new(Mutex::new(()));
        let shared_codex_appserver = SharedCodexAppServer::new_with_recovery(
            cfg,
            codex.shared_codex_home.clone(),
            repo.clone(),
            Some(pending_codex_threads.clone()),
            Some(crate::semantic_recovery::RecoveryService {
                repo: repo.clone(),
                events: events.clone(),
                write: write.clone(),
            }),
        );
        let plugin = Arc::new(PluginHost::new_full(
            Arc::new(registry),
            repo.clone(),
            plugins_dir,
            plugins_data_dir,
            cfg.plugins_disabled.clone(),
            events.clone(),
            write.clone(),
        ));
        let _ = plugin_host_cell.set(plugin.clone());
        let operation_repo = Arc::new(SqlxOperationRepo::new(
            repo.sqlite_pool()
                .ok_or_else(|| anyhow::anyhow!("OperationRuntime requires a sqlite-backed Repo"))?,
        ));
        let adapters = build_operation_adapters(OperationAdapterInputs {
            isolated_codex_backend: isolated_codex_backend.clone(),
            route_repo: route_repo.clone(),
            repo: repo.clone(),
            plugin: plugin.clone(),
            codex: codex.clone(),
            shared_codex_appserver: shared_codex_appserver.clone(),
            pending_codex_threads: pending_codex_threads.clone(),
            pending_codex_threads_spawn_serial: pending_codex_threads_spawn_serial.clone(),
            card_role_cache: card_role_cache.clone(),
            track_area_cache: track_area_cache.clone(),
            terminal_spawn_hook: None,
            harness: harness.clone(),
            mcp_server: Some(mcp_server.clone()),
            gate_logs_dir: gate_logs_dir.clone(),
            workspace_root: workspace_root.clone(),
        });
        let completion = OperationCompletionBus::new();
        let operation_runtime = Arc::new(
            OperationRuntime::new(
                operation_repo.clone(),
                adapters,
                events.clone(),
                completion.clone(),
                SpawnCtx::new(
                    route_repo.clone(),
                    operation_repo,
                    daemon.clone(),
                    terminal_renderer.clone(),
                    events.clone(),
                    completion,
                )
                .with_shared_codex_appserver(shared_codex_appserver.clone()),
            )
            .await?,
        );
        let _ = operation_runtime_cell.set(operation_runtime.clone());

        // Spawned between role-cache seed and plugin autospawn so the bus has a
        // `*.Requested`-aware listener before plugins start emitting.
        let dispatcher = Arc::new(
            crate::dispatcher::Dispatcher::spawn_with_terminal_renderer_and_harness_and_operation_runtime(
                repo.clone(),
                events.clone(),
                write.clone(),
                codex.clone(),
                daemon.clone(),
                terminal_renderer.clone(),
                Some(mcp_server.clone()),
                harness.clone(),
                shared_codex_appserver.clone(),
                operation_runtime.clone(),
                crate::dispatcher::Dispatcher::permits_from_env(8),
                task_budget_default,
            ),
        );

        // Per-plugin errors are logged inside `autospawn_enabled`; one broken plugin never blocks boot.
        plugin.autospawn_enabled().await;

        let running_plugin_ids = plugin.running_plugin_ids().await;
        for manifest in plugin.registry().list() {
            let plugin_id = manifest.id.clone();
            if !running_plugin_ids.contains(&plugin_id) {
                continue;
            }
            for entry in manifest.exposes_tools {
                let tool_name = entry.name;
                if let Err(e) = repo
                    .log_pure_event(
                        ActorId::Kernel,
                        EventScope::System,
                        None,
                        &events,
                        &card_role_cache,
                        &track_area_cache,
                        Event::PluginToolRegistered {
                            plugin_id: plugin_id.clone(),
                            tool_name: tool_name.clone(),
                        },
                    )
                    .await
                {
                    tracing::warn!(
                        plugin_id = %plugin_id,
                        tool_name = %tool_name,
                        error = %e,
                        "plugin_tool_registered event log failed"
                    );
                }
            }
        }

        let worker_flow = WorkerFlowDriver::from_state_parts(
            repo.clone(),
            shared_codex_appserver.clone(),
            events.clone(),
        );
        let database_id = repo.database_id();
        let state = BootState {
            repo,
            workspace_root,
            // Production: the root is the user's real directory, never swept.
            workspace_root_guard: None,
            task_budget_default,
            events,
            daemon,
            terminal_renderer,
            plugin,
            codex,
            db_instance_id: Arc::new(uuid::Uuid::new_v4().to_string()),
            database_id,
            templates,
            card_role_cache,
            track_area_cache,
            card_kind_registry,
            dispatcher,
            mcp_server: Some(mcp_server),
            mcp_context,
            harness,
            shared_codex_appserver,
            pending_codex_threads,
            pending_codex_threads_spawn_serial,
            operation_runtime,
            worker_flow,
            isolated_codex_backend,
        };
        let state = state.into_app_state();

        // Orphan-terminal sweeper; emits `TerminalDeleted` through the audited write pipeline. The
        // same tick also ends worker sessions left running on completed tracks.
        crate::terminal_sweeper::spawn(state.clone());

        // VCS objects are content-addressed and shared by multiple tracks, so deletion only
        // removes refs + commits; unreferenced objects are reclaimed hourly with a grace window.
        if let Some(pool) = state.raw.sqlite_pool() {
            crate::track_vcs::spawn_unreferenced_object_sweeper(pool.clone());
            crate::track_vcs::spawn_track_history_pruner(pool.clone());
            crate::events_prune::spawn_events_pruner(pool);
        }

        Ok(state)
    }
}

impl FromRef<AppState> for RouteState {
    fn from_ref(s: &AppState) -> Self {
        s.route.clone()
    }
}

impl FromRef<AppState> for WorkerState {
    fn from_ref(s: &AppState) -> Self {
        s.worker.clone()
    }
}

impl FromRef<AppState> for CodexShellState {
    fn from_ref(s: &AppState) -> Self {
        s.codex_shell.clone()
    }
}

impl FromRef<AppState> for WriteContext {
    fn from_ref(s: &AppState) -> Self {
        s.route.write.clone()
    }
}
