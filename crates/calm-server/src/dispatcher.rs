//! Dispatcher worker.
//!
//! Subscribes to `codex.worker_requested` / `terminal.worker_requested` envelopes,
//! plus the task, report, and hook events that drive spec-harness push
//! observations.
//!
//! Worker requests are thin operation starts: the dispatcher builds the
//! worker payload and starts an [`OperationRuntime`] operation of kind
//! `codex-worker` or `terminal-worker`.
//!
//! Idempotency is owned by the operations table: `operations.idempotency_key`
//! is unique per operation kind, so duplicate request envelopes reuse the
//! existing operation instead of spawning another worker. Rollback is owned
//! by the worker adapters through `plan_compensation` / `compensate_step`.
//!
//! Terminal process cleanup remains a hard boundary owned by
//! `terminal_sweeper`; adapter compensation only mirrors the required
//! reap-before-delete ordering when undoing a failed worker operation.

use std::sync::{Arc, Weak};
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use crate::db::sqlite::events_append_for_operation_tx;
use crate::db::{Repo, RouteRepo, write_with_actor_events_typed};
use crate::error::CalmError;
use crate::event::{
    BroadcastEnvelope, EditAuthor, Event, EventBus, EventScope, SYNC_EVENT_VERSION,
    SubscribeFilter, SubscribeScope,
};
use crate::event_cursor::EventCursorCache;
use crate::harness::{
    HarnessRegistry, HookKind as HarnessHookKind, Observation as HarnessObservation, PushLockGuard,
    is_harness_snapshot_value,
};
use crate::ids::{ActorId, CardId, WaveId};
use crate::model::{CardRole, WaveLifecycle};
use crate::operation::claude_adapter::ClaudeAdapter;
use crate::operation::claude_restart_adapter::ClaudeRestartAdapter;
use crate::operation::codex_adapter::{
    CodexAdapter, CodexWorkerAdapter, CodexWorkerOperationPayload,
};
use crate::operation::spec_harness_interrupt_adapter::SpecHarnessInterruptAdapter;
use crate::operation::spec_harness_shutdown_adapter::SpecHarnessShutdownAdapter;
use crate::operation::spec_harness_start_adapter::SpecHarnessStartAdapter;
use crate::operation::terminal_adapter::{
    TerminalAdapter, TerminalWorkerAdapter, TerminalWorkerOperationPayload,
    normalize_terminal_worker_cwd,
};
use crate::operation::{
    OperationCompletionBus, OperationKey, OperationOutcome, OperationResult, OperationRuntime,
    SpawnCtx, SqlxOperationRepo,
};
use crate::pending_codex_threads::PendingThreadStartRegistry;
use crate::routes::terminal_cards::stable_payload_hash;
use crate::runtime_repo::RuntimeKind;
use crate::scheduler::{DEFAULT_RECONCILE_SECS, Scheduler, TerminalTaskHook};
use crate::shared_codex_appserver::SharedCodexAppServer;
use crate::state::{CodexClient, DaemonClient, WriteContext};
use crate::terminal_renderer::TerminalRendererRegistry;
use sha2::{Digest, Sha256};

pub(crate) use crate::db::sqlite::card_with_terminal_rollback_tx;

/// Default number of permits when `NEIGE_DISPATCHER_PERMITS` is unset /
/// invalid / `0`. Mirrors the v2 spec for issue #136.
const DEFAULT_PERMITS: usize = 8;
const SQLITE_BUSY_MAX_RETRIES: usize = 5;

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
            !matches!(actor, ActorId::AiSpec(_))
        }
        Event::WaveReportEdited { author, .. } => *author == EditAuthor::User,
        Event::CodexHook { card_id, kind, .. } | Event::ClaudeHook { card_id, kind, .. } => {
            let is_turn_end = kind == "hook.codex.stop" || kind == "hook.claude.stop";
            let is_worker = role_for_card(card_id) == Some(CardRole::Worker);
            is_turn_end && is_worker
        }
        _ => false,
    }
}

async fn promote_dispatching_to_working_or_emit_failure<P, PFut, L, LFut>(
    idempotency_key: &str,
    scope: EventScope,
    mut promote: P,
    log_failure: L,
) -> bool
where
    P: FnMut() -> PFut,
    PFut: std::future::Future<Output = crate::error::Result<()>>,
    L: FnOnce(EventScope, Event) -> LFut,
    LFut: std::future::Future<Output = crate::error::Result<()>>,
{
    let mut promote_backoff = Duration::from_millis(10);
    let mut promote_err = None;
    for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
        match promote().await {
            Ok(()) => {
                promote_err = None;
                break;
            }
            Err(e) if is_sqlite_busy(&e) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                tracing::debug!(
                    idempotency_key = %idempotency_key,
                    attempt,
                    error = %e,
                    "dispatcher: transient SQLite contention on promotion; retrying"
                );
                tokio::time::sleep(promote_backoff).await;
                promote_backoff = (promote_backoff * 2).min(Duration::from_millis(200));
                continue;
            }
            Err(e) => {
                promote_err = Some(e);
                break;
            }
        }
    }

    if let Some(e) = promote_err {
        tracing::warn!(
            idempotency_key = %idempotency_key,
            error = %e,
            "dispatcher: lifecycle promotion failed permanently; emitting task.failed without spawning"
        );
        let fail_event = Event::TaskFailed {
            idempotency_key: idempotency_key.to_string(),
            reason: format!("lifecycle promotion failed: {e}"),
            agent_message: None,
        };
        if let Err(e2) = log_failure(scope, fail_event).await {
            tracing::warn!(
                idempotency_key = %idempotency_key,
                error = %e2,
                "dispatcher: failed to log lifecycle-promotion task.failed event batch"
            );
        }
        return false;
    }

    true
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
        write.cove_cache().clone(),
    ));
    let terminal_worker_adapter = Arc::new(TerminalWorkerAdapter::new(
        route_repo.clone(),
        write.role_cache().clone(),
        write.cove_cache().clone(),
    ));
    let codex_adapter = Arc::new(CodexAdapter::new(
        route_repo.clone(),
        codex.clone(),
        shared_codex_appserver.clone(),
        pending_codex_threads.clone(),
        pending_codex_threads_spawn_serial,
        write.role_cache().clone(),
        write.cove_cache().clone(),
    ));
    let mcp_socket_path = mcp_server
        .as_ref()
        .map(|s| s.shim_config.socket_path.clone());
    let codex_worker_adapter = Arc::new(CodexWorkerAdapter::new(
        route_repo.clone(),
        codex.clone(),
        shared_codex_appserver.clone(),
        mcp_server,
        write.role_cache().clone(),
        write.cove_cache().clone(),
    ));
    let claude_adapter = Arc::new(ClaudeAdapter::new(
        route_repo.clone(),
        codex.clone(),
        write.role_cache().clone(),
        write.cove_cache().clone(),
    ));
    let claude_restart_adapter = Arc::new(ClaudeRestartAdapter::new(
        route_repo.clone(),
        codex,
        write.role_cache().clone(),
        write.cove_cache().clone(),
    ));
    let spec_harness_start_adapter = Arc::new(SpecHarnessStartAdapter::new(
        repo.clone(),
        shared_codex_appserver.clone(),
        harness.clone(),
        write.role_cache().clone(),
        write.cove_cache().clone(),
        mcp_socket_path,
    ));
    let spec_harness_interrupt_adapter =
        Arc::new(SpecHarnessInterruptAdapter::new(harness.clone()));
    let spec_harness_shutdown_adapter = Arc::new(SpecHarnessShutdownAdapter::new(
        harness,
        shared_codex_appserver,
        repo,
    ));
    let task_verify_adapter = Arc::new(
        crate::operation::task_verify_adapter::TaskVerifyAdapter::new(
            crate::operation::task_verify_adapter::TaskVerifyAdapter::default_gate_logs_dir(),
        ),
    );
    let completion = OperationCompletionBus::new();
    Arc::new(OperationRuntime::new_unchecked(
        operation_repo.clone(),
        vec![
            terminal_adapter,
            terminal_worker_adapter,
            codex_adapter,
            codex_worker_adapter,
            claude_adapter,
            claude_restart_adapter,
            spec_harness_start_adapter,
            spec_harness_interrupt_adapter,
            spec_harness_shutdown_adapter,
            task_verify_adapter,
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
        ),
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
    /// §5.1 liveness backstop — slow periodic reconcile sweep
    /// (`NEIGE_SCHEDULER_RECONCILE_SECS`, default 300). Held so a future
    /// shutdown can `abort()` it; runs for the process lifetime today,
    /// like `handle`.
    #[allow(dead_code)]
    reconcile_handle: JoinHandle<()>,
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
    /// Wave-scope-only: the live push path discards events without a wave
    /// scope before they reach the observer; this helper preserves that
    /// invariant (caller filters to wave-scoped events).
    pub async fn catch_up_push(
        &self,
        wave_id: WaveId,
        event: crate::event::Event,
        envelope_id: i64,
    ) {
        Inner::observe_harness(&self.inner, wave_id, &event, envelope_id).await;
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
        permits: usize,
    ) -> Self {
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
        permits: usize,
    ) -> Self {
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
        _daemon: Arc<DaemonClient>,
        terminal_renderer: Arc<TerminalRendererRegistry>,
        _mcp_server: Option<Arc<crate::mcp_server::McpServer>>,
        harness: HarnessRegistry,
        _shared_codex_appserver: Arc<SharedCodexAppServer>,
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
            events: events.clone(),
            write,
            harness,
            operation_runtime: Arc::downgrade(&operation_runtime),
            scheduler: Arc::clone(&scheduler),
            // #293 PR3b — a DEDICATED push watermark cache. Intentionally
            // a SEPARATE instance from anything else: keyed by the spec
            // `CardId`;
            // a push only fires when `envelope_id > cursor`, making pushes
            // idempotent under the broadcast's at-least-once delivery.
            push_cursor: EventCursorCache::new(),
            // #293 PR3b (S1) — per-wave push serialization lock-map.
            push_locks: DashMap::new(),
            semaphore: Arc::clone(&semaphore),
        });

        // Filter: worker request events start operations; push events route
        // to harness observation delivery. Hook events are coarse-filtered
        // by `kind_tag()` here; the exact turn-ending hook discriminators are
        // checked synchronously in the push branch below.
        let kinds: Vec<String> = vec![
            "codex.worker_requested".into(),
            "terminal.worker_requested".into(),
            "task.completed".into(),
            "task.failed".into(),
            "wave.report_edited".into(),
            "codex.hook".into(),
            "claude.hook".into(),
            // Issue #644 PR-B — scheduler triggers (§5.1). These
            // only poke the scheduler; they never enter the push branch
            // or the worker-spawn path. `wave.updated` (round-2 review
            // F4) covers budget-changing PATCHes, which emit no
            // lifecycle event when the lifecycle is unchanged.
            "plan.updated".into(),
            "wave.lifecycle_changed".into(),
            "wave.updated".into(),
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
                        // A lag means we missed `n` events; if any of
                        // them was a `*.Requested`, the request emitter
                        // is responsible for retrying with the same
                        // idempotency_key, which we'll handle on the
                        // next emit. Log and continue.
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
                        tokio::spawn(async move {
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
        let tick_scheduler = Arc::clone(&scheduler);
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
                tick_scheduler.sweep_all().await;
            }
        });

        Self {
            semaphore,
            permits,
            handle,
            inner,
            operation_runtime,
            scheduler,
            reconcile_handle,
        }
    }
}

struct Inner {
    repo: Arc<dyn Repo>,
    events: EventBus,
    write: WriteContext,
    /// Harness-backed shared specs are driven by dispatcher observations
    /// through the active harness registry.
    harness: HarnessRegistry,
    operation_runtime: Weak<OperationRuntime>,
    /// Issue #644 PR-B — scheduler poked by the subscription arms
    /// (`plan.updated`, `wave.lifecycle_changed`, `wave.updated`, and
    /// the task report kinds after their push handling).
    scheduler: Arc<Scheduler>,
    /// #293 PR3b — DEDICATED push watermark cache keyed by the spec
    /// `CardId`. A push fires only when `envelope_id > cursor`, then bumps;
    /// this makes pushes idempotent under at-least-once broadcast delivery
    /// and survives a re-delivered envelope without double-pushing.
    push_cursor: EventCursorCache,
    /// #293 PR3b (S1) — per-wave serialization lock for the push path. The
    /// dispatcher runs `push_to_spec` concurrently (one `tokio::spawn` per
    /// envelope), so without serialization the watermark
    /// `(get → compare → bump → push_observation)` is a non-atomic
    /// read-modify-write: if envelope id 11 bumps the cursor before id 10 is
    /// checked, id 10 (a DISTINCT real event — e.g. a `task.failed` carrying
    /// a `reason`) is wrongly deduped and silently dropped. Holding this
    /// per-wave async `Mutex` across the whole dedup-check-and-deliver makes
    /// same-wave pushes process in id order, so the monotonic watermark only
    /// dedups TRUE redeliveries. Keyed by `WaveId` (one spec card per wave).
    /// Pushes are low-frequency, so per-wave serialization is cheap.
    push_locks: DashMap<WaveId, Arc<tokio::sync::Mutex<()>>>,
    semaphore: Arc<Semaphore>,
}

impl Inner {
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

        // #293 — push branch. The wave-event kinds the filter matches route
        // HERE (bounded by the same `_permit` the worker-spawn path holds),
        // never into the `DispatchRequest` extraction below. For
        // `wave.report_edited` we act ONLY on a User-authored edit —
        // Spec/AI-authored edits are the spec writing its own report, and
        // pushing those back would be a feedback loop. Worker hook events
        // also return from here, even when ignored, because they are
        // lifecycle notices rather than worker-spawn requests. The
        // worker-request path (the two `*.worker_requested` kinds) falls through
        // untouched.
        match &envelope.event {
            Event::TaskCompleted { .. } | Event::TaskFailed { .. } => {
                if event_warrants_spec_push(&envelope.event, &envelope.actor, &self.write) {
                    if let Some(wave_id) = envelope.scope.wave_id().cloned() {
                        self.observe_harness(wave_id, &envelope.event, envelope.id)
                            .await;
                    } else {
                        tracing::debug!(
                            kind = envelope.event.kind_tag(),
                            "dispatcher push: task event has no wave scope; skipping"
                        );
                    }
                }
                // Issue #644 PR-B (§5.1 trigger 2) — a task terminal
                // event may free budget / satisfy deps; poke the
                // scheduler AFTER the push branch. Fire-and-forget; the
                // scheduler's guards make spurious pokes no-ops.
                if let Some(wave_id) = envelope.scope.wave_id().cloned() {
                    self.scheduler.poke(wave_id);
                }
                return;
            }
            // Issue #644 PR-B (§5.1 triggers 1 + 4) — scheduler-only
            // arms. They never enter the push branch or the worker-spawn
            // path below.
            Event::PlanUpdated { wave_id, .. } => {
                self.scheduler.poke(wave_id.clone());
                return;
            }
            Event::WaveLifecycleChanged { id, .. } => {
                self.scheduler.poke(id.clone());
                return;
            }
            // Round-2 review F4 — `PATCH /api/waves` emits only
            // `wave.updated` when it changes `task_budget` without a
            // lifecycle transition; without this arm a raised budget
            // would strand pending tasks until the reconcile tick. Poke
            // only (never the push branch); pokes are idempotent and
            // cheap, so no budget diffing.
            Event::WaveUpdated(payload) => {
                self.scheduler.poke(payload.id.clone());
                return;
            }
            Event::WaveReportEdited {
                author, wave_id, ..
            } => {
                // Only user edits warrant a push. The spec authored
                // Spec/Kernel edits itself; re-notifying it would loop.
                if event_warrants_spec_push(&envelope.event, &envelope.actor, &self.write) {
                    self.observe_harness(wave_id.clone(), &envelope.event, envelope.id)
                        .await;
                } else {
                    tracing::trace!(
                        ?author,
                        "dispatcher push: ignoring non-user wave.report_edited"
                    );
                }
                return;
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
                // wake-up that asks the spec to re-read wave state.
                if event_warrants_spec_push(&envelope.event, &envelope.actor, &self.write) {
                    if let Some(wave_id) = envelope.scope.wave_id().cloned() {
                        self.observe_harness(wave_id, &envelope.event, envelope.id)
                            .await;
                    } else {
                        tracing::debug!(
                            kind = envelope.event.kind_tag(),
                            hook_kind = %kind,
                            card_id = %card_id,
                            "dispatcher push: worker hook stop has no wave scope; skipping"
                        );
                    }
                } else {
                    tracing::trace!(
                        hook_kind = %kind,
                        card_id = %card_id,
                        "dispatcher push: ignoring hook event"
                    );
                }
                return;
            }
            // Everything else (the two `*.worker_requested` kinds) falls
            // through to the worker-spawn path below, unchanged.
            _ => {}
        }

        // Extract the request shape we know how to handle. The filter
        // already narrowed us to two variants; the `_` arm exists for
        // future-proofing in case the filter ever widens.
        let req = match &envelope.event {
            Event::CodexWorkerRequested {
                idempotency_key,
                goal,
                context,
                acceptance_criteria,
                ..
            } => DispatchRequest::Codex {
                idempotency_key: idempotency_key.clone(),
                goal: goal.clone(),
                context: context.clone(),
                acceptance_criteria: acceptance_criteria.clone(),
            },
            Event::TerminalWorkerRequested {
                idempotency_key,
                cmd,
                cwd,
                ..
            } => DispatchRequest::Terminal {
                idempotency_key: idempotency_key.clone(),
                cmd: cmd.clone(),
                cwd: cwd.clone(),
            },
            other => {
                tracing::warn!(
                    kind = other.kind_tag(),
                    "dispatcher received non-request event; filter widened unexpectedly",
                );
                return;
            }
        };
        let idem = req.idempotency_key().to_string();
        let scope = envelope.scope.clone();

        // Promote Dispatching -> Working before spawning the worker so a fast
        // worker's task.completed (which auto-promotes Working -> Reviewing)
        // never races ahead of the promotion. A permanent promotion failure
        // aborts the dispatch before the worker starts, otherwise the wave can
        // remain stuck in Dispatching after the worker finishes.
        let promote_inner = Arc::clone(&self);
        let promote_scope = scope.clone();
        let log_inner = Arc::clone(&self);
        if !promote_dispatching_to_working_or_emit_failure(
            &idem,
            scope.clone(),
            move || {
                let promote_inner = Arc::clone(&promote_inner);
                let promote_scope = promote_scope.clone();
                async move {
                    promote_inner
                        .auto_promote_dispatching_to_working(&promote_scope)
                        .await
                }
            },
            move |scope, fail_event| {
                let log_inner = Arc::clone(&log_inner);
                async move {
                    log_inner
                        .log_task_failure_and_maybe_promote_working_to_reviewing(scope, fail_event)
                        .await
                }
            },
        )
        .await
        {
            return;
        }

        // Retry on transient SQLite BUSY/locked errors. With more
        // than one dispatcher in flight (permits > 1), SQLite can
        // refuse a write with "database is locked" or "deadlocked"
        // even though no real deadlock exists — sqlx surfaces the
        // sqlite-3 status code as an io / database error. We retry
        // a few times with exponential backoff before giving up
        // and emitting `task.failed`.
        let mut last_err: Option<crate::error::CalmError> = None;
        let mut backoff = Duration::from_millis(10);
        for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
            match self
                .dispatch(req.clone(), scope.clone(), envelope.actor.clone())
                .await
            {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                Err(e) if is_sqlite_busy(&e) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    tracing::debug!(
                        idempotency_key = %idem,
                        attempt,
                        error = %e,
                        "dispatcher: transient SQLite contention; retrying"
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_millis(200));
                    continue;
                }
                Err(e) => {
                    last_err = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = last_err {
            tracing::warn!(
                idempotency_key = %idem,
                error = %e,
                "dispatcher: spawn failed; emitting task.failed"
            );
            // Emit TaskFailed and any Working -> Reviewing promotion in one
            // audited transaction so TaskFailed observers never wake on stale
            // wave lifecycle state.
            let fail_event = Event::TaskFailed {
                idempotency_key: idem.clone(),
                reason: format!("{e}"),
                agent_message: None,
            };
            let log_result = self
                .log_task_failure_and_maybe_promote_working_to_reviewing(scope.clone(), fail_event)
                .await;
            if let Err(e2) = log_result {
                tracing::warn!(
                    idempotency_key = %idem,
                    error = %e2,
                    "dispatcher: failed to log task.failed event batch"
                );
            }
        }
    }

    async fn observe_harness(self: &Arc<Self>, wave_id: WaveId, event: &Event, envelope_id: i64) {
        let guard = self.acquire_push_lock(&wave_id).await;
        self.observe_harness_under_lock(&guard, event, envelope_id)
            .await;
    }

    /// #313 round-2 (B3) — per-wave push lock helper used by harness
    /// observation so same-wave replay and live pushes serialize around
    /// `(get → compare → bump)`.
    async fn acquire_push_lock(self: &Arc<Self>, wave_id: &WaveId) -> PushLockGuard {
        // IMPORTANT: do NOT bind the DashMap Entry to a `let` — the shard
        // guard must drop at this statement's `;` before we `.await` below.
        let lock = self
            .push_locks
            .entry(wave_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let guard = lock.lock_owned().await;
        PushLockGuard::new(wave_id.clone(), guard)
    }

    async fn observe_harness_under_lock(
        self: &Arc<Self>,
        guard: &PushLockGuard,
        event: &Event,
        envelope_id: i64,
    ) {
        let wave_id = guard.wave_id().clone();
        // Resolve the spec card for this wave via the role cache.
        let spec_card_id = match self.resolve_spec_card(&wave_id).await {
            Some(id) => id,
            None => {
                tracing::debug!(
                    wave_id = %wave_id,
                    "dispatcher push: no spec card found for wave; skipping"
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
        // per-wave lock above this check-then-bump is now atomic w.r.t. other
        // same-wave pushes.
        let cursor = self.push_cursor.get(&spec_card_id);
        if envelope_id <= cursor {
            tracing::debug!(
                wave_id = %wave_id,
                spec_card_id = %spec_card_id,
                envelope_id,
                cursor,
                "dispatcher push: envelope id not above watermark; deduped"
            );
            return;
        }

        let Some(runtime_id) = self.harness_runtime_id_for_spec_card(&spec_card_id).await else {
            tracing::debug!(
                wave_id = %wave_id,
                spec_card_id = %spec_card_id,
                envelope_id,
                kind = event.kind_tag(),
                "dispatcher push: spec card has no harness runtime; skipping observation"
            );
            return;
        };
        let Some(observation) = harness_observation_from_event(&wave_id, event) else {
            tracing::debug!(
                wave_id = %wave_id,
                spec_card_id = %spec_card_id,
                envelope_id,
                kind = event.kind_tag(),
                "dispatcher push: harness runtime found but event did not map to a harness observation"
            );
            return;
        };
        let Some(harness) = self.harness.get(&runtime_id) else {
            tracing::warn!(
                wave_id = %wave_id,
                spec_card_id = %spec_card_id,
                runtime_id = %runtime_id,
                envelope_id,
                kind = event.kind_tag(),
                "dispatcher push: no live SpecHarness for harness runtime; cursor NOT bumped so snapshot recovery will replay on boot"
            );
            return;
        };
        tracing::info!(
            wave_id = %wave_id,
            spec_card_id = %spec_card_id,
            runtime_id = %runtime_id,
            envelope_id,
            kind = event.kind_tag(),
            "dispatcher push: delivering observation to spec harness"
        );
        if let Err(e) = harness.observe_envelope(observation, envelope_id) {
            tracing::warn!(
                wave_id = %wave_id,
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

    /// Find the [`CardRole::Spec`] card for a wave. Scans the wave's cards
    /// and consults `card_role_cache` (write-through, in-memory) for the
    /// role. Returns `None` if the wave has no spec card (shouldn't happen
    /// for a live push-enabled wave) or the lookup errors.
    async fn resolve_spec_card(self: &Arc<Self>, wave_id: &WaveId) -> Option<CardId> {
        let cards = match self.repo.cards_by_wave(wave_id.as_str()).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    wave_id = %wave_id,
                    error = %e,
                    "dispatcher push: cards_by_wave failed; cannot resolve spec card"
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
            .runtime_get_active_for_card(&spec_card_id.to_string())
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
        if runtime.kind != RuntimeKind::SharedSpec {
            return None;
        }
        let handle_state = runtime.handle_state_json.as_ref()?;
        if is_harness_snapshot_value(handle_state) {
            Some(runtime.id)
        } else {
            None
        }
    }

    async fn auto_promote_dispatching_to_working(
        self: &Arc<Self>,
        scope: &EventScope,
    ) -> crate::error::Result<()> {
        self.auto_promote_wave_lifecycle(
            scope,
            WaveLifecycle::Dispatching,
            WaveLifecycle::Working,
            Some("[auto] worker spawned".to_string()),
        )
        .await
    }

    async fn auto_promote_wave_lifecycle(
        self: &Arc<Self>,
        scope: &EventScope,
        from: WaveLifecycle,
        to: WaveLifecycle,
        agent_message: Option<String>,
    ) -> crate::error::Result<()> {
        let Some(wave_id) = scope.wave_id().cloned() else {
            return Ok(());
        };
        let cove_id = scope.cove_id().cloned().ok_or_else(|| {
            CalmError::Internal(format!(
                "dispatcher: request scope has wave {} but no cove",
                wave_id.as_str()
            ))
        })?;
        let wave_scope = EventScope::Wave {
            wave: wave_id.clone(),
            cove: cove_id,
        };
        let pool = self.repo.sqlite_pool().ok_or_else(|| {
            CalmError::Internal("dispatcher lifecycle auto-promotion requires sqlite repo".into())
        })?;
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
        let events = match crate::wave_lifecycle::auto_transition_if_current_in_tx(
            &mut tx,
            &wave_id,
            from,
            to,
            &ActorId::KernelDispatcher,
            agent_message,
        )
        .await
        {
            Ok(Some(events)) => events,
            Ok(None) => {
                let _ = tx.rollback().await;
                return Ok(());
            }
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        for event in &events {
            #[allow(deprecated)]
            if let Err(violation) = crate::role_gate::enforce_role(
                &ActorId::KernelDispatcher,
                event,
                &wave_scope,
                self.write.role_cache(),
                self.write.cove_cache(),
            ) {
                let _ = tx.rollback().await;
                return Err(CalmError::Forbidden(violation.to_string()));
            }
        }

        let event_ids = match events_append_for_operation_tx(
            &mut tx,
            &ActorId::KernelDispatcher,
            &wave_scope,
            None,
            &events,
        )
        .await
        {
            Ok(ids) => ids,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        let emitted = event_ids.into_iter().zip(events).collect::<Vec<_>>();
        tx.commit().await?;
        for (event_id, event) in emitted {
            self.events.emit_envelope(BroadcastEnvelope {
                id: event_id,
                event_version: SYNC_EVENT_VERSION,
                actor: ActorId::KernelDispatcher,
                scope: wave_scope.clone(),
                event,
            });
        }
        Ok(())
    }

    async fn log_task_failure_and_maybe_promote_working_to_reviewing(
        self: &Arc<Self>,
        scope: EventScope,
        fail_event: Event,
    ) -> crate::error::Result<()> {
        let wave_id = scope.wave_id().cloned();
        let wave_scope = wave_id
            .clone()
            .zip(scope.cove_id().cloned())
            .map(|(wave, cove)| EventScope::Wave { wave, cove });

        write_with_actor_events_typed::<(), _>(
            self.repo.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    let mut events = vec![(ActorId::KernelDispatcher, scope, fail_event)];
                    if let (Some(wave_id), Some(wave_scope)) = (wave_id, wave_scope)
                        && let Some(auto_events) =
                            crate::wave_lifecycle::auto_transition_if_current_in_tx(
                                tx,
                                &wave_id,
                                WaveLifecycle::Working,
                                WaveLifecycle::Reviewing,
                                &ActorId::KernelDispatcher,
                                Some("[auto] worker spawn failed".to_string()),
                            )
                            .await?
                    {
                        events.extend(
                            auto_events.into_iter().map(|event| {
                                (ActorId::KernelDispatcher, wave_scope.clone(), event)
                            }),
                        );
                    }
                    Ok(((), events))
                })
            },
        )
        .await
        .map(|_| ())
    }

    async fn dispatch(
        self: &Arc<Self>,
        req: DispatchRequest,
        scope: EventScope,
        actor: ActorId,
    ) -> crate::error::Result<()> {
        // The request envelope must carry a wave (and therefore a cove)
        // — a dispatcher can't materialize a worker card without a
        // parent wave. System-scoped requests are rejected.
        let wave_id = scope
            .wave_id()
            .ok_or_else(|| {
                CalmError::BadRequest(format!(
                    "dispatcher: *.Requested event has no wave scope (got {scope:?})"
                ))
            })?
            .clone();

        match req {
            DispatchRequest::Codex {
                idempotency_key,
                goal,
                context,
                acceptance_criteria,
            } => {
                let Some(operation_runtime) = self.operation_runtime.upgrade() else {
                    tracing::debug!(
                        idempotency_key = %idempotency_key,
                        "dispatcher operation runtime dropped; skipping codex worker request"
                    );
                    return Ok(());
                };
                let payload = serde_json::to_value(CodexWorkerOperationPayload {
                    actor,
                    wave_id: wave_id.to_string(),
                    idempotency_key: idempotency_key.clone(),
                    goal,
                    context,
                    acceptance_criteria,
                })?;
                let payload_hash = stable_payload_hash(&payload)?;
                let op_id = operation_runtime
                    .start(
                        "codex-worker",
                        OperationKey {
                            operation_key: crate::model::new_id(),
                            idempotency_key: Some(idempotency_key),
                            payload_hash,
                        },
                        payload,
                    )
                    .await?;
                operation_result_to_dispatch_result(operation_runtime.wait(&op_id).await?)?;
            }
            DispatchRequest::Terminal {
                idempotency_key,
                cmd,
                cwd,
            } => {
                let Some(operation_runtime) = self.operation_runtime.upgrade() else {
                    tracing::debug!(
                        idempotency_key = %idempotency_key,
                        "dispatcher operation runtime dropped; skipping terminal worker request"
                    );
                    return Ok(());
                };
                let cwd = Some(normalize_terminal_worker_cwd(cwd));
                let payload = serde_json::to_value(TerminalWorkerOperationPayload {
                    actor,
                    wave_id: wave_id.to_string(),
                    idempotency_key: idempotency_key.clone(),
                    cmd,
                    cwd,
                })?;
                let payload_hash = stable_payload_hash(&payload)?;
                let op_id = operation_runtime
                    .start(
                        "terminal-worker",
                        OperationKey {
                            operation_key: crate::model::new_id(),
                            idempotency_key: Some(idempotency_key),
                            payload_hash,
                        },
                        payload,
                    )
                    .await?;
                operation_result_to_dispatch_result(operation_runtime.wait(&op_id).await?)?;
            }
        }
        Ok(())
    }
}

fn operation_result_to_dispatch_result(result: OperationResult) -> crate::error::Result<()> {
    match result.outcome {
        OperationOutcome::Succeeded { .. } | OperationOutcome::SucceededViaCollision { .. } => {
            Ok(())
        }
        OperationOutcome::Failed { last_error, .. } => Err(CalmError::Internal(last_error)),
        OperationOutcome::Stuck { reason, .. } => Err(CalmError::Internal(reason)),
    }
}

pub(crate) fn harness_observation_from_event(
    wave_id: &WaveId,
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
        Event::WaveReportEdited { body_after, .. } => Some(HarnessObservation::ReportEdited {
            wave_id: wave_id.clone(),
            body_sha256: sha256_hex(body_after),
            body: body_after.clone(),
        }),
        Event::CodexHook {
            card_id,
            kind,
            hook_idempotency_key,
            ..
        } if kind == "hook.codex.stop" => Some(HarnessObservation::WorkerHookStop {
            wave_id: wave_id.clone(),
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
            wave_id: wave_id.clone(),
            card_id: card_id.clone(),
            kind: HarnessHookKind::ClaudeStop,
            idempotency_key: hook_idempotency_key.clone(),
        }),
        _ => None,
    }
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

/// Returns true when the given error is a transient SQLite BUSY /
/// LOCKED status that the dispatcher should retry.
///
/// See https://www.sqlite.org/rescode.html — code 5 = `SQLITE_BUSY`,
/// code 6 = `SQLITE_LOCKED`. sqlx reports the code as a string on
/// `DatabaseError::code()`.
fn is_sqlite_busy(e: &crate::error::CalmError) -> bool {
    // Walk the error chain looking for a `sqlx::Error` we own. The
    // dispatcher's calls funnel through `CalmError::from(sqlx::Error)`
    // which boxes the original under the `Sql` variant; everything
    // else (Internal/etc) won't match.
    let sqlx_err = match e {
        crate::error::CalmError::Db(inner) => inner,
        _ => return false,
    };
    let Some(db_err) = sqlx_err.as_database_error() else {
        return false;
    };
    // SQLITE_BUSY = 5, SQLITE_LOCKED = 6 — both are transient
    // contention on the per-connection write lock, retry-safe.
    matches!(db_err.code().as_deref(), Some("5") | Some("6"))
}

/// Variant shape extracted from a `*.Requested` envelope. Carrying this
/// rather than the raw `Event` lets the dispatch path stay variant-
/// agnostic at the spawn site. `Clone` so the retry loop can re-issue
/// the dispatch after a transient SQLite contention error.
#[derive(Clone)]
enum DispatchRequest {
    Codex {
        idempotency_key: String,
        goal: String,
        context: serde_json::Value,
        acceptance_criteria: Option<String>,
    },
    Terminal {
        idempotency_key: String,
        cmd: String,
        cwd: Option<String>,
    },
}

impl DispatchRequest {
    fn idempotency_key(&self) -> &str {
        match self {
            DispatchRequest::Codex {
                idempotency_key, ..
            } => idempotency_key,
            DispatchRequest::Terminal {
                idempotency_key, ..
            } => idempotency_key,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Env-override permits parsing — covers the four cases the helper
    /// documents (unset, empty, unparseable, zero, valid).
    #[test]
    fn permits_from_env_fallback_paths() {
        // Save + restore so this test doesn't disturb its neighbors.
        let saved = std::env::var("NEIGE_DISPATCHER_PERMITS").ok();

        // Use a sub-fn so the unsafe SAFETY blocks are scoped tightly.
        // `set_var` / `remove_var` are unsafe in 2024-edition Rust.
        fn set(k: &str, v: &str) {
            // SAFETY: single-threaded test; no other reader of this env
            // var is racing.
            unsafe { std::env::set_var(k, v) };
        }
        fn remove(k: &str) {
            // SAFETY: see `set`.
            unsafe { std::env::remove_var(k) };
        }

        remove("NEIGE_DISPATCHER_PERMITS");
        assert_eq!(Dispatcher::permits_from_env(8), 8, "unset → default");

        set("NEIGE_DISPATCHER_PERMITS", "");
        assert_eq!(Dispatcher::permits_from_env(8), 8, "empty → default");

        set("NEIGE_DISPATCHER_PERMITS", "not-a-number");
        assert_eq!(Dispatcher::permits_from_env(8), 8, "garbage → default");

        set("NEIGE_DISPATCHER_PERMITS", "0");
        assert_eq!(Dispatcher::permits_from_env(8), 8, "zero → default");

        set("NEIGE_DISPATCHER_PERMITS", "3");
        assert_eq!(Dispatcher::permits_from_env(8), 3, "valid → override");

        // Restore.
        match saved {
            Some(v) => set("NEIGE_DISPATCHER_PERMITS", &v),
            None => remove("NEIGE_DISPATCHER_PERMITS"),
        }
    }

    // ---------------------------------------------------------------
    // #293 PR3b — push path: filter coverage and author gating.
    // ---------------------------------------------------------------

    use crate::card_role_cache::CardRoleCache;
    use crate::event::{ArtifactRef, BroadcastEnvelope};
    use crate::ids::CoveId;

    fn wave_scope(wave: &WaveId, cove: &CoveId) -> EventScope {
        EventScope::Wave {
            wave: wave.clone(),
            cove: cove.clone(),
        }
    }

    #[tokio::test]
    async fn pre_spawn_promotion_hard_failure_emits_task_failed_and_aborts() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let promote_calls = Arc::new(AtomicUsize::new(0));
        let logged = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let wave = WaveId::from("wave-promotion-hard-fail");
        let cove = CoveId::from("cove-promotion-hard-fail");
        let scope = wave_scope(&wave, &cove);

        let should_spawn = promote_dispatching_to_working_or_emit_failure(
            "promotion-hard-fail",
            scope.clone(),
            {
                let promote_calls = Arc::clone(&promote_calls);
                move || {
                    promote_calls.fetch_add(1, Ordering::SeqCst);
                    async {
                        Err::<(), _>(CalmError::Internal("forced promotion failure".to_string()))
                    }
                }
            },
            {
                let logged = Arc::clone(&logged);
                move |scope, event| {
                    let logged = Arc::clone(&logged);
                    async move {
                        logged.lock().await.push((scope, event));
                        Ok(())
                    }
                }
            },
        )
        .await;

        assert!(
            !should_spawn,
            "permanent promotion failure must abort before spawning the worker"
        );
        assert_eq!(
            promote_calls.load(Ordering::SeqCst),
            1,
            "hard promotion errors should not be retried"
        );

        let logged = logged.lock().await;
        assert_eq!(logged.len(), 1, "promotion failure should log TaskFailed");
        assert_eq!(logged[0].0, scope);
        match &logged[0].1 {
            Event::TaskFailed {
                idempotency_key,
                reason,
                agent_message,
            } => {
                assert_eq!(idempotency_key, "promotion-hard-fail");
                assert!(
                    reason.contains("lifecycle promotion failed"),
                    "TaskFailed reason should identify promotion failure: {reason}"
                );
                assert!(
                    reason.contains("forced promotion failure"),
                    "TaskFailed reason should include the original error: {reason}"
                );
                assert_eq!(agent_message, &None);
            }
            other => panic!("expected TaskFailed for promotion failure, got {other:?}"),
        }
    }

    /// The dispatcher's `SubscribeFilter` must now match the push kinds in
    /// addition to the two worker_requested kinds. We reconstruct the
    /// exact filter the spawn site builds and assert `matches()` for each
    /// kind, plus a non-matching kind to prove the list is still a closed
    /// allowlist (not "match everything").
    #[test]
    fn dispatcher_filter_matches_push_kinds() {
        let filter = SubscribeFilter {
            scope: SubscribeScope::Any,
            include_descendants: true,
            kinds: Some(vec![
                "codex.worker_requested".into(),
                "terminal.worker_requested".into(),
                "task.completed".into(),
                "task.failed".into(),
                "wave.report_edited".into(),
                "codex.hook".into(),
                "claude.hook".into(),
                "plan.updated".into(),
                "wave.lifecycle_changed".into(),
                "wave.updated".into(),
            ]),
        };
        let wave = WaveId::from("w");
        let cove = CoveId::from("c");
        let scope = wave_scope(&wave, &cove);

        let env = |ev: Event| BroadcastEnvelope {
            id: 1,
            event_version: 1,
            actor: ActorId::User,
            scope: scope.clone(),
            event: ev,
        };

        // The two worker_requested kinds still match.
        assert!(filter.matches(&env(Event::CodexWorkerRequested {
            idempotency_key: "k".into(),
            goal: "g".into(),
            context: serde_json::Value::Null,
            acceptance_criteria: None,
            agent_message: None,
        })));
        assert!(filter.matches(&env(Event::TerminalWorkerRequested {
            idempotency_key: "k".into(),
            cmd: "ls".into(),
            cwd: None,
            agent_message: None,
        })));
        // The push kinds match.
        assert!(filter.matches(&env(Event::TaskCompleted {
            idempotency_key: "k".into(),
            result: serde_json::Value::Null,
            artifacts: Vec::<ArtifactRef>::new(),
            agent_message: None,
        })));
        assert!(filter.matches(&env(Event::TaskFailed {
            idempotency_key: "k".into(),
            reason: "boom".into(),
            agent_message: None,
        })));
        assert!(filter.matches(&env(Event::WaveReportEdited {
            wave_id: wave.clone(),
            card_id: CardId::from("card"),
            author: EditAuthor::User,
            edit_id: "e".into(),
            summary_before: String::new(),
            summary_after: String::new(),
            body_before: String::new(),
            body_after: String::new(),
            agent_message: None,
        })));
        assert!(filter.matches(&env(Event::CodexHook {
            card_id: CardId::from("worker-codex"),
            kind: "hook.codex.stop".into(),
            hook_idempotency_key: "hook-codex".into(),
            payload: serde_json::Value::Null,
        })));
        assert!(filter.matches(&env(Event::ClaudeHook {
            card_id: CardId::from("worker-claude"),
            kind: "hook.claude.stop".into(),
            hook_idempotency_key: "hook-claude".into(),
            payload: serde_json::Value::Null,
        })));
        // Issue #644 PR-B — the scheduler trigger kinds match.
        assert!(filter.matches(&env(Event::PlanUpdated {
            wave_id: wave.clone(),
            changed_keys: vec!["impl-parser".into()],
            agent_message: None,
        })));
        assert!(filter.matches(&env(Event::WaveLifecycleChanged {
            id: wave.clone(),
            cove_id: cove.clone(),
            from: crate::model::WaveLifecycle::Draft,
            to: crate::model::WaveLifecycle::Planning,
            agent_message: None,
        })));
        // Round-2 review F4 — budget PATCHes emit only `wave.updated`
        // when the lifecycle is unchanged; it must reach the poke arm.
        assert!(filter.matches(&env(Event::WaveUpdated(
            crate::event::WaveUpdatedPayload::new(
                crate::model::Wave {
                    id: wave.clone(),
                    cove_id: cove.clone(),
                    title: "w".into(),
                    sort: 0.0,
                    archived_at: None,
                    pinned_at: None,
                    lifecycle: crate::model::WaveLifecycle::Working,
                    cwd: String::new(),
                    terminal_at: None,
                    created_at: 1,
                    updated_at: 1,
                },
                None,
            )
        ))));
        // `task.dispatched` is emitted BY the scheduler inside its claim
        // tx and deliberately NOT subscribed (§5.1).
        assert!(!filter.matches(&env(Event::TaskDispatched {
            idempotency_key: "w:k".into(),
            kind: "codex".into(),
            agent_message: None,
        })));
        // A kind NOT in the list must not match — the filter is still a
        // closed allowlist.
        assert!(!filter.matches(&env(Event::WaveDeleted {
            id: wave.clone(),
            cove_id: cove.clone(),
        })));
    }

    /// The push branch in `handle_envelope` acts on a User-authored
    /// `wave.report_edited` and ignores Spec/Kernel ones. The gating is a
    /// simple `author == EditAuthor::User` check; assert that predicate
    /// directly against each variant (the branch itself is exercised
    /// end-to-end by the gated e2e).
    #[test]
    fn wave_report_edited_author_gating() {
        assert!(EditAuthor::User == EditAuthor::User);
        assert!(EditAuthor::Spec != EditAuthor::User);
        assert!(EditAuthor::Kernel != EditAuthor::User);
    }

    #[test]
    fn event_warrants_spec_push_covers_push_allowlist() {
        let cache = CardRoleCache::new();
        let wave = WaveId::from("w");
        let cove = CoveId::from("c");
        let worker = CardId::from("worker");
        let spec = CardId::from("spec");
        let unknown = CardId::from("unknown");
        cache.insert(worker.clone(), CardRole::Worker, wave.clone());
        cache.insert(spec.clone(), CardRole::Spec, wave.clone());
        let write = WriteContext::new(cache, crate::wave_cove_cache::WaveCoveCache::new());

        let completed = Event::TaskCompleted {
            idempotency_key: "done".into(),
            result: serde_json::Value::Null,
            artifacts: Vec::new(),
            agent_message: None,
        };
        assert!(event_warrants_spec_push(
            &completed,
            &ActorId::AiCodex(worker.clone()),
            &write
        ));
        assert!(!event_warrants_spec_push(
            &completed,
            &ActorId::AiSpec(spec.clone()),
            &write
        ));

        let failed = Event::TaskFailed {
            idempotency_key: "fail".into(),
            reason: "boom".into(),
            agent_message: None,
        };
        assert!(event_warrants_spec_push(
            &failed,
            &ActorId::AiCodex(worker.clone()),
            &write
        ));
        assert!(!event_warrants_spec_push(
            &failed,
            &ActorId::AiSpec(spec.clone()),
            &write
        ));

        let report = |author| Event::WaveReportEdited {
            wave_id: wave.clone(),
            card_id: spec.clone(),
            author,
            edit_id: "edit".into(),
            summary_before: String::new(),
            summary_after: String::new(),
            body_before: String::new(),
            body_after: String::new(),
            agent_message: None,
        };
        assert!(event_warrants_spec_push(
            &report(EditAuthor::User),
            &ActorId::User,
            &write
        ));
        assert!(!event_warrants_spec_push(
            &report(EditAuthor::Spec),
            &ActorId::User,
            &write
        ));
        assert!(!event_warrants_spec_push(
            &report(EditAuthor::Kernel),
            &ActorId::User,
            &write
        ));

        let codex_hook = |card_id: CardId, kind: &str| Event::CodexHook {
            card_id,
            kind: kind.into(),
            hook_idempotency_key: format!("hook-codex-{kind}"),
            payload: serde_json::Value::Null,
        };
        let claude_hook = |card_id: CardId, kind: &str| Event::ClaudeHook {
            card_id,
            kind: kind.into(),
            hook_idempotency_key: format!("hook-claude-{kind}"),
            payload: serde_json::Value::Null,
        };
        assert!(event_warrants_spec_push(
            &codex_hook(worker.clone(), "hook.codex.stop"),
            &ActorId::User,
            &write
        ));
        assert!(event_warrants_spec_push(
            &claude_hook(worker.clone(), "hook.claude.stop"),
            &ActorId::User,
            &write
        ));
        assert!(!event_warrants_spec_push(
            &codex_hook(spec.clone(), "hook.codex.stop"),
            &ActorId::User,
            &write
        ));
        assert!(!event_warrants_spec_push(
            &claude_hook(spec.clone(), "hook.claude.stop"),
            &ActorId::User,
            &write
        ));
        assert!(!event_warrants_spec_push(
            &codex_hook(unknown.clone(), "hook.codex.stop"),
            &ActorId::User,
            &write
        ));
        assert!(!event_warrants_spec_push(
            &claude_hook(unknown, "hook.claude.stop"),
            &ActorId::User,
            &write
        ));
        assert!(!event_warrants_spec_push(
            &codex_hook(worker.clone(), "hook.codex.permission_request"),
            &ActorId::User,
            &write
        ));
        assert!(!event_warrants_spec_push(
            &codex_hook(worker, "hook.codex.post_tool_use"),
            &ActorId::User,
            &write
        ));
        assert!(!event_warrants_spec_push(
            &Event::WaveDeleted {
                id: wave,
                cove_id: cove,
            },
            &ActorId::User,
            &write
        ));
    }

    /// #679 PR0-E — actor-matrix pin for task terminal events plus the
    /// request-kind exclusion. `event_warrants_spec_push_covers_push_allowlist`
    /// above pins the AiCodex/AiSpec rows; this pins the remaining actor
    /// variants (only `AiSpec` is excluded — everything else pushes) and
    /// that the two `*.worker_requested` kinds never push back to the spec
    /// regardless of actor.
    #[test]
    fn event_warrants_spec_push_task_actor_matrix_and_request_kinds_pin() {
        let cache = CardRoleCache::new();
        let wave = WaveId::from("w");
        let worker = CardId::from("worker");
        let spec = CardId::from("spec");
        cache.insert(worker.clone(), CardRole::Worker, wave.clone());
        cache.insert(spec.clone(), CardRole::Spec, wave.clone());
        let write = WriteContext::new(cache, crate::wave_cove_cache::WaveCoveCache::new());

        let completed = Event::TaskCompleted {
            idempotency_key: "done".into(),
            result: serde_json::Value::Null,
            artifacts: Vec::new(),
            agent_message: None,
        };
        let failed = Event::TaskFailed {
            idempotency_key: "fail".into(),
            reason: "boom".into(),
            agent_message: None,
        };
        // Every non-AiSpec actor warrants a push for task terminal events —
        // including the kernel dispatcher itself (its spawn-failure
        // `task.failed` fallback must wake the spec).
        for actor in [
            ActorId::User,
            ActorId::Kernel,
            ActorId::KernelDispatcher,
            ActorId::Plugin("p".into()),
            ActorId::AiClaude(worker.clone()),
        ] {
            assert!(
                event_warrants_spec_push(&completed, &actor, &write),
                "task.completed must push for actor {actor}"
            );
            assert!(
                event_warrants_spec_push(&failed, &actor, &write),
                "task.failed must push for actor {actor}"
            );
        }
        assert!(!event_warrants_spec_push(
            &completed,
            &ActorId::AiSpec(spec.clone()),
            &write
        ));

        // The two request kinds are dispatcher *inputs*, never spec pushes
        // — for any actor, including the spec that authored them.
        let codex_req = Event::CodexWorkerRequested {
            idempotency_key: "k".into(),
            goal: "g".into(),
            context: serde_json::Value::Null,
            acceptance_criteria: None,
            agent_message: None,
        };
        let terminal_req = Event::TerminalWorkerRequested {
            idempotency_key: "k".into(),
            cmd: "ls".into(),
            cwd: None,
            agent_message: None,
        };
        for actor in [
            ActorId::User,
            ActorId::KernelDispatcher,
            ActorId::AiSpec(spec.clone()),
            ActorId::AiCodex(worker.clone()),
        ] {
            assert!(
                !event_warrants_spec_push(&codex_req, &actor, &write),
                "codex.worker_requested must never push for actor {actor}"
            );
            assert!(
                !event_warrants_spec_push(&terminal_req, &actor, &write),
                "terminal.worker_requested must never push for actor {actor}"
            );
        }
    }

    /// #679 PR0-E — characterization golden for the event → harness
    /// observation mapping. Both the live push path and the boot-recovery
    /// replay (`harness::replay_harness_events_since`) funnel through
    /// `harness_observation_from_event`; PR5-8 must preserve this mapping
    /// byte-for-byte or consciously edit this pin.
    #[test]
    fn harness_observation_from_event_mapping_pin() {
        let wave = WaveId::from("wave-map");
        let worker = CardId::from("card-map");

        // task.completed — idempotency key + verbatim result.
        assert_eq!(
            harness_observation_from_event(
                &wave,
                &Event::TaskCompleted {
                    idempotency_key: "map-a".into(),
                    result: serde_json::json!({"ok": true, "n": 7}),
                    artifacts: vec![ArtifactRef::from("art-1")],
                    agent_message: Some("ignored".into()),
                }
            ),
            Some(HarnessObservation::TaskCompleted {
                idempotency_key: "map-a".into(),
                result: serde_json::json!({"ok": true, "n": 7}),
            })
        );

        // task.failed — the event's `reason` becomes the observation `error`.
        assert_eq!(
            harness_observation_from_event(
                &wave,
                &Event::TaskFailed {
                    idempotency_key: "map-b".into(),
                    reason: "boom".into(),
                    agent_message: None,
                }
            ),
            Some(HarnessObservation::TaskFailed {
                idempotency_key: "map-b".into(),
                error: "boom".into(),
            })
        );

        // wave.report_edited — body_after verbatim + its sha256 (golden hex
        // computed externally, NOT via the same sha256_hex helper).
        assert_eq!(
            harness_observation_from_event(
                &wave,
                &Event::WaveReportEdited {
                    wave_id: wave.clone(),
                    card_id: worker.clone(),
                    author: EditAuthor::User,
                    edit_id: "e".into(),
                    summary_before: String::new(),
                    summary_after: "s".into(),
                    body_before: "old".into(),
                    body_after: "loop-pin-body".into(),
                    agent_message: None,
                }
            ),
            Some(HarnessObservation::ReportEdited {
                wave_id: wave.clone(),
                body_sha256: "09b37878497ec46015d1913ba0dff1cd051ca244859c80f4a3fc14d88a4a9465"
                    .into(),
                body: "loop-pin-body".into(),
            })
        );

        // Stop hooks — exact kind discriminators map to WorkerHookStop.
        assert_eq!(
            harness_observation_from_event(
                &wave,
                &Event::CodexHook {
                    card_id: worker.clone(),
                    kind: "hook.codex.stop".into(),
                    hook_idempotency_key: "hook-c".into(),
                    payload: serde_json::Value::Null,
                }
            ),
            Some(HarnessObservation::WorkerHookStop {
                wave_id: wave.clone(),
                card_id: worker.clone(),
                kind: HarnessHookKind::CodexStop,
                idempotency_key: "hook-c".into(),
            })
        );
        assert_eq!(
            harness_observation_from_event(
                &wave,
                &Event::ClaudeHook {
                    card_id: worker.clone(),
                    kind: "hook.claude.stop".into(),
                    hook_idempotency_key: "hook-l".into(),
                    payload: serde_json::Value::Null,
                }
            ),
            Some(HarnessObservation::WorkerHookStop {
                wave_id: wave.clone(),
                card_id: worker.clone(),
                kind: HarnessHookKind::ClaudeStop,
                idempotency_key: "hook-l".into(),
            })
        );

        // Non-stop hooks and non-push kinds map to nothing.
        assert_eq!(
            harness_observation_from_event(
                &wave,
                &Event::CodexHook {
                    card_id: worker.clone(),
                    kind: "hook.codex.permission_request".into(),
                    hook_idempotency_key: "hook-p".into(),
                    payload: serde_json::Value::Null,
                }
            ),
            None
        );
        assert_eq!(
            harness_observation_from_event(
                &wave,
                &Event::CodexWorkerRequested {
                    idempotency_key: "k".into(),
                    goal: "g".into(),
                    context: serde_json::Value::Null,
                    acceptance_criteria: None,
                    agent_message: None,
                }
            ),
            None
        );
    }

    /// #313 round-2 (B3) — the per-wave push lock map must serialize
    /// concurrent acquisitions for the SAME wave (so boot takeover's
    /// `Dispatcher::push_lock` and the live `push_to_spec`'s lock cannot run
    /// the dedup-check-and-deliver body concurrently — which would lose
    /// events in the seed→insert window). DIFFERENT waves must remain
    /// independent so a slow takeover for wave A doesn't block live
    /// pushes for wave B.
    ///
    /// Models the `DashMap::entry(...).or_insert_with(Arc::new Mutex)` +
    /// `clone().lock_owned().await` pattern `Inner::acquire_push_lock` uses.
    #[tokio::test]
    async fn per_wave_push_lock_serializes_same_wave_runs_in_parallel_across_waves() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Same map shape as `Inner::push_locks`.
        let push_locks: DashMap<WaveId, Arc<tokio::sync::Mutex<()>>> = DashMap::new();
        let take_lock = |wave_id: &WaveId| -> Arc<tokio::sync::Mutex<()>> {
            push_locks
                .entry(wave_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };

        // Track concurrent occupancy. Same-wave: must never exceed 1.
        let in_flight_a = Arc::new(AtomicUsize::new(0));
        let max_in_flight_a = Arc::new(AtomicUsize::new(0));
        let wave_a = WaveId::from("wave-a");

        let mut handles = vec![];
        for i in 0..8 {
            let lock = take_lock(&wave_a);
            let in_flight = in_flight_a.clone();
            let max_in_flight = max_in_flight_a.clone();
            handles.push(tokio::spawn(async move {
                let _g = lock.lock_owned().await;
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                max_in_flight.fetch_max(now, Ordering::SeqCst);
                // Simulate the dedup-check-and-deliver body holding the
                // lock for a few yields (representative of `push_to_spec`'s
                // async work).
                tokio::task::yield_now().await;
                tokio::time::sleep(std::time::Duration::from_millis(2 * (i as u64 + 1))).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(
            max_in_flight_a.load(Ordering::SeqCst),
            1,
            "same-wave per-wave lock must serialize: observed concurrent holders"
        );

        // Different waves: independent locks → can run in parallel.
        let in_flight_total = Arc::new(AtomicUsize::new(0));
        let max_in_flight_total = Arc::new(AtomicUsize::new(0));
        let mut handles = vec![];
        for i in 0..6 {
            let wave: WaveId = format!("wave-parallel-{i}").into();
            let lock = take_lock(&wave);
            let in_flight = in_flight_total.clone();
            let max_in_flight = max_in_flight_total.clone();
            handles.push(tokio::spawn(async move {
                let _g = lock.lock_owned().await;
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                max_in_flight.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(15)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        // We expect parallelism > 1 across distinct wave keys (otherwise
        // the per-wave keying is broken). With 6 spawns and ~15ms each on a
        // multi-threaded runtime they should overlap routinely.
        assert!(
            max_in_flight_total.load(Ordering::SeqCst) > 1,
            "different-wave locks must allow parallel runs; observed serialization"
        );
    }
}
