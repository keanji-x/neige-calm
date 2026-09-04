//! Shared app state passed to every handler.
//!
//! `Clone` is cheap — everything inside is wrapped in `Arc` or already
//! reference-counted internally.

use crate::card_kind::CardKindRegistry;
use crate::card_role_cache::CardRoleCache;
use crate::config::Config;
use crate::db::{Repo, RouteRepo};
use crate::dispatcher::Dispatcher;
use crate::event::{Event, EventBus, EventScope};

use crate::harness::HarnessRegistry;
use crate::ids::ActorId;
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

/// Fixed-size FIFO cache for hook ingest idempotency keys.
///
/// This is intentionally process-local: after a server restart the first
/// re-posted hook can emit again, and downstream harness/dispatcher replay
/// guards are the remaining defense.
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

/// #480 PR1 write-surface slice shared by route and worker substates.
/// Clone-cheap: both caches alias their underlying `Arc<DashMap<...>>`.
pub use calm_truth::state::WriteContext;

/// #480 PR1 route-facing state slice for future handler extraction.
/// Mirrors existing `AppState` handles without changing caller behavior.
#[derive(Clone)]
pub struct RouteState {
    pub repo: Arc<dyn RouteRepo>,
    /// #1147 D2 — root for server-managed track workspaces
    /// (`<root>/<area_id>/<track_id>`). Resolved once at boot from
    /// `--workspace-root` / `CALM_WORKSPACE_ROOT`; never read from env at
    /// request time.
    pub workspace_root: PathBuf,
    /// Server-resolved default used by both scheduler admission and report
    /// diagnostics. Read once at boot; request handlers never consult env.
    pub task_budget_default: i64,
    pub events: EventBus,
    pub plugin: Arc<PluginHost>,
    pub db_instance_id: Arc<String>,
    pub write: WriteContext,
    pub operation_runtime: Arc<OperationRuntime>,
    pub harness: HarnessRegistry,
    pub(crate) hook_ingest_cache: Arc<StdMutex<HookIngestCache>>,
    /// Issue #649 i2 — per-card serialization for `/planner/input` lazy harness
    /// recovery. Concurrent Sends racing a registry miss must not both call
    /// `spawn_recovered_harness` (the second spawn shuts the first down
    /// mid-turn). Entries self-clean when the last guard drops.
    ///
    /// Lock order: this is the INNER of the two per-card maps. See
    /// `conversation_first_message_locks` — `conversation_first_message_locks`
    /// → `planner_recovery_locks` is the only order any path takes today, and a
    /// path taking them in the reverse order would close a deadlock cycle
    /// against it.
    pub(crate) planner_recovery_locks: crate::per_card_lock::PerCardLocks,
    /// Per-card claim serializing the "send the conversation's first message"
    /// step of `POST /api/tracks/{id}/conversations`. Two
    /// concurrent POSTs under one `Idempotency-Key` share one operation, so
    /// without this both would observe "no user message yet" and send the same
    /// instruction twice.
    ///
    /// Deliberately a SEPARATE map from `planner_recovery_locks`: the claim is
    /// held across the call into `send_planner_input`, whose
    /// `ensure_live_planner_harness` takes `planner_recovery_locks` for the same
    /// card on the registry-miss path. `tokio::sync::Mutex` is not reentrant,
    /// so sharing one map would self-deadlock the request.
    ///
    /// **Two premises this lock's correctness rests on. Both are load-bearing;
    /// neither is enforced by the type system.**
    ///
    /// 1. *In-process only.* This is an `Arc<DashMap<_, tokio::Mutex<_>>>`
    ///    living in one `RouteState`. It serializes nothing across processes,
    ///    so it is correct exactly because the deployment is ONE calm-server
    ///    per SQLite data directory (the operation driver runs in this process
    ///    too). Run two instances against one DB and the claim degrades to
    ///    "each process reads `events` once": worst case one conversation's
    ///    first message is delivered twice. It can still never mint a second
    ///    card — that wall is the derived card id, which is independent of
    ///    this lock. Multi-instance would need a DB-level claim, not a bigger
    ///    map.
    /// 2. *One permitted lock order:* `conversation_first_message_locks` →
    ///    `planner_recovery_locks`, and never the reverse. The only nesting in
    ///    the tree is `create_track_conversation` holding this claim across
    ///    `send_planner_input` → `ensure_live_planner_harness`. Two distinct facts,
    ///    kept apart because round 3 caught them conflated:
    ///    * *Sharing ONE map* for both purposes self-deadlocks a single
    ///      request outright: it would re-enter the same `tokio::sync::Mutex`
    ///      for the same card while still holding its guard, and
    ///      `tokio::sync::Mutex` is not reentrant. That is why these are two
    ///      maps.
    ///    * *There are two forward-order nestings.*
    ///      `create_track_conversation` (#1189) and `routes::today_summary`'s
    ///      bootstrap recovery (#1253 PR2) both hold this claim across
    ///      `send_planner_input` → `ensure_live_planner_harness`. The Today path also
    ///      holds it across a `planner-harness-start` operation on its dormant
    ///      branch, which blocks on a codex RPC; that submits no per-card lock
    ///      of its own (the adapter's mint map is private and is taken and
    ///      released inside the operation), so it closes no cycle.
    ///    * *Taking the two maps in the reverse order* does NOT self-deadlock
    ///      — they are different mutexes, so one task alone completes fine.
    ///      It deadlocks only when it runs concurrently with a
    ///      forward-order holder for the same card, closing the cycle. That
    ///      failure is intermittent and load-dependent, which is precisely
    ///      why the order is stated here instead of being left to chance.
    ///      Today no reverse-order path exists (boot replay, `/planner/input`
    ///      and `/planner/reset` take only the recovery lock and never enter
    ///      conversation create); any new caller must preserve this order.
    pub(crate) conversation_first_message_locks: crate::per_card_lock::PerCardLocks,
    /// Per-track fence shared by single-track deletion and direct runtime
    /// recovery/reattach paths that bypass `OperationRuntime`. Once DELETE owns
    /// this lock, those paths cannot install a runtime behind its teardown
    /// snapshot; the guard transfers to the owned deletion saga and survives
    /// request cancellation through commit or compensation.
    ///
    /// The deployment contract is one calm-server per SQLite data directory.
    /// Multi-process serving would require a durable database fence.
    pub(crate) track_delete_locks: crate::per_card_lock::KeyedLocks,
    /// Serializes a user-area delete with the ordinary track-create route.
    /// The creator holds it through workspace materialization and planner
    /// startup; deletion therefore snapshots a closed member set.
    pub(crate) area_delete_locks: crate::per_card_lock::KeyedLocks,
}

/// #480 PR1 worker-facing state slice for dispatcher/background flows.
/// Mirrors existing `AppState` handles without changing caller behavior.
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

/// #480 PR1 codex-shell state slice for shared app-server flows.
/// Mirrors existing `AppState` handles without changing caller behavior.
#[derive(Clone)]
pub struct CodexShellState {
    pub codex: Arc<CodexClient>,
    pub shared_codex_appserver: Arc<SharedCodexAppServer>,
    pub pending_codex_threads: Arc<PendingThreadStartRegistry>,
    pub pending_codex_threads_spawn_serial: Arc<Mutex<()>>,
    pub plugin: Arc<PluginHost>,
}

/// #480 PR1 boot aggregate that materializes compat fields plus slices.
/// Constructors build this only after resolving all boot handles.
pub struct BootState {
    pub repo: Arc<dyn Repo>,
    /// #1147 D2 — see [`RouteState::workspace_root`].
    pub workspace_root: PathBuf,
    /// #1147 S2 — owns the auto-allocated workspace root when one was minted
    /// for a test/replay `AppState`. `None` in production, where the root is
    /// the user's real, persistent directory. Held so the sandbox is removed
    /// when the state drops instead of accumulating a git repository per test
    /// run under the system temp dir.
    pub workspace_root_guard: Option<Arc<tempfile::TempDir>>,
    pub task_budget_default: i64,
    pub events: EventBus,
    pub daemon: Arc<DaemonClient>,
    pub terminal_renderer: Arc<TerminalRendererRegistry>,
    pub plugin: Arc<PluginHost>,
    pub codex: Arc<CodexClient>,
    pub db_instance_id: Arc<String>,
    pub card_role_cache: CardRoleCache,
    pub track_area_cache: TrackAreaCache,
    /// #477 PR5 — kernel card-kind handler registry. Substate placement is
    /// left to PR2/PR3 once call sites migrate; for PR1 it stays on `AppState`
    /// alongside the other 17 compat fields and rides through `BootState`.
    pub card_kind_registry: Arc<CardKindRegistry>,
    pub dispatcher: Arc<Dispatcher>,
    pub mcp_server: Option<Arc<McpServer>>,
    pub harness: HarnessRegistry,
    pub shared_codex_appserver: Arc<SharedCodexAppServer>,
    pub pending_codex_threads: Arc<PendingThreadStartRegistry>,
    pub pending_codex_threads_spawn_serial: Arc<Mutex<()>>,
    pub operation_runtime: Arc<OperationRuntime>,
    pub worker_flow: Arc<WorkerFlowDriver>,
}

impl BootState {
    pub fn into_app_state(self) -> AppState {
        let route_repo: Arc<dyn RouteRepo> = self.repo.clone();
        let write = WriteContext::new(self.card_role_cache.clone(), self.track_area_cache.clone());
        let hook_ingest_cache = Arc::new(StdMutex::new(HookIngestCache::new(
            HOOK_INGEST_CACHE_CAPACITY,
        )));
        let route = RouteState {
            repo: route_repo.clone(),
            workspace_root: self.workspace_root.clone(),
            task_budget_default: self.task_budget_default,
            events: self.events.clone(),
            plugin: self.plugin.clone(),
            db_instance_id: self.db_instance_id.clone(),
            write: write.clone(),
            operation_runtime: self.operation_runtime.clone(),
            harness: self.harness.clone(),
            hook_ingest_cache,
            planner_recovery_locks: crate::per_card_lock::new_per_card_locks(),
            conversation_first_message_locks: crate::per_card_lock::new_per_card_locks(),
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
            raw: self.repo,
            workspace_root_guard: self.workspace_root_guard,
            route,
            worker,
            codex_shell,
        }
    }
}

/// Route-facing handle: the trait object `AppState::repo` exposes. Excludes
/// `RepoSyncDomainRaw` — see `db/mod.rs` module doc for the capability split.
///
/// `Arc<dyn RouteRepo>` is what handlers see; integration tests that need
/// to seed fixtures reach `&dyn Repo` via [`AppState::raw_repo`], which
/// is gated behind the `fixtures` cargo feature (only enabled for the
/// `tests/*.rs` integration crates via the self-loop dev-dep). No
/// production module reaches for `raw_repo` today.
#[derive(Clone)]
pub struct AppState {
    /// Narrow trait object: reads + eventized writes + out-of-domain writes.
    /// Sync-domain raw writes (`area_create`, `track_update`, `card_delete`,
    /// `overlay_upsert`, etc.) are unreachable from this handle — handlers
    /// must funnel them through `db::write_with_event_typed`.
    pub repo: Arc<dyn RouteRepo>,
    pub events: EventBus,
    /// #1253 — per-server observation of the one race in `routes::today` that
    /// is reachable. See [`crate::routes::today::SystemAreaMintCounters`] for
    /// why it lives on the state rather than in a `static`, and why it is not
    /// gated behind `fixtures`.
    pub system_area_mint: Arc<crate::routes::today::SystemAreaMintCounters>,
    /// #1253 — `None` in production. Armed by the system-area concurrency case
    /// so that race is created rather than waited for; see
    /// [`crate::routes::today::SystemAreaMintRendezvous`].
    pub system_area_mint_rendezvous: crate::routes::today::SystemAreaMintRendezvous,
    /// #1253 PR2 — per-server observation of `POST /api/today/summary`'s create
    /// arm. See [`crate::routes::today_summary::TodaySummaryCreateCounters`]
    /// for why it is not `fixtures`-gated.
    pub today_summary_create: Arc<crate::routes::today_summary::TodaySummaryCreateCounters>,
    /// #1253 PR2 — `None` in production. Armed by the create-race case so that
    /// one-request-wide window is created rather than waited for; see
    /// [`crate::routes::today_summary::TodaySummaryCreateRendezvous`].
    pub today_summary_create_rendezvous: crate::routes::today_summary::TodaySummaryCreateRendezvous,
    /// #1253 PR2 — `None` in production. Armed by the first-message race case;
    /// see [`crate::routes::today_summary::TodaySummaryBootstrapRendezvous`].
    pub today_summary_bootstrap_rendezvous:
        crate::routes::today_summary::TodaySummaryBootstrapRendezvous,
    pub daemon: Arc<DaemonClient>,
    pub terminal_renderer: Arc<TerminalRendererRegistry>,
    pub plugin: Arc<PluginHost>,
    pub codex: Arc<CodexClient>,
    /// UUID v4 minted once per server-process boot, surfaced on
    /// `/api/version` as `dbInstanceId`. Lets the web client detect when the
    /// underlying sqlite DB has been recreated under it (e.g. `make dev
    /// RESET_DB=1` or a fresh-migrations branch swap) and bust its
    /// IndexedDB-backed React Query cache + WS event cursor before they
    /// paint stale ids that 404 at the route loader.
    ///
    /// Deliberately not persisted to the DB: the whole point is that it
    /// changes whenever the DB *might* have changed underneath us. A new
    /// process = a new instance id, full stop. `Arc<String>` so the value
    /// is cheap to clone across handler dispatches.
    pub db_instance_id: Arc<String>,
    /// #854 slice 1 — ceiling on the number of rows a single WS replay may
    /// stream (see `ws::events::run_replay` for the over-cap routing).
    /// Resolved ONCE at construction from `NEIGE_WS_REPLAY_MAX_EVENTS`
    /// (default 10_000) rather than per-connection, so tests can inject a
    /// small cap via [`AppState::with_ws_replay_cap`] without mutating
    /// process-global env (env mutation raced sibling tests in the same
    /// binary — review finding on PR #867).
    pub ws_replay_cap: i64,
    /// PR3 (#136) — `CardId -> CardRole` cache used by `role_gate::enforce_role`
    /// at every audited write entry. Clone-cheap (`Arc<DashMap<…>>` inside).
    /// Production builds seed this from the cards table during
    /// [`AppState::new`]; tests construct an empty cache via
    /// [`AppState::from_parts`] when they don't need role-gating coverage,
    /// or pre-populate it manually otherwise. The cache is also threaded
    /// into every `_tx`-suffixed card helper so the insert/delete path
    /// stays write-through inside the surrounding transaction.
    pub card_role_cache: CardRoleCache,
    /// #234 — `TrackId -> AreaId` cache the role gate consults alongside
    /// `card_role_cache` to cross-check `scope.area` against a Worker
    /// card's home area. Mirrors the shape + clone semantics of
    /// `card_role_cache`. Production builds seed this from the tracks
    /// table in [`AppState::new`]; tests use the empty default via
    /// [`AppState::from_parts`] or pre-populate it manually.
    pub track_area_cache: TrackAreaCache,
    /// #477 PR5 — registry of kernel-owned card kind handlers. Unknown card
    /// kinds stay opaque; built-ins expose validation + metadata for future
    /// OpenAPI / metrics readers.
    pub card_kind_registry: Arc<CardKindRegistry>,
    /// PR5 (#136) — dispatcher worker handle. Subscribes via
    /// [`EventBus::subscribe_filtered`] to `*.worker_requested` envelopes
    /// and starts the matching worker operation for each, gated by a
    /// global semaphore (default 8 permits, override via
    /// `NEIGE_DISPATCHER_PERMITS`). Held as
    /// `Arc<Dispatcher>` so tests can probe permit counts via
    /// [`Dispatcher::permits`] / [`Dispatcher::semaphore`]; production
    /// callers don't touch the field after construction. Dropping the
    /// `AppState` doesn't immediately abort the dispatcher task —
    /// closure happens when the event bus's `tx` drops too.
    pub dispatcher: Arc<Dispatcher>,
    /// PR7a (#136) — kernel-as-MCP-server handle. Bound to a Unix domain
    /// socket under `<data_dir>/mcp/kernel.sock`; per-card codex daemons
    /// connect through `neige-mcp-stdio-shim` and authenticate via the
    /// per-card token in `card_mcp_tokens`. The handle's `shim_config`
    /// is passed through card MCP token setup so codex-launched shim
    /// processes can reach the kernel MCP server.
    ///
    /// `Option` because `from_parts` (replay / unit tests) skips the
    /// listener boot — neither the replay binary nor most integration
    /// tests need a live MCP server. The production `AppState::new`
    /// path always populates this.
    pub mcp_server: Option<Arc<McpServer>>,
    pub harness: HarnessRegistry,
    /// PR4 (#410) — one server-wide codex app-server supervisor.
    pub shared_codex_appserver: Arc<SharedCodexAppServer>,
    /// FIFO attribution registry for empty cards that fresh-start a thread
    /// through the shared daemon's TUI.
    pub pending_codex_threads: Arc<PendingThreadStartRegistry>,
    /// Serializes the shared empty-card `(pending register, PTY spawn)` pair
    /// so FIFO pending attribution matches actual TUI fresh-start order.
    pub pending_codex_threads_spawn_serial: Arc<Mutex<()>>,
    pub operation_runtime: Arc<OperationRuntime>,
    pub worker_flow: Arc<WorkerFlowDriver>,
    /// Full-capability handle. Held separately from `repo` so the gate at
    /// `AppState::repo` survives even though the underlying concrete impl
    /// is the same `SqlxRepo`. Kept private — callers must go through
    /// [`AppState::raw_repo`] (only visible under `--features fixtures`).
    /// `allow(dead_code)` because in non-`fixtures` builds (the production
    /// binary, the `replay` lib, etc.) nothing reads this field — it's
    /// stored so the `fixtures`-only accessor still has something to hand
    /// out, but the production build keeps the field opaque on purpose.
    #[allow(dead_code)]
    raw: Arc<dyn Repo>,
    /// #1147 S2 — see [`BootState::workspace_root_guard`].
    ///
    /// `allow(dead_code)` because this is an **RAII guard**: its value is never
    /// read, only dropped, and the drop is the entire point (it removes the
    /// per-`AppState` sandbox). The one place that touches it afterwards —
    /// `with_workspace_root`, which releases it — is `fixtures`-gated, so under
    /// default features nothing mentions the field at all.
    ///
    /// Not `expect(dead_code)`: whether the lint fires depends on the feature
    /// set (under `fixtures` the write in `with_workspace_root` counts as a
    /// use), and an unfulfilled `expect` is itself a warning — which, under
    /// this workspace's `RUSTFLAGS=-D warnings`, would just move the build
    /// failure to the other feature combination.
    #[allow(dead_code)]
    workspace_root_guard: Option<Arc<tempfile::TempDir>>,
    route: RouteState,
    worker: WorkerState,
    codex_shell: CodexShellState,
}

struct OperationAdapterInputs {
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
    /// #1147 D2 — see [`RouteState::workspace_root`].
    workspace_root: PathBuf,
}

fn build_operation_adapters(input: OperationAdapterInputs) -> Vec<Arc<dyn ProviderAdapter>> {
    let terminal_adapter: Arc<dyn ProviderAdapter> =
        if let Some(spawn_hook) = input.terminal_spawn_hook.clone() {
            Arc::new(TerminalAdapter::new_with_spawn_hook(
                input.route_repo.clone(),
                input.card_role_cache.clone(),
                input.track_area_cache.clone(),
                spawn_hook,
            ))
        } else {
            Arc::new(TerminalAdapter::new(
                input.route_repo.clone(),
                input.card_role_cache.clone(),
                input.track_area_cache.clone(),
            ))
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
    /// Bypass the sync-domain gate. **For test-fixture seeding only** —
    /// production code MUST go through `write_with_event_typed` /
    /// `log_pure_event`. Gated behind the `fixtures` cargo feature so
    /// production builds (the binary, `routes/*`, `plugin_host/*`,
    /// `terminal_sweeper`, and the `replay` lib) physically cannot reach
    /// this method — invoking it from a production module fails at
    /// compile time with E0599 (`no method named raw_repo`). Integration
    /// tests pick up the feature automatically via the `[dev-dependencies]`
    /// self-loop in `Cargo.toml`.
    #[cfg(feature = "fixtures")]
    pub fn raw_repo(&self) -> &dyn Repo {
        self.raw.as_ref()
    }

    pub(crate) fn sqlite_pool(&self) -> Option<sqlx::SqlitePool> {
        self.raw.sqlite_pool()
    }

    /// Override the WS replay cap (#854 slice 1). Test seam: lets a test
    /// pin a small cap on its own `AppState` instead of mutating the
    /// process-global `NEIGE_WS_REPLAY_MAX_EVENTS` env var, which is racy
    /// against sibling tests booting servers in the same binary. Production
    /// keeps the env-derived default from construction.
    pub fn with_ws_replay_cap(mut self, cap: i64) -> Self {
        self.ws_replay_cap = cap;
        self
    }

    /// #1147 D2 — the managed workspace root this process was booted with.
    pub fn workspace_root(&self) -> &std::path::Path {
        &self.route.workspace_root
    }

    pub(crate) fn track_delete_locks(&self) -> &crate::per_card_lock::KeyedLocks {
        &self.route.track_delete_locks
    }

    /// #1147 S2 test seam — pin the managed workspace root. `from_parts`
    /// defaults to a per-`AppState` directory under the system temp dir so no
    /// test can silently materialize repositories into the developer's real
    /// `$HOME/neige-workspaces`; a test that wants to *inspect* the tree
    /// points this at its own `TempDir` instead.
    ///
    /// `fixtures`-gated because it MUST rebuild the operation runtime: the
    /// codex-worker adapter carries its own copy of the root (it re-runs
    /// materialization when taking a lease), and a stale copy there would make
    /// the adapter reject the very workspace the routes just created — the
    /// containment assertion would compare against the wrong root.
    #[cfg(feature = "fixtures")]
    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.route.workspace_root = root;
        // Release the auto-allocated sandbox: the caller supplied its own root,
        // so the default one is unreachable and would otherwise only be swept
        // when this state drops.
        self.workspace_root_guard = None;
        self.rebuild_operation_runtime();
        self
    }

    /// #1253 — arm the system-area mint rendezvous so the concurrency case can
    /// *create* that race instead of hoping for it.
    ///
    /// **These builders are an attribute-sensitive run: adding a function
    /// between an existing `#[cfg(..)]` and the function it was written for
    /// silently retargets the attribute.** This one was first inserted
    /// immediately above `with_workspace_root` and stole its
    /// `#[cfg(feature = "fixtures")]`, leaving that function unconditional
    /// while the `rebuild_operation_runtime` it calls stayed fixtures-only —
    /// so the non-fixtures lib stopped compiling, and nothing that enables
    /// `fixtures` (which is every test command, via the dev-dep self-loop)
    /// could see it. Put a new builder *after* a complete function, never
    /// between a function and the attributes above it.
    ///
    /// Gating the BUILDER behind `fixtures` costs nothing the "production and
    /// test run the same instructions" rule protects: the field itself is
    /// unconditional and the mint path's `if let Some(..)` is compiled into
    /// every build. Only the ability to arm it is test-only, exactly like
    /// `with_workspace_root` above.
    #[cfg(feature = "fixtures")]
    #[doc(hidden)]
    pub fn with_system_area_mint_rendezvous(
        mut self,
        barrier: std::sync::Arc<tokio::sync::Barrier>,
    ) -> Self {
        self.system_area_mint_rendezvous = Some(barrier);
        self
    }

    /// #1253 PR2 — arm the create-race rendezvous. Same shape, same reasons as
    /// [`Self::with_system_area_mint_rendezvous`]: the field is unconditional
    /// and the `if let Some(..)` is compiled into every build; only the ability
    /// to arm it is test-only.
    #[cfg(feature = "fixtures")]
    #[doc(hidden)]
    pub fn with_today_summary_create_rendezvous(
        mut self,
        barrier: std::sync::Arc<tokio::sync::Barrier>,
    ) -> Self {
        self.today_summary_create_rendezvous = Some(barrier);
        self
    }

    /// #1253 PR2 — arm the first-message race rendezvous. Same shape and same
    /// reasons as the sibling above.
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

    /// #953 §5 — arm the deferred (post-heal) planner harness recovery task.
    /// Called from the boot path ONLY when the daemon spawn failed; the task
    /// waits on the supervisor readiness watch and runs a claim-based
    /// recovery pass on the first observed `running: true`. The boot caller
    /// detaches the returned JoinHandle (PR2 review D3): the task owns
    /// Arc-cloned parts (including the supervisor, so the watch sender it
    /// waits on can never drop under it) and lives until a pass completes
    /// or process teardown.
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

    /// Test / replay-lib hatch: build an `AppState` from already-constructed
    /// pieces, skipping the boot-time plugin registry load + background
    /// task spawn that `new` does. Public so `replay::boot_in_memory` and
    /// integration tests can compose the struct without bypassing the
    /// `raw` field's privacy (which is what guards the capability split
    /// from external `AppState { ... }` literals).
    ///
    /// PR3 (#136): `card_role_cache` defaults to an empty cache when the
    /// caller passes `None`. Tests that exercise role-gating manually
    /// pre-populate the cache via `CardRoleCache::insert` before calling
    /// this; the replay path uses an empty cache because replay events
    /// are seeded via `log_pure_event` from `ActorId::User` (which the
    /// gate lets through without a cache lookup).
    ///
    /// #234: `track_area_cache` follows the same shape — `None` yields
    /// an empty cache. Tests that exercise the Worker area-cross-check
    /// pre-populate the cache via `TrackAreaCache::insert` before
    /// calling this. Most existing tests don't touch the Worker path,
    /// so an empty cache is fine.
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

    /// Replay-lib hatch for constructing the first `OperationRuntime`
    /// with a terminal spawn hook. The dispatcher is spawned from that
    /// same runtime, so replay worker requests cannot fall back to the
    /// real process supervisor.
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
        let card_role_cache = card_role_cache.unwrap_or_default();
        let track_area_cache = track_area_cache.unwrap_or_default();
        let harness = HarnessRegistry::new();
        let pending_codex_threads = Arc::new(PendingThreadStartRegistry::new(
            repo.clone(),
            events.clone(),
        ));
        let pending_codex_threads_spawn_serial = Arc::new(Mutex::new(()));
        let shared_codex_appserver = SharedCodexAppServer::new_stub(repo.clone());
        // #1147 S2 — allocated before the adapters so the codex-worker adapter
        // and the routes agree on one root (it re-materializes on lease).
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
        // PR5 (#136): every `AppState` carries a live dispatcher. Test
        // call sites that need to assert on dispatcher behavior reach
        // through `state.dispatcher`; the rest see a passive worker
        // that's silent until something emits a `*.worker_requested`
        // event. Permit count honors `NEIGE_DISPATCHER_PERMITS` for
        // the rare test that twiddles the env var; the default 8 is
        // the value tests will see otherwise.
        let dispatcher = Arc::new(
            Dispatcher::spawn_with_terminal_renderer_and_harness_and_operation_runtime(
                repo.clone(),
                events.clone(),
                write.clone(),
                codex.clone(),
                daemon.clone(),
                terminal_renderer.clone(),
                // `from_parts` is the test / replay hatch — no live MCP
                // server. PR7a.1 (#136 followup) added this slot.
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
        BootState {
            repo,
            // #1147 S2 — `from_parts` is the test / replay hatch. A per-instance
            // `TempDir` keeps managed materialization inside a sandbox AND
            // sweeps it when the state drops; an un-owned path under the system
            // temp dir accumulated a git repository per test run. A test that
            // wants to *inspect* the tree calls `with_workspace_root`.
            workspace_root: workspace_root_sandbox.path().to_path_buf(),
            workspace_root_guard: Some(workspace_root_sandbox),
            task_budget_default,
            events,
            daemon,
            terminal_renderer,
            plugin,
            codex,
            // Fresh UUID per `AppState` — same boot-scoped semantics as
            // `AppState::new`. Each integration test gets its own id,
            // which is the right behavior: two tests sharing one binary
            // are conceptually two server "boots".
            db_instance_id: Arc::new(uuid::Uuid::new_v4().to_string()),
            card_role_cache,
            track_area_cache,
            card_kind_registry,
            dispatcher,
            // `from_parts` is the test / replay-lib hatch — no live MCP
            // server. Production goes through `new` below.
            mcp_server: None,
            harness,
            shared_codex_appserver,
            pending_codex_threads,
            pending_codex_threads_spawn_serial,
            operation_runtime,
            worker_flow,
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

    #[cfg(feature = "fixtures")]
    fn rebuild_operation_runtime(&mut self) {
        let route_repo: Arc<dyn RouteRepo> = self.raw.clone();
        let operation_repo =
            Arc::new(SqlxOperationRepo::new(self.raw.sqlite_pool().expect(
                "OperationRuntime rebuild requires a sqlite-backed Repo",
            )));
        let adapters = build_operation_adapters(OperationAdapterInputs {
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
        self.route.operation_runtime = runtime;
    }

    pub fn card_kind_registry(&self) -> &CardKindRegistry {
        &self.card_kind_registry
    }

    pub fn write(&self) -> &WriteContext {
        &self.route.write
    }

    /// Real boot-time constructor. Loads the plugin manifest registry from
    /// `cfg.plugins_dir`, creating the directory if it doesn't exist (fresh
    /// install path), wires up `DaemonClient` + `EventBus` + `PluginHost`,
    /// and auto-spawns every enabled plugin via `PluginHost::autospawn_enabled`.
    ///
    /// If the registry load returns an error we surface it: `load_from_dir`
    /// only errors when `plugins_dir` itself cannot be read, which is a hard
    /// misconfiguration the operator needs to fix. Per-plugin failures — a
    /// broken manifest, an unresolvable entry, or a second entry claiming an
    /// id already loaded (#1168) — are downgraded to `tracing::warn!` plus a
    /// `LoadReport::skipped` entry so one broken plugin can't block boot; the
    /// count below is a summary, the per-entry detail is in those warnings.
    /// Shared CODEX_HOME seeding stays here because it is colocated with the
    /// CodexClient owner and `AppState::new` is the boot-time-only path.
    pub async fn new(cfg: &Config, repo: Arc<dyn Repo>) -> anyhow::Result<Self> {
        let plugins_dir = cfg.plugins_dir_resolved();
        if !plugins_dir.exists() {
            // Fresh-install path: a missing dir is normal on first boot. We
            // create it so that subsequent installs (Slice D) have a target.
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

        // #1147 D2 — the managed workspace root. Created at boot for the same
        // reason the plugin dirs are: the first track create should not be the
        // thing that discovers the parent is unwritable.
        let workspace_root = cfg.workspace_root_resolved();
        if !workspace_root.exists() {
            tracing::info!(
                workspace_root = %workspace_root.display(),
                "creating managed workspace root"
            );
            std::fs::create_dir_all(&workspace_root)?;
        }
        // #1147 D3 contract (2) — canonicalize ONCE, at boot. Every downstream
        // prefix comparison (materialization's containment assertion, S5's
        // recycle guard) is only sound against a canonical root; comparing a
        // canonical path against a root that still contains a symlink or a
        // `..` would reject correct workspaces and, worse, could accept
        // incorrect ones.
        let workspace_root = std::fs::canonicalize(&workspace_root).map_err(|error| {
            anyhow::anyhow!(
                "canonicalize managed workspace root {}: {error}",
                workspace_root.display()
            )
        })?;

        // Same treatment for the data dir — Slice B/C will write into per-plugin
        // subdirs of this, so make sure the root exists at boot.
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

        // PR3 (#136) — boot-time role cache. Seed from the cards table
        // *after* migrations have run (which `SqlxRepo::open` did) and
        // *before* any background task is spawned, so the FSM projector
        // / sweeper / plugin host all see the same cache state the first
        // REST write will. Cache is clone-cheap; we stash one clone on
        // `AppState` and hand the FSM/sweeper their own clones —
        // `Arc<DashMap<…>>` under the hood, so it's the same underlying
        // map.
        let card_role_cache = CardRoleCache::new();
        repo.seed_card_role_cache(&card_role_cache).await?;
        // #234 — boot-time track→area cache. Same seed-then-spawn order
        // as the role cache: every background task that runs the role
        // gate downstream (FSM, sweeper, dispatcher, plugin host, MCP
        // server) needs both caches populated before it can authorize
        // a write.
        let track_area_cache = TrackAreaCache::new();
        repo.seed_track_area_cache(&track_area_cache).await?;
        let card_kind_registry = Arc::new(CardKindRegistry::builtins());
        let write = WriteContext::new(card_role_cache.clone(), track_area_cache.clone());

        // Per-card FSM (phase 1: codex cards only). Subscribes to the bus
        // and projects `codex.hook` events onto a 6-state FSM, writing
        // `Overlay { kind: "status" }` rows for cards and track-union rows
        // for tracks. See `card_fsm` module docs for the scope rationale.
        crate::card_fsm::spawn(repo.clone(), events.clone(), write.clone());

        // Share one `DaemonClient` + `CodexClient` between the
        // dispatcher and the `AppState` fields — both are
        // construction-cheap, but a single instance keeps the
        // resolved-binary state consistent (the codex bin path
        // resolution writes its result into the struct, so two
        // instances could diverge if `current_exe()` shifts between
        // calls, which is a no-op today but unnecessary risk).
        let daemon = Arc::new(DaemonClient::new(cfg));
        let codex = Arc::new(CodexClient::new(cfg));
        if let Err(e) = codex.shared_codex_home.seed() {
            tracing::warn!(
                error = %e,
                "shared CODEX_HOME seed failed; continuing; legacy per-card homes still functional"
            );
        }
        // #863 one-time boot repair for historically-seeded homes: strip
        // unexpected `[mcp_servers.*]` / `hooks` from config.toml and delete
        // a leaked `.env`. Runs after seed and before ensure_daemon_mcp_config
        // / the shared-daemon boot guard, so a legacy verbatim-seeded home
        // converges without operator action. On failure the launch-time guard
        // refuses the shared daemon; calm-server itself stays up.
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

        // PR7a (#136) — boot the kernel-as-MCP-server. Socket lives at
        // `<data_dir>/mcp/kernel.sock`; `neige-mcp-stdio-shim` is the
        // bridge binary the codex daemon launches per session. We
        // build the tool registry now (emit + track-state + track-report
        // tools) and let `McpServer::spawn` own the listener task. Boot
        // failure surfaces as a hard
        // anyhow error — no MCP server means planner / worker cards
        // can't emit events, which would silently break the track
        // FSM. The operator deserves a clear boot-time failure.
        //
        // PR7a.1 (#136 followup) — moved up before `Dispatcher::spawn`
        // so the dispatcher can take an `Arc<McpServer>` at construction
        // time and use it for worker codex daemon spawn (mirrors the
        // planner card path in `routes::tracks::create_track`).
        let mcp_socket_path =
            crate::mcp_server::transport::default_socket_path(&cfg.data_dir_resolved());
        let mcp_shim_bin = resolve_mcp_stdio_shim_bin(cfg);
        let mcp_registry = crate::mcp_server::build_default_registry();
        let daemon_mcp_token =
            crate::mcp_server::auth::get_or_generate_daemon_token(&cfg.data_dir_resolved())?;
        let daemon_mcp_token_hash = crate::mcp_server::auth::hash_token(&daemon_mcp_token);
        let plugin_host_cell = Arc::new(tokio::sync::OnceCell::new());
        let operation_runtime_cell = Arc::new(tokio::sync::OnceCell::new());
        // Issue #644 PR-C (PR #685 F3) — ONE resolution of the gate-logs
        // dir, shared by the gate runner (TaskVerifyAdapter below) and
        // the MCP `plan/<key>/gate.log` view, so a `--data-dir` CLI flag
        // without `CALM_DATA_DIR` cannot split writer and reader.
        let gate_logs_dir = cfg.data_dir_resolved().join("gate-logs");
        let mcp_server = crate::mcp_server::McpServer::spawn(
            repo.clone(),
            events.clone(),
            write.clone(),
            mcp_socket_path,
            mcp_shim_bin,
            mcp_registry,
            Some(daemon_mcp_token_hash),
            plugin_host_cell.clone(),
            operation_runtime_cell.clone(),
            gate_logs_dir.clone(),
            task_budget_default,
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
        let harness = HarnessRegistry::new();
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
        let shared_codex_appserver = SharedCodexAppServer::new_with_pending(
            cfg,
            codex.shared_codex_home.clone(),
            repo.clone(),
            Some(pending_codex_threads.clone()),
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

        // PR5 (#136) — dispatcher worker. Subscribes to
        // `*.worker_requested` envelopes and starts worker operations
        // (Cap: `NEIGE_DISPATCHER_PERMITS` env override, default 8).
        // Spawned here (between role-cache seed and plugin autospawn)
        // so the bus has at least one *.Requested-aware listener
        // before plugins start emitting; the role cache is already
        // seeded so the dispatcher's `card_create_with_id_tx` write-
        // through into the cache sees the seeded state.
        let dispatcher = Arc::new(
            crate::dispatcher::Dispatcher::spawn_with_terminal_renderer_and_harness_and_operation_runtime(
                repo.clone(),
                events.clone(),
                write.clone(),
                codex.clone(),
                daemon.clone(),
                terminal_renderer.clone(),
                // PR7a.1 — hand the MCP server handle to the dispatcher so
                // worker codex spawns can join the same MCP wire the planner
                // card uses.
                Some(mcp_server.clone()),
                harness.clone(),
                shared_codex_appserver.clone(),
                operation_runtime.clone(),
                crate::dispatcher::Dispatcher::permits_from_env(8),
                task_budget_default,
            ),
        );

        // Auto-spawn every enabled plugin row. Per-plugin errors are logged
        // inside `autospawn_enabled`; we never let one broken plugin block
        // the rest of the boot path.
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
            // See struct doc for `db_instance_id`: one fresh UUID v4 per
            // process boot. `AppState::new` is called exactly once from
            // `main.rs`, so this is the boot-scoped id the rest of the
            // server hands out via `/api/version`.
            db_instance_id: Arc::new(uuid::Uuid::new_v4().to_string()),
            card_role_cache,
            track_area_cache,
            card_kind_registry,
            dispatcher,
            mcp_server: Some(mcp_server),
            harness,
            shared_codex_appserver,
            pending_codex_threads,
            pending_codex_threads_spawn_serial,
            operation_runtime,
            worker_flow,
        };
        let state = state.into_app_state();

        // Orphan-terminal sweeper (Scope C). Ticks every 30s, reaps
        // terminal rows whose card has no active worker session (with a
        // 1-minute grace window), and emits `Event::TerminalDeleted`
        // through the same `write_with_event`
        // pipeline every other write uses so the cleanup is audited. See
        // `terminal_sweeper` module docs and `docs/sync-engine-design.md` §10.
        crate::terminal_sweeper::spawn(state.clone());

        // Track VCS objects are content-addressed and can be shared by multiple
        // tracks, so track/area deletion only removes refs + commits. Reclaim
        // unreferenced objects on a slower hourly cadence with a one-hour
        // grace window; see `track_vcs::sweep_unreferenced_objects_once`.
        if let Some(pool) = state.raw.sqlite_pool() {
            crate::track_vcs::spawn_unreferenced_object_sweeper(pool.clone());
            crate::track_vcs::spawn_track_history_pruner(pool.clone());
            // Events retention pruner (#854 slice 2). Allowlist-only,
            // age-horizoned, keep-latest overlay carve-out; see
            // `calm_truth::events_prune` module docs and
            // `docs/events-retention.md`.
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
