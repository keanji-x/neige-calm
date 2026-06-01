//! Spec push delivery state, queueing, registry, and notification consumer.
//!
//! Process spawn/resume and socket ownership stay in `spec_appserver`; this
//! module owns the per-wave push handle, queue persistence, delivery decision,
//! registry, and consumer loop.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use serde_json::Value;
use tokio::process::Child;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tokio::time::Instant as TokioInstant;

use crate::codex_appserver::{CodexAppServer, InputItem, Notification, NotificationStream};
use crate::error::{CalmError, Result};
use crate::ids::WaveId;

/// #318 INV-4 (codex P1 follow-up) — how long [`build_handle_after_spawn_resume`]
/// waits for a lifecycle notification (`turn/started` or `turn/completed`)
/// to arrive on a `thread/resume`-d connection before assuming the server
/// is idle and promoting [`SpecPushPhase::Resumed`] → [`SpecPushPhase::TurnCompleted`].
/// This is a resume-phase reconciliation shim, not an app-server readiness
/// budget.
///
/// Why a timer is needed: `thread/resume` does NOT replay prior-boot
/// lifecycle notifications. If the server was *idle* between turns when
/// the kernel crashed, NO `turn/started` and NO `turn/completed` will
/// ever arrive on the resumed stream, so the consumer task's
/// `turn/completed`-triggered [`flush_push_queue`] will never run, and a
/// catch-up observation enqueued under `decide(Resumed) == Enqueue` would
/// be stuck in the in-memory queue indefinitely (until some unrelated
/// future external turn completes).
///
/// The timer is the synthetic "the server is idle, go ahead and flush"
/// reconcile until we consume the raw `thread.status` already present on
/// `thread/resume` responses. A later PR can turn that into a deterministic
/// split (`idle` → immediately flush; `active` → stay event-driven and wait
/// for `turn/completed`). 5 s is generous for a healthy mid-turn server to
/// emit its in-flight turn's `turn/completed` if it's about to land, while
/// still being short enough that the user-visible delay for a boot-takeover
/// catch-up push on a truly-idle thread is bounded.
///
/// **Trade-off**: if a prior-boot turn is genuinely mid-flight and
/// running longer than this budget, the timer fires first and the
/// resulting `turn/start` would be silently dropped by codex (the very
/// hazard `Resumed` was meant to prevent). Accepted as the lesser of
/// two evils: the alternative (no timer) loses the observation 100% of
/// the time on idle resumes; the timer loses it only on the rare
/// "mid-long-turn at crash" intersection. Consuming the `thread.status`
/// value from `thread/resume` would remove this trade-off.
pub(crate) const RESUMED_RECONCILE_BUDGET: Duration = Duration::from_secs(5);

/// Maximum wall-clock duration a single accepted spec turn may run before
/// the kernel treats the app-server notification stream as wedged and asks
/// codex to interrupt that turn. This is an absolute per-turn budget:
/// intermediate notifications and deltas deliberately do not reset it, so a
/// long but silent legitimate tool call is not penalized by an idle timer.
pub const MAX_TURN_DURATION: Duration = Duration::from_secs(30 * 60);

/// After the runtime watchdog sends `turn/interrupt`, the bounded time spent
/// draining notifications for the matching `turn/completed` with
/// `turn.status == "interrupted"`. If this budget elapses (or the interrupt
/// request itself fails), the remaining recovery is process-level respawn
/// via the registry/takeover path; that supervision hook is intentionally not
/// embedded in the per-wave consumer task.
pub const INTERRUPT_COMPLETION_BUDGET: Duration = Duration::from_secs(30);

/// Runtime watchdog timing used by the notification consumer. Production
/// uses [`MAX_TURN_DURATION`] and [`INTERRUPT_COMPLETION_BUDGET`]; tests can
/// pass short values through the `_with_watchdog_config` constructors.
#[derive(Debug, Clone, Copy)]
pub struct TurnWatchdogConfig {
    pub max_turn_duration: Duration,
    pub interrupt_completion_budget: Duration,
}

impl Default for TurnWatchdogConfig {
    fn default() -> Self {
        Self {
            max_turn_duration: MAX_TURN_DURATION,
            interrupt_completion_budget: INTERRUPT_COMPLETION_BUDGET,
        }
    }
}

/// Last-seen thread/turn lifecycle status the consumer task tracks for
/// PR3b to read. PR3a only *records* it; the dispatcher push path (PR3b)
/// will consult it to decide between `turn/start` / `turn/steer` /
/// `thread/inject_items` per the issue's decision rule.
///
/// Kept deliberately small: a coarse phase plus the most recent thread/turn
/// ids. The full status `Value` from `thread/status/changed` is not
/// retained — PR3b can subscribe to the live stream if it needs detail.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpecPushStatus {
    /// Coarse phase derived from the notification stream.
    pub phase: SpecPushPhase,
    /// Most recently observed thread id (from any thread/turn notification).
    pub last_thread_id: Option<String>,
    /// Most recently observed turn id (from `turn/started`/`turn/completed`).
    pub last_turn_id: Option<String>,
}

/// Coarse lifecycle phase tracked by the consumer task.
///
/// ## Why `Issuing` exists (B1 — flush-vs-push race)
///
/// `Idle`/`TurnCompleted` both mean "between turns" — a fresh `turn/start`
/// is safe. But the *decision* to issue and the *claim* of the right to
/// issue must be atomic under ONE status-lock acquisition, because TWO
/// independent code paths can issue a `turn/start`: a dispatcher
/// [`SpecPusher::push_observation`] AND the consumer task's
/// [`flush_push_queue`] on `turn/completed`. If both could observe "between
/// turns" and each issue, codex would receive two `turn/start`s and
/// **silently drop the second** (verified) — losing one set of
/// observations.
///
/// `Issuing` is the single-winner gate: the first caller to observe a
/// between-turns phase atomically flips it to `Issuing` and becomes THE
/// issuer; every other caller (and the flush loser) only enqueues / no-ops.
/// The winner drains the whole queue (plus, for the push case, its own
/// observation) into ONE `turn/start`. The server's `turn/started`
/// reconciles `Issuing`→`TurnRunning`; a `turn/start` error rolls
/// `Issuing`→`TurnCompleted` (and re-buffers).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SpecPushPhase {
    /// Empty-goal hot path: the app-server is initialized and the remote TUI
    /// is expected to fresh-start the codex thread. Until the first
    /// `turn/started`/`turn/completed` supplies the real thread id, dispatcher
    /// pushes are persisted + buffered but never issued.
    PendingThreadStart,
    /// No turn observed yet on this connection.
    #[default]
    Idle,
    /// A single winner has claimed the right to issue a `turn/start` (B1)
    /// and is mid-issue (the `turn/start` ack / `turn/started` has not yet
    /// landed). Treated like `TurnRunning` for the *decision* (enqueue /
    /// no-op) so no second issuer races in; reconciled to `TurnRunning` by
    /// the server's `turn/started`, or rolled back to `TurnCompleted` on a
    /// `turn/start` error.
    Issuing,
    /// A `turn/started` was seen and no later `turn/completed`.
    TurnRunning,
    /// The last lifecycle signal was `turn/completed`.
    TurnCompleted,
    /// #318 INV-4 (R2-B2) — the connection was `thread/resume`-d during a
    /// boot takeover and no lifecycle notification has yet arrived to
    /// reconcile it. We do not yet consume the raw `thread.status` returned
    /// by `thread/resume`, so this PR keeps the conservative event-driven
    /// posture: issuing a `turn/start` here while codex is still running the
    /// prior boot's turn would be silently dropped (verified; see [`decide`]
    /// / [`PushAction::Enqueue`]). PR3 can refine this by using
    /// `thread.status` (`idle` → immediately advance/flush, `active` → keep
    /// waiting for lifecycle). The first `turn/started` reconciles us to
    /// `TurnRunning`; the first `turn/completed` reconciles us to
    /// `TurnCompleted` (at which point the consumer task's
    /// [`flush_push_queue`] will drain anything that piled up in the
    /// meantime).
    Resumed,
    /// The runtime watchdog proved the active turn is not recoverable over
    /// the existing app-server connection: `turn/interrupt` failed, the
    /// stream closed while awaiting interrupted completion, or codex acked
    /// the interrupt but never emitted `turn/completed(status=interrupted)`.
    ///
    /// This is deliberately outside the running/enqueue states. The queue
    /// attached to this handle has no trustworthy consumer anymore; new
    /// observations must leave the durable watermark untouched so the
    /// process-level recovery supervisor can replay them through
    /// `register_and_catch_up` after reaping and resuming a fresh process.
    Wedged,
}

/// What [`SpecPushHandle::push_observation`] should do with a single
/// observation, given the connection's current [`SpecPushPhase`].
///
/// PR3b's delivery decision is encapsulated here as a pure function
/// ([`decide`]) so it is trivially unit-testable without a live
/// app-server. The rule is dictated by **empirically verified** codex
/// behavior (issue #293 PR3b probe against codex-cli):
///
///   * `turn/start` issued while a turn is *already running* returns an
///     OK ack with a fresh turn id **but the second turn's work never
///     executes** — no `turn/started`, no `turn/completed`; it is
///     silently dropped server-side. So we must NOT fire `turn/start`
///     while a turn is active.
///   * `turn/steer` is avoided too: its `expectedTurnId` is racy against
///     the live notification stream (the turn could complete between our
///     status read and the steer).
///
/// Therefore: deliver immediately only when the thread is idle / between
/// turns; otherwise enqueue and let the consumer task flush the queue on
/// the next `turn/completed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushAction {
    /// Idle / TurnCompleted — safe to start a turn right now.
    StartTurnNow,
    /// TurnRunning — queue the observation; the consumer flushes it on the
    /// next `turn/completed`.
    Enqueue,
    /// Wedged — do not enqueue into this handle. The process-level recovery
    /// supervisor owns replay from the durable event log.
    RejectWedged,
}

/// #313 problem #1 (B1) — outcome reported back to the dispatcher so the
/// **durable watermark** is only persisted for observations that codex
/// has actually received (in a `turn/start` we issued), not merely
/// buffered into the in-memory queue.
///
/// The dispatcher consumes this verbatim in `push_to_spec`:
///   * `Issued { max_envelope_id }` — `turn/start` succeeded. The
///     `max_envelope_id` is the highest envelope id among (drained queue
///     items + this push's observation). Persisting watermark =
///     `max_envelope_id` is safe: every id ≤ it was either previously
///     persisted or just rode this same turn/start.
///   * `Enqueued` — observation lives only in the in-memory queue;
///     dispatcher MUST NOT advance the durable watermark (a kernel crash
///     before the queue flushes would lose it; boot catch-up replays
///     `id > watermark` and would skip it). The watermark is bumped later
///     by the queue-flushed path via the [`WatermarkSink`] callback (see
///     [`SpecPushHandle::install_watermark_sink`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// Observation rode a successful `turn/start` (alone or coalesced with
    /// previously-queued items). Dispatcher persists watermark =
    /// `max_envelope_id` (highest id issued in this turn).
    Issued {
        /// Highest persisted `events.id` delivered in this coalesced turn.
        /// May be > the current push's envelope id (the drain pulled in
        /// items with higher ids — see flush below) OR < (the drain
        /// pulled in older items that were enqueued earlier; the new push
        /// is the max).
        max_envelope_id: i64,
    },
    /// Observation was buffered (a turn was already running / being issued).
    /// Dispatcher MUST NOT persist watermark — boot catch-up needs to
    /// replay it on crash before the flush.
    Enqueued,
}

/// Bounded channel depth for "this process is wedged; supervisor must reap
/// and resume" signals. One request per handle is enough: once a handle is in
/// [`SpecPushPhase::Wedged`], additional pushes are rejected rather than
/// producing more recovery work.
const RECOVERY_SIGNAL_CHANNEL_DEPTH: usize = 1;

/// Reason the watchdog gave up on in-process interrupt recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecRecoveryReason {
    /// JSON-RPC `turn/interrupt` itself failed.
    InterruptFailed,
    /// The notification stream closed while waiting for the interrupted
    /// `turn/completed`.
    StreamClosedAwaitingInterruptedCompletion,
    /// `turn/interrupt` was acked but the interrupted completion never
    /// arrived within `INTERRUPT_COMPLETION_BUDGET`.
    InterruptedCompletionTimedOut,
}

/// Request sent by the notification consumer to the runtime recovery
/// supervisor. The consumer intentionally carries no `AppState`; it only
/// reports the wave/thread/turn that wedged.
#[derive(Debug, Clone)]
pub struct SpecRecoveryRequest {
    pub wave_id: WaveId,
    pub thread_id: String,
    pub turn_id: String,
    pub reason: SpecRecoveryReason,
}

/// Sender half held by a [`NotificationConsumer`]. The receiver is owned by
/// the app-server registry supervisor, which has the `AppState`, card id,
/// env/settings, and per-wave locking context needed to reuse the boot
/// recovery path without bypassing invariants.
#[derive(Debug, Clone)]
pub struct SpecRecoverySignal {
    wave_id: WaveId,
    tx: mpsc::Sender<SpecRecoveryRequest>,
}

/// Create the consumer→supervisor recovery channel for one parked
/// app-server handle.
pub fn recovery_signal_channel(
    wave_id: WaveId,
) -> (SpecRecoverySignal, mpsc::Receiver<SpecRecoveryRequest>) {
    let (tx, rx) = mpsc::channel(RECOVERY_SIGNAL_CHANNEL_DEPTH);
    (SpecRecoverySignal { wave_id, tx }, rx)
}

/// Pure decision: given the connection's coarse [`SpecPushPhase`], should
/// a push observation start a turn immediately or be enqueued for the
/// next `turn/completed` flush? See [`PushAction`] for the rationale.
///
/// Kept free-standing + pure so the delivery policy is unit-tested in
/// isolation (no app-server, no async) — the issue calls for exactly this
/// table test.
pub fn decide(phase: SpecPushPhase) -> PushAction {
    match phase {
        // Between turns (or no turn yet) — a fresh `turn/start` runs. The
        // CALLER must, under the SAME status-lock acquisition, flip the
        // phase to `Issuing` before releasing the lock so it becomes the
        // single winner (B1); a concurrent caller then sees `Issuing` and
        // enqueues instead of issuing a second (silently-dropped)
        // `turn/start`.
        SpecPushPhase::Idle | SpecPushPhase::TurnCompleted => PushAction::StartTurnNow,
        // Empty-goal fresh-start window: no codex thread id exists yet.
        // Buffer; the notification consumer flushes once the TUI-created
        // thread reaches a safe between-turns lifecycle.
        SpecPushPhase::PendingThreadStart => PushAction::Enqueue,
        // A turn is mid-flight (`TurnRunning`) OR another caller has already
        // claimed the issue right (`Issuing`) — a `turn/start` now would be
        // silently dropped (verified) / would lose the single-winner race,
        // so buffer and let the winner / next `turn/completed` flush it.
        SpecPushPhase::TurnRunning | SpecPushPhase::Issuing => PushAction::Enqueue,
        // #318 INV-4 (R2-B2) — `thread/resume` does NOT prove the server is
        // between turns; a prior-boot turn could still be running on the
        // resumed thread. Issuing a `turn/start` now would be silently
        // dropped by codex (the same B1-style hazard), losing the catch-up
        // observation. Enqueue and let the consumer task's
        // `flush_push_queue` issue once the next `turn/completed` proves
        // we are between turns. If instead the server is already between
        // turns, the first push's `turn/started` reconciles `Resumed` →
        // `TurnRunning`, and the matching `turn/completed` flips us to
        // `TurnCompleted` and flushes — same delivery, one extra
        // notification round-trip.
        SpecPushPhase::Resumed => PushAction::Enqueue,
        // Layer B (#347): do NOT enqueue into a handle whose consumer has
        // already declared the app-server process wedged. Runtime recovery
        // replays from the durable watermark after reaping/resuming.
        SpecPushPhase::Wedged => PushAction::RejectWedged,
    }
}

/// Shared, cloneable handle onto the consumer-tracked status.
pub(crate) type SharedStatus = Arc<Mutex<SpecPushStatus>>;

/// Per-handle queue of observations buffered while a turn is running.
/// Flushed by the consumer task as a single coalesced `turn/start` on the
/// next `turn/completed`. `Arc<Mutex<…>>` because both the dispatcher
/// (enqueue side, via [`SpecPushHandle::push_observation`]) and the
/// consumer task (flush side) touch it.
///
/// #313 problem #1 (B1) — each entry remembers the persisted `events.id`
/// of the envelope that produced it. The dispatcher hands the id in via
/// `push_observation`; on flush, the consumer task reports
/// `max(envelope_id)` of the drained batch back to the dispatcher (via
/// [`WatermarkSink`]) so the durable `push_watermark` advances past the
/// just-delivered items. Without per-item ids the watermark would either
/// (a) advance prematurely on enqueue and lose data on crash-before-flush,
/// or (b) never advance for queued-then-flushed items.
///
/// #318 INV-3 (R2-B1) — the in-memory `VecDeque` is now a CACHE in front
/// of a durable `spec_push_queue` table. Every `QueuedObservation` whose
/// enqueue went through a [`QueuePersistSlot`]-installed handle also
/// carries the persisted `db_id`, so the flush path can DELETE the right
/// rows after a successful `turn/start`. A kernel crash between persist
/// and the flush leaves the rows; boot-takeover's
/// [`SpecPushHandle::rehydrate_queue_from_persist`] rebuilds the
/// `VecDeque` from the table before catch-up replay starts. Test paths
/// without a persist slot still use the in-memory cache alone (entries
/// have `db_id = None`).
pub(crate) type PushQueue = Arc<Mutex<VecDeque<QueuedObservation>>>;

/// Thread id known to the push handle. Normal non-empty/resume paths install
/// it at construction time. Empty-goal fresh-start handles begin as `None`
/// and the notification consumer fills it from the first TUI-driven turn
/// lifecycle notification.
pub(crate) type ThreadIdSlot = Arc<Mutex<Option<String>>>;

/// One queued observation: the envelope id (so the flush path can report
/// the max delivered id to the dispatcher) plus the rendered text codex
/// will receive, plus — when a [`QueuePersistSlot`] is installed on the
/// handle — the `spec_push_queue.id` row id assigned at persist-time so
/// the flush path can dequeue it from the durable store after a
/// successful `turn/start` (#318 INV-3).
#[derive(Debug, Clone)]
pub(crate) struct QueuedObservation {
    pub(crate) envelope_id: i64,
    pub(crate) text: String,
    /// `Some(row_id)` when the entry was persisted via the
    /// [`QueuePersist::enqueue`] callback; `None` for entries that only
    /// live in the in-memory cache (test paths that skip the slot, or
    /// the brief window after a `turn/start` error re-buffers a drained
    /// batch — those rows remain in the durable store under their
    /// original ids so the requeue is purely the in-memory side).
    pub(crate) db_id: Option<i64>,
}

/// #318 INV-3 — closure surface the dispatcher installs on a
/// [`SpecPushHandle`] so the queue paths can persist + dequeue
/// observations through the durable `spec_push_queue` table without
/// importing `crate::Repo` into this module. Mirrors the
/// [`WatermarkSink`] callback pattern: the install site
/// (`routes/waves.rs::spawn_push_appserver` for create-wave,
/// `lib.rs::register_and_catch_up` for boot-takeover) captures `repo` +
/// `card_id` at handle-construction time and builds the closures.
///
/// Three operations:
///   * `enqueue(envelope_id, text) -> Option<i64>` — persist one row,
///     return the assigned `spec_push_queue.id`. `None` on a repo
///     error (logged at the install site / inside the closure) so the
///     in-memory cache still receives the entry — at worst, the next
///     boot has nothing extra to rehydrate, matching today's
///     in-memory-only baseline.
///   * `dequeue(ids)` — batch-delete the rows for a successful flush;
///     no-op on empty input.
///   * `list()` — read every pending row for the handle's card id, used
///     once by [`SpecPushHandle::rehydrate_queue_from_persist`].
pub struct QueuePersist {
    pub enqueue: QueuePersistEnqueueFn,
    pub dequeue: QueuePersistDequeueFn,
    pub list: QueuePersistListFn,
}

/// One-row INSERT into the durable `spec_push_queue`. `Some(row_id)` on
/// success, `None` on a repo error (the install-site closure logs + maps
/// the error to `None`, so the in-memory cache still receives the entry).
pub type QueuePersistEnqueueFn =
    Arc<dyn Fn(i64, String) -> futures_util::future::BoxFuture<'static, Option<i64>> + Send + Sync>;

/// Batch DELETE by row id. Empty input is a no-op at the closure layer.
pub type QueuePersistDequeueFn =
    Arc<dyn Fn(Vec<i64>) -> futures_util::future::BoxFuture<'static, ()> + Send + Sync>;

/// SELECT every pending row for this handle's card id, as
/// `(row_id, envelope_id, text)` in id-ASC order — the same order the
/// in-memory `VecDeque` was filled in.
pub type QueuePersistListFn = Arc<
    dyn Fn() -> futures_util::future::BoxFuture<'static, Vec<(i64, i64, String)>> + Send + Sync,
>;

/// `Arc<Mutex<Option<QueuePersist>>>` so the handle can construct itself
/// without a persist slot (the boot/create paths don't have the repo
/// handle in scope at construction time) and the dispatcher installs it
/// later via [`SpecPushHandle::install_queue_persist`]. `None` is the
/// test-path posture, matching [`WatermarkSinkSlot`].
pub(crate) type QueuePersistSlot = Arc<Mutex<Option<Arc<QueuePersist>>>>;

/// Callback installed on a [`SpecPushHandle`] so the notification consumer
/// can persist the TUI-created thread id and clear the empty-goal bootstrap
/// marker once a turn lifecycle proves codex has created a resumable rollout
/// for that thread.
pub type InitialPromptReadySink =
    Arc<dyn Fn(String) -> futures_util::future::BoxFuture<'static, ()> + Send + Sync>;

/// `Arc<Mutex<Option<InitialPromptReadySink>>>` mirrors the watermark sink
/// install pattern: construction is transport-only, then the boot/create
/// site installs the repo-backed side effect before parking the handle.
pub(crate) type InitialPromptReadySinkSlot = Arc<Mutex<Option<InitialPromptReadySink>>>;

/// Callback the dispatcher installs on a [`SpecPushHandle`] so the
/// queue-flush path (which runs from the consumer task on `turn/completed`)
/// can persist the durable `push_watermark` for envelope ids that were
/// only delivered out of the queue.
///
/// Boxed because it captures the dispatcher's repo handle + the spec
/// card id at handle-construction time; we never want to import
/// `crate::Repo` into `spec_appserver.rs`.
///
/// Called with the highest `events.id` actually delivered in a successful
/// `turn/start`. Failure (e.g. SQLite write error) is the implementation's
/// responsibility to log — the flush path is fire-and-forget here.
pub type WatermarkSink =
    Arc<dyn Fn(i64) -> futures_util::future::BoxFuture<'static, ()> + Send + Sync>;

/// `Arc<Mutex<Option<WatermarkSink>>>` so the handle can construct itself
/// without a sink (the boot/create path doesn't have the repo handle in
/// scope at construction time) and the dispatcher installs it later via
/// [`SpecPushHandle::install_watermark_sink`]. `Option` (not "always
/// present"): test paths (`fake_handle()`) skip it; on those, queue flushes
/// simply don't persist a watermark — fine, because tests don't run
/// dispatcher-side persistence assertions.
pub(crate) type WatermarkSinkSlot = Arc<Mutex<Option<WatermarkSink>>>;

/// Everything the kernel owns for one spec card's push channel.
///
/// Dropping this:
///   * sends `SIGTERM` to the child's **process group** (`kill(-pgid)`) —
///     this is what reaps the *native* `codex app-server` child that the
///     `node` launcher forks (see the module doc; `kill_on_drop` alone
///     only kills the launcher and leaks the native child),
///   * `kill_on_drop(true)` additionally SIGKILLs the launcher (belt-and-
///     suspenders), and
///   * aborts the [`NotificationStream`] consumer task (`consumer` is a
///     [`JoinHandle`] — its `Drop` does NOT abort, so we abort explicitly
///     in [`SpecPushHandle::drop`]).
///
/// The [`CodexAppServer`] client itself aborts its background reader task
/// on drop (see PR2), so tearing down the handle leaves no orphan tasks.
///
/// For a *graceful* teardown the registry path
/// ([`crate::terminal_sweeper::reap_spec_push`]) escalates SIGTERM → grace
/// → SIGKILL on the group and removes the socket/dir; `Drop` is the
/// best-effort safety net for every other path (mid-sequence errors,
/// dropped replaced handles, test teardown).
pub struct SpecPushHandle {
    /// The kernel-owned `codex app-server` child (the `node` launcher).
    /// `kill_on_drop(true)` is set at spawn as a belt-and-suspenders
    /// reaper of the launcher; the load-bearing reap is the group kill via
    /// [`pgid`](Self::pgid).
    pub child: Child,
    /// Process-group id of the launcher (`== child.id()` because the child
    /// is spawned as a group leader via `process_group(0)`). Teardown
    /// signals `kill(-pgid, …)` to reap the launcher *and* the native
    /// `codex app-server` child it forks. Persisted on the spec-card
    /// payload (`appserver_pgid`) for boot-time crash recovery.
    pub pgid: i32,
    /// #318 INV-5 (R3-B1) — `starttime` (clock-ticks since boot) of the
    /// launcher pid captured at spawn from `/proc/<pid>/stat`. Persisted
    /// on the spec-card payload as `appserver_start_time` alongside
    /// `appserver_pgid` + `appserver_boot_id`. The boot-recovery path
    /// calls [`verify_owned_pid`] with `(pgid, this stamp, boot_id)`
    /// BEFORE `signal_process_group(pgid, …)` so a recycled pid (post-
    /// reboot OR mid-boot recycle) cannot route the SIGTERM/SIGKILL to
    /// an unrelated process.
    ///
    /// `None` only on non-Linux targets / a `/proc` read failure at
    /// spawn time (test fixtures on macOS, transient ENOENT) — the
    /// boot-recovery path conservatively skips the kill when the
    /// persisted stamp is absent, same as today's mismatch behavior.
    pub start_time: Option<u64>,
    /// #318 INV-5 (R3-B1) — kernel boot UUID captured at spawn from
    /// `/proc/sys/kernel/random/boot_id`. Persisted alongside
    /// `appserver_start_time` so the boot-recovery path can distinguish
    /// "same kernel boot, just a kernel restart" from "host rebooted —
    /// every pid from the prior boot is dead". A `boot_id` mismatch
    /// short-circuits [`verify_owned_pid`] to `false` regardless of
    /// `start_time` (the prior boot's process namespace is gone).
    ///
    /// `None` only on non-Linux / a `/proc` read failure — same
    /// conservative-skip-the-kill posture as a missing stamp.
    pub boot_id: Option<String>,
    /// Programmatic client connected to `child` over WS-over-UDS. PR3b
    /// will call `turn_start`/`turn_steer`/`inject_items` on this.
    pub client: Arc<CodexAppServer>,
    /// The thread id turn #1 ran on. Persisted on the spec card payload as
    /// `codex_thread_id`; the `--remote` TUI resumes it. `None` means an
    /// empty-goal handle is waiting for the TUI to fresh-start a thread.
    pub thread_id: Option<String>,
    pub(crate) thread_id_slot: ThreadIdSlot,
    /// The listen socket path (`<data_dir>/appserver/<card_id>/app.sock`).
    pub sock: PathBuf,
    /// Consumer task draining the notification stream (status tracking +
    /// approval-shape warning + PR3b push-queue flush on `turn/completed`).
    /// Aborted on drop.
    pub(crate) consumer: JoinHandle<()>,
    /// #318 INV-4 (codex P1 follow-up) — optional reconcile task spawned
    /// only by the [`build_handle_after_spawn_resume`] path. After
    /// [`RESUMED_RECONCILE_BUDGET`] it atomically promotes
    /// [`SpecPushPhase::Resumed`] → [`SpecPushPhase::TurnCompleted`] (only
    /// if still `Resumed` — a real lifecycle notification reconciling the
    /// phase aborts the promotion via the CAS) and runs one
    /// [`flush_push_queue`] so an idle-resume case (no lifecycle
    /// notifications ever arrive) doesn't strand a catch-up observation in
    /// the in-memory queue forever. `None` on the spawn path (which seeds
    /// `Idle` via `turn/started`'s normal reconciliation). Aborted on
    /// drop alongside `consumer`.
    pub(crate) resume_reconciler: Option<JoinHandle<()>>,
    /// Shared status the consumer task writes; PR3b reads it.
    pub(crate) status: SharedStatus,
    /// PR3b push queue. Observations buffered by
    /// [`push_observation`](Self::push_observation) while a turn is running;
    /// the consumer task drains them into one coalesced `turn/start` on the
    /// next `turn/completed`. Shared with the consumer task.
    pub(crate) queue: PushQueue,
    /// #313 problem #1 (B1) — durable watermark persister. The consumer
    /// task's [`flush_push_queue`] calls this with the max envelope id of
    /// the just-flushed batch so the durable `push_watermark` advances past
    /// items that were ONLY delivered out of the queue (the dispatcher
    /// itself never sees those items hit codex).
    ///
    /// Installed post-construction by the dispatcher
    /// ([`install_watermark_sink`](Self::install_watermark_sink)). `None` on
    /// the brief window before it lands and in tests that don't exercise
    /// the persistence path.
    pub(crate) watermark_sink: WatermarkSinkSlot,
    /// #318 INV-3 (R2-B1) — durable enqueue/dequeue callbacks installed
    /// alongside the watermark sink. When `Some`, every `Enqueue`-arm
    /// observation is persisted to `spec_push_queue` BEFORE the
    /// in-memory `push_back` (so a crash between persist and the next
    /// flush leaves the row for boot-takeover to rehydrate). `None` on
    /// the brief window before install + in test paths that skip it.
    ///
    /// Installed via [`install_queue_persist`](Self::install_queue_persist)
    /// at the same sites that install the watermark sink.
    pub(crate) queue_persist: QueuePersistSlot,
    /// Persists the TUI-created thread id and clears
    /// `appserver_needs_initial_prompt` after the first observed turn
    /// lifecycle on this thread. This is intentionally independent from
    /// push watermark advancement because manual TUI input creates a rollout
    /// without touching the push channel.
    pub(crate) initial_prompt_ready_sink: InitialPromptReadySinkSlot,
}

/// PR3b — a cheaply-cloneable handle onto just the parts of a
/// [`SpecPushHandle`] that [`push_observation`](SpecPusher::push_observation)
/// needs (all `Arc`-shared / owned `String`). The dispatcher obtains one via
/// [`SpecPushRegistry::pusher`] without holding the registry's `DashMap`
/// guard across the `async` delivery — extracting these `Arc`s is a cheap
/// sync clone under the brief guard, after which the guard is released.
#[derive(Clone)]
pub struct SpecPusher {
    pub(crate) client: Arc<CodexAppServer>,
    pub(crate) thread_id_slot: ThreadIdSlot,
    pub(crate) status: SharedStatus,
    pub(crate) queue: PushQueue,
    /// Shared with the parent handle (`Arc<Mutex<Option<…>>>`). Read only
    /// by the consumer task's flush path; pushers carry it so the queue
    /// itself stays self-contained alongside its persistence callback.
    #[allow(dead_code)]
    pub(crate) watermark_sink: WatermarkSinkSlot,
    /// #318 INV-3 — shared with the parent handle. The `Enqueue` arm of
    /// [`push_observation`](Self::push_observation) reads this and
    /// (when populated) persists to `spec_push_queue` before the
    /// in-memory `push_back`. The `StartTurnNow` winner's drain reads
    /// it to dequeue persisted ids after a successful coalesced
    /// `turn/start`.
    pub(crate) queue_persist: QueuePersistSlot,
}

impl SpecPusher {
    async fn thread_id(&self) -> Option<String> {
        self.thread_id_slot.lock().await.clone()
    }

    async fn thread_id_for_log(&self) -> String {
        self.thread_id()
            .await
            .unwrap_or_else(|| "<pending-thread-start>".to_string())
    }

    /// Snapshot the consumer-tracked status via the shared `Arc` (N2 — used
    /// by [`SpecPushRegistry::status`] for a non-destructive read).
    pub async fn status(&self) -> SpecPushStatus {
        self.status.lock().await.clone()
    }

    /// PR3b — deliver a single observation to the spec's codex thread.
    ///
    /// Consults the consumer-tracked [`SpecPushPhase`] via [`decide`] under
    /// ONE status-lock acquisition (B1 — single-winner issuance):
    ///
    ///   * [`PushAction::StartTurnNow`] (Idle / TurnCompleted) — this caller
    ///     is the single winner. **Before releasing the status lock** it
    ///     flips the phase to [`SpecPushPhase::Issuing`], which makes every
    ///     concurrent `push_observation` AND the consumer's
    ///     [`flush_push_queue`] enqueue / no-op instead of issuing a second
    ///     `turn/start` (codex silently drops a `turn/start` issued while a
    ///     turn is active — verified). The winner then **drains the entire
    ///     queue and appends its own observation**, issuing exactly ONE
    ///     coalesced `turn/start` with all of them. The server's
    ///     `turn/started` reconciles `Issuing`→`TurnRunning`; a `turn/start`
    ///     error rolls `Issuing`→`TurnCompleted` and re-buffers the drained
    ///     items (front of queue, order preserved) so the next
    ///     `turn/completed` retries them.
    ///   * [`PushAction::Enqueue`] (TurnRunning / Issuing) — push the text
    ///     onto the per-handle queue; the winner (this cycle's issuer) or the
    ///     consumer task flushes the whole queue as one coalesced
    ///     `turn/start` on the next `turn/completed`. After the append, the
    ///     status is re-checked: if a `turn/completed` raced past us while
    ///     `persist_one` was awaiting the DB insert, the consumer's flush
    ///     drained an empty queue and walked the phase to `TurnCompleted` —
    ///     stranding our just-appended row. The Enqueue arm drives an
    ///     idempotent `flush_pending` in that case so nothing is left
    ///     undelivered (#325 round-2 P1).
    ///
    /// Errors only on the immediate-`turn/start` transport path (a queued
    /// observation can't fail here — its delivery is the consumer task's
    /// job). The caller (dispatcher) treats an error as a warn-and-move-on.
    /// There is no pull backstop anymore (#293 cutover); a dropped push
    /// means the observation may be lost.
    ///
    /// ## Lock ordering (deadlock-free)
    ///
    /// This fn never holds the status lock and the queue lock at the same
    /// time: it takes status (decide + flip), releases it, then takes queue
    /// (drain + append), releases it, then issues. The Enqueue arm's
    /// post-append `status.lock()` is also taken alone (released before
    /// `flush_pending` runs, which itself acquires the same lock).
    /// [`flush_push_queue`] uses the same status-then-queue, never-nested
    /// order. Preserving this keeps the path deadlock-free (the review
    /// confirmed today's code is).
    pub async fn push_observation(&self, envelope_id: i64, text: &str) -> Result<PushOutcome> {
        // Decide + (winner only) claim the issue right atomically under ONE
        // status-lock acquisition: a between-turns phase is flipped to
        // `Issuing` before the lock is released, so exactly one caller wins
        // and every other caller (and the flush) enqueues / no-ops.
        let action = {
            let mut g = self.status.lock().await;
            let action = decide(g.phase);
            if action == PushAction::StartTurnNow {
                g.phase = SpecPushPhase::Issuing;
            }
            action
        };
        if action == PushAction::RejectWedged {
            let thread_id = self.thread_id_for_log().await;
            return Err(CalmError::CodexAppServer(format!(
                "spec app-server thread {} is wedged; runtime recovery will replay from durable watermark",
                thread_id
            )));
        }

        // #318 INV-3 — persist-first. The durable `spec_push_queue` row goes
        // in BEFORE we either issue or enqueue, so a crash anywhere between
        // here and (a) `turn/start` success / (b) consumer-task flush success
        // leaves a recoverable row that boot-takeover's
        // `rehydrate_queue_from_persist` will surface to the next process.
        //
        // The `Enqueue` arm's contract — `Ok(Enqueued)` is the system's
        // promise that the observation will be delivered — is the load-
        // bearing case (the dispatcher cooperatively withholds the
        // `push_watermark` on `Enqueued`, PR #315 PR4 B1, so the events-log
        // replay is a safety net; INV-3 says the queue must also hold its
        // own durability rather than transitively borrow that net).
        //
        // The `StartTurnNow` arm persists too so a `turn/start` failure that
        // re-buffers the just-issued observation has the same recovery
        // guarantee as the items that were already in the queue.
        //
        // `None` from `persist_one` is the no-slot case (test paths that
        // skip the install + the brief window before the dispatcher
        // installs the slot in production); the in-memory cache still
        // receives the entry, matching pre-fix posture.
        let db_id = self.persist_one(envelope_id, text).await;

        match action {
            PushAction::StartTurnNow => {
                let Some(thread_id) = self.thread_id().await else {
                    // Defensive: `PendingThreadStart` should have selected
                    // the Enqueue arm, but if a future phase transition races
                    // ahead of the thread-id fill, keep the INV-3 promise by
                    // buffering instead of issuing a malformed `turn/start`.
                    self.queue.lock().await.push_back(QueuedObservation {
                        envelope_id,
                        text: text.to_string(),
                        db_id,
                    });
                    let mut g = self.status.lock().await;
                    if g.phase == SpecPushPhase::Issuing {
                        g.phase = SpecPushPhase::PendingThreadStart;
                    }
                    tracing::debug!(
                        envelope_id,
                        db_id,
                        "spec push: thread id not ready after issue claim — buffered observation"
                    );
                    return Ok(PushOutcome::Enqueued);
                };
                // We are the single winner. Drain the whole queue and append
                // our own observation so nothing buffered before this drain
                // is lost — it all rides one `turn/start`. (Anything
                // enqueued AFTER this drain waits for the next cycle, which
                // is correct: it arrived while a turn is being issued.)
                let mut items: Vec<QueuedObservation> = {
                    let mut q = self.queue.lock().await;
                    q.drain(..).collect()
                };
                items.push(QueuedObservation {
                    envelope_id,
                    text: text.to_string(),
                    db_id,
                });
                // #313 B1: durable watermark must advance to the MAX
                // envelope id we are about to deliver, not just the
                // current push's id. The drained items were enqueued
                // earlier (typically lower ids), but we still compute
                // `max` defensively so the contract holds even if a
                // future scheduler ordering or out-of-order persistence
                // sent a higher id into the queue first.
                let max_envelope_id = items
                    .iter()
                    .map(|o| o.envelope_id)
                    .max()
                    .unwrap_or(envelope_id);
                let coalesced = items
                    .iter()
                    .map(|o| o.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                tracing::debug!(
                    thread_id = %thread_id,
                    count = items.len(),
                    max_envelope_id,
                    "spec push: winner issuing coalesced turn/start (queue drained + new observation)"
                );
                if let Err(e) = self
                    .client
                    .turn_start(&thread_id, vec![InputItem::text(&coalesced)])
                    .await
                {
                    // Roll the `Issuing` claim back and re-buffer the drained
                    // items (front, order preserved) so a later
                    // `turn/completed` flush retries them. The persisted
                    // rows stay — `flush_push_queue` (or the next boot's
                    // rehydrate) will deliver and then dequeue them.
                    //
                    // #318 INV-3 db_id interaction: each re-buffered
                    // `QueuedObservation` carries its original `db_id`
                    // (the row was persisted before this `turn/start`
                    // attempt — see `persist_one` above). We deliberately
                    // do NOT call `dequeue_many` on the items: the rows
                    // are still pending delivery, so the next successful
                    // flush will dequeue them via the same id. The
                    // in-memory re-buffer + on-disk persistence therefore
                    // stay 1:1 — a kernel crash here still leaves a
                    // recoverable `spec_push_queue` row per drained item,
                    // matching the steady-state Enqueue-arm posture.
                    {
                        let mut q = self.queue.lock().await;
                        for obs in items.into_iter().rev() {
                            q.push_front(obs);
                        }
                    }
                    let mut g = self.status.lock().await;
                    if g.phase == SpecPushPhase::Issuing {
                        g.phase = SpecPushPhase::TurnCompleted;
                    }
                    return Err(e);
                }
                // #318 INV-3 — `turn/start` succeeded; dequeue every
                // persisted row in this coalesced batch. Anything without
                // a `db_id` (test paths / no-slot windows) is naturally
                // skipped by `dequeue_many`.
                self.dequeue_many(items.iter().filter_map(|o| o.db_id).collect())
                    .await;
                Ok(PushOutcome::Issued { max_envelope_id })
            }
            PushAction::Enqueue => {
                let thread_id = self.thread_id_for_log().await;
                self.queue.lock().await.push_back(QueuedObservation {
                    envelope_id,
                    text: text.to_string(),
                    db_id,
                });
                tracing::debug!(
                    thread_id = %thread_id,
                    envelope_id,
                    db_id,
                    "spec push: turn active / being issued — enqueued observation for flush"
                );
                // #325 round-2 P1 — close the persist-await race window.
                // `action` was fixed to `Enqueue` BEFORE the `persist_one`
                // await above. If a `turn/completed` arrived while
                // `persist_one` was awaiting the DB insert, the consumer
                // task's `flush_push_queue` may have already drained the
                // queue (saw it empty, since our row wasn't appended yet)
                // and walked the phase to `TurnCompleted` — at which point
                // NO future `turn/completed` can be expected to flush our
                // just-appended entry, and it would sit stranded until an
                // unrelated live event or process restart nudged the
                // queue.
                //
                // Re-acquire the status lock AFTER the append. If the
                // phase is no longer in {Issuing, TurnRunning}, drive
                // `flush_pending` to deliver the entry we just buffered.
                // `flush_pending` is idempotent (it's a no-op when
                // another issuer already owns the cycle or the queue is
                // empty), so the worst case is a redundant call when a
                // new `Issuing` claim landed in the gap — harmless. The
                // status mutex is released before `flush_pending` runs
                // (it takes the same lock itself), keeping the
                // status-then-queue ordering and avoiding any
                // re-entrancy.
                let needs_flush = {
                    let g = self.status.lock().await;
                    !matches!(
                        g.phase,
                        SpecPushPhase::PendingThreadStart
                            | SpecPushPhase::Issuing
                            | SpecPushPhase::TurnRunning
                    )
                };
                if needs_flush {
                    tracing::debug!(
                        thread_id = %thread_id,
                        envelope_id,
                        "spec push: persist-await race detected (phase walked past Issuing/TurnRunning while persist_one awaited); driving flush_pending to avoid stranding the enqueued row"
                    );
                    self.flush_pending().await;
                }
                Ok(PushOutcome::Enqueued)
            }
            PushAction::RejectWedged => unreachable!("handled before persistence"),
        }
    }

    /// #318 INV-3 — small wrapper around the [`QueuePersist::enqueue`] slot
    /// so the call sites (`push_observation`'s two arms, and the consumer
    /// task's never-reached "fresh enqueue" path) read as one line. Returns
    /// `Some(row_id)` on success and `None` either when no slot is
    /// installed (test paths / pre-install window) or when the persist
    /// callback itself returned `None` (the install-site closure swallowed
    /// the repo error). A `None` here means "this entry is in-memory only";
    /// the in-memory cache still receives it (matching pre-fix posture),
    /// at worst leaving the next boot with nothing extra to rehydrate.
    async fn persist_one(&self, envelope_id: i64, text: &str) -> Option<i64> {
        let persist = self.queue_persist.lock().await.clone()?;
        (persist.enqueue)(envelope_id, text.to_string()).await
    }

    /// #318 INV-3 — batch counterpart of [`persist_one`] for the dequeue
    /// side. Empty input is a no-op; missing slot is a no-op (and a
    /// `debug_assert!` would fire in production install paths since the
    /// dispatcher installs both watermark + persist atomically — but we
    /// don't assert here because the test fakes legitimately skip the
    /// slot).
    async fn dequeue_many(&self, ids: Vec<i64>) {
        if ids.is_empty() {
            return;
        }
        let persist = match self.queue_persist.lock().await.clone() {
            Some(p) => p,
            None => return,
        };
        (persist.dequeue)(ids).await;
    }
}

impl SpecPushHandle {
    /// Snapshot the consumer-tracked status. Cheap; clones a small struct
    /// under a short-held lock.
    pub async fn status(&self) -> SpecPushStatus {
        self.status.lock().await.clone()
    }

    /// PR3b — a cheaply-cloneable [`SpecPusher`] over this handle's shared
    /// parts. The dispatcher uses this to deliver observations without
    /// holding the registry guard across the async push.
    pub fn pusher(&self) -> SpecPusher {
        SpecPusher {
            client: self.client.clone(),
            thread_id_slot: self.thread_id_slot.clone(),
            status: self.status.clone(),
            queue: self.queue.clone(),
            watermark_sink: self.watermark_sink.clone(),
            queue_persist: self.queue_persist.clone(),
        }
    }

    /// PR3b — convenience delegate so callers holding a `&SpecPushHandle`
    /// (and the unit tests) can push without first cloning a [`SpecPusher`].
    pub async fn push_observation(&self, envelope_id: i64, text: &str) -> Result<PushOutcome> {
        self.pusher().push_observation(envelope_id, text).await
    }

    /// #313 problem #1 (B1) — install the dispatcher-side persistence
    /// callback used by the queue-flush path. Called by the dispatcher
    /// (`Inner::push_to_spec`) right after the handle is registered, so
    /// the consumer task's [`flush_push_queue`] can persist the durable
    /// watermark for items it delivers out of the queue.
    ///
    /// Installing twice replaces the previous sink (the second
    /// dispatcher's repo handle wins). Production calls this exactly once
    /// per handle.
    pub async fn install_watermark_sink(&self, sink: WatermarkSink) {
        *self.watermark_sink.lock().await = Some(sink);
    }

    /// #313 problem #1 round-3 (B2 / N7) — probe whether a
    /// [`WatermarkSink`] has been installed on this handle. Used by
    /// `debug_assert!` at every site that constructs a handle and parks
    /// it in [`SpecPushRegistry`], so a future refactor that forgets to
    /// install the sink fails fast in dev/test builds.
    ///
    /// Production callers must NOT branch on this — `Option<WatermarkSink>`
    /// is internal to the queue-flush bookkeeping; an uninstalled sink
    /// silently no-ops on flush (the bug the assertion guards against).
    pub async fn has_watermark_sink(&self) -> bool {
        self.watermark_sink.lock().await.is_some()
    }

    /// #318 INV-3 — install the durable enqueue/dequeue/list callbacks.
    /// Called by both production install sites
    /// (`routes/waves.rs::spawn_push_appserver` and
    /// `lib.rs::register_and_catch_up`) right after `install_watermark_sink`,
    /// so a push landing immediately after registration has both the
    /// persist path AND the watermark path available.
    ///
    /// Installing twice replaces the previous slot (latest dispatcher
    /// wins, symmetric with [`install_watermark_sink`]).
    pub async fn install_queue_persist(&self, persist: QueuePersist) {
        *self.queue_persist.lock().await = Some(Arc::new(persist));
    }

    /// #318 INV-3 — debug-assert symmetric with [`has_watermark_sink`].
    /// Production callers must NOT branch on this — an absent slot means
    /// the in-memory `VecDeque` is the only durability surface (today's
    /// pre-fix behavior, intentional only on test paths).
    pub async fn has_queue_persist(&self) -> bool {
        self.queue_persist.lock().await.is_some()
    }

    /// Install the callback used by the notification consumer to persist
    /// the TUI-created thread id and clear the empty-goal bootstrap marker
    /// when it observes the first turn lifecycle for this thread.
    pub async fn install_initial_prompt_ready_sink(&self, sink: InitialPromptReadySink) {
        *self.initial_prompt_ready_sink.lock().await = Some(sink);
    }

    /// #318 INV-3 — rehydrate the in-memory `VecDeque` from the durable
    /// `spec_push_queue` rows. Called by boot-takeover's
    /// `register_and_catch_up` AFTER `install_queue_persist`, BEFORE
    /// catch-up replay starts, so observations a prior process enqueued
    /// but didn't flush are still available for the next `turn/completed`
    /// flush.
    ///
    /// The current in-memory cache is left in place — typical case is
    /// "freshly-constructed handle whose cache is empty", but a defensive
    /// `extend_back` is correct on any cache state (we never write
    /// duplicate rows: the durable rows weren't in the in-memory cache
    /// before this call, and any in-memory entry added after this call
    /// goes through the live enqueue path).
    ///
    /// No-op when no persist slot is installed (returns an empty `Vec`).
    ///
    /// ## Watermark filtering (#325 round-2 P2)
    ///
    /// `watermark` is the durable `push_watermark` for this card. Any row
    /// whose `envelope_id <= watermark` has ALREADY been delivered to
    /// codex (the watermark advances on flush success — see
    /// `flush_push_queue`), so its persistence is stale: the flush
    /// succeeded, the watermark advanced, but the process crashed before
    /// the `dequeue` write committed (or the dequeue write itself failed
    /// — the install closure logs and swallows; see
    /// `dispatcher::queue_persist_for`). Without filtering, those rows
    /// would be re-pushed into the in-memory queue and `flush_pending`
    /// (or the next live `turn/completed`) would redeliver them to codex
    /// — bypassing the durable watermark that `events_since(watermark)`
    /// itself correctly skips. We instead delete those rows in the same
    /// rehydrate call (via the installed `dequeue` callback) so a
    /// future boot won't see them either, and return only the LIVE
    /// envelope_ids (`> watermark`).
    ///
    /// ## Return value (#325 fix)
    ///
    /// Returns the persisted `envelope_id`s the rehydrated rows carried, in
    /// the same FIFO order they were re-pushed into the in-memory queue —
    /// after stale-row filtering. Boot-takeover's catch-up replay path
    /// (`lib.rs::register_and_catch_up`) feeds these ids into a skip-set
    /// so the subsequent `events_since(watermark)` replay doesn't deliver
    /// the SAME envelope a second time: a crash between `Ok(Enqueued)` and
    /// the consumer's `turn/completed` flush leaves the durable
    /// `push_watermark` BELOW the queued envelope's id (by design — see
    /// PR #315 PR4 B1), so `events_since(watermark)` and the rehydrated
    /// rows overlap on exactly that envelope. Without dedup, the
    /// `StartTurnNow` triggered by the catch-up call would drain the
    /// rehydrated row AND append the catch-up envelope as a duplicate,
    /// breaking the "at-least-once across restart" promise into
    /// "predictably twice on every recovery".
    ///
    /// Caller can use `.len()` on the returned slice for the observability
    /// count.
    pub async fn rehydrate_queue_from_persist(&self, watermark: i64) -> Vec<i64> {
        let persist = match self.queue_persist.lock().await.clone() {
            Some(p) => p,
            None => return Vec::new(),
        };
        let rows = (persist.list)().await;
        if rows.is_empty() {
            return Vec::new();
        }
        // #325 round-2 P2 — partition by watermark. `stale_db_ids` will be
        // dequeued (deleted) below so a future boot doesn't see them; the
        // live rows are appended to the in-memory queue and their
        // envelope_ids are returned for the caller's dedup skip-set.
        let mut envelope_ids = Vec::with_capacity(rows.len());
        let mut stale_db_ids: Vec<i64> = Vec::new();
        {
            let mut q = self.queue.lock().await;
            for (db_id, envelope_id, text) in rows {
                if envelope_id <= watermark {
                    stale_db_ids.push(db_id);
                    continue;
                }
                envelope_ids.push(envelope_id);
                q.push_back(QueuedObservation {
                    envelope_id,
                    text,
                    db_id: Some(db_id),
                });
            }
        }
        if !stale_db_ids.is_empty() {
            tracing::info!(
                thread_id = %self.thread_id.as_deref().unwrap_or("<pending-thread-start>"),
                watermark,
                stale_count = stale_db_ids.len(),
                "spec push: rehydrate dropped rows already covered by durable watermark (envelope_id <= watermark); deleting from spec_push_queue so the next boot won't see them again"
            );
            (persist.dequeue)(stale_db_ids).await;
        }
        envelope_ids
    }

    /// #325 fix — drive a one-shot `flush_push_queue` against the handle's
    /// own queue + status + persistence slots. Used by boot-takeover after
    /// `rehydrate_queue_from_persist` re-loaded the in-memory queue from
    /// disk in the edge case where catch-up replay is fully deduped
    /// against the rehydrated set (so no `StartTurnNow` was triggered to
    /// drain the rehydrated rows). Without this nudge, rehydrated items
    /// would sit in the queue until the next live event arrived — fine
    /// for liveness, but the explicit flush keeps the "boot recovers a
    /// pending observation" path symmetric with the create-wave path
    /// (where the consumer's `turn/completed` handler always flushes).
    ///
    /// Idempotent: if the queue is empty or another issuer already owns
    /// the cycle (phase is `Issuing`/`TurnRunning`), this no-ops — same
    /// guarantees as the consumer task's `turn/completed` flush.
    pub async fn flush_pending(&self) {
        self.pusher().flush_pending().await;
    }
}

impl SpecPusher {
    async fn rehydrate_queue_from_persist(&self, watermark: i64) -> Vec<i64> {
        let persist = match self.queue_persist.lock().await.clone() {
            Some(p) => p,
            None => return Vec::new(),
        };
        let rows = (persist.list)().await;
        if rows.is_empty() {
            return Vec::new();
        }

        let mut envelope_ids = Vec::with_capacity(rows.len());
        let mut stale_db_ids: Vec<i64> = Vec::new();
        {
            let mut q = self.queue.lock().await;
            for (db_id, envelope_id, text) in rows {
                if envelope_id <= watermark {
                    stale_db_ids.push(db_id);
                    continue;
                }
                envelope_ids.push(envelope_id);
                q.push_back(QueuedObservation {
                    envelope_id,
                    text,
                    db_id: Some(db_id),
                });
            }
        }
        if !stale_db_ids.is_empty() {
            // Extract the awaited thread-id string before the macro
            // captures it: holding the `Arguments<'_>` constructed inside
            // `tracing::info!` across the `await` makes the surrounding
            // future `!Send`, which axum requires for handler futures
            // (see #421/#424 reconciliation). The pattern matches other
            // log sites in this file (`thread_id_for_log().await` then
            // pass the `String` into the macro).
            let thread_id_log = self.thread_id_for_log().await;
            tracing::info!(
                thread_id = %thread_id_log,
                watermark,
                stale_count = stale_db_ids.len(),
                "spec push: rehydrate dropped rows already covered by durable watermark (envelope_id <= watermark); deleting from spec_push_queue so the next boot won't see them again"
            );
            (persist.dequeue)(stale_db_ids).await;
        }
        envelope_ids
    }

    /// #325 fix — see [`SpecPushHandle::flush_pending`]. Delegated here so
    /// callers holding a [`SpecPusher`] (e.g. the registry) can drive the
    /// boot-takeover post-rehydrate flush without holding a `DashMap`
    /// guard across the `.await`.
    pub async fn flush_pending(&self) {
        let Some(thread_id) = self.thread_id().await else {
            tracing::debug!("spec push: flush_pending no-op — waiting for TUI-created thread id");
            return;
        };
        flush_push_queue(
            &thread_id,
            &self.status,
            &self.client,
            &self.queue,
            &self.watermark_sink,
            &self.queue_persist,
        )
        .await;
    }
}

impl Drop for SpecPushHandle {
    fn drop(&mut self) {
        // Best-effort synchronous group SIGTERM so a handle dropped on ANY
        // error path (mid-sequence spawn failure, a replaced registry
        // entry, test teardown) reaps the native `codex app-server` child
        // — not just the node launcher `kill_on_drop` would catch. A bare
        // `libc::kill` is async-signal-safe and fine to call from `Drop`.
        // The graceful teardown path (`reap_spec_push`) has already
        // escalated to SIGKILL by the time it drops the handle, so this is
        // typically a no-op (ESRCH) there.
        crate::spec_appserver::signal_process_group(self.pgid, libc::SIGTERM);
        // Abort the consumer task so it doesn't outlive the connection.
        // (`JoinHandle::drop` detaches rather than aborts, so this is
        // required.) The `CodexAppServer` reader task aborts on its own
        // drop.
        self.consumer.abort();
        // #318 INV-4 — abort the resume reconciler timer too if present,
        // so it doesn't outlive the handle on early-drop / replacement.
        if let Some(t) = self.resume_reconciler.take() {
            t.abort();
        }
    }
}

/// One spec card per wave, so the push handles key by [`WaveId`].
///
/// Clone-cheap (`Arc<DashMap<…>>` inside) — mirrors
/// [`crate::card_role_cache::CardRoleCache`]. Held on
/// [`crate::state::AppState`]; the create-wave route inserts, the
/// terminal sweeper / wave-delete path removes (which drops the handle →
/// kills the child).
#[derive(Clone, Default)]
pub struct SpecPushRegistry(Arc<DashMap<WaveId, SpecPushHandle>>);

impl SpecPushRegistry {
    /// Fresh, empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or replace) the handle for a wave. A replaced handle is
    /// returned so the caller can observe it; dropping it kills its child.
    ///
    /// **Production code should use [`Self::park`] instead** — `park` runs
    /// the aspect framework's
    /// [`JoinPoint::BeforeHandleParkInRegistry`](crate::aspect::JoinPoint::BeforeHandleParkInRegistry)
    /// invariants (INV-6's `WatermarkSinkInstalledAspect` today). `insert`
    /// is retained for the spec-push test crate, which constructs handles
    /// without going through the full install path and intentionally
    /// bypasses the aspect check.
    pub fn insert(&self, wave_id: WaveId, handle: SpecPushHandle) -> Option<SpecPushHandle> {
        self.0.insert(wave_id, handle)
    }

    /// #322 — production registration path. Runs every aspect installed on
    /// the [`crate::aspect::JoinPoint::BeforeHandleParkInRegistry`] slot,
    /// then `insert`s the handle. An aspect violation panics (release-mode
    /// fail-fast: a kernel that parks a handle missing its watermark sink
    /// has already corrupted the durable push-watermark contract and the
    /// only safe action is to crash so the supervisor restart re-runs
    /// boot-takeover from persistent state — see [`crate::aspect`] module
    /// doc).
    ///
    /// Why not collapse with `insert`: tests in `mod tests` build
    /// [`SpecPushHandle`]s without a watermark sink (they don't exercise
    /// the queue-flush path), so they'd trip
    /// [`crate::aspect::WatermarkSinkInstalledAspect`]. `park` is the
    /// production entry point; `insert` is the bare-insert escape hatch
    /// tests keep using.
    ///
    /// The `aspects: &AspectRegistry` arg is the explicit framework wiring
    /// the design landed on (see #322 instructions): the registry has
    /// no opinion on aspect dispatch, the caller passes the aspect
    /// registry it already holds on [`crate::state::AppState`]. Approach A
    /// from the issue body — simpler than embedding the aspect registry
    /// into `SpecPushRegistry` because (a) `SpecPushRegistry::new` is
    /// called from `Default::default()` chains the aspect registry isn't
    /// available in, and (b) keeping the registries orthogonal lets the
    /// aspect framework grow without churning `SpecPushRegistry`'s
    /// construction sites.
    pub async fn park(
        &self,
        wave_id: WaveId,
        handle: SpecPushHandle,
        aspects: &crate::aspect::AspectRegistry,
    ) -> Option<SpecPushHandle> {
        // Run BeforeHandleParkInRegistry aspects. The aspect dispatcher
        // panics on the first failure (see `AspectRegistry::run_before_handle_park`).
        // Scope the context so its borrows of `handle` / `wave_id` are
        // released before the `insert` move below.
        {
            let ctx = crate::aspect::HandleContext {
                handle: &handle,
                wave_id: &wave_id,
            };
            aspects.run_before_handle_park(&ctx).await;
        }
        self.0.insert(wave_id, handle)
    }

    /// Remove + return the handle for a wave (if any). Dropping the
    /// returned handle kills the `app-server` child via `kill_on_drop`.
    /// Returns `None` when no push channel exists for the wave (flag off,
    /// or already reaped).
    pub fn remove(&self, wave_id: &WaveId) -> Option<SpecPushHandle> {
        self.0.remove(wave_id).map(|(_k, v)| v)
    }

    /// Whether a push channel exists for a wave. Read-only probe (used by
    /// tests and PR3b's resolve step).
    pub fn contains(&self, wave_id: &WaveId) -> bool {
        self.0.contains_key(wave_id)
    }

    /// PR3b — a cloneable [`SpecPusher`] for a wave's push channel, or
    /// `None` when no handle is registered (flag off at create-time, or a
    /// kernel restart lost the in-memory handle). The `DashMap` `Ref` guard
    /// is held only for the brief sync `pusher()` clone and dropped before
    /// this returns, so the caller never holds the registry lock across the
    /// async `push_observation`.
    pub fn pusher(&self, wave_id: &WaveId) -> Option<SpecPusher> {
        self.0.get(wave_id).map(|h| h.pusher())
    }

    /// PR3b — a **non-destructive** status read for a wave's push channel
    /// (N2). Unlike `remove → status → insert`, this never removes the
    /// handle, so a concurrent `push_observation` racing the read can't fall
    /// into a window where no handle is registered (which would silently drop
    /// the push). The `DashMap` `Ref` guard is held only for the brief sync
    /// `pusher()` clone (which clones the shared status `Arc`); the guard is
    /// dropped before we `.await` the status lock. `None` when no channel is
    /// registered.
    pub async fn status(&self, wave_id: &WaveId) -> Option<SpecPushStatus> {
        // Clone the cheap `SpecPusher` (shared `Arc`s) under the guard, drop
        // the guard, then read the status — never hold the shard guard across
        // the `.await`.
        let pusher = self.0.get(wave_id).map(|h| h.pusher())?;
        Some(pusher.status().await)
    }

    /// Number of live push channels. Test/diagnostic helper.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when no push channels are registered.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// **Observability seam (#318, INV-6)**: probe whether the
    /// [`WatermarkSink`] has been installed on the parked handle for
    /// `wave_id`. Returns:
    ///   * `Some(true)`  — handle registered AND the sink slot is filled
    ///     (the correct post-init state for both init paths today),
    ///   * `Some(false)` — handle registered but no sink installed (the
    ///     bug-shape this seam catches: a future entry point that parks
    ///     a handle without `install_watermark_sink` — flushed queue
    ///     items would silently fail to persist their watermark),
    ///   * `None`        — no handle registered for this wave.
    ///
    /// Mirrors [`SpecPushHandle::has_watermark_sink`] (already `pub`) but
    /// reachable without first extracting the handle out of the registry
    /// (the registry stores `SpecPushHandle` by value; a `get(&self) ->
    /// Option<&SpecPushHandle>` would require leaking the `DashMap` ref
    /// guard, which the rest of this surface deliberately avoids).
    /// Holds the shard guard for a single cheap `Arc` clone of the sink
    /// slot before releasing the guard and awaiting on the slot's mutex,
    /// so callers never hold the registry lock across the `.await`.
    ///
    /// No production caller branches on this — `install_watermark_sink`
    /// is colocated with the two parking sites and verified by
    /// `debug_assert!`. The seam exists so integration tests can prove
    /// the symmetry contract holds without depending on the
    /// release-elided `debug_assert!`.
    pub async fn has_watermark_sink(&self, wave_id: &WaveId) -> Option<bool> {
        // Clone the cheap `SpecPusher` (shared `Arc`s) under the guard,
        // drop the guard, then read the sink — never hold the shard
        // guard across the `.await`. The pusher carries the same
        // `watermark_sink: WatermarkSinkSlot` Arc as the parent handle,
        // so a Some/None probe via the pusher is equivalent to probing
        // via the handle.
        let pusher = self.0.get(wave_id).map(|h| h.pusher())?;
        Some(pusher.watermark_sink.lock().await.is_some())
    }

    /// Rehydrate the durable queue rows into the parked handle for `wave_id`.
    /// Reset recovery uses this after replacing the handle in the registry;
    /// boot takeover calls the same handle method before parking.
    pub async fn rehydrate_queue_from_persist(&self, wave_id: &WaveId, watermark: i64) -> Vec<i64> {
        let pusher = self.0.get(wave_id).map(|h| h.pusher());
        match pusher {
            Some(pusher) => pusher.rehydrate_queue_from_persist(watermark).await,
            None => Vec::new(),
        }
    }
}

/// #318 INV-4 (codex P1) — the idle-resume reconcile timer body, extracted
/// so unit tests can drive it under `tokio::time::pause()` /
/// `tokio::time::advance` without standing up a real codex app-server.
///
/// Sleep `budget`, then under ONE [`SharedStatus`] lock acquisition CAS
/// [`SpecPushPhase::Resumed`] → [`SpecPushPhase::TurnCompleted`] iff still
/// `Resumed` — a concurrent `record()` driven by a real
/// `turn/started`/`turn/completed` notification arriving during the budget
/// wins the race and the timer no-ops. On a successful promotion the timer
/// invokes [`flush_push_queue`] once so any catch-up observation enqueued
/// under `decide(Resumed) == Enqueue` rides a coalesced `turn/start`
/// instead of being stranded indefinitely on a truly-idle resumed thread.
pub(crate) async fn resume_reconcile_task(
    budget: Duration,
    thread_id: String,
    status: SharedStatus,
    client: Arc<CodexAppServer>,
    queue: PushQueue,
    watermark_sink: WatermarkSinkSlot,
    queue_persist: QueuePersistSlot,
) {
    tokio::time::sleep(budget).await;
    // CAS under the status lock: only promote if still Resumed. A real
    // `turn/started`/`turn/completed` via `record()` would have already
    // moved the phase to `TurnRunning`/`TurnCompleted` — in which case
    // the consumer task already owns the lifecycle, do nothing.
    let promoted = {
        let mut g = status.lock().await;
        if g.phase == SpecPushPhase::Resumed {
            g.phase = SpecPushPhase::TurnCompleted;
            true
        } else {
            false
        }
    };
    if !promoted {
        tracing::debug!(
            thread_id,
            "spec push (resume reconcile): phase already reconciled by lifecycle notification; no-op"
        );
        return;
    }
    tracing::info!(
        thread_id,
        budget_secs = budget.as_secs(),
        "spec push (resume reconcile): no lifecycle notification within budget; \
         promoting Resumed -> TurnCompleted and flushing queued observations"
    );
    flush_push_queue(
        &thread_id,
        &status,
        &client,
        &queue,
        &watermark_sink,
        &queue_persist,
    )
    .await;
}

/// Shared tail of both build paths (spawn + resume): spawn the consumer
/// task that drains the notification stream, then park everything into a
/// live [`SpecPushHandle`]. Kept intentionally minimal — the only thing
/// that differs across paths is whether a turn was driven before this
/// runs.
#[allow(clippy::too_many_arguments)]
pub(crate) fn park_handle(
    child: Child,
    pgid: i32,
    start_time: Option<u64>,
    boot_id: Option<String>,
    client: Arc<CodexAppServer>,
    thread_id: Option<String>,
    sock: &Path,
    notifs: NotificationStream,
    status: SharedStatus,
    watchdog: TurnWatchdogConfig,
    recovery_signal: Option<SpecRecoverySignal>,
) -> SpecPushHandle {
    let queue: PushQueue = Arc::new(Mutex::new(VecDeque::new()));
    let thread_id_slot: ThreadIdSlot = Arc::new(Mutex::new(thread_id.clone()));
    // #313 B1 — sink slot is empty here; the dispatcher installs the real
    // persister right after registering the handle.
    let watermark_sink: WatermarkSinkSlot = Arc::new(Mutex::new(None));
    // #318 INV-3 — queue persist slot is empty here; the same dispatcher
    // sites that install the watermark sink also install this.
    let queue_persist: QueuePersistSlot = Arc::new(Mutex::new(None));
    let initial_prompt_ready_sink: InitialPromptReadySinkSlot = Arc::new(Mutex::new(None));
    let consumer_status = status.clone();
    let consumer_thread_id_slot = thread_id_slot.clone();
    let consumer_sink = watermark_sink.clone();
    let consumer_persist = queue_persist.clone();
    let consumer_initial_prompt_ready = initial_prompt_ready_sink.clone();
    let consumer = tokio::spawn(consume_notifications(
        notifs,
        consumer_thread_id_slot,
        consumer_status,
        client.clone(),
        queue.clone(),
        consumer_sink,
        consumer_persist,
        consumer_initial_prompt_ready,
        watchdog,
        recovery_signal,
    ));
    SpecPushHandle {
        child,
        pgid,
        start_time,
        boot_id,
        client,
        thread_id: thread_id.clone(),
        thread_id_slot,
        sock: sock.to_path_buf(),
        consumer,
        // Populated only by `build_handle_after_spawn_resume` (post-park).
        // The spawn path seeds `Idle` and reconciles via the normal
        // `turn/started`/`turn/completed` lifecycle, so it needs no timer.
        resume_reconciler: None,
        status,
        queue,
        watermark_sink,
        queue_persist,
        initial_prompt_ready_sink,
    }
}

/// PR3a/PR3b consumer: drain the stream, tracking lifecycle status into
/// shared state for the dispatcher to read, warn loudly if an
/// approval-shaped notification ever arrives (it should not — the spec
/// cards run with `approval_policy = "never"` per
/// [`crate::spec_card::build_role_codex_config_toml`]), and — PR3b — **flush the push
/// queue on each `turn/completed`**: drain any buffered observations into a
/// single coalesced `turn/start`.
///
/// Exits when the connection closes (`recv` → `None`); aborted on
/// [`SpecPushHandle`] drop otherwise.
#[derive(Debug, Clone)]
pub(crate) struct ActiveTurnWatchdog {
    pub(crate) turn_id: String,
    pub(crate) deadline: TokioInstant,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn consume_notifications(
    notifs: NotificationStream,
    thread_id_slot: ThreadIdSlot,
    status: SharedStatus,
    client: Arc<CodexAppServer>,
    queue: PushQueue,
    watermark_sink: WatermarkSinkSlot,
    queue_persist: QueuePersistSlot,
    initial_prompt_ready_sink: InitialPromptReadySinkSlot,
    watchdog: TurnWatchdogConfig,
    recovery_signal: Option<SpecRecoverySignal>,
) {
    let mut state = NotificationConsumer {
        notifs,
        thread_id_slot,
        status,
        client,
        queue,
        watermark_sink,
        queue_persist,
        initial_prompt_ready_sink,
        watchdog,
        recovery_signal,
        active_turn: None,
        initial_prompt_ready_attempted: false,
    };
    state.seed_watchdog_from_status().await;
    state.run().await;
}

pub(crate) struct NotificationConsumer {
    pub(crate) notifs: NotificationStream,
    pub(crate) thread_id_slot: ThreadIdSlot,
    pub(crate) status: SharedStatus,
    pub(crate) client: Arc<CodexAppServer>,
    pub(crate) queue: PushQueue,
    pub(crate) watermark_sink: WatermarkSinkSlot,
    pub(crate) queue_persist: QueuePersistSlot,
    pub(crate) initial_prompt_ready_sink: InitialPromptReadySinkSlot,
    pub(crate) watchdog: TurnWatchdogConfig,
    pub(crate) recovery_signal: Option<SpecRecoverySignal>,
    pub(crate) active_turn: Option<ActiveTurnWatchdog>,
    pub(crate) initial_prompt_ready_attempted: bool,
}

impl NotificationConsumer {
    async fn current_thread_id(&self) -> Option<String> {
        self.thread_id_slot.lock().await.clone()
    }

    async fn thread_id_for_log(&self) -> String {
        self.current_thread_id()
            .await
            .unwrap_or_else(|| "<pending-thread-start>".to_string())
    }

    async fn notification_matches_current_thread(&self, thread_id: &str) -> bool {
        if thread_id.is_empty() {
            return true;
        }
        match self.current_thread_id().await {
            Some(current) => thread_id == current,
            // Empty-goal fresh-start handles do not know the TUI-created
            // thread id until the first lifecycle notification; accept that
            // first notification so it can fill the slot.
            None => true,
        }
    }

    async fn seed_watchdog_from_status(&mut self) {
        let snapshot = self.status.lock().await.clone();
        if snapshot.phase != SpecPushPhase::TurnRunning {
            return;
        }
        let Some(turn_id) = snapshot.last_turn_id else {
            let thread_id = self.thread_id_for_log().await;
            tracing::warn!(
                thread_id = %thread_id,
                "spec push watchdog: consumer started during TurnRunning but no turn id was recorded; watchdog cannot arm until the next turn/started"
            );
            return;
        };
        self.arm_watchdog(turn_id, "initial-status").await;
    }

    async fn run(&mut self) {
        loop {
            if let Some(active) = self.active_turn.clone() {
                tokio::select! {
                    notification = self.notifs.recv_result() => {
                        let n = match notification {
                            Ok(n) => n,
                            Err(e) => {
                                let thread_id = self.thread_id_for_log().await;
                                tracing::debug!(
                                    thread_id = %thread_id,
                                    error = %e,
                                    "spec push: notification stream closed; consumer exiting"
                                );
                                break;
                            }
                        };
                        self.process_notification(n).await;
                    }
                    _ = tokio::time::sleep_until(active.deadline) => {
                        let still_current = match &self.active_turn {
                            Some(current) => current.turn_id == active.turn_id,
                            None => false,
                        };
                        if still_current {
                            self.handle_watchdog_deadline(active.turn_id).await;
                        }
                }
                }
            } else {
                let Some(n) = self.notifs.recv().await else {
                    let thread_id = self.thread_id_for_log().await;
                    tracing::debug!(
                        thread_id = %thread_id,
                        "spec push: notification stream closed; consumer exiting"
                    );
                    break;
                };
                self.process_notification(n).await;
            }
        }
    }

    pub(crate) async fn handle_watchdog_deadline(&mut self, turn_id: String) {
        let Some(thread_id) = self.current_thread_id().await else {
            self.active_turn = None;
            tracing::warn!(
                turn_id = %turn_id,
                "spec push watchdog: deadline fired before thread id was known; skipping interrupt"
            );
            return;
        };
        tracing::warn!(
            thread_id = %thread_id,
            turn_id = %turn_id,
            max_turn_secs = self.watchdog.max_turn_duration.as_secs(),
            "spec push watchdog: max turn duration elapsed; sending turn/interrupt"
        );
        if let Err(e) = self.client.turn_interrupt(&thread_id, &turn_id).await {
            self.active_turn = None;
            tracing::error!(
                thread_id = %thread_id,
                turn_id = %turn_id,
                error = %e,
                "spec push watchdog: turn/interrupt failed; process-level restart is required"
            );
            self.signal_process_recovery(turn_id, SpecRecoveryReason::InterruptFailed)
                .await;
            return;
        }

        let deadline = TokioInstant::now() + self.watchdog.interrupt_completion_budget;
        loop {
            tokio::select! {
                notification = self.notifs.recv_result() => {
                    let n = match notification {
                        Ok(n) => n,
                        Err(e) => {
                            self.active_turn = None;
                            tracing::error!(
                                thread_id = %thread_id,
                                turn_id = %turn_id,
                                error = %e,
                                "spec push watchdog: stream closed while waiting for interrupted turn/completed"
                            );
                            self.signal_process_recovery(
                                turn_id,
                                SpecRecoveryReason::StreamClosedAwaitingInterruptedCompletion,
                            )
                            .await;
                            return;
                        }
                    };
                    let completed = is_completion_for_turn(&n, &turn_id);
                    let interrupted = is_interrupted_completion_for_turn(&n, &turn_id);
                    self.process_notification(n).await;
                    if completed && !interrupted {
                        tracing::info!(
                            thread_id = %thread_id,
                            turn_id = %turn_id,
                            "spec push watchdog: turn completed naturally after interrupt ack"
                        );
                        return;
                    }
                    if interrupted {
                        tracing::info!(
                            thread_id = %thread_id,
                            turn_id = %turn_id,
                            "spec push watchdog: interrupted turn completed"
                        );
                        return;
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    let still_current = matches!(
                        &self.active_turn,
                        Some(active) if active.turn_id == turn_id
                    );
                    if !still_current {
                        tracing::info!(
                            thread_id = %thread_id,
                            turn_id = %turn_id,
                            "spec push watchdog: interrupt wait elapsed after watched turn completed"
                        );
                        return;
                    }
                    self.active_turn = None;
                    tracing::error!(
                        thread_id = %thread_id,
                        turn_id = %turn_id,
                        interrupt_completion_secs = self.watchdog.interrupt_completion_budget.as_secs(),
                        "spec push watchdog: interrupt acked but no interrupted turn/completed arrived; process-level restart is required"
                    );
                    self.signal_process_recovery(
                        turn_id,
                        SpecRecoveryReason::InterruptedCompletionTimedOut,
                    )
                    .await;
                    return;
                }
            }
        }
    }

    pub(crate) async fn signal_process_recovery(
        &mut self,
        turn_id: String,
        reason: SpecRecoveryReason,
    ) {
        let Some(thread_id) = self.current_thread_id().await else {
            tracing::debug!(
                turn_id = %turn_id,
                ?reason,
                "spec push: skipping recovery signal before TUI thread id is known"
            );
            return;
        };
        {
            let mut g = self.status.lock().await;
            g.phase = SpecPushPhase::Wedged;
            g.last_turn_id = Some(turn_id.clone());
        }
        let Some(signal) = &self.recovery_signal else {
            tracing::error!(
                thread_id = %thread_id,
                turn_id = %turn_id,
                ?reason,
                "spec push watchdog: process-level recovery required but no recovery supervisor is wired"
            );
            return;
        };
        let request = SpecRecoveryRequest {
            wave_id: signal.wave_id.clone(),
            thread_id: thread_id.clone(),
            turn_id,
            reason,
        };
        match signal.tx.try_send(request) {
            Ok(()) => {
                tracing::warn!(
                    thread_id = %thread_id,
                    wave_id = %signal.wave_id,
                    ?reason,
                    "spec push watchdog: signaled process-level recovery supervisor"
                );
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(
                    thread_id = %thread_id,
                    wave_id = %signal.wave_id,
                    ?reason,
                    "spec push watchdog: recovery supervisor already has a pending request"
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::error!(
                    thread_id = %thread_id,
                    wave_id = %signal.wave_id,
                    ?reason,
                    "spec push watchdog: recovery supervisor channel is closed"
                );
            }
        }
    }

    pub(crate) async fn process_notification(&mut self, n: Notification) {
        warn_on_approval(&n);
        record(&self.status, &n).await;
        self.mark_initial_prompt_ready_after_rollout_observed(&n)
            .await;
        match &n {
            Notification::TurnStarted { thread_id, turn } => {
                if self.notification_matches_current_thread(thread_id).await
                    && let Some(turn_id) = turn_id(turn)
                {
                    self.arm_watchdog(turn_id.to_string(), "turn-started").await;
                }
            }
            Notification::TurnCompleted { thread_id, turn } => {
                if self.notification_matches_current_thread(thread_id).await
                    && let Some(active) = &self.active_turn
                {
                    let completed_turn = turn_id(turn);
                    if match completed_turn {
                        Some(id) => id == active.turn_id,
                        None => true,
                    } {
                        self.active_turn = None;
                    }
                }
            }
            _ => {}
        }

        // PR3b flush: a turn just finished — if observations piled up while
        // it ran, deliver them now as ONE coalesced turn so the spec sees
        // them between turns (codex silently drops a turn/start issued while
        // a turn is active, so we can only start one here, between turns).
        if matches!(n, Notification::TurnCompleted { .. })
            && let Some(thread_id) = self.current_thread_id().await
        {
            flush_push_queue(
                &thread_id,
                &self.status,
                &self.client,
                &self.queue,
                &self.watermark_sink,
                &self.queue_persist,
            )
            .await;
        }
    }

    async fn mark_initial_prompt_ready_after_rollout_observed(&mut self, n: &Notification) {
        if self.initial_prompt_ready_attempted {
            return;
        }
        let lifecycle_thread_id = match n {
            Notification::TurnStarted { thread_id, .. }
            | Notification::TurnCompleted { thread_id, .. }
                if self.notification_matches_current_thread(thread_id).await =>
            {
                thread_id.as_str()
            }
            _ => "",
        };
        let lifecycle_for_thread = !lifecycle_thread_id.is_empty()
            || matches!(
                n,
                Notification::TurnStarted { thread_id, .. }
                    | Notification::TurnCompleted { thread_id, .. }
                    if thread_id.is_empty()
            );
        if !lifecycle_for_thread {
            return;
        }
        let rollout_observed = self.status.lock().await.last_turn_id.is_some();
        if !rollout_observed {
            return;
        }
        let thread_id = if lifecycle_thread_id.is_empty() {
            match self.current_thread_id().await {
                Some(id) => id,
                None => return,
            }
        } else {
            lifecycle_thread_id.to_string()
        };
        {
            let mut slot = self.thread_id_slot.lock().await;
            if slot.is_none() {
                *slot = Some(thread_id.clone());
            }
        }
        let Some(sink) = self.initial_prompt_ready_sink.lock().await.clone() else {
            let thread_id = self.thread_id_for_log().await;
            tracing::debug!(
                thread_id = %thread_id,
                "spec push: observed turn lifecycle before initial-prompt ready sink was installed"
            );
            self.initial_prompt_ready_attempted = true;
            return;
        };
        sink(thread_id).await;
        self.initial_prompt_ready_attempted = true;
    }

    async fn arm_watchdog(&mut self, turn_id: String, source: &'static str) {
        let thread_id = self.thread_id_for_log().await;
        let deadline = TokioInstant::now() + self.watchdog.max_turn_duration;
        tracing::debug!(
            thread_id = %thread_id,
            turn_id = %turn_id,
            source,
            max_turn_secs = self.watchdog.max_turn_duration.as_secs(),
            "spec push watchdog: armed runtime max-turn watchdog"
        );
        self.active_turn = Some(ActiveTurnWatchdog { turn_id, deadline });
    }
}

fn turn_id(turn: &Value) -> Option<&str> {
    turn.get("id").and_then(Value::as_str)
}

fn turn_status(turn: &Value) -> Option<&str> {
    turn.get("status").and_then(Value::as_str)
}

fn is_completion_for_turn(n: &Notification, expected_turn_id: &str) -> bool {
    let Notification::TurnCompleted { turn, .. } = n else {
        return false;
    };
    turn_id(turn) == Some(expected_turn_id)
}

fn is_interrupted_completion_for_turn(n: &Notification, expected_turn_id: &str) -> bool {
    let Notification::TurnCompleted { turn, .. } = n else {
        return false;
    };
    turn_id(turn) == Some(expected_turn_id) && turn_status(turn) == Some("interrupted")
}

/// Flush the push queue on `turn/completed` (B1 — single-winner issuance).
///
/// Called from the consumer task right after `record()` set the phase to
/// `TurnCompleted`. To avoid racing a concurrent
/// [`SpecPusher::push_observation`] (which may also be trying to issue),
/// the flush **atomically claims the issue right** under ONE status-lock
/// acquisition: if the phase is between-turns (`Idle`/`TurnCompleted`) it
/// flips to [`SpecPushPhase::Issuing`] and becomes the winner; if a
/// `push_observation` already won (phase is `Issuing`) or a turn is somehow
/// active (`TurnRunning`), the flush **no-ops** — the winner already
/// drained (or will drain) the queue, so re-issuing here would be the very
/// `turn/start`-while-active that codex silently drops.
///
/// As the winner it then drains the whole queue into one coalesced
/// `turn/start`. The server's `turn/started` reconciles `Issuing`→
/// `TurnRunning`; a `turn/start` error rolls `Issuing`→`TurnCompleted` and
/// re-buffers (front, order preserved) for the next `turn/completed`.
///
/// Lock order matches [`SpecPusher::push_observation`]: status (claim),
/// release, queue (drain), release, issue — never nested, so deadlock-free.
pub(crate) async fn flush_push_queue(
    thread_id: &str,
    status: &SharedStatus,
    client: &Arc<CodexAppServer>,
    queue: &PushQueue,
    watermark_sink: &WatermarkSinkSlot,
    queue_persist: &QueuePersistSlot,
) {
    // Atomically claim the issue right: only flip to `Issuing` (and win) if
    // we observe a between-turns phase. If a `push_observation` already won
    // (`Issuing`) or a turn is running, do NOT issue — that would be a
    // silently-dropped concurrent `turn/start` (the B1 race).
    {
        let mut g = status.lock().await;
        match g.phase {
            SpecPushPhase::Idle | SpecPushPhase::TurnCompleted => {
                g.phase = SpecPushPhase::Issuing;
            }
            SpecPushPhase::PendingThreadStart => {
                tracing::debug!(
                    thread_id,
                    "spec push: flush no-op — waiting for TUI-created thread id"
                );
                return;
            }
            SpecPushPhase::Issuing | SpecPushPhase::TurnRunning => {
                // Another issuer owns this cycle (or a turn is active);
                // anything queued is the winner's to drain. No-op.
                tracing::debug!(
                    thread_id,
                    "spec push: flush no-op — another issuer owns this cycle"
                );
                return;
            }
            SpecPushPhase::Resumed => {
                // #318 INV-4 (R2-B2) — defensive: this branch is only
                // reachable if `flush_push_queue` runs before any
                // lifecycle notification has arrived (the consumer task
                // only invokes flush on `TurnCompleted`, which itself
                // reconciles phase → `TurnCompleted` via `record()` first,
                // so in practice we won't see `Resumed` here). Stay
                // conservative: don't issue while the resume state is
                // unproven.
                tracing::debug!(
                    thread_id,
                    "spec push: flush no-op — phase is Resumed (waiting for lifecycle signal)"
                );
                return;
            }
            SpecPushPhase::Wedged => {
                tracing::debug!(
                    thread_id,
                    "spec push: flush no-op — phase is Wedged (runtime recovery owns replay)"
                );
                return;
            }
        }
    }

    // We are the winner. Drain everything currently queued.
    let drained: Vec<QueuedObservation> = {
        let mut q = queue.lock().await;
        q.drain(..).collect()
    };
    if drained.is_empty() {
        // Nothing to send. Release the `Issuing` claim back to
        // `TurnCompleted` so a later push isn't blocked thinking we're still
        // mid-issue (no `turn/started` will ever arrive to reconcile it).
        let mut g = status.lock().await;
        if g.phase == SpecPushPhase::Issuing {
            g.phase = SpecPushPhase::TurnCompleted;
        }
        return;
    }
    // #313 B1: compute the max envelope id we are about to deliver so the
    // dispatcher-side watermark sink can advance the durable
    // `push_watermark` past every item in this coalesced turn.
    let max_envelope_id = drained
        .iter()
        .map(|o| o.envelope_id)
        .max()
        .expect("drained is non-empty (checked above)");
    let text = drained
        .iter()
        .map(|o| o.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    tracing::debug!(
        thread_id,
        count = drained.len(),
        max_envelope_id,
        "spec push: flush winner issuing queued observations as one coalesced turn/start"
    );
    if let Err(e) = client
        .turn_start(thread_id, vec![InputItem::text(&text)])
        .await
    {
        tracing::warn!(
            thread_id,
            error = %e,
            "spec push: flush turn/start failed; re-buffering observations for next turn/completed"
        );
        // #293: a failed flush has NO self-driven retry. We re-buffer + roll
        // the phase back, but nothing re-attempts the `turn/start` on its own
        // — the items only flush again when an UNRELATED later
        // `turn/completed` fires (which drives another `flush_push_queue`). If
        // the thread then sits idle, the observations stay buffered and
        // undelivered: there is no pull backstop anymore (the cutover removed
        // `wait_for_events`). Accepted limitation; a dedicated retry (timer /
        // on-idle re-flush) is deferred to a later PR.
        // Re-buffer at the front, preserving order, so a later flush retries.
        {
            let mut q = queue.lock().await;
            for obs in drained.into_iter().rev() {
                q.push_front(obs);
            }
        }
        // Roll the `Issuing` claim back — no turn actually started.
        let mut g = status.lock().await;
        if g.phase == SpecPushPhase::Issuing {
            g.phase = SpecPushPhase::TurnCompleted;
        }
        return;
    }

    // #313 B1 — flush succeeded. Persist the durable watermark via the
    // dispatcher-installed sink so a kernel crash AFTER this point doesn't
    // cause boot catch-up to redeliver items codex already accepted. The
    // sink is `None` only in test paths that don't exercise persistence.
    // The two production install sites are:
    //   * `routes/waves.rs::spawn_push_appserver` — for create-wave path,
    //   * `lib.rs::register_and_catch_up`        — for boot-takeover path.
    // Both install BEFORE the handle is reachable by any push, so by the
    // time a flush runs the sink slot is always populated in production.
    let sink = watermark_sink.lock().await.clone();
    if let Some(sink) = sink {
        sink(max_envelope_id).await;
    } else {
        tracing::debug!(
            thread_id,
            max_envelope_id,
            "spec push: flush succeeded but no watermark sink installed (test path) — \
             durable watermark not advanced for queue-flushed items"
        );
    }

    // #318 INV-3 — `turn/start` succeeded; dequeue every persisted row in
    // this coalesced batch. Drained entries without a `db_id` (test paths /
    // pre-install windows) are naturally filtered out. Done AFTER the
    // watermark sink so that a watermark-persist failure that strands the
    // in-memory cache bump does not block dequeueing rows codex already
    // accepted (rows kept around when codex didn't accept them would be
    // double-delivered on the next boot's rehydrate).
    let to_dequeue: Vec<i64> = drained.iter().filter_map(|o| o.db_id).collect();
    if !to_dequeue.is_empty() {
        let persist = queue_persist.lock().await.clone();
        if let Some(persist) = persist {
            (persist.dequeue)(to_dequeue).await;
        } else {
            tracing::debug!(
                thread_id,
                count = drained.len(),
                "spec push: flush succeeded but no queue-persist slot installed (test path) — \
                 durable queue rows not dequeued (none should exist either)"
            );
        }
    }
}

/// Fold one notification into the tracked status.
pub(crate) async fn record(status: &SharedStatus, n: &Notification) {
    let mut g = status.lock().await;
    match n {
        Notification::ThreadStarted { params } => {
            if let Some(id) = params
                .get("thread")
                .and_then(|t| t.get("id"))
                .and_then(Value::as_str)
            {
                g.last_thread_id = Some(id.to_string());
            }
        }
        Notification::ThreadStatusChanged { thread_id, .. } => {
            if !thread_id.is_empty() {
                g.last_thread_id = Some(thread_id.clone());
            }
        }
        Notification::TurnStarted { thread_id, turn } => {
            if !thread_id.is_empty() {
                g.last_thread_id = Some(thread_id.clone());
            }
            if let Some(id) = turn.get("id").and_then(Value::as_str) {
                g.last_turn_id = Some(id.to_string());
            }
            // Reconciles the B1 `Issuing` claim → `TurnRunning` once the
            // server confirms the winner's `turn/start` actually started.
            if g.phase != SpecPushPhase::Wedged {
                g.phase = SpecPushPhase::TurnRunning;
            }
        }
        Notification::TurnCompleted { thread_id, turn } => {
            if !thread_id.is_empty() {
                g.last_thread_id = Some(thread_id.clone());
            }
            if let Some(id) = turn.get("id").and_then(Value::as_str) {
                g.last_turn_id = Some(id.to_string());
            }
            if g.phase != SpecPushPhase::Wedged {
                g.phase = SpecPushPhase::TurnCompleted;
            }
        }
        Notification::Item { .. } | Notification::Other { .. } => {}
    }
}

/// Warn when a notification looks like a server→client approval request.
/// Under `approval_policy = "never"` these should never fire; if one does,
/// the spec agent would silently stall (PR3a does not answer approvals).
/// This is the early-warning hook the issue calls for.
pub(crate) fn warn_on_approval(n: &Notification) {
    let method = match n {
        Notification::Item { method, .. } | Notification::Other { method, .. } => method.as_str(),
        _ => return,
    };
    if method.contains("requestApproval") || method.contains("requestUserInput") {
        tracing::warn!(
            method,
            "spec push: unexpected approval-shaped notification under approval_policy=never; \
             PR3a does not answer it — the spec agent may stall (investigate codex config)"
        );
    }
}
