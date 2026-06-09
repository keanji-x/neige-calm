//! Shared app state passed to every handler.
//!
//! `Clone` is cheap — everything inside is wrapped in `Arc` or already
//! reference-counted internally.

use crate::aspect::AspectRegistry;
use crate::card_kind::CardKindRegistry;
use crate::card_role_cache::CardRoleCache;
use crate::config::Config;
use crate::db::{Repo, RouteRepo};
use crate::dispatcher::Dispatcher;
use crate::event::EventBus;
use crate::harness::HarnessRegistry;
use crate::ids::{CardId, CoveId, WaveId};
use crate::mcp_server::McpServer;
use crate::model::CardRole;
use crate::operation::claude_adapter::ClaudeAdapter;
use crate::operation::codex_adapter::CodexAdapter;
use crate::operation::spec_harness_interrupt_adapter::SpecHarnessInterruptAdapter;
use crate::operation::spec_harness_shutdown_adapter::SpecHarnessShutdownAdapter;
use crate::operation::spec_harness_start_adapter::SpecHarnessStartAdapter;
use crate::operation::terminal_adapter::TerminalAdapter;
use crate::operation::{OperationRuntime, SpawnCtx, SqlxOperationRepo};
use crate::pending_codex_threads::{PendingThreadStartRegistry, spawn_periodic_expire_task};
use crate::plugin_host::{PluginHost, PluginRegistry};
use crate::shared_codex_appserver::SharedCodexAppServer;
use crate::shared_codex_home::SharedCodexHome;
use crate::terminal_renderer::TerminalRendererRegistry;
use crate::wave_cove_cache::WaveCoveCache;
use axum::extract::FromRef;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::sync::Mutex;

const HOOK_INGEST_CACHE_CAPACITY: usize = 4096;

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
#[derive(Clone)]
pub struct WriteContext {
    role_cache: CardRoleCache,
    cove_cache: WaveCoveCache,
}

impl WriteContext {
    pub fn new(role_cache: CardRoleCache, cove_cache: WaveCoveCache) -> Self {
        Self {
            role_cache,
            cove_cache,
        }
    }

    /// #480 PR3a — cluster check: card role lookup. None if card unknown.
    pub fn verify_role(&self, card_id: &CardId) -> Option<CardRole> {
        self.role_cache.get(card_id)
    }

    /// #480 PR3a — cluster check: wave's home cove. None if wave unknown.
    pub fn verify_cove(&self, wave_id: &WaveId) -> Option<CoveId> {
        self.cove_cache.cove_of(wave_id)
    }

    #[deprecated(
        since = "0.1.0",
        note = "use WriteContext::verify_role / verify_cove (or pass the WriteContext to write_with_event_typed) — raw getters survive only for legacy db chain glue"
    )]
    pub fn role_cache(&self) -> &CardRoleCache {
        &self.role_cache
    }

    #[deprecated(
        since = "0.1.0",
        note = "use WriteContext::verify_role / verify_cove (or pass the WriteContext to write_with_event_typed) — raw getters survive only for legacy db chain glue"
    )]
    pub fn cove_cache(&self) -> &WaveCoveCache {
        &self.cove_cache
    }
}

/// #480 PR1 route-facing state slice for future handler extraction.
/// Mirrors existing `AppState` handles without changing caller behavior.
#[derive(Clone)]
pub struct RouteState {
    pub repo: Arc<dyn RouteRepo>,
    pub events: EventBus,
    pub db_instance_id: Arc<String>,
    pub write: WriteContext,
    pub aspects: Arc<AspectRegistry>,
    pub operation_runtime: Arc<OperationRuntime>,
    pub(crate) hook_ingest_cache: Arc<StdMutex<HookIngestCache>>,
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
    pub events: EventBus,
    pub daemon: Arc<DaemonClient>,
    pub terminal_renderer: Arc<TerminalRendererRegistry>,
    pub plugin: Arc<PluginHost>,
    pub codex: Arc<CodexClient>,
    pub db_instance_id: Arc<String>,
    pub card_role_cache: CardRoleCache,
    pub wave_cove_cache: WaveCoveCache,
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
    pub aspects: Arc<AspectRegistry>,
    pub operation_runtime: Arc<OperationRuntime>,
}

impl BootState {
    pub fn into_app_state(self) -> AppState {
        let route_repo: Arc<dyn RouteRepo> = self.repo.clone();
        let write = WriteContext::new(self.card_role_cache.clone(), self.wave_cove_cache.clone());
        let hook_ingest_cache = Arc::new(StdMutex::new(HookIngestCache::new(
            HOOK_INGEST_CACHE_CAPACITY,
        )));
        let route = RouteState {
            repo: route_repo.clone(),
            events: self.events.clone(),
            db_instance_id: self.db_instance_id.clone(),
            write: write.clone(),
            aspects: self.aspects.clone(),
            operation_runtime: self.operation_runtime.clone(),
            hook_ingest_cache,
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
            daemon: self.daemon,
            terminal_renderer: self.terminal_renderer,
            plugin: self.plugin,
            codex: self.codex,
            db_instance_id: self.db_instance_id,
            card_role_cache: self.card_role_cache,
            wave_cove_cache: self.wave_cove_cache,
            card_kind_registry: self.card_kind_registry,
            dispatcher: self.dispatcher,
            mcp_server: self.mcp_server,
            harness: self.harness,
            shared_codex_appserver: self.shared_codex_appserver,
            pending_codex_threads: self.pending_codex_threads,
            pending_codex_threads_spawn_serial: self.pending_codex_threads_spawn_serial,
            aspects: self.aspects,
            operation_runtime: self.operation_runtime,
            raw: self.repo,
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
    /// Sync-domain raw writes (`cove_create`, `wave_update`, `card_delete`,
    /// `overlay_upsert`, etc.) are unreachable from this handle — handlers
    /// must funnel them through `db::write_with_event_typed`.
    pub repo: Arc<dyn RouteRepo>,
    pub events: EventBus,
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
    /// PR3 (#136) — `CardId -> CardRole` cache used by `role_gate::enforce_role`
    /// at every audited write entry. Clone-cheap (`Arc<DashMap<…>>` inside).
    /// Production builds seed this from the cards table during
    /// [`AppState::new`]; tests construct an empty cache via
    /// [`AppState::from_parts`] when they don't need role-gating coverage,
    /// or pre-populate it manually otherwise. The cache is also threaded
    /// into every `_tx`-suffixed card helper so the insert/delete path
    /// stays write-through inside the surrounding transaction.
    pub card_role_cache: CardRoleCache,
    /// #234 — `WaveId -> CoveId` cache the role gate consults alongside
    /// `card_role_cache` to cross-check `scope.cove` against a Worker
    /// card's home cove. Mirrors the shape + clone semantics of
    /// `card_role_cache`. Production builds seed this from the waves
    /// table in [`AppState::new`]; tests use the empty default via
    /// [`AppState::from_parts`] or pre-populate it manually.
    pub wave_cove_cache: WaveCoveCache,
    /// #477 PR5 — registry of kernel-owned card kind handlers. Unknown card
    /// kinds stay opaque; built-ins expose validation + metadata for future
    /// OpenAPI / metrics readers.
    pub card_kind_registry: Arc<CardKindRegistry>,
    /// PR5 (#136) — dispatcher worker handle. Subscribes via
    /// [`EventBus::subscribe_filtered`] to `*.job_requested` envelopes
    /// and mints worker-roled cards (+ optionally spawns the codex /
    /// session daemon) for each, gated by a global semaphore (default
    /// 8 permits, override via `NEIGE_DISPATCHER_PERMITS`). Held as
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
    /// #322 — aspect / join-point framework registry. `Arc` so route
    /// handlers, the dispatcher, and any future aspect-enforcing callsite
    /// share one registry without re-installing aspects per request. The set
    /// of aspects is fixed at boot — no runtime mutation.
    pub aspects: Arc<AspectRegistry>,
    pub operation_runtime: Arc<OperationRuntime>,
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
    route: RouteState,
    worker: WorkerState,
    codex_shell: CodexShellState,
}

/// #322 — boot-time aspect registration. The single source of truth for
/// "which aspects ship in the kernel" — both [`AppState::new`] (production)
/// and [`AppState::from_parts`] (tests / replay lib) go through this so a
/// new aspect lands on every code path that constructs an `AppState`.
fn build_aspect_registry() -> Arc<AspectRegistry> {
    Arc::new(AspectRegistry::new())
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

    pub async fn recover_harnesses_on_boot(&self) -> crate::error::Result<usize> {
        crate::harness::recover_harnesses_on_boot(
            self.raw.clone(),
            self.events.clone(),
            self.card_role_cache.clone(),
            self.wave_cove_cache.clone(),
            self.shared_codex_appserver.clone(),
            &self.harness,
        )
        .await
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
    /// #234: `wave_cove_cache` follows the same shape — `None` yields
    /// an empty cache. Tests that exercise the Worker cove-cross-check
    /// pre-populate the cache via `WaveCoveCache::insert` before
    /// calling this. Most existing tests don't touch the Worker path,
    /// so an empty cache is fine.
    pub fn from_parts(
        repo: Arc<dyn Repo>,
        events: EventBus,
        daemon: Arc<DaemonClient>,
        plugin: Arc<PluginHost>,
        codex: Arc<CodexClient>,
        card_role_cache: Option<CardRoleCache>,
        wave_cove_cache: Option<WaveCoveCache>,
    ) -> Self {
        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
        let card_role_cache = card_role_cache.unwrap_or_default();
        let wave_cove_cache = wave_cove_cache.unwrap_or_default();
        let harness = HarnessRegistry::new();
        let pending_codex_threads = Arc::new(PendingThreadStartRegistry::new(
            repo.clone(),
            events.clone(),
        ));
        let pending_codex_threads_spawn_serial = Arc::new(Mutex::new(()));
        let shared_codex_appserver = SharedCodexAppServer::new_stub(repo.clone());
        let operation_repo = Arc::new(SqlxOperationRepo::new(
            repo.sqlite_pool()
                .expect("AppState::from_parts requires a sqlite-backed Repo"),
        ));
        let terminal_adapter = Arc::new(TerminalAdapter::new(
            route_repo.clone(),
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        ));
        let codex_adapter = Arc::new(CodexAdapter::new(
            route_repo.clone(),
            codex.clone(),
            shared_codex_appserver.clone(),
            pending_codex_threads.clone(),
            pending_codex_threads_spawn_serial.clone(),
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        ));
        let claude_adapter = Arc::new(ClaudeAdapter::new(
            route_repo.clone(),
            codex.clone(),
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        ));
        let spec_harness_start_adapter = Arc::new(SpecHarnessStartAdapter::new(
            repo.clone(),
            shared_codex_appserver.clone(),
            harness.clone(),
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        ));
        let spec_harness_interrupt_adapter =
            Arc::new(SpecHarnessInterruptAdapter::new(harness.clone()));
        let spec_harness_shutdown_adapter = Arc::new(SpecHarnessShutdownAdapter::new(
            harness.clone(),
            shared_codex_appserver.clone(),
            repo.clone(),
        ));
        let operation_runtime = Arc::new(OperationRuntime::new_unchecked(
            operation_repo,
            vec![
                terminal_adapter,
                codex_adapter,
                claude_adapter,
                spec_harness_start_adapter,
                spec_harness_interrupt_adapter,
                spec_harness_shutdown_adapter,
            ],
            events.clone(),
            SpawnCtx::new(
                route_repo.clone(),
                daemon.clone(),
                terminal_renderer.clone(),
                events.clone(),
            ),
        ));
        let card_kind_registry = Arc::new(CardKindRegistry::builtins());
        let write = WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone());
        // PR5 (#136): every `AppState` carries a live dispatcher. Test
        // call sites that need to assert on dispatcher behavior reach
        // through `state.dispatcher`; the rest see a passive worker
        // that's silent until something emits a `*.job_requested`
        // event. Permit count honors `NEIGE_DISPATCHER_PERMITS` for
        // the rare test that twiddles the env var; the default 8 is
        // the value tests will see otherwise.
        let dispatcher = Arc::new(Dispatcher::spawn_with_terminal_renderer_and_harness(
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
            Dispatcher::permits_from_env(8),
        ));
        BootState {
            repo,
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
            wave_cove_cache,
            card_kind_registry,
            dispatcher,
            // `from_parts` is the test / replay-lib hatch — no live MCP
            // server. Production goes through `new` below.
            mcp_server: None,
            harness,
            shared_codex_appserver,
            pending_codex_threads,
            pending_codex_threads_spawn_serial,
            aspects: build_aspect_registry(),
            operation_runtime,
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
        self.rebuild_fixture_operation_runtime();
        self
    }

    #[cfg(feature = "fixtures")]
    pub fn with_pending_codex_threads(mut self, pending: Arc<PendingThreadStartRegistry>) -> Self {
        self.pending_codex_threads = pending.clone();
        self.codex_shell.pending_codex_threads = pending;
        self.rebuild_fixture_operation_runtime();
        self
    }

    #[cfg(feature = "fixtures")]
    fn rebuild_fixture_operation_runtime(&mut self) {
        let route_repo: Arc<dyn RouteRepo> = self.raw.clone();
        let operation_repo =
            Arc::new(SqlxOperationRepo::new(self.raw.sqlite_pool().expect(
                "fixture OperationRuntime requires a sqlite-backed Repo",
            )));
        let terminal_adapter = Arc::new(TerminalAdapter::new(
            route_repo.clone(),
            self.card_role_cache.clone(),
            self.wave_cove_cache.clone(),
        ));
        let codex_adapter = Arc::new(CodexAdapter::new(
            route_repo.clone(),
            self.codex.clone(),
            self.shared_codex_appserver.clone(),
            self.pending_codex_threads.clone(),
            self.pending_codex_threads_spawn_serial.clone(),
            self.card_role_cache.clone(),
            self.wave_cove_cache.clone(),
        ));
        let claude_adapter = Arc::new(ClaudeAdapter::new(
            route_repo.clone(),
            self.codex.clone(),
            self.card_role_cache.clone(),
            self.wave_cove_cache.clone(),
        ));
        let spec_harness_start_adapter = Arc::new(SpecHarnessStartAdapter::new(
            self.raw.clone(),
            self.shared_codex_appserver.clone(),
            self.harness.clone(),
            self.card_role_cache.clone(),
            self.wave_cove_cache.clone(),
        ));
        let spec_harness_interrupt_adapter =
            Arc::new(SpecHarnessInterruptAdapter::new(self.harness.clone()));
        let spec_harness_shutdown_adapter = Arc::new(SpecHarnessShutdownAdapter::new(
            self.harness.clone(),
            self.shared_codex_appserver.clone(),
            self.raw.clone(),
        ));
        let runtime = Arc::new(OperationRuntime::new_unchecked(
            operation_repo,
            vec![
                terminal_adapter,
                codex_adapter,
                claude_adapter,
                spec_harness_start_adapter,
                spec_harness_interrupt_adapter,
                spec_harness_shutdown_adapter,
            ],
            self.events.clone(),
            SpawnCtx::new(
                route_repo,
                self.daemon.clone(),
                self.terminal_renderer.clone(),
                self.events.clone(),
            ),
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
    /// If the registry load returns an error (e.g. duplicate plugin id) we
    /// surface it: that's a hard misconfiguration the operator needs to fix.
    /// Per-plugin parse failures (and per-plugin spawn failures) are already
    /// downgraded to `tracing::warn!` so one broken plugin can't block boot.
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
        // #234 — boot-time wave→cove cache. Same seed-then-spawn order
        // as the role cache: every background task that runs the role
        // gate downstream (FSM, sweeper, dispatcher, plugin host, MCP
        // server) needs both caches populated before it can authorize
        // a write.
        let wave_cove_cache = WaveCoveCache::new();
        repo.seed_wave_cove_cache(&wave_cove_cache).await?;
        let card_kind_registry = Arc::new(CardKindRegistry::builtins());
        let write = WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone());

        // Per-card FSM (phase 1: codex cards only). Subscribes to the bus
        // and projects `codex.hook` events onto a 6-state FSM, writing
        // `Overlay { kind: "status" }` rows for cards and wave-union rows
        // for waves. See `card_fsm` module docs for the scope rationale.
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

        // PR7a (#136) — boot the kernel-as-MCP-server. Socket lives at
        // `<data_dir>/mcp/kernel.sock`; `neige-mcp-stdio-shim` is the
        // bridge binary the codex daemon launches per session. We
        // build the tool registry now (emit + wave-state + wave-report
        // tools) and let `McpServer::spawn` own the listener task. Boot
        // failure surfaces as a hard
        // anyhow error — no MCP server means spec / worker cards
        // can't emit events, which would silently break the wave
        // FSM. The operator deserves a clear boot-time failure.
        //
        // PR7a.1 (#136 followup) — moved up before `Dispatcher::spawn`
        // so the dispatcher can take an `Arc<McpServer>` at construction
        // time and use it for worker codex daemon spawn (mirrors the
        // spec card path in `routes::waves::create_wave`).
        let mcp_socket_path = cfg.data_dir_resolved().join("mcp").join("kernel.sock");
        let mcp_shim_bin = resolve_mcp_stdio_shim_bin(cfg);
        let mcp_registry = crate::mcp_server::build_default_registry();
        let daemon_mcp_token =
            crate::mcp_server::auth::get_or_generate_daemon_token(&cfg.data_dir_resolved())?;
        let daemon_mcp_token_hash = crate::mcp_server::auth::hash_token(&daemon_mcp_token);
        let mcp_server = crate::mcp_server::McpServer::spawn(
            repo.clone(),
            events.clone(),
            write.clone(),
            mcp_socket_path,
            mcp_shim_bin,
            mcp_registry,
            Some(daemon_mcp_token_hash),
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
        let operation_repo = Arc::new(SqlxOperationRepo::new(
            repo.sqlite_pool()
                .ok_or_else(|| anyhow::anyhow!("OperationRuntime requires a sqlite-backed Repo"))?,
        ));
        let terminal_adapter = Arc::new(TerminalAdapter::new(
            route_repo.clone(),
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        ));
        let codex_adapter = Arc::new(CodexAdapter::new(
            route_repo.clone(),
            codex.clone(),
            shared_codex_appserver.clone(),
            pending_codex_threads.clone(),
            pending_codex_threads_spawn_serial.clone(),
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        ));
        let claude_adapter = Arc::new(ClaudeAdapter::new(
            route_repo.clone(),
            codex.clone(),
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        ));
        let spec_harness_start_adapter = Arc::new(SpecHarnessStartAdapter::new(
            repo.clone(),
            shared_codex_appserver.clone(),
            harness.clone(),
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        ));
        let spec_harness_interrupt_adapter =
            Arc::new(SpecHarnessInterruptAdapter::new(harness.clone()));
        let spec_harness_shutdown_adapter = Arc::new(SpecHarnessShutdownAdapter::new(
            harness.clone(),
            shared_codex_appserver.clone(),
            repo.clone(),
        ));
        let operation_runtime = Arc::new(
            OperationRuntime::new(
                operation_repo,
                vec![
                    terminal_adapter,
                    codex_adapter,
                    claude_adapter,
                    spec_harness_start_adapter,
                    spec_harness_interrupt_adapter,
                    spec_harness_shutdown_adapter,
                ],
                events.clone(),
                SpawnCtx::new(
                    route_repo.clone(),
                    daemon.clone(),
                    terminal_renderer.clone(),
                    events.clone(),
                ),
            )
            .await?,
        );

        // PR5 (#136) — dispatcher worker. Subscribes to
        // `*.job_requested` envelopes and mints worker-roled cards
        // (Cap: `NEIGE_DISPATCHER_PERMITS` env override, default 8).
        // Spawned here (between role-cache seed and plugin autospawn)
        // so the bus has at least one *.Requested-aware listener
        // before plugins start emitting; the role cache is already
        // seeded so the dispatcher's `card_create_with_id_tx` write-
        // through into the cache sees the seeded state.
        let dispatcher = Arc::new(
            crate::dispatcher::Dispatcher::spawn_with_terminal_renderer_and_harness(
                repo.clone(),
                events.clone(),
                write.clone(),
                codex.clone(),
                daemon.clone(),
                terminal_renderer.clone(),
                // PR7a.1 — hand the MCP server handle to the dispatcher so
                // worker codex spawns can join the same MCP wire the spec
                // card uses.
                Some(mcp_server.clone()),
                harness.clone(),
                shared_codex_appserver.clone(),
                crate::dispatcher::Dispatcher::permits_from_env(8),
            ),
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

        // Auto-spawn every enabled plugin row. Per-plugin errors are logged
        // inside `autospawn_enabled`; we never let one broken plugin block
        // the rest of the boot path.
        plugin.autospawn_enabled().await;

        let state = BootState {
            repo,
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
            wave_cove_cache,
            card_kind_registry,
            dispatcher,
            mcp_server: Some(mcp_server),
            harness,
            shared_codex_appserver,
            pending_codex_threads,
            pending_codex_threads_spawn_serial,
            aspects: build_aspect_registry(),
            operation_runtime,
        };
        let state = state.into_app_state();

        // Orphan-terminal sweeper (Scope C). Ticks every 30s, reaps
        // terminal rows that no active runtime references via
        // `runtimes.terminal_run_id` (with a 1-minute grace window), and
        // emits `Event::TerminalDeleted` through the same `write_with_event`
        // pipeline every other write uses so the cleanup is audited. See
        // `terminal_sweeper` module docs and `docs/sync-engine-design.md` §10.
        crate::terminal_sweeper::spawn(state.clone());

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

// ---------------------------------------------------------------------------
// DaemonClient — terminal support paths shared by renderer-backed flows.
// ---------------------------------------------------------------------------

/// Lightweight handle the REST + WS halves both consult. It owns the
/// per-terminal data paths and the optional proc-supervisor socket used by
/// renderer-backed sessions.
pub struct DaemonClient {
    /// Per-terminal sockets live under this directory as `<terminal_id>.sock`.
    /// Created on first use by `routes::terminal::create`. Defaults to
    /// `<config.data_dir>/terminals`.
    pub data_dir: PathBuf,
    /// Control socket for `calm-proc-supervisor`. Production config resolves
    /// this to `<CALM_DATA_DIR>/proc-supervisor.sock`; fixture tests may leave
    /// it unset to use an in-process framed supervisor.
    pub proc_supervisor_sock: Option<PathBuf>,
}

impl DaemonClient {
    /// Real constructor. Pulls terminal data paths from the resolved config.
    pub fn new(cfg: &Config) -> Self {
        let data_dir = cfg.data_dir_resolved().join("terminals");
        Self {
            data_dir,
            proc_supervisor_sock: Some(cfg.proc_supervisor_sock_resolved()),
        }
    }

    /// Placeholder for tests / dev paths that don't have a full `Config`.
    /// Sockets land in a per-uid tempdir.
    pub fn new_stub() -> Self {
        let tmp = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("calm-terminals");
        Self {
            data_dir: tmp,
            proc_supervisor_sock: None,
        }
    }

    /// Socket path for a given terminal id.
    pub fn sock_path(&self, terminal_id: &str) -> PathBuf {
        self.data_dir.join(format!("{terminal_id}.sock"))
    }

    /// PR3a (#293) — per-card directory for a spec card's `codex
    /// app-server` listen socket: `<data_dir>/appserver/<card_id>/`.
    ///
    /// **Must be user-owned**, NOT a bare sticky `/tmp` directory: the
    /// `codex app-server` `chmod 0700`s the socket's *parent* dir at boot
    /// and EPERMs if it can't (spike caveat #2). We hang it off the daemon
    /// data dir's parent (`self.data_dir` is `<data_dir>/terminals`, so
    /// `parent()` is the resolved `data_dir`, which is the user-owned
    /// `$HOME/.local/share/neige-calm` in production and a per-test
    /// tempdir under test). The 0700 chmod lands on this per-card subdir,
    /// **never** the shared `data_dir` itself. Falls back to `self.data_dir`
    /// only in the degenerate case where it has no parent.
    pub fn appserver_sock_dir(&self, card_id: &str) -> PathBuf {
        let base = self.data_dir.parent().unwrap_or(&self.data_dir);
        base.join("appserver").join(card_id)
    }

    /// PR3a (#293) — the `app.sock` path inside [`appserver_sock_dir`].
    /// Passed to `codex app-server --listen unix://<path>` (kernel side)
    /// and `codex resume <tid> --remote unix://<path>` (TUI side).
    pub fn appserver_sock_path(&self, card_id: &str) -> PathBuf {
        self.appserver_sock_dir(card_id).join("app.sock")
    }

    /// Kernel-private transient stdin injection. Routes directly through
    /// the in-process renderer's supervisor writer and waits for the
    /// matching InputAck generated from the supervisor WriteAck.
    pub async fn inject_stdin_renderer(
        &self,
        renderer: &TerminalRendererRegistry,
        terminal_id: &str,
        bytes: &[u8],
        timeout: Duration,
    ) -> anyhow::Result<()> {
        tokio::time::timeout(timeout, async move {
            let entry = renderer
                .get(terminal_id)
                .ok_or_else(|| anyhow::anyhow!("no live renderer for terminal {terminal_id}"))?;
            let (ack_tx, mut ack_rx) = tokio::sync::mpsc::unbounded_channel();
            entry
                .handle
                .supervisor_tx
                .send(crate::terminal_renderer::SupervisorControl::Write(
                    crate::terminal_renderer::PtyWrite {
                        data: bytes.to_vec(),
                        input_seq: 1,
                        ack: Some(ack_tx),
                    },
                ))
                .map_err(|_| anyhow::anyhow!("renderer supervisor writer is closed"))?;
            match ack_rx.recv().await {
                Some(calm_session::DaemonMsg::InputAck { input_seq: 1 }) => Ok(()),
                Some(other) => Err(anyhow::anyhow!(
                    "expected InputAck(1) from renderer, got {other:?}"
                )),
                None => Err(anyhow::anyhow!("renderer ack channel closed")),
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("inject_stdin to {terminal_id} timed out after {timeout:?}"))?
    }
}

// ---------------------------------------------------------------------------
// CodexClient — owned by Track Codex.
//
// Carries the codex CLI path, the hook bridge path, and the ingest URL.
// The actual spawn lives in `routes::codex_cards::create_codex_card`.
// ---------------------------------------------------------------------------

pub struct CodexClient {
    /// `codex` CLI to spawn. Defaults to `codex` (PATH lookup).
    pub codex_bin: String,
    /// `claude` CLI to spawn for manually-created Claude worker cards.
    /// Defaults to `claude` (PATH lookup).
    pub claude_bin: String,
    /// `neige-codex-bridge` binary path. The actual command codex invokes
    /// is `/usr/local/bin/neige-codex-bridge` (declared in
    /// `docker/codex-requirements.toml` as a policy-managed hook); this
    /// field records the canonical local path so the binary lookup at
    /// `cargo run` / packaging time picks up the workspace build. Resolved
    /// as a sibling of `calm-server` exe, falling back to bare name.
    pub bridge_bin: PathBuf,
    /// Loopback URL the bridge POSTs to (`http://127.0.0.1:<port>`).
    pub ingest_url: String,
    /// Per-card CODEX_HOME parent. Lives under `data_dir/codex-homes/`,
    /// which is `$HOME/.local/share/neige-calm/codex-homes/` by default
    /// — bind-mounted into the container, so it survives `docker compose
    /// down/up` and the codex card's auth.json + state stay alive across
    /// restarts. (The old `/tmp/`-based location was wiped on every
    /// container recreate, leaving the daemon stuck in a respawn loop.)
    pub codex_homes_dir: PathBuf,
    /// Single shared CODEX_HOME for the future shared Codex app-server.
    /// PR1 seeds/configures it only; legacy per-card callers keep using
    /// `codex_homes_dir` until later #410 PRs switch them.
    pub shared_codex_home: Arc<SharedCodexHome>,
    /// Parent directory for generated per-Claude-card `settings.json`
    /// files. This is only a hook settings sidecar, not a Claude home.
    pub claude_settings_dir: PathBuf,
    /// Test-only handle. When `new_stub()` constructs the client it stows
    /// a `tempfile::TempDir` here whose path contains both the legacy
    /// `codex_homes_dir` and PR1's shared `codex-home`.
    /// Holding the handle for the lifetime of the `CodexClient` (which
    /// is itself held inside `Arc<CodexClient>` on `AppState.codex`)
    /// guarantees the per-card `$CODEX_HOME` subdirs created under it
    /// get cleaned up when the test drops its `AppState` — closing the
    /// 134 GB-per-day leak described in issue #267 where the prior
    /// hardcoded `temp_dir().join("neige-codex-homes-stub")` shared one
    /// global dir across every test run.
    ///
    /// Production (`new`) leaves this `None`: `data_dir_resolved()` is a
    /// long-lived path that must survive the server process and the
    /// orchestration layer manages its lifecycle.
    _codex_homes_tempdir: Option<tempfile::TempDir>,
}

impl CodexClient {
    pub fn new(cfg: &Config) -> Self {
        let data_dir = cfg.data_dir_resolved();
        let legacy_homes_parent = data_dir.join("codex-homes");
        Self {
            codex_bin: cfg.codex_bin.clone(),
            claude_bin: cfg.claude_bin.clone(),
            bridge_bin: cfg
                .codex_bridge_bin
                .clone()
                .unwrap_or_else(resolve_codex_bridge_bin),
            ingest_url: cfg.codex_ingest_url_resolved(),
            codex_homes_dir: legacy_homes_parent.clone(),
            shared_codex_home: Arc::new(SharedCodexHome::new(
                data_dir.join("codex-home"),
                legacy_homes_parent,
            )),
            claude_settings_dir: data_dir.join("claude-settings"),
            _codex_homes_tempdir: None,
        }
    }

    /// Test stub — never actually spawns codex; tests that touch the
    /// codex routes don't need a real binary on PATH.
    ///
    /// **#267 — per-test temp dir for `codex_homes_dir`.** Earlier
    /// versions hardcoded the path to
    /// `std::env::temp_dir().join("neige-codex-homes-stub")`, a single
    /// global dir every test instance wrote into and nobody cleaned up
    /// — across enough test runs the dir grew to 100+ GB of codex
    /// session state (per-card `logs_*.sqlite*`, `history`, the seeded
    /// `~/.codex` copy). Now each `new_stub()` mints its own
    /// `tempfile::TempDir`, stashed in `_codex_homes_tempdir`, with
    /// `codex-homes/` for legacy per-card homes and `codex-home/` for
    /// PR1's shared home under it. The directory disappears when the
    /// `CodexClient` (and the `Arc` on `AppState.codex`) drops at test
    /// teardown. Falls back to the old shared path only if
    /// `TempDir::new()` fails — vanishingly rare in practice and the
    /// failure case isn't worth losing test coverage.
    pub fn new_stub() -> Self {
        let (temp_root, tmp) = match tempfile::Builder::new()
            .prefix("neige-codex-homes-stub-")
            .tempdir()
        {
            Ok(tmp) => (tmp.path().to_path_buf(), Some(tmp)),
            Err(e) => {
                // #272 (N2) — bumped from `warn!` to `error!`. This
                // fallback resurrects the pre-#267 shared `/tmp/neige-
                // codex-homes-stub` leak path; if it fires in CI the
                // test will silently revive the 134 GB/day leak fixed
                // by PR #271. `error!` is loud enough that triage
                // catches it on first occurrence instead of after the
                // next disk-full incident.
                tracing::error!(
                    error = %e,
                    "failed to create per-test codex_homes tempdir; \
                     falling back to shared `/tmp/neige-codex-homes-stub` \
                     — RESURRECTS THE #267 LEAK PATH (this test run will leak)"
                );
                (std::env::temp_dir().join("neige-codex-homes-stub"), None)
            }
        };
        let codex_homes_dir = temp_root.join("codex-homes");
        let shared_codex_home = Arc::new(SharedCodexHome::new(
            temp_root.join("codex-home"),
            codex_homes_dir.clone(),
        ));
        if let Err(e) = std::fs::create_dir_all(&codex_homes_dir) {
            tracing::error!(
                error = %e,
                path = %codex_homes_dir.display(),
                "failed to create stub codex_homes_dir"
            );
        }
        Self {
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            bridge_bin: PathBuf::from("neige-codex-bridge"),
            ingest_url: "http://127.0.0.1:0".into(),
            claude_settings_dir: codex_homes_dir.join("claude-settings"),
            codex_homes_dir,
            shared_codex_home,
            _codex_homes_tempdir: tmp,
        }
    }

    /// Shared CODEX_HOME accessor (`codex_home_dir()` in the #410 PR gates).
    pub fn codex_home_dir(&self) -> &Path {
        self.shared_codex_home.path()
    }
}

fn resolve_codex_bridge_bin() -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join("neige-codex-bridge");
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from("neige-codex-bridge")
}

/// PR7a (#136) — resolve the path to `neige-mcp-stdio-shim`. Same
/// "explicit override, sibling of running exe, else bare-name PATH lookup"
/// pattern as the codex-bridge resolver. The codex daemon will spawn this
/// binary from the path baked into each per-card `$CODEX_HOME/config.toml`'s
/// `[mcp_servers.calm].command` entry.
fn resolve_mcp_stdio_shim_bin(cfg: &Config) -> PathBuf {
    if let Some(path) = &cfg.mcp_stdio_shim_bin {
        return path.clone();
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join("neige-mcp-stdio-shim");
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from("neige-mcp-stdio-shim")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PR3a (#293) — the per-card app-server socket must land under the
    /// user-owned data dir (the `app-server` 0700-chmods the socket's
    /// parent, which EPERMs on a shared sticky /tmp), in a per-card subdir
    /// — NOT directly in the shared data dir.
    #[test]
    fn appserver_sock_path_is_under_user_owned_data_dir_per_card() {
        // Mirror production: data_dir = <data_dir>/terminals.
        let data_dir = PathBuf::from("/home/u/.local/share/neige-calm");
        let daemon = DaemonClient {
            data_dir: data_dir.join("terminals"),
            proc_supervisor_sock: None,
        };

        let dir = daemon.appserver_sock_dir("card-abc");
        let sock = daemon.appserver_sock_path("card-abc");

        // Per-card subdir under <data_dir>/appserver/<card_id>/.
        assert_eq!(dir, data_dir.join("appserver").join("card-abc"));
        assert_eq!(sock, dir.join("app.sock"));

        // The 0700 chmod lands on the per-card subdir, never the shared
        // data dir itself.
        assert_ne!(dir, data_dir);
        assert!(sock.starts_with(&data_dir));
        assert!(sock.starts_with(data_dir.join("appserver")));
        // Each card gets its own subdir.
        assert_ne!(
            daemon.appserver_sock_dir("card-abc"),
            daemon.appserver_sock_dir("card-xyz")
        );
    }
}
