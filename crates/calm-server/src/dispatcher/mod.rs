//! Dispatcher worker.
//!
//! Subscribes to task, report, hook, plan, and track events that drive
//! spec-harness push observations and scheduler pokes.
//!
//! Worker spawns are now owned by the plan scheduler: specs maintain
//! `calm.plan.*`, the scheduler emits `task.dispatched`, and the worker
//! adapters start `codex-worker` / `terminal-worker` operations from there.
//!
//! Terminal process cleanup remains a hard boundary owned by
//! `terminal_sweeper`; adapter compensation only mirrors the required
//! reap-before-delete ordering when undoing a failed worker operation.

use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use crate::db::{Repo, RouteRepo};
use crate::event::{
    BroadcastEnvelope, EditAuthor, Event, EventBus, SubscribeFilter, SubscribeScope,
};
use crate::event_cursor::EventCursorCache;
use crate::harness::{
    HarnessRegistry, HookKind as HarnessHookKind, Observation as HarnessObservation, PushLockGuard,
    is_harness_snapshot_value,
};
use crate::ids::{ActorId, CardId, TrackId};
use crate::model::CardRole;
use crate::operation::child_track_adapter::ChildTrackAdapter;
use crate::operation::claude_adapter::{ClaudeAdapter, ClaudeWorkerAdapter};
use crate::operation::claude_restart_adapter::ClaudeRestartAdapter;
use crate::operation::codex_adapter::{CodexAdapter, CodexWorkerAdapter};
use crate::operation::spec_harness_interrupt_adapter::SpecHarnessInterruptAdapter;
use crate::operation::spec_harness_shutdown_adapter::SpecHarnessShutdownAdapter;
use crate::operation::spec_harness_start_adapter::SpecHarnessStartAdapter;
use crate::operation::terminal_adapter::{TerminalAdapter, TerminalWorkerAdapter};
use crate::operation::{OperationCompletionBus, OperationRuntime, SpawnCtx, SqlxOperationRepo};
use crate::pending_codex_threads::PendingThreadStartRegistry;
use crate::plugin_host::{PluginHost, PluginRegistry};
use crate::provider_registry::WorkerProviderRegistry;
use crate::reaper::{DEFAULT_REAPER_RECONCILE_SECS, Reaper, reaper_disabled_from_env};
use crate::scheduler::{DEFAULT_RECONCILE_SECS, Scheduler, TerminalTaskHook};
use crate::session_projection_repo::WorkerSessionKind;
use crate::shared_codex_appserver::SharedCodexAppServer;
use crate::state::{CodexClient, DaemonClient, WriteContext};
use crate::task_context::TaskContextMonitor;
use crate::terminal_renderer::TerminalRendererRegistry;
use sha2::{Digest, Sha256};

pub(crate) use crate::db::sqlite::card_with_terminal_rollback_tx;

/// Default number of permits when `NEIGE_DISPATCHER_PERMITS` is unset /
/// invalid / `0`. Mirrors the v2 spec for issue #136.
const DEFAULT_PERMITS: usize = 8;

/// The report-edit authors that wake the spec agent, as a single source of
/// truth: `event_warrants_spec_push_with_role` reads this list, and the spec
/// system prompt renders it (`spec_card::render_system_prompt`), so the
/// prompt cannot drift away from dispatch behaviour. Spec/Kernel authors are
/// deliberately absent — the spec (or the kernel on its behalf) wrote those,
/// and pushing them back would loop.
pub(crate) const SPEC_WAKE_AUTHORS: &[EditAuthor] =
    &[EditAuthor::User, EditAuthor::Plugin, EditAuthor::Assistant];

fn supervisor_sock_for_provider_registry(daemon: &DaemonClient) -> PathBuf {
    daemon
        .proc_supervisor_sock
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("neige-reaper-missing-proc-supervisor.sock"))
}
pub(crate) fn event_warrants_spec_push(
    event: &Event,
    actor: &ActorId,
    write: &WriteContext,
) -> bool {
    event_warrants_spec_push_with_role(event, actor, |card_id| write.verify_role(card_id))
}

pub(crate) fn event_warrants_spec_push_with_role(
    event: &Event,
    actor: &ActorId,
    mut role_for_card: impl FnMut(&CardId) -> Option<CardRole>,
) -> bool {
    match event {
        Event::TaskCompleted { .. } | Event::TaskFailed { .. } => {
            !crate::track_lifecycle::actor_is_spec_author(actor)
        }
        // Issue #644 PR-C (§6.5) — the gate runner's verdict is always
        // pushed: it is kernel-only at the role gate (actor
        // `KernelDispatcher`), so no self-push loop is possible. For a
        // gated task this is the wake-up that replaces the suppressed
        // worker self-report (the gated-self-report consultation is a
        // tasks-row lookup and lives with the async callers — see
        // `is_gated_self_report`).
        Event::TaskGateResult { .. } => true,
        // Issue #955 §5.7 — plugin-authored report edits (the accept
        // transaction's Batch apply) wake the spec exactly like user
        // edits: the report is the spec's work product, and neither a
        // user nor a plugin edit was authored by the spec itself, so
        // no self-push loop is possible. Spec/Kernel authors stay
        // suppressed (the spec wrote those — or the kernel rewrote on
        // its behalf — and pushing them back would loop).
        //
        // #1189 §3.4 — `Assistant` joins that set for the same reason and
        // by explicit ruling: an assistant session is a *different*
        // session editing the spec's work product, so it cannot loop, and
        // leaving the spec unaware that its report changed under it is a
        // worse failure than one extra wake-up.
        Event::TrackReportEdited { author, .. } => SPEC_WAKE_AUTHORS.contains(author),
        Event::WorkspaceLeased { .. } | Event::WorkspaceReleased { .. } => true,
        Event::ForgePrMerged { .. }
        | Event::ReviewRound { .. }
        | Event::RatifyRequested { .. }
        | Event::RatifyResolved { .. }
        | Event::ForgeScanCompleted { .. }
        | Event::ForgePrOpened { .. }
        | Event::ForgePrChecks { .. }
        | Event::ForgeIssueClosed { .. }
        | Event::WorktreeProvisioned { .. }
        | Event::WorktreeCommitted { .. } => true,
        Event::CodexHook { card_id, kind, .. } | Event::ClaudeHook { card_id, kind, .. } => {
            let is_turn_end = kind == "hook.codex.stop" || kind == "hook.claude.stop";
            let is_worker = role_for_card(card_id) == Some(CardRole::Worker);
            is_turn_end && is_worker
        }
        Event::AreaUpdated(_)
        | Event::AreaDeleted { .. }
        | Event::TrackUpdated(_)
        | Event::TrackDeleted { .. }
        | Event::TrackLifecycleChanged { .. }
        | Event::CardAdded(_)
        | Event::CardUpdated(_)
        | Event::CardDeleted { .. }
        | Event::RuntimeStarted { .. }
        | Event::RuntimeStatusChanged { .. }
        | Event::RuntimeSuperseded { .. }
        | Event::HarnessItemAdded { .. }
        | Event::HarnessPhaseChanged { .. }
        | Event::HarnessTranscriptCleared { .. }
        | Event::HarnessUserMessageEnqueued { .. }
        | Event::OverlaySet(_)
        | Event::OverlayDeleted { .. }
        | Event::TerminalDeleted { .. }
        | Event::PluginState { .. }
        | Event::PluginToolRegistered { .. }
        | Event::CodexWorkerRequested { .. }
        | Event::TerminalWorkerRequested { .. }
        | Event::PlanUpdated { .. }
        | Event::TaskDispatched { .. }
        | Event::TaskContextFrozen { .. }
        | Event::TaskContextAdvanced { .. }
        | Event::ForgePrDiffRead { .. }
        | Event::ForgeIssueRead { .. }
        | Event::ProposalSubmitted { .. }
        | Event::ProposalResolved { .. }
        | Event::WorktreeRemoved { .. } => false,
    }
}

/// Issue #644 PR-C (§6.5) — the gated-self-report predicate shared by
/// the live push branch and the boot replay
/// (`harness::replay_harness_events_since`): a worker `task.completed`
/// whose idempotency key resolves to a tasks row **with `gate_json`
/// set** is not pushed — the spec hears the gate verdict
/// (`task.gate_result`), not the self-report. Deliberately NOT
/// status-based: a fast gate can flip the row terminal before this
/// read, and a status predicate would then push both.
///
/// Round-3 review F1 — a `task.failed` for a GATED row is suppressed
/// too UNLESS the failure actually landed on the row pre-gate
/// (`failed` + `worker-reported`/`spawn-failed`/`worker-timeout`, the
/// details the worker/kernel failure flip writes — design §6.5's "worker
/// `task.failed` pushes as today; no gate runs on failure"). Any
/// other row state means the gate already owns the task: a stale or
/// retried `calm.task.fail` against a `verifying` row (or one the
/// gate already decided — `done`, or `failed` with a `gate-*` detail)
/// is a claim that lost the race, and pushing it would let the worker
/// wake/mislead the spec instead of the machine `task.gate_result`.
///
/// Ungated tasks, non-task keys (legacy), and lookup errors
/// (fail-open: a spurious self-report push is benign; a silently lost
/// wake-up is not) all push as today.
pub(crate) async fn is_gated_self_report(repo: &dyn crate::db::Repo, event: &Event) -> bool {
    let (idempotency_key, is_failure) = match event {
        Event::TaskCompleted {
            idempotency_key, ..
        } => (idempotency_key, false),
        Event::TaskFailed {
            idempotency_key, ..
        } => (idempotency_key, true),
        _ => return false,
    };
    match repo.task_get(idempotency_key).await {
        Ok(Some(task)) => {
            if task.gate_json.is_none() {
                return false;
            }
            if !is_failure {
                return true;
            }
            // #1147 ① — `status_detail` may carry a `": <reason>"` tail
            // now; the vocabulary lives in the classifier prefix.
            let failure_landed_pre_gate = task.status == crate::model::TaskStatus::Failed
                && matches!(
                    task.status_detail
                        .as_deref()
                        .map(crate::db::sqlite::status_detail_class),
                    Some("worker-reported") | Some("spawn-failed") | Some("worker-timeout")
                );
            !failure_landed_pre_gate
        }
        Ok(None) => false,
        Err(e) => {
            tracing::warn!(
                idempotency_key = %idempotency_key,
                error = %e,
                "dispatcher push: gated-self-report lookup failed; pushing self-report (fail-open)"
            );
            false
        }
    }
}

fn empty_plugin_host_for_dispatcher_runtime(
    repo: Arc<dyn Repo>,
    events: EventBus,
    write: WriteContext,
) -> Arc<PluginHost> {
    let route_repo: Arc<dyn RouteRepo> = repo;
    Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        route_repo,
        PathBuf::new(),
        std::env::temp_dir().join("calm-dispatcher-plugins-data"),
        Vec::new(),
        events,
        write,
    ))
}

#[allow(deprecated, clippy::too_many_arguments)]
fn dispatcher_operation_runtime(
    repo: Arc<dyn Repo>,
    events: EventBus,
    write: WriteContext,
    codex: Arc<CodexClient>,
    daemon: Arc<DaemonClient>,
    terminal_renderer: Arc<TerminalRendererRegistry>,
    mcp_server: Option<Arc<crate::mcp_server::McpServer>>,
    shared_codex_appserver: Arc<SharedCodexAppServer>,
    harness: HarnessRegistry,
    plugin: Arc<PluginHost>,
    workspace_root: std::path::PathBuf,
) -> Arc<OperationRuntime> {
    let route_repo: Arc<dyn RouteRepo> = repo.clone();
    let operation_repo = Arc::new(SqlxOperationRepo::new(
        repo.sqlite_pool()
            .expect("Dispatcher operation runtime requires a sqlite-backed Repo"),
    ));
    let pending_codex_threads = Arc::new(PendingThreadStartRegistry::new(
        repo.clone(),
        events.clone(),
    ));
    let pending_codex_threads_spawn_serial = Arc::new(tokio::sync::Mutex::new(()));
    let terminal_adapter = Arc::new(TerminalAdapter::new(
        route_repo.clone(),
        write.role_cache().clone(),
        write.area_cache().clone(),
    ));
    let terminal_worker_adapter = Arc::new(TerminalWorkerAdapter::new(
        route_repo.clone(),
        write.role_cache().clone(),
        write.area_cache().clone(),
    ));
    let codex_adapter = Arc::new(CodexAdapter::new(
        route_repo.clone(),
        codex.clone(),
        shared_codex_appserver.clone(),
        pending_codex_threads.clone(),
        pending_codex_threads_spawn_serial,
        write.role_cache().clone(),
        write.area_cache().clone(),
    ));
    let mcp_socket_path = mcp_server
        .as_ref()
        .map(|s| s.shim_config.socket_path.clone());
    let codex_worker_adapter = Arc::new(CodexWorkerAdapter::new(
        route_repo.clone(),
        codex.clone(),
        shared_codex_appserver.clone(),
        mcp_server.clone(),
        write.role_cache().clone(),
        write.area_cache().clone(),
        workspace_root.clone(),
    ));
    let claude_adapter = Arc::new(ClaudeAdapter::new(
        route_repo.clone(),
        codex.clone(),
        write.role_cache().clone(),
        write.area_cache().clone(),
    ));
    let claude_worker_adapter = Arc::new(ClaudeWorkerAdapter::new(
        route_repo.clone(),
        codex.clone(),
        mcp_server.clone(),
        write.role_cache().clone(),
        write.area_cache().clone(),
    ));
    let claude_restart_adapter = Arc::new(ClaudeRestartAdapter::new(
        route_repo.clone(),
        codex,
        write.role_cache().clone(),
        write.area_cache().clone(),
    ));
    let spec_harness_start_adapter = Arc::new(SpecHarnessStartAdapter::new(
        repo.clone(),
        shared_codex_appserver.clone(),
        harness.clone(),
        plugin,
        write.role_cache().clone(),
        write.area_cache().clone(),
        mcp_socket_path,
    ));
    let spec_harness_interrupt_adapter =
        Arc::new(SpecHarnessInterruptAdapter::new(harness.clone()));
    let spec_harness_shutdown_adapter = Arc::new(SpecHarnessShutdownAdapter::new(
        harness,
        shared_codex_appserver.clone(),
        repo,
    ));
    let task_verify_adapter = Arc::new(
        crate::operation::task_verify_adapter::TaskVerifyAdapter::new(
            crate::operation::task_verify_adapter::TaskVerifyAdapter::default_gate_logs_dir(),
        ),
    );
    let forge_action_adapter =
        Arc::new(crate::operation::forge_action_adapter::ForgeActionAdapter::new());
    let child_track_adapter = Arc::new(ChildTrackAdapter::new(
        write.role_cache().clone(),
        write.area_cache().clone(),
        workspace_root.clone(),
    ));
    let completion = OperationCompletionBus::new();
    Arc::new(OperationRuntime::new_unchecked(
        operation_repo.clone(),
        vec![
            terminal_adapter,
            terminal_worker_adapter,
            codex_adapter,
            codex_worker_adapter,
            claude_adapter,
            claude_worker_adapter,
            claude_restart_adapter,
            spec_harness_start_adapter,
            spec_harness_interrupt_adapter,
            spec_harness_shutdown_adapter,
            task_verify_adapter,
            forge_action_adapter,
            child_track_adapter,
        ],
        events.clone(),
        completion.clone(),
        SpawnCtx::new(
            route_repo,
            operation_repo,
            daemon,
            terminal_renderer,
            events,
            completion,
        )
        .with_shared_codex_appserver(shared_codex_appserver.clone()),
    ))
}

/// Subscribed handle. Holding the [`Dispatcher`] keeps the spawned
/// task alive; dropping it closes the broadcast receiver's end (the
/// task exits cleanly on the next `Closed` recv).
///
/// Today nothing outside `AppState::new` reaches in here — the
/// dispatcher is fire-and-forget. We still hand back the struct so
/// `AppState` can store it as `Arc<Dispatcher>` (matching the
/// terminal_sweeper / card_fsm convention) and so tests can assert on
/// the configured permit count.
pub struct Dispatcher {
    semaphore: Arc<Semaphore>,
    /// Number of permits the semaphore was constructed with — surfaced
    /// for tests so they don't have to introspect `Semaphore` itself.
    permits: usize,
    /// Background task handle. Kept on the struct so future shutdown
    /// can `abort()` it; not used today (we let the broadcast `Closed`
    /// signal drive the loop down naturally).
    #[allow(dead_code)]
    handle: JoinHandle<()>,
    /// #313 problem #1 — catch-up reaches harness observation through
    /// this. Held as a strong `Arc` so the same instance the background
    /// task is consuming is the one [`Dispatcher::catch_up_push`] calls
    /// into; the background task also holds its own clone, so the
    /// dispatcher stays alive as long as either side does.
    inner: Arc<Inner>,
    /// Owns a dispatcher-local runtime while the dispatcher handle is alive.
    /// The background task only keeps a `Weak` so it cannot keep AppState
    /// resources alive after shutdown.
    #[allow(dead_code)]
    operation_runtime: Arc<OperationRuntime>,
    /// Issue #644 PR-B — the kernel task scheduler, owned here (the
    /// dispatcher construction site owns the operation runtime + event
    /// subscription loop, design §5). Exposed via
    /// [`Dispatcher::scheduler`] for the boot sweep and tests.
    scheduler: Arc<Scheduler>,
    context_monitor: Arc<TaskContextMonitor>,
    /// §5.1 liveness backstop — slow periodic reconcile sweep
    /// (`NEIGE_SCHEDULER_RECONCILE_SECS`, default 300). Held so a future
    /// shutdown can `abort()` it; runs for the process lifetime today,
    /// like `handle`.
    #[allow(dead_code)]
    reconcile_handle: JoinHandle<()>,
    /// #679 PR8a — observational worker-session liveness reaper.
    /// `None` when `NEIGE_REAPER_DISABLED` is set.
    #[allow(dead_code)]
    reaper_handle: Option<JoinHandle<()>>,
    /// #741 §1.3 — the durable codex worker-liveness feeder (OBSERVATIONAL).
    /// Push-feeds `worker_sessions.{last_activity_ms,last_thread_status}` from
    /// the daemon notification stream. `None` (not spawned) when the reaper is
    /// disabled, since nothing consumes the columns then. Held so a future
    /// shutdown can `abort()` it.
    #[allow(dead_code)]
    liveness_feeder_handle: Option<JoinHandle<()>>,
}

impl Dispatcher {
    /// Resolve the permit count from `NEIGE_DISPATCHER_PERMITS` (parsed
    /// as `usize`), falling back to [`DEFAULT_PERMITS`] when unset,
    /// empty, unparseable, or zero. Surfaced as a free helper so tests
    /// can verify the env-override logic without spawning a full
    /// dispatcher.
    pub fn permits_from_env(default: usize) -> usize {
        match std::env::var("NEIGE_DISPATCHER_PERMITS") {
            Ok(raw) => match raw.trim().parse::<usize>() {
                Ok(n) if n > 0 => n,
                _ => default,
            },
            Err(_) => default,
        }
    }

    /// Configured permit count. Exposed for assertions in tests.
    pub fn permits(&self) -> usize {
        self.permits
    }

    /// Test-only — read the current in-memory push cursor for a card.
    /// Used by harness catch-up tests to assert that delivered envelopes
    /// advance the push cursor.
    #[doc(hidden)]
    pub fn push_cursor_for_test(&self, spec_card_id: &CardId) -> i64 {
        self.inner.push_cursor.get(spec_card_id)
    }

    /// #313 problem #1 (catch-up) — replay an already-persisted
    /// `(envelope_id, scope, event)` through the dispatcher's push path,
    /// **without** going through the broadcast bus.
    ///
    /// Used by boot/recovery paths to catch a harness-backed spec runtime up
    /// with events that landed while the kernel was down. Reuses the same
    /// harness observation helper that live envelopes go through.
    ///
    /// `envelope_id` must be the real persisted `events.id` — the watermark
    /// dedup keys on it. If the caller hands the same `(id, event)` twice
    /// (e.g. via a redelivery on the bus right after catch-up), the second
    /// call is a no-op (it `<= cursor`); see the dedup invariant in
    /// `Inner::push_to_spec`.
    ///
    /// Track-scope-only: the live push path discards events without a track
    /// scope before they reach the observer; this helper preserves that
    /// invariant (caller filters to track-scoped events).
    pub async fn catch_up_push(
        &self,
        track_id: TrackId,
        event: crate::event::Event,
        envelope_id: i64,
    ) {
        Inner::observe_harness(&self.inner, track_id, &event, envelope_id).await;
    }

    /// Reference to the global semaphore. Exposed so tests can probe
    /// `available_permits()` to verify the cap.
    pub fn semaphore(&self) -> Arc<Semaphore> {
        Arc::clone(&self.semaphore)
    }

    /// Issue #644 PR-B — handle to the kernel task scheduler. Used by
    /// the boot sweep (`lib.rs::scheduler_sweep_on_boot`) and tests.
    pub fn scheduler(&self) -> Arc<Scheduler> {
        Arc::clone(&self.scheduler)
    }

    pub fn context_monitor(&self) -> Arc<TaskContextMonitor> {
        Arc::clone(&self.context_monitor)
    }

    /// Fixtures that exercise request handlers without scheduler behavior can
    /// stop only the event listener before emitting any events. The dispatcher
    /// handle remains available to satisfy `AppState`'s production-shaped
    /// state, while `PlanUpdated` cannot race the fixture's next request.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn abort_event_listener_for_test(&self) {
        self.handle.abort();
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub async fn reconcile_tick_for_test(&self) {
        self.inner.reconcile_once().await;
    }

    /// Spawn the dispatcher background task.
    ///
    /// `permits` configures the global concurrent-spawn cap. The
    /// production caller (`AppState::new`) uses
    /// [`Dispatcher::permits_from_env`]`(DEFAULT_PERMITS)` so the
    /// `NEIGE_DISPATCHER_PERMITS` env var stays the single dial.
    /// Tests inject an explicit count.
    ///
    /// The codex / daemon / renderer / MCP handles are threaded into the
    /// dispatcher-local operation runtime for compatibility callers. The
    /// dispatcher itself only keeps the operation runtime after construction.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        codex: Arc<CodexClient>,
        daemon: Arc<DaemonClient>,
        mcp_server: Option<Arc<crate::mcp_server::McpServer>>,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        workspace_root: std::path::PathBuf,
        permits: usize,
    ) -> Self {
        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo);
        Self::spawn_with_terminal_renderer(
            repo,
            events,
            write,
            codex,
            daemon,
            terminal_renderer,
            mcp_server,
            shared_codex_appserver,
            workspace_root,
            permits,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_operation_runtime(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        codex: Arc<CodexClient>,
        daemon: Arc<DaemonClient>,
        mcp_server: Option<Arc<crate::mcp_server::McpServer>>,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        operation_runtime: Arc<OperationRuntime>,
        permits: usize,
    ) -> Self {
        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo);
        Self::spawn_with_terminal_renderer_and_operation_runtime(
            repo,
            events,
            write,
            codex,
            daemon,
            terminal_renderer,
            mcp_server,
            shared_codex_appserver,
            operation_runtime,
            permits,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_terminal_renderer(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        codex: Arc<CodexClient>,
        daemon: Arc<DaemonClient>,
        terminal_renderer: Arc<TerminalRendererRegistry>,
        mcp_server: Option<Arc<crate::mcp_server::McpServer>>,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        workspace_root: std::path::PathBuf,
        permits: usize,
    ) -> Self {
        let plugin =
            empty_plugin_host_for_dispatcher_runtime(repo.clone(), events.clone(), write.clone());
        let operation_runtime = dispatcher_operation_runtime(
            repo.clone(),
            events.clone(),
            write.clone(),
            codex.clone(),
            daemon.clone(),
            terminal_renderer.clone(),
            mcp_server.clone(),
            shared_codex_appserver.clone(),
            HarnessRegistry::new(),
            plugin,
            workspace_root,
        );
        Self::spawn_with_terminal_renderer_and_harness_and_operation_runtime(
            repo,
            events,
            write,
            codex,
            daemon,
            terminal_renderer,
            mcp_server,
            HarnessRegistry::new(),
            shared_codex_appserver,
            operation_runtime,
            permits,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_terminal_renderer_and_operation_runtime(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        codex: Arc<CodexClient>,
        daemon: Arc<DaemonClient>,
        terminal_renderer: Arc<TerminalRendererRegistry>,
        mcp_server: Option<Arc<crate::mcp_server::McpServer>>,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        operation_runtime: Arc<OperationRuntime>,
        permits: usize,
    ) -> Self {
        Self::spawn_with_terminal_renderer_and_harness_and_operation_runtime(
            repo,
            events,
            write,
            codex,
            daemon,
            terminal_renderer,
            mcp_server,
            HarnessRegistry::new(),
            shared_codex_appserver,
            operation_runtime,
            permits,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_terminal_renderer_and_harness(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        codex: Arc<CodexClient>,
        daemon: Arc<DaemonClient>,
        terminal_renderer: Arc<TerminalRendererRegistry>,
        mcp_server: Option<Arc<crate::mcp_server::McpServer>>,
        harness: HarnessRegistry,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        workspace_root: std::path::PathBuf,
        permits: usize,
    ) -> Self {
        let plugin =
            empty_plugin_host_for_dispatcher_runtime(repo.clone(), events.clone(), write.clone());
        let operation_runtime = dispatcher_operation_runtime(
            repo.clone(),
            events.clone(),
            write.clone(),
            codex.clone(),
            daemon.clone(),
            terminal_renderer.clone(),
            mcp_server.clone(),
            shared_codex_appserver.clone(),
            harness.clone(),
            plugin,
            workspace_root,
        );
        Self::spawn_with_terminal_renderer_and_harness_and_operation_runtime(
            repo,
            events,
            write,
            codex,
            daemon,
            terminal_renderer,
            mcp_server,
            harness,
            shared_codex_appserver,
            operation_runtime,
            permits,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_terminal_renderer_and_harness_and_operation_runtime(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        _codex: Arc<CodexClient>,
        daemon: Arc<DaemonClient>,
        terminal_renderer: Arc<TerminalRendererRegistry>,
        _mcp_server: Option<Arc<crate::mcp_server::McpServer>>,
        harness: HarnessRegistry,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        operation_runtime: Arc<OperationRuntime>,
        permits: usize,
    ) -> Self {
        let permits = if permits == 0 {
            DEFAULT_PERMITS
        } else {
            permits
        };
        let semaphore = Arc::new(Semaphore::new(permits));
        // Issue #644 PR-B — the scheduler lives at the dispatcher
        // construction site: same `Weak<OperationRuntime>` discipline,
        // same global spawn semaphore (§5.3).
        let scheduler = Scheduler::new(
            repo.clone(),
            events.clone(),
            write.clone(),
            Arc::downgrade(&operation_runtime),
            Arc::clone(&semaphore),
        );
        let context_monitor = Arc::new(TaskContextMonitor::new_with_metrics(
            repo.clone(),
            events.clone(),
            write.clone(),
            scheduler.context_metrics(),
        ));
        // #741 §1.3 — take the durable-liveness feeder's notification
        // subscription BEFORE `shared_codex_appserver` is moved into the
        // provider registry below, and clone the repo before it is moved into
        // `Inner`. The feeder is spawned (behind the same kill-switch as the
        // reaper) further down.
        let liveness_feeder_rx = shared_codex_appserver.subscribe_notifications();
        let liveness_feeder_repo = repo.clone();
        let provider_registry = WorkerProviderRegistry::new(
            supervisor_sock_for_provider_registry(&daemon),
            shared_codex_appserver,
        );
        let reaper = Arc::new(Reaper::new(
            repo.clone(),
            provider_registry,
            events.clone(),
            write.clone(),
        ));
        // Issue #644 M2 (live path) — install the terminal-exit
        // completion bundle on the renderer registry so the
        // attach-reader exit branch can flip plan-task rows.
        terminal_renderer.set_task_hook(TerminalTaskHook::new(
            repo.clone(),
            events.clone(),
            write.clone(),
        ));
        let inner = Arc::new(Inner {
            repo,
            write,
            harness,
            scheduler: Arc::clone(&scheduler),
            context_monitor: Arc::clone(&context_monitor),
            // #293 PR3b — a DEDICATED push watermark cache. Intentionally
            // a SEPARATE instance from anything else: keyed by the spec
            // `CardId`;
            // a push only fires when `envelope_id > cursor`, making pushes
            // idempotent under the broadcast's at-least-once delivery.
            push_cursor: EventCursorCache::new(),
            // #293 PR3b (S1) — per-track push serialization lock-map.
            push_locks: DashMap::new(),
            semaphore: Arc::clone(&semaphore),
        });

        // Filter: push events route to harness observation delivery;
        // scheduler trigger events poke the plan scheduler. Hook events
        // are coarse-filtered by `kind_tag()` here; the exact turn-ending
        // hook discriminators are checked synchronously in the push branch
        // below.
        let kinds: Vec<String> = vec![
            "task.completed".into(),
            "task.failed".into(),
            // Issue #644 PR-C — the gate runner's verdict: pushed to
            // the spec (hard-fire) and a scheduler trigger (a gate
            // verdict terminalizes the task — budget freed / deps
            // satisfiable).
            "task.gate_result".into(),
            "track.report_edited".into(),
            "track.deleted".into(),
            "area.deleted".into(),
            "workspace.leased".into(),
            "workspace.released".into(),
            "forge.scan.completed".into(),
            "forge.pr.opened".into(),
            "forge.pr.checks".into(),
            "forge.issue.closed".into(),
            "worktree.provisioned".into(),
            "worktree.committed".into(),
            "forge.pr.merged".into(),
            "review.round".into(),
            "ratify.requested".into(),
            "ratify.resolved".into(),
            "codex.hook".into(),
            "claude.hook".into(),
            // Issue #644 PR-B — scheduler triggers (§5.1). These
            // only poke the scheduler; they never enter the push branch.
            // `track.updated` (round-2 review
            // F4) covers budget-changing PATCHes, which emit no
            // lifecycle event when the lifecycle is unchanged.
            "plan.updated".into(),
            "track.lifecycle_changed".into(),
            "track.updated".into(),
        ];
        let filter = SubscribeFilter {
            scope: SubscribeScope::Any,
            include_descendants: true,
            kinds: Some(kinds),
        };
        let mut rx = events.subscribe_filtered();

        let inner_for_task = Arc::clone(&inner);
        let filter_for_task = filter.clone();
        let handle = tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(envelope) => {
                        // Apply the filter — `subscribe_filtered`
                        // hands back the raw firehose, callers run the
                        // match themselves (see `EventBus::subscribe_filtered`
                        // doc on why we ship that shape rather than a
                        // BroadcastStream wrapper).
                        if !filter_for_task.matches(&envelope) {
                            continue;
                        }
                        let inner = Arc::clone(&inner_for_task);
                        // Per-event spawn is fire-and-forget: the bus
                        // reader keeps draining while the
                        // semaphore-gated handler is in flight.
                        tokio::spawn(async move {
                            inner.handle_envelope(envelope).await;
                        });
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        // A lag means we missed `n` events. The scheduler
                        // sweep below is the durable backstop for missed
                        // plan/task trigger events. Log and continue.
                        tracing::warn!(
                            skipped = n,
                            "dispatcher subscriber lagged; missed events may need a retry from the requester"
                        );
                        // Issue #644 PR-B (§5.1 backstop a): a lagged
                        // `plan.updated` / `task.completed` would strand
                        // pending tasks until the next reconcile tick —
                        // schedule a full sweep now. Every sweep arm is
                        // guarded + idempotent, so racing live handling
                        // is a no-op. `sweep_all` is boot-gated (round-3
                        // review F2): a lag during boot no-ops here and
                        // the boot sweep itself covers the missed events.
                        let scheduler = Arc::clone(&inner_for_task.scheduler);
                        let context_monitor = Arc::clone(&inner_for_task.context_monitor);
                        tokio::spawn(async move {
                            if let Err(error) = context_monitor.sweep().await {
                                tracing::warn!(%error, "task context sweep after lag failed");
                            } else {
                                scheduler.open_context_sweep_gate().await;
                            }
                            scheduler.sweep_all().await;
                        });
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // §5.1 backstop b — slow reconcile tick running the same sweep
        // as boot. Correctness never depends on it (every arm is
        // guarded); it restores liveness after a lost envelope.
        let tick_inner = Arc::clone(&inner);
        let reconcile_handle = tokio::spawn(async move {
            let period = std::time::Duration::from_secs(Scheduler::reconcile_secs_from_env(
                DEFAULT_RECONCILE_SECS,
            ));
            let mut interval = tokio::time::interval(period);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // The first tick fires immediately; skip it — boot runs its
            // own sweep in the asserted boot order. Later ticks that
            // still beat the boot funnel (low reconcile period / slow
            // recovery) are handled by `sweep_all`'s boot gate (round-3
            // review F2): they no-op until `sweep_boot` completes.
            interval.tick().await;
            loop {
                interval.tick().await;
                tick_inner.reconcile_once().await;
            }
        });
        let reaper_handle = if reaper_disabled_from_env() {
            None
        } else {
            let tick_reaper = Arc::clone(&reaper);
            Some(tokio::spawn(async move {
                let period =
                    std::time::Duration::from_secs(Scheduler::reconcile_secs_from_env_var(
                        "NEIGE_REAPER_RECONCILE_SECS",
                        DEFAULT_REAPER_RECONCILE_SECS,
                    ));
                let mut interval = tokio::time::interval(period);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                // The first tick fires immediately; skip it. The reaper
                // has its own boot gate and remains observational after it opens.
                interval.tick().await;
                loop {
                    interval.tick().await;
                    tick_reaper.sweep_all().await;
                    // #741-4 (DR-2/DR-5) — the dead-ROOT convergence scan runs
                    // as a sibling in the same boot-gated reconcile loop.
                    tick_reaper.sweep_dead_roots().await;
                }
            }))
        };

        // #741 §1.3 — the durable liveness feeder, gated behind the SAME
        // kill-switch as the reaper: if the reaper is disabled, its writes
        // would be unused, so don't spawn it. (`daemon_connected_at_ms`
        // tracking stays always-on in the connect path — it's cheap.)
        let liveness_feeder_handle = if reaper_disabled_from_env() {
            None
        } else {
            Some(crate::liveness_feeder::spawn_liveness_feeder(
                liveness_feeder_repo,
                liveness_feeder_rx,
            ))
        };

        Self {
            semaphore,
            permits,
            handle,
            inner,
            operation_runtime,
            scheduler,
            context_monitor,
            reconcile_handle,
            reaper_handle,
            liveness_feeder_handle,
        }
    }
}

struct Inner {
    repo: Arc<dyn Repo>,
    write: WriteContext,
    /// Harness-backed shared specs are driven by dispatcher observations
    /// through the active harness registry.
    harness: HarnessRegistry,
    /// Issue #644 PR-B — scheduler poked by the subscription arms
    /// (`plan.updated`, `track.lifecycle_changed`, `track.updated`, and
    /// the task report kinds after their push handling).
    scheduler: Arc<Scheduler>,
    context_monitor: Arc<TaskContextMonitor>,
    /// #293 PR3b — DEDICATED push watermark cache keyed by the spec
    /// `CardId`. A push fires only when `envelope_id > cursor`, then bumps;
    /// this makes pushes idempotent under at-least-once broadcast delivery
    /// and survives a re-delivered envelope without double-pushing.
    push_cursor: EventCursorCache,
    /// #293 PR3b (S1) — per-track serialization lock for the push path. The
    /// dispatcher runs `push_to_spec` concurrently (one `tokio::spawn` per
    /// envelope), so without serialization the watermark
    /// `(get → compare → bump → push_observation)` is a non-atomic
    /// read-modify-write: if envelope id 11 bumps the cursor before id 10 is
    /// checked, id 10 (a DISTINCT real event — e.g. a `task.failed` carrying
    /// a `reason`) is wrongly deduped and silently dropped. Holding this
    /// per-track async `Mutex` across the whole dedup-check-and-deliver makes
    /// same-track pushes process in id order, so the monotonic watermark only
    /// dedups TRUE redeliveries. Keyed by `TrackId` (one spec card per track).
    /// Pushes are low-frequency, so per-track serialization is cheap.
    push_locks: DashMap<TrackId, Arc<tokio::sync::Mutex<()>>>,
    semaphore: Arc<Semaphore>,
}

impl Inner {
    /// Run one production reconcile cycle. The periodic loop and focused
    /// scheduler tests share this exact body so ordering changes are exercised,
    /// not inferred from a copied test helper.
    async fn reconcile_once(&self) {
        if let Err(error) = self.context_monitor.sweep().await {
            tracing::warn!(%error, "periodic task context sweep failed");
            self.scheduler.sweep_all().await;
        } else if !self.scheduler.open_context_sweep_gate().await {
            self.scheduler.sweep_all().await;
        }
    }

    async fn handle_envelope(self: Arc<Self>, envelope: BroadcastEnvelope) {
        // Acquire a permit before doing any per-spawn work. Dropped on
        // task end (the `_permit` binding holds it across the function).
        let _permit = match Arc::clone(&self.semaphore).acquire_owned().await {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!("dispatcher semaphore closed; aborting spawn");
                return;
            }
        };

        // #293 — push branch. The track-event kinds the filter matches route
        // HERE. For `track.report_edited` we act ONLY on a User- or
        // Plugin-authored edit (#955 §5.7) — Spec/Kernel-authored edits are
        // the spec writing its own report, and
        // pushing those back would be a feedback loop. Worker hook events
        // also return from here, even when ignored, because they are
        // lifecycle notices rather than scheduler requests.
        match &envelope.event {
            Event::TaskCompleted { .. }
            | Event::TaskFailed { .. }
            | Event::TaskGateResult { .. } => {
                // Issue #644 PR-C (§6.5) — gated self-report
                // suppression: a `task.completed` whose key resolves
                // to a tasks row WITH a gate is a claim, not evidence;
                // the spec hears the gate result instead. Round-3
                // review F1 extends this to a gated `task.failed`
                // that did not land a pre-gate row failure (stale /
                // retried report while the gate is in flight or
                // already decided) — see `is_gated_self_report`.
                if event_warrants_spec_push(&envelope.event, &envelope.actor, &self.write)
                    && !is_gated_self_report(self.repo.as_ref(), &envelope.event).await
                {
                    if let Some(track_id) = envelope.scope.track_id().cloned() {
                        self.observe_harness(track_id, &envelope.event, envelope.id)
                            .await;
                    } else {
                        tracing::debug!(
                            kind = envelope.event.kind_tag(),
                            "dispatcher push: task event has no track scope; skipping"
                        );
                    }
                }
                // Issue #644 PR-B (§5.1 trigger 2) — a task terminal
                // event may free budget / satisfy deps; poke the
                // scheduler AFTER the push branch. Fire-and-forget; the
                // scheduler's guards make spurious pokes no-ops.
                if let Some(track_id) = envelope.scope.track_id().cloned() {
                    self.scheduler.poke(track_id);
                }
            }
            // Issue #644 PR-B (§5.1 triggers 1 + 4) — scheduler-only
            // arms. They never enter the push branch or the worker-spawn
            // path below.
            Event::PlanUpdated { track_id, .. } => {
                self.scheduler.poke(track_id.clone());
            }
            Event::TrackLifecycleChanged { id, .. } => {
                self.scheduler.reconcile_child_track(id.clone());
                self.scheduler.poke(id.clone());
            }
            // Round-2 review F4 — `PATCH /api/tracks` emits only
            // `track.updated` when it changes `task_budget` without a
            // lifecycle transition; without this arm a raised budget
            // would strand pending tasks until the reconcile tick. Poke
            // only (never the push branch); pokes are idempotent and
            // cheap, so no budget diffing.
            Event::TrackUpdated(payload) => {
                self.scheduler.poke(payload.id.clone());
            }
            Event::TrackReportEdited {
                author, track_id, ..
            } => {
                // #985 PR3a-ii: mechanical invalidation is independent of
                // author and runs before the self-push suppression below.
                let context_monitor = Arc::clone(&self.context_monitor);
                let detection_track_id = track_id.clone();
                tokio::spawn(async move {
                    if let Err(error) = context_monitor
                        .detect_track_edit(detection_track_id.as_str())
                        .await
                    {
                        tracing::warn!(%error, track_id = %detection_track_id, "task context edit detection failed");
                    }
                });
                // Only user/plugin edits warrant a push (#955 §5.7).
                // The spec authored Spec/Kernel edits itself;
                // re-notifying it would loop.
                if event_warrants_spec_push(&envelope.event, &envelope.actor, &self.write) {
                    self.observe_harness(track_id.clone(), &envelope.event, envelope.id)
                        .await;
                } else {
                    tracing::trace!(
                        ?author,
                        "dispatcher push: ignoring spec/kernel-authored track.report_edited"
                    );
                }
            }
            Event::TrackDeleted { id, .. } => {
                self.scheduler.reconcile_child_track(id.clone());
                let context_monitor = Arc::clone(&self.context_monitor);
                tokio::spawn(async move {
                    if let Err(error) = context_monitor.sweep().await {
                        tracing::warn!(%error, "task context deletion sweep failed");
                    }
                });
            }
            Event::AreaDeleted { .. } => {
                // Payloads intentionally stay unchanged. The tasks-based
                // sweep discovers vanished tracks/areas fail-closed.
                let context_monitor = Arc::clone(&self.context_monitor);
                tokio::spawn(async move {
                    if let Err(error) = context_monitor.sweep().await {
                        tracing::warn!(%error, "task context deletion sweep failed");
                    }
                });
            }
            Event::WorkspaceLeased { track_id, .. } | Event::WorkspaceReleased { track_id, .. } => {
                if event_warrants_spec_push(&envelope.event, &envelope.actor, &self.write) {
                    self.observe_harness(track_id.clone(), &envelope.event, envelope.id)
                        .await;
                }
            }
            Event::ForgePrMerged { track_id, .. }
            | Event::ReviewRound { track_id, .. }
            | Event::RatifyRequested { track_id, .. }
            | Event::RatifyResolved { track_id, .. }
            | Event::ForgeScanCompleted { track_id, .. }
            | Event::ForgePrOpened { track_id, .. }
            | Event::ForgePrChecks { track_id, .. }
            | Event::ForgeIssueClosed { track_id, .. }
            | Event::WorktreeProvisioned { track_id, .. }
            | Event::WorktreeCommitted { track_id, .. } => {
                if event_warrants_spec_push(&envelope.event, &envelope.actor, &self.write) {
                    self.observe_harness(track_id.clone(), &envelope.event, envelope.id)
                        .await;
                }
            }
            Event::CodexHook { card_id, kind, .. } | Event::ClaudeHook { card_id, kind, .. } => {
                // Only the precise Stop hooks mean a worker turn truly
                // ended. Other hooks may project to the same FSM state (for
                // example `hook.codex.permission_request` -> AwaitingInput)
                // but are mid-turn pauses, so they must not wake the spec.
                //
                // The Worker role gate prevents spec self-push loops: spec
                // cards can emit their own hook lifecycle events, but only
                // worker cards should notify the spec. Stop hooks carry no
                // result/artifacts, so the pushed observation is a light
                // wake-up that asks the spec to re-read track state.
                if event_warrants_spec_push(&envelope.event, &envelope.actor, &self.write) {
                    if let Some(track_id) = envelope.scope.track_id().cloned() {
                        self.observe_harness(track_id, &envelope.event, envelope.id)
                            .await;
                    } else {
                        tracing::debug!(
                            kind = envelope.event.kind_tag(),
                            hook_kind = %kind,
                            card_id = %card_id,
                            "dispatcher push: worker hook stop has no track scope; skipping"
                        );
                    }
                } else {
                    tracing::trace!(
                        hook_kind = %kind,
                        card_id = %card_id,
                        "dispatcher push: ignoring hook event"
                    );
                }
            }
            Event::AreaUpdated(_)
            | Event::CardAdded(_)
            | Event::CardUpdated(_)
            | Event::CardDeleted { .. }
            | Event::RuntimeStarted { .. }
            | Event::RuntimeStatusChanged { .. }
            | Event::RuntimeSuperseded { .. }
            | Event::HarnessItemAdded { .. }
            | Event::HarnessPhaseChanged { .. }
            | Event::HarnessTranscriptCleared { .. }
            | Event::HarnessUserMessageEnqueued { .. }
            | Event::OverlaySet(_)
            | Event::OverlayDeleted { .. }
            | Event::TerminalDeleted { .. }
            | Event::PluginState { .. }
            | Event::PluginToolRegistered { .. }
            | Event::CodexWorkerRequested { .. }
            | Event::TerminalWorkerRequested { .. }
            | Event::TaskDispatched { .. }
            | Event::TaskContextFrozen { .. }
            | Event::TaskContextAdvanced { .. }
            | Event::ForgePrDiffRead { .. }
            | Event::ForgeIssueRead { .. }
            // Issue #955 — proposal lifecycle events reach the spec
            // indirectly: an accepted proposal lands a plugin-authored
            // `track.report_edited` in the same tx, and THAT frame is
            // the wake-up. The proposal records themselves are
            // adjudication history (user/plugin facing), so they are
            // not in the dispatcher's kind filter.
            | Event::ProposalSubmitted { .. }
            | Event::ProposalResolved { .. }
            | Event::WorktreeRemoved { .. } => {
                tracing::warn!(
                    kind = envelope.event.kind_tag(),
                    "dispatcher received event with no handler; filter widened unexpectedly",
                );
            }
        }
    }

    async fn observe_harness(self: &Arc<Self>, track_id: TrackId, event: &Event, envelope_id: i64) {
        let guard = self.acquire_push_lock(&track_id).await;
        self.observe_harness_under_lock(&guard, event, envelope_id)
            .await;
    }

    /// #313 round-2 (B3) — per-track push lock helper used by harness
    /// observation so same-track replay and live pushes serialize around
    /// `(get → compare → bump)`.
    async fn acquire_push_lock(self: &Arc<Self>, track_id: &TrackId) -> PushLockGuard {
        // IMPORTANT: do NOT bind the DashMap Entry to a `let` — the shard
        // guard must drop at this statement's `;` before we `.await` below.
        let lock = self
            .push_locks
            .entry(track_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let guard = lock.lock_owned().await;
        PushLockGuard::new(track_id.clone(), guard)
    }

    async fn observe_harness_under_lock(
        self: &Arc<Self>,
        guard: &PushLockGuard,
        event: &Event,
        envelope_id: i64,
    ) {
        let track_id = guard.track_id().clone();
        // Resolve the spec card for this track via the role cache.
        let spec_card_id = match self.resolve_spec_card(&track_id).await {
            Some(id) => id,
            None => {
                tracing::debug!(
                    track_id = %track_id,
                    "dispatcher push: no spec card found for track; skipping"
                );
                return;
            }
        };

        // Dedup: push only when this envelope is newer than the watermark
        // for the spec card. A persisted event always has a positive id;
        // a synthetic id-0 envelope (test `EventBus::emit`) is never above
        // the initial 0 cursor, so it is skipped — we only push real,
        // persisted, ordered events. `bump` is monotonic, so a re-delivered
        // (lower-or-equal) id is a no-op and can't double-push. Under the
        // per-track lock above this check-then-bump is now atomic w.r.t. other
        // same-track pushes.
        let cursor = self.push_cursor.get(&spec_card_id);
        if envelope_id <= cursor {
            tracing::debug!(
                track_id = %track_id,
                spec_card_id = %spec_card_id,
                envelope_id,
                cursor,
                "dispatcher push: envelope id not above watermark; deduped"
            );
            return;
        }

        let Some(runtime_id) = self.harness_runtime_id_for_spec_card(&spec_card_id).await else {
            tracing::debug!(
                track_id = %track_id,
                spec_card_id = %spec_card_id,
                envelope_id,
                kind = event.kind_tag(),
                "dispatcher push: spec card has no harness runtime; skipping observation"
            );
            return;
        };
        let Some(observation) = harness_observation_from_event(&track_id, event) else {
            tracing::debug!(
                track_id = %track_id,
                spec_card_id = %spec_card_id,
                envelope_id,
                kind = event.kind_tag(),
                "dispatcher push: harness runtime found but event did not map to a harness observation"
            );
            return;
        };
        let Some(harness) = self.harness.get(&runtime_id) else {
            tracing::warn!(
                track_id = %track_id,
                spec_card_id = %spec_card_id,
                runtime_id = %runtime_id,
                envelope_id,
                kind = event.kind_tag(),
                "dispatcher push: no live SpecHarness for harness runtime; cursor NOT bumped so snapshot recovery will replay on boot"
            );
            return;
        };
        tracing::info!(
            track_id = %track_id,
            spec_card_id = %spec_card_id,
            runtime_id = %runtime_id,
            envelope_id,
            kind = event.kind_tag(),
            "dispatcher push: delivering observation to spec harness"
        );
        if let Err(e) = harness.observe_envelope(observation, envelope_id) {
            tracing::warn!(
                track_id = %track_id,
                spec_card_id = %spec_card_id,
                runtime_id = %runtime_id,
                envelope_id,
                kind = event.kind_tag(),
                error = %e,
                "dispatcher push: SpecHarness observation enqueue failed; cursor NOT bumped so snapshot recovery will replay on boot"
            );
            return;
        }
        self.push_cursor.bump(spec_card_id.clone(), envelope_id);
    }

    /// Find the [`CardRole::Spec`] card for a track. Scans the track's cards
    /// and consults `card_role_cache` (write-through, in-memory) for the
    /// role. Returns `None` if the track has no spec card (shouldn't happen
    /// for a live push-enabled track) or the lookup errors.
    async fn resolve_spec_card(self: &Arc<Self>, track_id: &TrackId) -> Option<CardId> {
        let cards = match self.repo.cards_by_track(track_id.as_str()).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    track_id = %track_id,
                    error = %e,
                    "dispatcher push: cards_by_track failed; cannot resolve spec card"
                );
                return None;
            }
        };
        cards.into_iter().find_map(|c| {
            if self.write.verify_role(&c.id) == Some(CardRole::Spec) {
                Some(c.id)
            } else {
                None
            }
        })
    }

    async fn harness_runtime_id_for_spec_card(
        self: &Arc<Self>,
        spec_card_id: &CardId,
    ) -> Option<String> {
        let runtime = match self
            .repo
            .session_projection_active_for_card(&spec_card_id.to_string())
            .await
        {
            Ok(runtime) => runtime?,
            Err(e) => {
                tracing::warn!(
                    spec_card_id = %spec_card_id,
                    error = %e,
                    "dispatcher push: active runtime lookup failed; skipping harness observation"
                );
                return None;
            }
        };
        if runtime.kind != WorkerSessionKind::SharedSpec {
            return None;
        }
        let handle_state = runtime.handle_state_json.as_ref()?;
        if is_harness_snapshot_value(handle_state) {
            Some(runtime.id)
        } else {
            None
        }
    }
}

pub(crate) fn harness_observation_from_event(
    track_id: &TrackId,
    event: &Event,
) -> Option<HarnessObservation> {
    match event {
        Event::TaskCompleted {
            idempotency_key,
            result,
            ..
        } => Some(HarnessObservation::TaskCompleted {
            idempotency_key: idempotency_key.clone(),
            result: result.clone(),
        }),
        Event::TaskFailed {
            idempotency_key,
            reason,
            ..
        } => Some(HarnessObservation::TaskFailed {
            idempotency_key: idempotency_key.clone(),
            error: reason.clone(),
        }),
        // Issue #644 PR-C (§6.5) — the gate runner's verdict. The plan
        // key is recovered from the task-id convention
        // `"{track_id}:{key}"` (§2.1) for the turn text's
        // `plan/<key>/gate.log` path.
        Event::TaskGateResult {
            task_id,
            idempotency_key,
            passed,
            failing_step,
            exit_code,
            log_tail,
            attempt,
            ..
        } => Some(HarnessObservation::TaskGateResult {
            idempotency_key: idempotency_key.clone(),
            key: task_id
                .strip_prefix(&format!("{}:", track_id.as_str()))
                .unwrap_or(task_id)
                .to_string(),
            passed: *passed,
            failing_step: failing_step.clone(),
            exit_code: *exit_code,
            log_tail: log_tail.clone(),
            attempt: *attempt,
        }),
        Event::TrackReportEdited {
            body_after, author, ..
        } => Some(HarnessObservation::ReportEdited {
            track_id: track_id.clone(),
            body_sha256: sha256_hex(body_after),
            body: body_after.clone(),
            // #1252 S0 R1/F2 — the event's own attribution, carried through
            // so the turn text names the real author instead of calling
            // every edit a user edit.
            author: Some(*author),
        }),
        Event::WorkspaceLeased {
            card_id,
            lease_id,
            path,
            ..
        } => Some(HarnessObservation::WorkspaceLeased {
            track_id: track_id.clone(),
            card_id: card_id.clone(),
            lease_id: lease_id.clone(),
            path: path.clone(),
        }),
        Event::WorkspaceReleased {
            card_id, lease_id, ..
        } => Some(HarnessObservation::WorkspaceReleased {
            track_id: track_id.clone(),
            card_id: card_id.clone(),
            lease_id: lease_id.clone(),
        }),
        Event::ForgePrMerged { subject, .. } => Some(HarnessObservation::ForgePrMerged {
            track_id: track_id.clone(),
            pr_number: subject.pr_number,
        }),
        Event::ReviewRound {
            subject,
            head_sha,
            n,
            cap,
            converged,
            ..
        } => Some(HarnessObservation::ReviewRound {
            track_id: track_id.clone(),
            phase: subject.phase.clone(),
            slice_id: subject.slice_id.clone(),
            pr_number: subject.pr_number,
            head_sha: head_sha.clone(),
            n: *n,
            cap: *cap,
            converged: *converged,
        }),
        Event::RatifyRequested { reason, .. } => Some(HarnessObservation::RatifyRequested {
            track_id: track_id.clone(),
            reason: reason.clone(),
        }),
        Event::RatifyResolved { decision, .. } => Some(HarnessObservation::RatifyResolved {
            track_id: track_id.clone(),
            decision: *decision,
        }),
        Event::ForgeScanCompleted {
            overlapping_prs, ..
        } => Some(HarnessObservation::ForgeScanCompleted {
            track_id: track_id.clone(),
            overlapping_prs: overlapping_prs.clone(),
        }),
        Event::ForgePrOpened { pr_number, .. } => Some(HarnessObservation::ForgePrOpened {
            track_id: track_id.clone(),
            pr_number: *pr_number,
        }),
        Event::ForgePrChecks {
            pr_number,
            conclusion,
            ..
        } => Some(HarnessObservation::ForgePrChecks {
            track_id: track_id.clone(),
            pr_number: *pr_number,
            conclusion: conclusion.clone(),
        }),
        Event::ForgeIssueClosed { issue_number, .. } => {
            Some(HarnessObservation::ForgeIssueClosed {
                track_id: track_id.clone(),
                issue_number: *issue_number,
            })
        }
        Event::WorktreeProvisioned { card_id, path, .. } => {
            Some(HarnessObservation::WorktreeProvisioned {
                track_id: track_id.clone(),
                card_id: card_id.clone(),
                path: path.clone(),
            })
        }
        Event::WorktreeCommitted {
            card_id,
            commit_sha,
            branch,
            ..
        } => Some(HarnessObservation::WorktreeCommitted {
            track_id: track_id.clone(),
            card_id: card_id.clone(),
            commit_sha: commit_sha.clone(),
            branch: branch.clone(),
        }),
        Event::CodexHook {
            card_id,
            kind,
            hook_idempotency_key,
            ..
        } if kind == "hook.codex.stop" => Some(HarnessObservation::WorkerHookStop {
            track_id: track_id.clone(),
            card_id: card_id.clone(),
            kind: HarnessHookKind::CodexStop,
            idempotency_key: hook_idempotency_key.clone(),
        }),
        Event::ClaudeHook {
            card_id,
            kind,
            hook_idempotency_key,
            ..
        } if kind == "hook.claude.stop" => Some(HarnessObservation::WorkerHookStop {
            track_id: track_id.clone(),
            card_id: card_id.clone(),
            kind: HarnessHookKind::ClaudeStop,
            idempotency_key: hook_idempotency_key.clone(),
        }),
        Event::CodexHook { .. } | Event::ClaudeHook { .. } => None,
        Event::AreaUpdated(_)
        | Event::AreaDeleted { .. }
        | Event::TrackUpdated(_)
        | Event::TrackDeleted { .. }
        | Event::TrackLifecycleChanged { .. }
        | Event::CardAdded(_)
        | Event::CardUpdated(_)
        | Event::CardDeleted { .. }
        | Event::RuntimeStarted { .. }
        | Event::RuntimeStatusChanged { .. }
        | Event::RuntimeSuperseded { .. }
        | Event::HarnessItemAdded { .. }
        | Event::HarnessPhaseChanged { .. }
        | Event::HarnessTranscriptCleared { .. }
        | Event::HarnessUserMessageEnqueued { .. }
        | Event::OverlaySet(_)
        | Event::OverlayDeleted { .. }
        | Event::TerminalDeleted { .. }
        | Event::PluginState { .. }
        | Event::PluginToolRegistered { .. }
        | Event::CodexWorkerRequested { .. }
        | Event::TerminalWorkerRequested { .. }
        | Event::PlanUpdated { .. }
        | Event::TaskDispatched { .. }
        | Event::TaskContextFrozen { .. }
        | Event::TaskContextAdvanced { .. }
        | Event::ForgePrDiffRead { .. }
        | Event::ForgeIssueRead { .. }
        | Event::ProposalSubmitted { .. }
        | Event::ProposalResolved { .. }
        | Event::WorktreeRemoved { .. } => None,
    }
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests;
