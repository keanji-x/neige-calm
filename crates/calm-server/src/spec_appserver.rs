//! Two-process-per-spec-card model + supervisor (issue #293 PR3a).
//!
//! ## What this is
//!
//! The push migration (#293) moved spec agents off the old 30 s long-poll
//! (pull) model entirely onto a codex app-server *push* channel — the pull
//! machinery (`calm.wait_for_events` + the Stop-hook fallback) was deleted in
//! the cutover. The architecture splits one spec card across **two processes
//! that share a single codex thread**:
//!
//!   1. a **`codex app-server --listen unix://<sock>`** child — the
//!      long-lived agent the *kernel* drives programmatically (via PR2's
//!      [`CodexAppServer`] client), and
//!   2. the browser-facing **`codex resume <thread_id> --remote
//!      unix://<sock>`** TUI, spawned under the existing PTY
//!      `calm-session-daemon` so the WS render path
//!      (`RenderPlane`/`RenderPatch`) is byte-identical to today.
//!
//! The spike (`docs/spikes/293-appserver-thread-sharing.md`) verified the
//! `--remote` TUI and a programmatic client can drive/observe the *same*
//! thread against the real binary.
//!
//! ## What this PR (3a) does and does NOT do
//!
//! This module gives the kernel the ability to **own** the `app-server`
//! child and drive turn #1 on the create-wave hot path. Push is the only
//! path (#293 cutover — no flag, no pull coexistence). The
//! [`SpecPushHandle`] is parked in a [`SpecPushRegistry`] keyed by
//! [`WaveId`] (one spec card per wave) so the dispatcher can resolve a
//! wave's app-server client and push observations onto it.
//!
//! ## PR3b — dispatcher push delivery (added on top of 3a)
//!
//! [`SpecPushHandle::push_observation`] is the delivery primitive the
//! dispatcher ([`crate::dispatcher`]) calls when a wave event (task
//! completed/failed, user-authored report edit) lands. The decision of
//! *how* to deliver is the pure [`decide`] fn over the consumer-tracked
//! [`SpecPushPhase`]:
//!
//!   * **Idle / TurnCompleted** (between turns) → the caller atomically
//!     claims the right to issue (flips phase to **Issuing**) and fires one
//!     coalesced `turn/start` (its observation + anything already queued).
//!   * **Issuing / TurnRunning** → enqueue; the single in-cycle issuer (or
//!     the [`NotificationStream`] consumer task on the next `turn/completed`)
//!     flushes the queue as one coalesced `turn/start`.
//!
//! This enqueue+flush design is **required, not optional**: verified
//! against the real codex binary (PR3b probe), a `turn/start` issued while
//! a turn is active returns an OK ack but its work is **silently dropped**
//! (no `turn/started`/`turn/completed` ever fires for it). `turn/steer` is
//! avoided because its `expectedTurnId` races the live stream.
//!
//! ## B1 — single-winner turn issuance (flush-vs-push race)
//!
//! Two paths can issue a `turn/start`: a dispatcher `push_observation` and
//! the consumer's `flush_push_queue` (on `turn/completed`). If both saw
//! "between turns" and each issued, codex would silently drop one turn's
//! work. The fix is the **Issuing** phase: the decision to issue and the
//! claim of the right to issue are atomic under ONE status-lock
//! acquisition, so exactly one caller wins per cycle; the loser (and any
//! concurrent push) only enqueues. The winner drains the whole queue (plus
//! its own observation, for the push case) into a single `turn/start`. No
//! observation is lost: anything enqueued before the winner drains rides
//! that turn; anything after waits for the next cycle.
//!
//! ## DECISION A — create-wave blocking sequence
//!
//! In the create-wave path (under the flag) the kernel awaits, **before
//! returning the 201 and spawning the `--remote` TUI**:
//!
//!   boot `app-server` → poll socket ready → [`CodexAppServer::connect`] →
//!   [`initialize`](CodexAppServer::initialize) →
//!   [`thread_start`](CodexAppServer::thread_start) →
//!   [`turn_start`](CodexAppServer::turn_start)`([text(goal)])` →
//!   **await the `turn/started` notification**
//!
//! Awaiting `turn/started` guarantees a *rollout exists on disk* (the
//! spike's hard constraint) so the `--remote` TUI's `thread/resume` can
//! rejoin the same thread. We deliberately do **not** wait for
//! `turn/completed` (that can be tens of seconds); the turn keeps running
//! while the 201 is returned and the TUI mounts. This is synchronous on
//! the create hot path by design.
//!
//! ## Supervision (2a + B1 process-group fix)
//!
//! The **kernel** spawns and owns the child via
//! [`tokio::process::Command`] — required because the kernel must talk to
//! the app-server (the steps above) *before* the PTY daemon (and therefore
//! the `--remote` TUI) exists. The child lives inside the
//! [`SpecPushHandle`] held by the registry.
//!
//! ### Why `kill_on_drop` alone is NOT enough (B1, empirically proven)
//!
//! Against real codex-cli 0.133.0 the spawned PID is a **`node` launcher**
//! (`codex.js`) that forks a **native `codex app-server` child** which
//! holds the listening socket FDs. `kill_on_drop(true)` SIGKILLs only the
//! node launcher; the native child **survives**, is reparented
//! (PPID→1, under `systemd --user`), and keeps serving the socket. So a
//! pid-only kill (or `kill_on_drop`) leaks the real server on the *normal*
//! teardown path.
//!
//! The load-bearing fix is to spawn the launcher in its **own process
//! group** ([`Command::process_group`]`(0)` → the launcher becomes a group
//! leader so `pgid == launcher pid`, and the native child it forks
//! inherits that pgid). Teardown then signals the **whole group** with
//! `kill(-pgid, …)` (SIGTERM, short grace, SIGKILL), which reaps both the
//! launcher and the native child. We keep `kill_on_drop(true)` as a
//! belt-and-suspenders for the launcher; the group kill is what actually
//! reaps the server.
//!
//! [`SpecPushHandle::drop`] does a synchronous best-effort
//! `kill(-pgid, SIGTERM)` (a bare libc call, fine in `Drop`) so a dropped
//! handle on *any* error path never leaks the native child. The consumer
//! task is aborted on drop too (no orphan task after the connection ends).
//! The pgid is also persisted on the spec-card payload so a kernel
//! hard-crash can reap the orphaned group on the next boot.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use serde_json::Value;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::codex_appserver::{
    ClientInfo, CodexAppServer, InputItem, Notification, NotificationStream,
};
use crate::error::{CalmError, Result};
use crate::ids::WaveId;

/// How long to poll for the app-server's listen socket to appear + accept
/// a connection before giving up. The server creates the socket after
/// binding; we reuse the same `UnixStream::connect` poll cadence the PTY
/// daemon spawn uses (`routes::terminal::spawn_daemon_with_parts` —
/// 75 × 40 ms). 20 s is generous for a local `app-server` boot (a model
/// turn is NOT required for the socket to come up) while still bounding a
/// binary that never binds (e.g. missing auth → exit during boot).
const SOCKET_READY_POLL: Duration = Duration::from_millis(150);
/// Total wall-clock budget for the socket-ready poll.
const SOCKET_READY_BUDGET: Duration = Duration::from_secs(20);

/// How long DECISION A waits for the `turn/started` notification after
/// `turn/start` returns its ack. `turn/started` is the signal a rollout
/// now exists on disk; it arrives promptly (it precedes any model work).
/// We bound it so a misbehaving server can't wedge the create hot path.
const TURN_STARTED_BUDGET: Duration = Duration::from_secs(30);

/// S1 — the **single overall wall-clock cap** on DECISION A's whole boot
/// sequence (poll-connect → initialize → thread/start → turn/start →
/// await `turn/started`). The per-step budgets above
/// (`SOCKET_READY_BUDGET` + the per-request codex_appserver
/// `DEFAULT_REQUEST_TIMEOUT` (30 s) x 3 +
/// `TURN_STARTED_BUDGET`) stack independently, so a degraded
/// codex/model that limps through each step just under its own budget
/// could otherwise block `POST /api/waves` for **well over a minute**.
/// This deadline wraps the entire post-spawn sequence so the create
/// hot path is bounded regardless of where the slowness lands; the
/// inner per-step budgets remain as finer-grained safety nets. On
/// overall timeout the [`SpawnRollback`] guard still fires (group
/// SIGTERM + socket-dir cleanup) so no orphan child / parked registry
/// entry is left behind, and `create_wave` returns a clean
/// [`CalmError`]. 45 s is comfortably above a healthy local boot
/// (socket up in <1 s, `turn/started` in a few hundred ms) while still
/// being a hard ceiling a sick model can't blow past.
const OVERALL_BOOT_BUDGET: Duration = Duration::from_secs(45);

/// #318 INV-4 (codex P1 follow-up) — how long [`build_handle_after_spawn_resume`]
/// waits for a lifecycle notification (`turn/started` or `turn/completed`)
/// to arrive on a `thread/resume`-d connection before assuming the server
/// is idle and promoting [`SpecPushPhase::Resumed`] → [`SpecPushPhase::TurnCompleted`].
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
/// reconcile we'd otherwise get from a `thread/status` probe (no such
/// codex JSON-RPC exists). 5 s is generous for a healthy mid-turn server
/// to emit its in-flight turn's `turn/completed` if it's about to land,
/// while still being short enough that the user-visible delay for a
/// boot-takeover catch-up push on a truly-idle thread is bounded.
///
/// **Trade-off**: if a prior-boot turn is genuinely mid-flight and
/// running longer than this budget, the timer fires first and the
/// resulting `turn/start` would be silently dropped by codex (the very
/// hazard `Resumed` was meant to prevent). Accepted as the lesser of
/// two evils: the alternative (no timer) loses the observation 100% of
/// the time on idle resumes; the timer loses it only on the rare
/// "mid-long-turn at crash" intersection. A future codex `thread/status`
/// API (or an idle-detect heuristic over the notification stream) would
/// remove this trade-off.
const RESUMED_RECONCILE_BUDGET: Duration = Duration::from_secs(5);

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
    /// reconcile it. `thread/resume` does **not** tell us whether a turn
    /// is mid-flight on the server, so we must NOT treat the thread as
    /// between turns: issuing a `turn/start` here while codex is still
    /// running the prior boot's turn would be silently dropped (verified;
    /// see [`decide`] / [`PushAction::Enqueue`]). The first `turn/started`
    /// reconciles us to `TurnRunning`; the first `turn/completed`
    /// reconciles us to `TurnCompleted` (at which point the consumer
    /// task's [`flush_push_queue`] will drain anything that piled up in
    /// the meantime).
    Resumed,
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
    }
}

/// Shared, cloneable handle onto the consumer-tracked status.
type SharedStatus = Arc<Mutex<SpecPushStatus>>;

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
type PushQueue = Arc<Mutex<VecDeque<QueuedObservation>>>;

/// One queued observation: the envelope id (so the flush path can report
/// the max delivered id to the dispatcher) plus the rendered text codex
/// will receive, plus — when a [`QueuePersistSlot`] is installed on the
/// handle — the `spec_push_queue.id` row id assigned at persist-time so
/// the flush path can dequeue it from the durable store after a
/// successful `turn/start` (#318 INV-3).
#[derive(Debug, Clone)]
struct QueuedObservation {
    envelope_id: i64,
    text: String,
    /// `Some(row_id)` when the entry was persisted via the
    /// [`QueuePersist::enqueue`] callback; `None` for entries that only
    /// live in the in-memory cache (test paths that skip the slot, or
    /// the brief window after a `turn/start` error re-buffers a drained
    /// batch — those rows remain in the durable store under their
    /// original ids so the requeue is purely the in-memory side).
    db_id: Option<i64>,
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
type QueuePersistSlot = Arc<Mutex<Option<Arc<QueuePersist>>>>;

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
type WatermarkSinkSlot = Arc<Mutex<Option<WatermarkSink>>>;

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
    /// Programmatic client connected to `child` over WS-over-UDS. PR3b
    /// will call `turn_start`/`turn_steer`/`inject_items` on this.
    pub client: Arc<CodexAppServer>,
    /// The thread id turn #1 ran on. Persisted on the spec card payload as
    /// `codex_thread_id`; the `--remote` TUI resumes it.
    pub thread_id: String,
    /// The listen socket path (`<data_dir>/appserver/<card_id>/app.sock`).
    pub sock: PathBuf,
    /// Consumer task draining the notification stream (status tracking +
    /// approval-shape warning + PR3b push-queue flush on `turn/completed`).
    /// Aborted on drop.
    consumer: JoinHandle<()>,
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
    resume_reconciler: Option<JoinHandle<()>>,
    /// Shared status the consumer task writes; PR3b reads it.
    status: SharedStatus,
    /// PR3b push queue. Observations buffered by
    /// [`push_observation`](Self::push_observation) while a turn is running;
    /// the consumer task drains them into one coalesced `turn/start` on the
    /// next `turn/completed`. Shared with the consumer task.
    queue: PushQueue,
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
    watermark_sink: WatermarkSinkSlot,
    /// #318 INV-3 (R2-B1) — durable enqueue/dequeue callbacks installed
    /// alongside the watermark sink. When `Some`, every `Enqueue`-arm
    /// observation is persisted to `spec_push_queue` BEFORE the
    /// in-memory `push_back` (so a crash between persist and the next
    /// flush leaves the row for boot-takeover to rehydrate). `None` on
    /// the brief window before install + in test paths that skip it.
    ///
    /// Installed via [`install_queue_persist`](Self::install_queue_persist)
    /// at the same sites that install the watermark sink.
    queue_persist: QueuePersistSlot,
}

/// PR3b — a cheaply-cloneable handle onto just the parts of a
/// [`SpecPushHandle`] that [`push_observation`](SpecPusher::push_observation)
/// needs (all `Arc`-shared / owned `String`). The dispatcher obtains one via
/// [`SpecPushRegistry::pusher`] without holding the registry's `DashMap`
/// guard across the `async` delivery — extracting these `Arc`s is a cheap
/// sync clone under the brief guard, after which the guard is released.
#[derive(Clone)]
pub struct SpecPusher {
    client: Arc<CodexAppServer>,
    thread_id: String,
    status: SharedStatus,
    queue: PushQueue,
    /// Shared with the parent handle (`Arc<Mutex<Option<…>>>`). Read only
    /// by the consumer task's flush path; pushers carry it so the queue
    /// itself stays self-contained alongside its persistence callback.
    #[allow(dead_code)]
    watermark_sink: WatermarkSinkSlot,
    /// #318 INV-3 — shared with the parent handle. The `Enqueue` arm of
    /// [`push_observation`](Self::push_observation) reads this and
    /// (when populated) persists to `spec_push_queue` before the
    /// in-memory `push_back`. The `StartTurnNow` winner's drain reads
    /// it to dequeue persisted ids after a successful coalesced
    /// `turn/start`.
    queue_persist: QueuePersistSlot,
}

impl SpecPusher {
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
                    thread_id = %self.thread_id,
                    count = items.len(),
                    max_envelope_id,
                    "spec push: winner issuing coalesced turn/start (queue drained + new observation)"
                );
                if let Err(e) = self
                    .client
                    .turn_start(&self.thread_id, vec![InputItem::text(&coalesced)])
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
                self.queue.lock().await.push_back(QueuedObservation {
                    envelope_id,
                    text: text.to_string(),
                    db_id,
                });
                tracing::debug!(
                    thread_id = %self.thread_id,
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
                    !matches!(g.phase, SpecPushPhase::Issuing | SpecPushPhase::TurnRunning)
                };
                if needs_flush {
                    tracing::debug!(
                        thread_id = %self.thread_id,
                        envelope_id,
                        "spec push: persist-await race detected (phase walked past Issuing/TurnRunning while persist_one awaited); driving flush_pending to avoid stranding the enqueued row"
                    );
                    self.flush_pending().await;
                }
                Ok(PushOutcome::Enqueued)
            }
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
            thread_id: self.thread_id.clone(),
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
                thread_id = %self.thread_id,
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
    /// #325 fix — see [`SpecPushHandle::flush_pending`]. Delegated here so
    /// callers holding a [`SpecPusher`] (e.g. the registry) can drive the
    /// boot-takeover post-rehydrate flush without holding a `DashMap`
    /// guard across the `.await`.
    pub async fn flush_pending(&self) {
        flush_push_queue(
            &self.thread_id,
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
        signal_process_group(self.pgid, libc::SIGTERM);
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

/// Send `signal` to the process **group** `pgid` (`kill(-pgid, signal)`).
///
/// This is the load-bearing reap for the spec-push child. The `node`
/// launcher and the native `codex app-server` it forks share `pgid` (the
/// launcher is spawned as a group leader via `process_group(0)`), so one
/// group signal reaps both. Best-effort: a non-positive `pgid` (never
/// expected — the child is always a real positive pid) is refused so we
/// can't accidentally signal our own group or every process; `ESRCH`
/// (group already gone) is swallowed.
///
/// Returns `true` if the signal was delivered to at least one process,
/// `false` on `ESRCH`/refused (for the escalation logic in
/// [`crate::terminal_sweeper::reap_spec_push`]).
pub fn signal_process_group(pgid: i32, signal: libc::c_int) -> bool {
    if pgid <= 1 {
        // Guard against persistence corruption / a 0 pgid: kill(-1, …)
        // would signal every process we can reach, kill(0, …)/kill(-0, …)
        // would hit our own group. Never legitimate for a spawned child.
        tracing::warn!(
            pgid,
            "spec push: refusing to signal non-positive process group"
        );
        return false;
    }
    // SAFETY: `kill(2)` with a negative pid targets the process group
    // `pgid`. No memory is shared; the call is async-signal-safe.
    let rc = unsafe { libc::kill(-pgid, signal) };
    if rc == 0 {
        true
    } else {
        let err = std::io::Error::last_os_error();
        // ESRCH (no such process group) is the expected terminal state.
        tracing::debug!(pgid, signal, error = %err, "spec push: kill(-pgid) returned error (likely already gone)");
        false
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
}

/// Boot a `codex app-server` for a spec card, drive turn #1, and return
/// the live handle — DECISION A's blocking sequence.
///
/// Steps (each `?`-propagates a [`CalmError::CodexAppServer`] /
/// [`CalmError::Internal`] on failure so the create-wave route can map it
/// to a 5xx, same contract as `seed_and_spawn_spec_daemon`):
///
///   1. spawn `codex app-server --listen unix://<sock>` with the SAME env
///      the codex PTY daemon gets (`env_map` — `CODEX_HOME`, proxy, MCP
///      vars; built by [`crate::spec_card::build_codex_env_map`] +
///      post-commit augmentation in the route),
///   2. poll the socket until it accepts a connection,
///   3. [`connect`](CodexAppServer::connect) +
///      [`initialize`](CodexAppServer::initialize) +
///      [`thread_start`](CodexAppServer::thread_start),
///   4. [`turn_start`](CodexAppServer::turn_start) with the goal text,
///   5. **await `turn/started`** on the notification stream (rollout now
///      on disk),
///   6. spawn the status-tracking consumer task over the rest of the
///      stream and return everything as [`SpecPushHandle`].
///
/// `codex_bin` is the resolved `codex` CLI path (`CodexClient::codex_bin`).
/// `env_map` is the `serde_json` object map of env vars (string values
/// only are applied — non-string values are ignored, matching
/// `spawn_daemon_with_parts`).
pub async fn spawn_spec_appserver(
    codex_bin: &str,
    env_map: &Value,
    goal_text: &str,
    sock: &Path,
) -> Result<SpecPushHandle> {
    // The server `chmod 0700`s the socket's PARENT dir, so the parent must
    // be a user-owned dir (not bare sticky /tmp). The caller creates the
    // per-card subdir under the user-owned data dir; we only ensure it
    // exists + clear a stale socket file so `bind()` doesn't EADDRINUSE.
    if let Some(parent) = sock.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            CalmError::Internal(format!(
                "mkdir appserver sock parent {}: {e}",
                parent.display()
            ))
        })?;
    }
    if sock.exists() {
        let _ = std::fs::remove_file(sock);
    }
    let listen = format!("unix://{}", sock.display());

    // 1. Spawn the app-server child with the codex daemon's env. We start
    //    from a clean env-application loop (same shape as
    //    `spawn_daemon_with_parts`): apply each string-valued entry. This
    //    carries CODEX_HOME + proxy (so model turns work) + MCP vars.
    let mut cmd = Command::new(codex_bin);
    cmd.arg("app-server")
        .arg("--listen")
        .arg(&listen)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        // B1: spawn in its OWN process group so `pgid == launcher pid`.
        // The `node` launcher forks the native `codex app-server` child,
        // which inherits this pgid; teardown `kill(-pgid, …)` then reaps
        // BOTH (the native child is what holds the socket and survives a
        // launcher-only kill). `kill_on_drop` stays as a belt-and-
        // suspenders reaper of the launcher.
        .process_group(0)
        .kill_on_drop(true);
    if let Some(map) = env_map.as_object() {
        for (k, v) in map {
            if let Some(val) = v.as_str() {
                cmd.env(k, val);
            }
        }
    }
    let child = cmd
        .spawn()
        .map_err(|e| CalmError::CodexAppServer(format!("spawn codex app-server: {e}")))?;
    // `process_group(0)` makes the child a group leader, so its pgid equals
    // its pid. `child.id()` is `Some` until the child is reaped (we never
    // `wait()` it here — it lives in the handle), so the unwrap path only
    // fails if the child already exited, which `poll_connect` catches next.
    let pgid: i32 = match child.id() {
        Some(pid) => i32::try_from(pid).map_err(|_| {
            CalmError::CodexAppServer(format!("app-server pid {pid} out of i32 range"))
        })?,
        None => {
            // Child already exited before we read its pid — surface a clear
            // spawn error (kill_on_drop reaps whatever is left on return).
            return Err(CalmError::CodexAppServer(
                "codex app-server exited immediately after spawn (no pid)".to_string(),
            ));
        }
    };
    tracing::info!(pid = pgid, pgid, sock = %sock.display(), "spec push: spawned codex app-server (own process group)");

    // From here on, any early return drops `child` (→ kill_on_drop on the
    // launcher) but we must also reap the native child's GROUP. Wrap the
    // remaining fallible sequence so every `?` triggers a group SIGTERM +
    // socket-dir cleanup before propagating (S2 rollback). A `Drop`-based
    // guard keeps this DRY across the half-dozen `?` sites below.
    let mut rollback = SpawnRollback::new(pgid, sock);
    // S1 — bound the WHOLE post-spawn boot sequence under ONE overall
    // wall-clock deadline (the per-step budgets inside
    // `build_handle_after_spawn` remain as inner safety nets). On
    // timeout the future is dropped: `child` inside it drops too
    // (`kill_on_drop` reaps the launcher), and then the still-armed
    // `rollback` guard below fires the group SIGTERM + socket-dir
    // cleanup — so a wedged/degraded codex leaves no orphan group and
    // no parked registry entry, and `create_wave` gets a clean error.
    let handle = match tokio::time::timeout(
        OVERALL_BOOT_BUDGET,
        build_handle_after_spawn(child, pgid, goal_text, sock),
    )
    .await
    {
        Ok(res) => res?,
        Err(_elapsed) => {
            // `rollback` is still armed → its Drop reaps the group and
            // clears the socket dir as we return. (The `?` arm relies on
            // the same guard.)
            return Err(CalmError::CodexAppServer(format!(
                "codex app-server boot did not complete within {}s overall \
                 (spec/wave could not start)",
                OVERALL_BOOT_BUDGET.as_secs()
            )));
        }
    };
    rollback.disarm();
    Ok(handle)
}

/// #313 problem #1 — boot-time variant of [`spawn_spec_appserver`] for
/// **takeover after a kernel restart**: respawn a fresh `codex app-server`
/// for a spec card whose `codex_thread_id` is already persisted, then
/// `initialize` + `thread/resume(<thread_id>)` against it (no `turn/start`).
///
/// Same shape as [`spawn_spec_appserver`]: spawn the child in its OWN
/// process group ([`Command::process_group`]`(0)` → `pgid == launcher pid`),
/// arm a [`SpawnRollback`] guard so any `?`/timeout reaps the launcher's
/// whole group, and wrap the post-spawn sequence under
/// [`OVERALL_BOOT_BUDGET`]. The only differences from the create-wave
/// happy path are:
///   * the post-spawn sequence calls
///     [`build_handle_after_spawn_resume`] instead of
///     [`build_handle_after_spawn`] — `thread/resume(thread_id)` in place of
///     `thread/start` + `turn/start` + await `turn/started`, and
///   * a `-32600 "no rollout found"` from `thread/resume` (the wave never
///     ran turn #1 last boot — the "inert wave" case) surfaces here as a
///     [`CalmError::CodexAppServer`]; the caller treats it as non-fatal
///     and leaves the wave inert (matches the create-wave error posture).
///
/// Resume only — does NOT issue `turn/start`. The dispatcher's catch-up
/// replay will push any `id > push_watermark` events through the normal
/// push path right after this returns; the first such push issues the
/// first new turn (just like in steady state). If there are no catch-up
/// events the wave sits idle until the next live event lands.
pub async fn resume_spec_appserver(
    codex_bin: &str,
    env_map: &Value,
    thread_id: &str,
    sock: &Path,
) -> Result<SpecPushHandle> {
    if let Some(parent) = sock.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            CalmError::Internal(format!(
                "mkdir appserver sock parent {}: {e}",
                parent.display()
            ))
        })?;
    }
    if sock.exists() {
        let _ = std::fs::remove_file(sock);
    }
    let listen = format!("unix://{}", sock.display());

    let mut cmd = Command::new(codex_bin);
    cmd.arg("app-server")
        .arg("--listen")
        .arg(&listen)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .process_group(0)
        .kill_on_drop(true);
    if let Some(map) = env_map.as_object() {
        for (k, v) in map {
            if let Some(val) = v.as_str() {
                cmd.env(k, val);
            }
        }
    }
    let child = cmd
        .spawn()
        .map_err(|e| CalmError::CodexAppServer(format!("spawn codex app-server (resume): {e}")))?;
    let pgid: i32 = match child.id() {
        Some(pid) => i32::try_from(pid).map_err(|_| {
            CalmError::CodexAppServer(format!("app-server pid {pid} out of i32 range"))
        })?,
        None => {
            return Err(CalmError::CodexAppServer(
                "codex app-server exited immediately after spawn (no pid) on resume".to_string(),
            ));
        }
    };
    tracing::info!(
        pid = pgid, pgid, sock = %sock.display(), %thread_id,
        "spec push (resume): spawned codex app-server (own process group)",
    );

    let mut rollback = SpawnRollback::new(pgid, sock);
    let handle = match tokio::time::timeout(
        OVERALL_BOOT_BUDGET,
        build_handle_after_spawn_resume(child, pgid, thread_id, sock),
    )
    .await
    {
        Ok(res) => res?,
        Err(_elapsed) => {
            return Err(CalmError::CodexAppServer(format!(
                "codex app-server resume did not complete within {}s overall",
                OVERALL_BOOT_BUDGET.as_secs()
            )));
        }
    };
    rollback.disarm();
    Ok(handle)
}

// #313 PR4-round2 (B2): `adopt_live_appserver` removed.
//
// Earlier rounds tried to *adopt* a persisted-still-alive app-server (the
// rare case where the kernel `SIGKILL`ed but the native `codex
// app-server` child was reparented under `systemd --user` and stayed
// bound to the socket). Safe adoption required either probing the
// server's live turn-phase (no `thread/status` query exists on the
// codex JSON-RPC) or seeding the handle pessimistically as
// `TurnRunning` + a fallback timer — both add complexity for a marginal
// optimization. Worse, the round-1 implementation left the adopted
// handle's phase as the default `Idle`; boot catch-up then fired a
// `turn/start` against a possibly-mid-turn server and codex silently
// dropped it (the very bug #293 PR3b's queue exists to prevent).
//
// The cleanest correctness fix is to ALWAYS respawn on takeover:
// kill the persisted process group (best-effort SIGTERM + SIGKILL),
// clean the stale socket, and call [`resume_spec_appserver`]. The
// `thread/resume(<thread_id>)` rejoins the rollout on disk; the fresh
// server's notification stream reconciles the consumer-tracked phase
// (`Idle` until a `turn/started`/`turn/completed` arrives), so a push
// landing immediately after takeover either starts a new turn (idle —
// safe) or rides the next flush (mid-turn — buffered correctly).
// The downside (one extra spawn per restart in the rare adopt-eligible
// case) is well worth the simpler, race-free code path.

/// Best-effort rollback guard for the post-spawn fallible sequence
/// (S2/B1). While *armed*, dropping it sends `SIGTERM` to the child's
/// process group and clears the per-card socket dir, so any `?` early
/// return after the child is spawned can't leak the native `codex
/// app-server`. [`disarm`](Self::disarm) is called once the
/// [`SpecPushHandle`] (which then owns teardown) is built successfully.
struct SpawnRollback {
    pgid: i32,
    sock: PathBuf,
    armed: bool,
}

impl SpawnRollback {
    fn new(pgid: i32, sock: &Path) -> Self {
        Self {
            pgid,
            sock: sock.to_path_buf(),
            armed: true,
        }
    }
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for SpawnRollback {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        tracing::warn!(
            pgid = self.pgid,
            sock = %self.sock.display(),
            "spec push: spawn sequence failed after child spawn — killing process group + cleaning socket dir"
        );
        signal_process_group(self.pgid, libc::SIGTERM);
        cleanup_sock_dir(&self.sock);
    }
}

/// Remove the listen socket and its now-empty per-card dir
/// (`<data_dir>/appserver/<card_id>/`). Best-effort: a missing socket /
/// non-empty dir is fine. Mirrors the PTY `remove_file(sock)` cleanup in
/// [`crate::terminal_sweeper::reap_terminal_artifacts`].
pub fn cleanup_sock_dir(sock: &Path) {
    let _ = std::fs::remove_file(sock);
    if let Some(dir) = sock.parent() {
        // `remove_dir` only succeeds when empty — exactly what we want
        // (don't nuke a dir that unexpectedly holds other files).
        let _ = std::fs::remove_dir(dir);
    }
}

/// #313 problem #1 round-3 (B1) — verify that the per-card socket at
/// `sock` has a live listener BEFORE the caller signals the persisted
/// `pgid`.
///
/// **Why this exists.** After a host reboot the persisted `appserver_pgid`
/// almost certainly belongs to an unrelated process (PIDs/PGIDs are
/// recycled), so a `kill(-pgid, SIGTERM/SIGKILL)` could nuke arbitrary
/// user processes. The per-card socket path is UUID-scoped
/// (`<data_dir>/appserver/<card_id>/sock`), so a live listener on that
/// exact path is overwhelmingly likely to be our codex app-server (the
/// only thing that ever binds it). We use that as a cheap ownership
/// proxy: if the socket accepts a `connect(2)`, we trust the persisted
/// pgid points at the listener; if it doesn't, we skip the kill entirely
/// and just clean the dead socket file.
///
/// Returns `true` when the kill is **safe** (socket connect succeeded —
/// caller should proceed with `signal_process_group`), `false` when the
/// caller should **skip** the kill (socket missing or refused — caller
/// should still `cleanup_sock_dir` to wipe the stale path before
/// respawn).
///
/// We classify errors strictly: only `NotFound` (`ENOENT`) and
/// `ConnectionRefused` (`ECONNREFUSED`) are treated as "stale → safe to
/// skip". Any other error (e.g. `PermissionDenied`) is logged and treated
/// the same as a refused connection (skip the kill — we can't prove
/// ownership). This is the conservative default: a false-negative (we
/// skip a kill we could have done) is harmless because boot recovery's
/// `cleanup_sock_dir` plus respawn still works (the `bind(2)` succeeds
/// when the listener really is gone), while a false-positive (we kill
/// the wrong process) is the bug we're guarding against.
///
/// Optionally, callers could layer a JSON-RPC `initialize` probe on top
/// for belt-and-suspenders ownership confirmation. We don't here — the
/// UUID-scoped path is sufficient on its own and the extra round-trip
/// would delay boot. If a future regression introduces socket-path reuse
/// across non-codex daemons, fold an `initialize` probe in here.
pub async fn socket_owned_by_appserver(sock: &Path) -> bool {
    match tokio::net::UnixStream::connect(sock).await {
        Ok(_stream) => {
            // Connect succeeded → a listener is accepting on this exact
            // per-card path. Trust it as ours and proceed with the kill.
            // We drop `_stream` immediately; the listener saw an empty
            // connection but that's harmless (no `initialize` was sent).
            tracing::debug!(
                sock = %sock.display(),
                "takeover ownership probe: socket connect OK — persisted pgid presumed ours"
            );
            true
        }
        Err(e) => {
            let kind = e.kind();
            match kind {
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                    // ENOENT — socket file gone (graceful teardown / host
                    // wipe) → no listener exists, nothing to kill.
                    // ECONNREFUSED — socket path exists, no listener bound
                    // (stale dirent from a crashed process) → likewise
                    // nothing of ours to kill.
                    tracing::info!(
                        sock = %sock.display(),
                        error = %e,
                        "takeover ownership probe: socket has no live listener — \
                         skipping kill of persisted pgid (post-reboot PID may be unrelated); \
                         caller should still cleanup_sock_dir before respawn"
                    );
                    false
                }
                _ => {
                    // Any other error (EACCES, EAGAIN, …): we can't prove
                    // ownership. Default to skipping the kill — safety
                    // over reaping a leaked group (the respawn path can
                    // retry, but reviving a SIGKILLed user process can't).
                    //
                    // #315 round-4 (N3) — the conservative-skip-kill on
                    // unrecognized errors trades a worst-case "stale
                    // socket file leaks forever" (no listener, but we
                    // also don't clean up its dirent on every boot) for
                    // the worst-case "we SIGTERM/SIGKILL an unrelated
                    // process group whose pid was recycled into our
                    // persisted pgid slot post-reboot". The leak is
                    // benign — the next boot's takeover sees the same
                    // probe outcome and respawn still works (bind(2)
                    // succeeds on the path once the OS frees it via
                    // socket-file unlink in `cleanup_sock_dir`); nuking
                    // an unrelated process group is unrecoverable.
                    tracing::warn!(
                        sock = %sock.display(),
                        error = %e,
                        error_kind = ?kind,
                        "takeover ownership probe: socket connect failed with \
                         non-NotFound/ConnectionRefused error — skipping kill \
                         to avoid signaling unrelated process group"
                    );
                    false
                }
            }
        }
    }
}

/// The fallible post-spawn sequence (connect → initialize → thread/start →
/// turn/start → await turn/started → spawn consumer). Split out so the
/// [`SpawnRollback`] guard in [`spawn_spec_appserver`] wraps every `?`.
async fn build_handle_after_spawn(
    mut child: Child,
    pgid: i32,
    goal_text: &str,
    sock: &Path,
) -> Result<SpecPushHandle> {
    // 2. Poll the socket for readiness, bailing early if the child dies
    //    during boot (the common no-auth / bad-env failure mode).
    let connected = poll_connect(&mut child, sock).await?;
    let (client, mut notifs) = connected;
    let client = Arc::new(client);

    // 3. initialize + thread/start.
    client
        .initialize(ClientInfo {
            name: "neige-calm-spec-push".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        })
        .await?;
    let thread = client.thread_start().await?;
    let thread_id = thread
        .thread_id()
        .ok_or_else(|| {
            CalmError::CodexAppServer("thread/start result missing thread.id".to_string())
        })?
        .to_string();
    tracing::info!(thread_id = %thread_id, "spec push: thread started");

    // 4. turn/start with the goal. This returns the turn *ack*; the actual
    //    work streams as notifications. We do NOT wait for completion.
    let turn = client
        .turn_start(&thread_id, vec![InputItem::text(goal_text)])
        .await?;
    tracing::info!(thread_id = %thread_id, turn_id = ?turn.turn_id(), "spec push: turn #1 started (ack)");

    // 5. Await `turn/started` so a rollout exists on disk before the
    //    `--remote` TUI tries to resume. DECISION A's load-bearing step.
    //    `await_turn_started` reads notifications off the SAME `notifs`
    //    receiver the consumer task takes over below; it folds each one it
    //    pulls (including the `turn/started`) into `status` as it goes.
    //    Nothing is buffered or replayed — we simply hand the still-open
    //    receiver to the consumer task afterwards, so no notification is
    //    lost (anything not yet consumed is still queued on the mpsc).
    let status: SharedStatus = Arc::new(Mutex::new(SpecPushStatus {
        last_thread_id: Some(thread_id.clone()),
        ..Default::default()
    }));
    await_turn_started(&mut notifs, &thread_id, &status).await?;

    // 6. Spawn the consumer task and park the live handle (see
    //    [`park_handle`] for the shared tail).
    Ok(park_handle(
        child, pgid, client, thread_id, sock, notifs, status,
    ))
}

/// #313 problem #1 — boot-time takeover variant of [`build_handle_after_spawn`].
///
/// Same shape (poll-connect → initialize → spawn consumer → park-handle)
/// but **swaps `thread/start` + `turn/start` + await `turn/started`** for a
/// single `thread/resume(thread_id)`. No turn is issued on resume: the wave
/// may be mid-turn from the prior boot, or simply between turns; either way
/// the kernel's role here is to **re-attach** so the dispatcher can push
/// catch-up events onto the live thread again, not to drive a fresh turn.
///
/// A `thread/resume` failure (`-32600 "no rollout found"` on a thread that
/// never ran turn #1 in the prior boot, or any transport error) propagates
/// as [`CalmError::CodexAppServer`]; the caller treats it as non-fatal and
/// leaves the wave inert (matches the create-wave error posture).
async fn build_handle_after_spawn_resume(
    mut child: Child,
    pgid: i32,
    thread_id: &str,
    sock: &Path,
) -> Result<SpecPushHandle> {
    // 2. Poll the socket for readiness (same as the spawn path).
    let (client, notifs) = poll_connect(&mut child, sock).await?;
    let client = Arc::new(client);

    // 3. initialize + thread/resume. No `turn/start`, no `await turn/started`
    //    — resume rejoins the persisted thread by id; if a turn is mid-flight
    //    we'll observe its `turn/completed` on the notification stream and
    //    reconcile the consumer-tracked phase.
    client
        .initialize(ClientInfo {
            name: "neige-calm-spec-push".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        })
        .await?;
    let resumed = client.thread_resume(thread_id).await?;
    // Defensive: the server should echo back the same id on a successful
    // resume; if it doesn't, log and prefer the persisted id (we keyed
    // everything off it).
    if let Some(echoed) = resumed.thread_id()
        && echoed != thread_id
    {
        tracing::warn!(
            persisted = %thread_id,
            echoed = %echoed,
            "spec push (resume): server echoed a different thread id than persisted; using persisted",
        );
    }
    tracing::info!(thread_id = %thread_id, "spec push (resume): thread resumed");

    // #318 INV-4 (R2-B2) — seed the consumer-tracked phase as `Resumed`,
    // NOT `Idle`. `thread/resume` does not prove the server is between
    // turns: a prior-boot turn could still be running on the resumed
    // thread. `decide(Resumed) == Enqueue` so a catch-up push buffers
    // instead of firing a (silently-dropped) `turn/start` mid-turn; the
    // server's first `turn/started` / `turn/completed` then reconciles
    // the phase (via `record()`) before any issue actually runs.
    let status: SharedStatus = Arc::new(Mutex::new(SpecPushStatus {
        phase: SpecPushPhase::Resumed,
        last_thread_id: Some(thread_id.to_string()),
        last_turn_id: None,
    }));

    let mut handle = park_handle(
        child,
        pgid,
        client.clone(),
        thread_id.to_string(),
        sock,
        notifs,
        status.clone(),
    );

    // #318 INV-4 (codex P1) — spawn the idle-resume reconcile timer. See
    // [`RESUMED_RECONCILE_BUDGET`] for the full rationale. The timer
    // CAS-promotes `Resumed → TurnCompleted` only if a real lifecycle
    // notification has NOT already moved the phase (via `record()`); if it
    // has, the timer no-ops. After a successful promotion it invokes
    // [`flush_push_queue`] once so any catch-up observations the
    // dispatcher enqueued under `decide(Resumed) == Enqueue` get issued
    // as a coalesced `turn/start` instead of being stranded forever on a
    // truly-idle resumed thread (the case where neither `turn/started`
    // nor `turn/completed` will EVER arrive).
    let reconciler = tokio::spawn(resume_reconcile_task(
        RESUMED_RECONCILE_BUDGET,
        thread_id.to_string(),
        status,
        client,
        handle.queue.clone(),
        handle.watermark_sink.clone(),
    ));
    handle.resume_reconciler = Some(reconciler);

    Ok(handle)
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
async fn resume_reconcile_task(
    budget: Duration,
    thread_id: String,
    status: SharedStatus,
    client: Arc<CodexAppServer>,
    queue: PushQueue,
    watermark_sink: WatermarkSinkSlot,
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
    flush_push_queue(&thread_id, &status, &client, &queue, &watermark_sink).await;
}

/// Shared tail of both build paths (spawn + resume): spawn the consumer
/// task that drains the notification stream, then park everything into a
/// live [`SpecPushHandle`]. Kept intentionally minimal — the only thing
/// that differs across paths is whether a turn was driven before this
/// runs.
fn park_handle(
    child: Child,
    pgid: i32,
    client: Arc<CodexAppServer>,
    thread_id: String,
    sock: &Path,
    notifs: NotificationStream,
    status: SharedStatus,
) -> SpecPushHandle {
    let queue: PushQueue = Arc::new(Mutex::new(VecDeque::new()));
    // #313 B1 — sink slot is empty here; the dispatcher installs the real
    // persister right after registering the handle.
    let watermark_sink: WatermarkSinkSlot = Arc::new(Mutex::new(None));
    // #318 INV-3 — queue persist slot is empty here; the same dispatcher
    // sites that install the watermark sink also install this.
    let queue_persist: QueuePersistSlot = Arc::new(Mutex::new(None));
    let consumer_status = status.clone();
    let consumer_thread = thread_id.clone();
    let consumer_sink = watermark_sink.clone();
    let consumer_persist = queue_persist.clone();
    let consumer = tokio::spawn(consume_notifications(
        notifs,
        consumer_thread,
        consumer_status,
        client.clone(),
        queue.clone(),
        consumer_sink,
        consumer_persist,
    ));
    SpecPushHandle {
        child,
        pgid,
        client,
        thread_id,
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
    }
}

/// Poll `UnixStream::connect(sock)` until it succeeds (the app-server has
/// bound), bailing out as a skip-able error if the child exits during
/// boot. Mirrors the readiness loop in `spawn_daemon_with_parts`.
async fn poll_connect(
    child: &mut Child,
    sock: &Path,
) -> Result<(CodexAppServer, NotificationStream)> {
    let deadline = tokio::time::Instant::now() + SOCKET_READY_BUDGET;
    while tokio::time::Instant::now() < deadline {
        if sock.exists()
            && let Ok(pair) = CodexAppServer::connect(sock).await
        {
            return Ok(pair);
        }
        // Child died during boot — surface a clear error instead of
        // spinning until the deadline.
        if let Ok(Some(status)) = child.try_wait() {
            return Err(CalmError::CodexAppServer(format!(
                "codex app-server exited during boot (status {status})"
            )));
        }
        tokio::time::sleep(SOCKET_READY_POLL).await;
    }
    Err(CalmError::CodexAppServer(format!(
        "codex app-server did not accept a connection within {}s",
        SOCKET_READY_BUDGET.as_secs()
    )))
}

/// Drain the stream until a `turn/started` for `thread_id` arrives,
/// recording lifecycle status as we go. Errors (skip-able) if the stream
/// closes first or the budget elapses.
async fn await_turn_started(
    notifs: &mut NotificationStream,
    thread_id: &str,
    status: &SharedStatus,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + TURN_STARTED_BUDGET;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(CalmError::CodexAppServer(format!(
                "no turn/started for thread {thread_id} within {}s",
                TURN_STARTED_BUDGET.as_secs()
            )));
        }
        match tokio::time::timeout(remaining, notifs.recv()).await {
            Ok(Some(n)) => {
                if let Notification::TurnStarted { thread_id: t, turn } = &n {
                    let turn_id = turn.get("id").and_then(Value::as_str).map(str::to_string);
                    record(status, &n).await;
                    if t == thread_id {
                        tracing::debug!(thread_id, ?turn_id, "spec push: observed turn/started");
                        return Ok(());
                    }
                } else {
                    record(status, &n).await;
                }
            }
            Ok(None) => {
                return Err(CalmError::CodexAppServer(
                    "app-server connection closed before turn/started".to_string(),
                ));
            }
            Err(_) => {
                return Err(CalmError::CodexAppServer(format!(
                    "no turn/started for thread {thread_id} within {}s",
                    TURN_STARTED_BUDGET.as_secs()
                )));
            }
        }
    }
}

/// PR3a/PR3b consumer: drain the stream, tracking lifecycle status into
/// shared state for the dispatcher to read, warn loudly if an
/// approval-shaped notification ever arrives (it should not — the spec
/// cards run with `approval_policy = "never"` per
/// `build_codex_config_toml_with_prompt`), and — PR3b — **flush the push
/// queue on each `turn/completed`**: drain any buffered observations into a
/// single coalesced `turn/start`.
///
/// Exits when the connection closes (`recv` → `None`); aborted on
/// [`SpecPushHandle`] drop otherwise.
async fn consume_notifications(
    mut notifs: NotificationStream,
    thread_id: String,
    status: SharedStatus,
    client: Arc<CodexAppServer>,
    queue: PushQueue,
    watermark_sink: WatermarkSinkSlot,
    queue_persist: QueuePersistSlot,
) {
    while let Some(n) = notifs.recv().await {
        warn_on_approval(&n);
        record(&status, &n).await;
        // PR3b flush: a turn just finished — if observations piled up while
        // it ran, deliver them now as ONE coalesced turn so the spec sees
        // them between turns (codex silently drops a turn/start issued while
        // a turn is active, so we can only start one here, between turns).
        if matches!(n, Notification::TurnCompleted { .. }) {
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
    }
    tracing::debug!(
        thread_id,
        "spec push: notification stream closed; consumer exiting"
    );
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
async fn flush_push_queue(
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
async fn record(status: &SharedStatus, n: &Notification) {
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
            g.phase = SpecPushPhase::TurnRunning;
        }
        Notification::TurnCompleted { thread_id, turn } => {
            if !thread_id.is_empty() {
                g.last_thread_id = Some(thread_id.clone());
            }
            if let Some(id) = turn.get("id").and_then(Value::as_str) {
                g.last_turn_id = Some(id.to_string());
            }
            g.phase = SpecPushPhase::TurnCompleted;
        }
        Notification::Item { .. } | Notification::Other { .. } => {}
    }
}

/// Warn when a notification looks like a server→client approval request.
/// Under `approval_policy = "never"` these should never fire; if one does,
/// the spec agent would silently stall (PR3a does not answer approvals).
/// This is the early-warning hook the issue calls for.
fn warn_on_approval(n: &Notification) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a real [`SpecPushHandle`] over an in-process WS pair + a
    /// long-running dummy child (so `kill_on_drop` has something to reap),
    /// for registry-mechanics tests without a `codex` binary. Returns the
    /// handle plus the server WS end the handle's client is talking to —
    /// the caller keeps the server end alive for the handle's lifetime.
    async fn fake_handle() -> (
        SpecPushHandle,
        tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>,
    ) {
        let (client, notifs, server) = CodexAppServer::connect_pair_for_test().await;
        // A real child so `kill_on_drop(true)` reaps something on drop.
        // `sleep` exists on every unix CI image; the test never waits on it.
        // Spawn it in its OWN process group (as production does) so the
        // handle's pgid is meaningful and the group-kill Drop path is
        // exercised.
        let child = Command::new("sleep")
            .arg("60")
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .expect("spawn dummy child");
        let pgid = i32::try_from(child.id().expect("dummy child pid")).expect("pid fits i32");
        let status: SharedStatus = Arc::new(Mutex::new(SpecPushStatus::default()));
        let queue: PushQueue = Arc::new(Mutex::new(VecDeque::new()));
        let watermark_sink: WatermarkSinkSlot = Arc::new(Mutex::new(None));
        let queue_persist: QueuePersistSlot = Arc::new(Mutex::new(None));
        let client = Arc::new(client);
        let consumer = tokio::spawn(consume_notifications(
            notifs,
            "thread-test".to_string(),
            status.clone(),
            client.clone(),
            queue.clone(),
            watermark_sink.clone(),
            queue_persist.clone(),
        ));
        let handle = SpecPushHandle {
            child,
            pgid,
            client,
            thread_id: "thread-test".into(),
            sock: PathBuf::from("/tmp/test/app.sock"),
            consumer,
            resume_reconciler: None,
            status,
            queue,
            watermark_sink,
            queue_persist,
        };
        (handle, server)
    }

    #[tokio::test]
    async fn registry_insert_get_remove() {
        let reg = SpecPushRegistry::new();
        let wave = WaveId::from("wave-1");

        // Empty-state contract.
        assert!(reg.is_empty());
        assert!(!reg.contains(&wave));
        assert!(reg.remove(&wave).is_none());

        // Insert a live handle, then observe it via `contains` / `len`.
        let (handle, _server) = fake_handle().await;
        let thread_id = handle.thread_id.clone();
        assert!(reg.insert(wave.clone(), handle).is_none());
        assert!(reg.contains(&wave));
        assert!(!reg.is_empty());
        assert_eq!(reg.len(), 1);

        // A different wave is independent.
        let other = WaveId::from("wave-2");
        assert!(!reg.contains(&other));

        // Remove returns the handle (same thread id) and empties the map.
        let removed = reg
            .remove(&wave)
            .expect("remove returns the inserted handle");
        assert_eq!(removed.thread_id, thread_id);
        drop(removed); // child reaped via kill_on_drop here.
        assert!(!reg.contains(&wave));
        assert!(reg.is_empty());
        // Removing again is a clean None.
        assert!(reg.remove(&wave).is_none());
    }

    #[tokio::test]
    async fn registry_insert_replaces_and_returns_prior_handle() {
        let reg = SpecPushRegistry::new();
        let wave = WaveId::from("wave-replace");
        let (h1, _s1) = fake_handle().await;
        let (h2, _s2) = fake_handle().await;
        assert!(reg.insert(wave.clone(), h1).is_none());
        // Second insert on the same key returns the prior handle.
        let prior = reg
            .insert(wave.clone(), h2)
            .expect("prior handle returned on replace");
        drop(prior);
        assert_eq!(reg.len(), 1);
        let _ = reg.remove(&wave);
    }

    /// #322 — `park` is the production registration path; it runs the
    /// aspect framework's `BeforeHandleParkInRegistry` checks before
    /// inserting. With the production aspect set (INV-6's
    /// `WatermarkSinkInstalledAspect` is registered by
    /// `state::build_aspect_registry`), parking a handle that's missing
    /// its watermark sink MUST panic — the `fake_handle` helper builds
    /// exactly this shape (no sink installed) so this test pins the
    /// "park without sink panics" contract on the production aspect set.
    ///
    /// Without this enforcement, a future refactor that splits the
    /// `install_watermark_sink` call from the park site would silently
    /// drop queued-then-flushed envelopes from the durable watermark —
    /// the original #313 bug class, now caught at park time in release
    /// builds too (not just under `debug_assert!`).
    #[tokio::test]
    #[should_panic(expected = "watermark-sink-installed")]
    async fn park_panics_when_aspect_fails() {
        use crate::aspect::{AspectRegistry, WatermarkSinkInstalledAspect};

        let reg = SpecPushRegistry::new();
        let wave = WaveId::from("wave-no-sink");
        let (handle, _server) = fake_handle().await;
        // `fake_handle` builds a handle with `watermark_sink = None`;
        // INV-6's aspect must trip.
        let mut aspects = AspectRegistry::new();
        aspects.register_before_handle_park(Arc::new(WatermarkSinkInstalledAspect));
        // Panics inside the aspect dispatcher; the `expected` substring
        // pins the aspect name in the panic message so a rename is a
        // visible diff.
        let _ = reg.park(wave, handle, &aspects).await;
    }

    /// Sibling positive case: with the watermark sink installed, the
    /// aspect passes and `park` behaves like a bare `insert`. Pins the
    /// "park is a transparent wrapper around insert on the happy path"
    /// contract so a future aspect refactor can't break the production
    /// register sequence.
    #[tokio::test]
    async fn park_succeeds_when_invariant_holds() {
        use crate::aspect::{AspectRegistry, WatermarkSinkInstalledAspect};

        let reg = SpecPushRegistry::new();
        let wave = WaveId::from("wave-with-sink");
        let (handle, _server) = fake_handle().await;

        // Install a no-op sink so INV-6 passes. The sink itself is
        // never invoked here — the aspect only probes presence via
        // `has_watermark_sink`.
        let sink: WatermarkSink = Arc::new(|_id| Box::pin(async move {}));
        handle.install_watermark_sink(sink).await;

        let mut aspects = AspectRegistry::new();
        aspects.register_before_handle_park(Arc::new(WatermarkSinkInstalledAspect));

        // First park: no prior handle.
        assert!(reg.park(wave.clone(), handle, &aspects).await.is_none());
        assert!(reg.contains(&wave));
        let _ = reg.remove(&wave);
    }

    /// `park` with an empty aspect registry is a noop dispatcher around
    /// `insert` — proves the aspect framework's zero-aspect case stays
    /// cheap (and exists so tests that don't care about aspects can
    /// still exercise the production code path without registering
    /// every aspect every time).
    #[tokio::test]
    async fn park_with_empty_aspect_registry_just_inserts() {
        use crate::aspect::AspectRegistry;

        let reg = SpecPushRegistry::new();
        let wave = WaveId::from("wave-empty-aspects");
        let (handle, _server) = fake_handle().await;
        let aspects = AspectRegistry::new();
        assert!(reg.park(wave.clone(), handle, &aspects).await.is_none());
        assert!(reg.contains(&wave));
        let _ = reg.remove(&wave);
    }

    #[tokio::test]
    async fn handle_status_snapshot_round_trips() {
        let (handle, _server) = fake_handle().await;
        // Fresh handle: default phase.
        let st = handle.status().await;
        assert_eq!(st.phase, SpecPushPhase::Idle);
    }

    #[test]
    fn spec_push_phase_default_is_idle() {
        assert_eq!(SpecPushPhase::default(), SpecPushPhase::Idle);
        let st = SpecPushStatus::default();
        assert_eq!(st.phase, SpecPushPhase::Idle);
        assert!(st.last_thread_id.is_none());
        assert!(st.last_turn_id.is_none());
    }

    /// PR3b delivery-decision table. The whole policy lives in the pure
    /// [`decide`] fn so it's testable without an app-server. Idle /
    /// TurnCompleted are "between turns" → start a turn now; TurnRunning AND
    /// Issuing → enqueue (a `turn/start` issued mid-turn is silently dropped
    /// by codex, verified against the real binary in the PR3b probe;
    /// `Issuing` means another caller already won the right to issue — B1).
    #[test]
    fn decide_push_action_table() {
        assert_eq!(decide(SpecPushPhase::Idle), PushAction::StartTurnNow);
        assert_eq!(
            decide(SpecPushPhase::TurnCompleted),
            PushAction::StartTurnNow
        );
        assert_eq!(decide(SpecPushPhase::TurnRunning), PushAction::Enqueue);
        // B1: a caller observing `Issuing` must enqueue, never issue a second
        // (silently-dropped) turn/start.
        assert_eq!(decide(SpecPushPhase::Issuing), PushAction::Enqueue);
        // #318 INV-4 (R2-B2): a caller observing `Resumed` (boot-takeover
        // path right after `thread/resume`, before any lifecycle
        // notification has reconciled the phase) must enqueue — the
        // server may still be running the prior boot's turn and a
        // `turn/start` would be silently dropped.
        assert_eq!(decide(SpecPushPhase::Resumed), PushAction::Enqueue);
    }

    /// `push_observation` on an idle handle issues a `turn/start` the fake
    /// server observes, and claims the issue right by flipping the tracked
    /// phase to `Issuing` (B1). The phase only advances to `TurnRunning`
    /// once the server's `turn/started` lands (not sent in this test).
    #[tokio::test]
    async fn push_observation_starts_turn_when_idle() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let (handle, mut server) = fake_handle().await;
        // Idle by default → push should fire a turn/start.
        let push = tokio::spawn(async move {
            let outcome = handle.push_observation(1, "an observation").await.unwrap();
            // Single observation, no queue drain → max id is this push's id.
            assert_eq!(outcome, PushOutcome::Issued { max_envelope_id: 1 });
            // Phase claimed as `Issuing` by the winning push_observation (B1);
            // a real `turn/started` would later reconcile it to TurnRunning.
            assert_eq!(handle.status().await.phase, SpecPushPhase::Issuing);
            handle
        });

        // Server side: read the request frame, confirm it's a turn/start
        // carrying our text, and answer its id so the client's request
        // resolves.
        let req = loop {
            match server.next().await.expect("frame").expect("ws ok") {
                Message::Text(t) => break serde_json::from_str::<Value>(&t).unwrap(),
                _ => continue,
            }
        };
        assert_eq!(
            req.get("method").and_then(Value::as_str),
            Some("turn/start")
        );
        let input_text = req
            .pointer("/params/input/0/text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(input_text, "an observation");
        let id = req.get("id").cloned().unwrap();
        server
            .send(Message::Text(
                serde_json::to_string(&serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "turn": { "id": "t-push-1" } }
                }))
                .unwrap(),
            ))
            .await
            .unwrap();

        let _handle = push.await.unwrap();
        // Keep the server end alive until here.
        let _server = server;
    }

    /// `push_observation` while a turn is running enqueues (no turn/start
    /// frame is sent); the queued text is then flushed by the consumer on
    /// the next `turn/completed`.
    #[tokio::test]
    async fn push_observation_enqueues_during_turn_then_flushes_on_completed() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let (handle, mut server) = fake_handle().await;
        // Force the tracked phase to TurnRunning.
        {
            let mut g = handle.status.lock().await;
            g.phase = SpecPushPhase::TurnRunning;
        }
        // Two pushes while "running" — both enqueue, no frame on the wire.
        assert_eq!(
            handle.push_observation(10, "obs one").await.unwrap(),
            PushOutcome::Enqueued
        );
        assert_eq!(
            handle.push_observation(11, "obs two").await.unwrap(),
            PushOutcome::Enqueued
        );
        assert_eq!(handle.queue.lock().await.len(), 2);

        // Drive a turn/completed THROUGH the real notification channel so
        // the consumer task's flush path runs. The consumer is reading the
        // client's NotificationStream, which the fake server feeds via WS
        // notification frames.
        server
            .send(Message::Text(
                serde_json::to_string(&serde_json::json!({
                    "jsonrpc": "2.0", "method": "turn/completed",
                    "params": { "threadId": "thread-test", "turn": { "id": "t-done" } }
                }))
                .unwrap(),
            ))
            .await
            .unwrap();

        // The consumer should now flush a single coalesced turn/start with
        // both observations joined by a newline.
        let req = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match server.next().await.expect("frame").expect("ws ok") {
                    Message::Text(t) => {
                        let v: Value = serde_json::from_str(&t).unwrap();
                        if v.get("method").and_then(Value::as_str) == Some("turn/start") {
                            break v;
                        }
                    }
                    _ => continue,
                }
            }
        })
        .await
        .expect("flush turn/start must arrive after turn/completed");

        let input_text = req
            .pointer("/params/input/0/text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            input_text, "obs one\nobs two",
            "flush must coalesce queued observations into one turn"
        );
        // Queue drained.
        assert!(handle.queue.lock().await.is_empty());
        let _server = server;
    }

    /// #325 regression — `rehydrate_queue_from_persist` returns the
    /// envelope_ids it just re-loaded, in FIFO order. Boot-takeover's
    /// `register_and_catch_up` feeds these into a skip-set so the
    /// subsequent `events_since(watermark)` catch-up replay doesn't
    /// re-deliver the same envelope that is already sitting in the
    /// rehydrated queue.
    ///
    /// Without the returned ids, the dedup invariant would be impossible
    /// to enforce — the catch-up loop has no other way to know which
    /// rows came from disk vs. which arrived live.
    #[tokio::test]
    async fn rehydrate_queue_from_persist_returns_envelope_ids_for_dedup() {
        use std::sync::Mutex as StdMutex;

        let (handle, _server) = fake_handle().await;

        // Stub QueuePersist: `list` returns three pre-seeded rows with
        // known envelope_ids; `enqueue` / `dequeue` aren't exercised here.
        let stored: Arc<StdMutex<Vec<(i64, i64, String)>>> = Arc::new(StdMutex::new(vec![
            (101, 5, "obs-five".to_string()),
            (102, 7, "obs-seven".to_string()),
            (103, 9, "obs-nine".to_string()),
        ]));
        let stored_for_list = Arc::clone(&stored);
        let persist = QueuePersist {
            enqueue: Arc::new(|_, _| Box::pin(async { None })),
            dequeue: Arc::new(|_| Box::pin(async {})),
            list: Arc::new(move || {
                let snapshot = stored_for_list.lock().unwrap().clone();
                Box::pin(async move { snapshot })
            }),
        };
        handle.install_queue_persist(persist).await;

        // watermark=0 means "nothing already delivered" — all three rows
        // are live and should rehydrate (this test predates the round-2
        // watermark-filter; see `rehydrate_filters_stale_rows_against_watermark`
        // for the filter coverage).
        let ids = handle.rehydrate_queue_from_persist(0).await;
        assert_eq!(
            ids,
            vec![5, 7, 9],
            "#325: rehydrate must return the envelope_ids in FIFO order so \
             catch-up can dedup against them"
        );

        // Queue restored.
        let q = handle.queue.lock().await;
        assert_eq!(q.len(), 3);
        assert_eq!(q[0].envelope_id, 5);
        assert_eq!(q[0].db_id, Some(101));
        assert_eq!(q[1].envelope_id, 7);
        assert_eq!(q[2].envelope_id, 9);
    }

    /// #325 regression — full Enqueue-persist → crash → rehydrate +
    /// catch-up flow, asserting NO duplicate `turn/start` payload.
    ///
    /// The scenario codex's P1 review flagged:
    ///
    /// 1. Pre-crash: a `task.completed` envelope (id=10) arrives mid-turn
    ///    → `Inner::push_to_spec` returns `Ok(Enqueued)` → the Enqueue
    ///    arm of `push_observation` persists a `spec_push_queue` row +
    ///    pushes onto the in-memory `VecDeque`. The dispatcher
    ///    deliberately does NOT advance the durable `push_watermark` on
    ///    `Enqueued` (PR #315 PR4 B1).
    /// 2. Kernel crashes between persist and the consumer task's flush.
    /// 3. Boot-takeover resumes the spec thread (phase = Idle).
    ///    `rehydrate_queue_from_persist` re-loads row id=10 into the
    ///    in-memory queue. `events_since(watermark)` ALSO returns id=10
    ///    (watermark < 10 because step 1 deliberately didn't advance it).
    /// 4. Without dedup: catch-up replay calls
    ///    `catch_up_push_under_lock(10)` → `push_observation` on the Idle
    ///    handle → `StartTurnNow` → drains rehydrated row + appends the
    ///    catch-up envelope → ONE `turn/start` with TWO copies of the
    ///    SAME observation. (And a SECOND `spec_push_queue` row is
    ///    persisted for the appended item if there's contention; here we
    ///    assert on the wire because the wire is the codex-facing
    ///    contract that matters.)
    /// 5. With dedup (this PR's fix): catch-up skips id=10 because it's
    ///    in the rehydrated skip-set. The explicit `flush_pending` then
    ///    drives ONE `turn/start` with ONE copy of the observation.
    ///
    /// The test stays under the spec_appserver layer (no dispatcher
    /// wiring) — we directly model what `register_and_catch_up` does
    /// after rehydrate: build the skip-set from rehydrate's return value
    /// and only deliver catch-up envelopes whose id isn't in it.
    #[tokio::test]
    async fn rehydrate_then_catch_up_does_not_double_deliver_same_envelope() {
        use futures_util::{SinkExt, StreamExt};
        use std::collections::HashSet;
        use std::sync::Mutex as StdMutex;
        use tokio_tungstenite::tungstenite::Message;

        let (handle, mut server) = fake_handle().await;
        let handle = Arc::new(handle);

        // Stub persist: pre-seeded with one "surviving from prior process"
        // row at envelope_id = 10. Track dequeue ids to verify the flush
        // wires up correctly post-recovery.
        let stored: Arc<StdMutex<Vec<(i64, i64, String)>>> = Arc::new(StdMutex::new(vec![(
            999,
            10,
            "task.completed-for-id-10".to_string(),
        )]));
        let dequeued: Arc<StdMutex<Vec<i64>>> = Arc::new(StdMutex::new(Vec::new()));
        let stored_for_list = Arc::clone(&stored);
        let stored_for_enq = Arc::clone(&stored);
        let dequeued_for_close = Arc::clone(&dequeued);
        let persist = QueuePersist {
            enqueue: Arc::new(move |envelope_id, text| {
                // Mimic the SQL INSERT: append + return a fresh row id.
                let stored = Arc::clone(&stored_for_enq);
                Box::pin(async move {
                    let mut g = stored.lock().unwrap();
                    let new_id = g.iter().map(|(id, _, _)| *id).max().unwrap_or(0) + 1;
                    g.push((new_id, envelope_id, text));
                    Some(new_id)
                })
            }),
            dequeue: Arc::new(move |ids| {
                let dequeued = Arc::clone(&dequeued_for_close);
                Box::pin(async move {
                    dequeued.lock().unwrap().extend(ids);
                })
            }),
            list: Arc::new(move || {
                let snap = stored_for_list.lock().unwrap().clone();
                Box::pin(async move { snap })
            }),
        };
        handle.install_queue_persist(persist).await;

        // Step 3: rehydrate. The skip-set is built from the returned ids
        // — same as `register_and_catch_up` does in production.
        // watermark=9: the prior process's dispatcher cooperatively
        // withheld push_watermark on Enqueued (PR #315 PR4 B1), so the
        // durable watermark is BELOW envelope_id=10 and the rehydrate
        // filter keeps the row live.
        let rehydrated_ids = handle.rehydrate_queue_from_persist(9).await;
        assert_eq!(
            rehydrated_ids,
            vec![10],
            "rehydrate must return the envelope_ids of pending rows"
        );
        let skip: HashSet<i64> = rehydrated_ids.iter().copied().collect();

        // Step 4: simulate catch-up replay. `events_since(watermark)`
        // would return id=10 (since the watermark is below it — see
        // step 1 in the docstring). We model the catch-up loop's
        // dedup-then-deliver: if id is in skip-set, do NOT call
        // push_observation; otherwise call it.
        //
        // With dedup ON (this PR's fix): id=10 is in skip → loop does
        // nothing → no StartTurnNow fires.
        let catch_up_ids = [10i64];
        let mut replayed = 0usize;
        for id in catch_up_ids {
            if skip.contains(&id) {
                continue;
            }
            // We don't actually exercise the dispatcher here — the
            // production catch_up_push_under_lock would call
            // push_observation. The skip path means this branch is dead
            // in this test; asserting via `replayed` below.
            let _ = handle
                .push_observation(id, "would-be-duplicate-payload")
                .await;
            replayed += 1;
        }
        assert_eq!(
            replayed, 0,
            "#325 dedup: id=10 must be skipped because the same envelope \
             already sits in the rehydrated queue"
        );

        // Step 5: explicit flush_pending — the no-other-catch-up edge
        // case `register_and_catch_up` now drives. With our fix, this
        // issues ONE `turn/start` with ONE copy of the rehydrated
        // observation.
        let flush_handle = Arc::clone(&handle);
        let flush = tokio::spawn(async move {
            flush_handle.flush_pending().await;
        });

        // Read frames from the server side; capture every `turn/start`
        // payload and ack so the client's request resolves.
        let observed_turns: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let observed_for_task = Arc::clone(&observed_turns);
        let server_task = tokio::spawn(async move {
            // Bounded — one turn/start expected, plus generous slack.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
            while tokio::time::Instant::now() < deadline {
                let f = match tokio::time::timeout(Duration::from_millis(250), server.next()).await
                {
                    Ok(Some(Ok(Message::Text(t)))) => t,
                    Ok(Some(Ok(_))) => continue,
                    Ok(None) | Ok(Some(Err(_))) => break,
                    Err(_) => continue,
                };
                let v: Value = match serde_json::from_str(&f) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if v.get("method").and_then(Value::as_str) != Some("turn/start") {
                    continue;
                }
                let id = v.get("id").cloned().unwrap_or(Value::Null);
                let text = v
                    .pointer("/params/input/0/text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                observed_for_task.lock().unwrap().push(text);
                // Ack so the client's `turn_start` future resolves.
                let _ = server
                    .send(Message::Text(
                        serde_json::to_string(&serde_json::json!({
                            "jsonrpc": "2.0", "id": id,
                            "result": { "turn": { "id": "t-recovery" } }
                        }))
                        .unwrap(),
                    ))
                    .await;
            }
            server
        });

        // Wait for the flush to finish (client side resolves).
        flush.await.expect("flush_pending joins");
        // Give the server task time to observe the turn/start frame.
        let _ = tokio::time::timeout(Duration::from_millis(500), server_task).await;

        let turns = observed_turns.lock().unwrap().clone();
        assert_eq!(
            turns.len(),
            1,
            "#325: exactly one `turn/start` must be issued after rehydrate + \
             catch-up — not two (duplicate-payload bug) and not zero \
             (rehydrated items left undelivered). got: {turns:?}"
        );
        assert_eq!(
            turns[0], "task.completed-for-id-10",
            "#325: the single `turn/start` must carry the rehydrated \
             observation's text exactly once (no double-line coalescing)"
        );

        // Sanity: the rehydrated row was dequeued after delivery, so a
        // hypothetical *third* boot wouldn't see it again. This mirrors
        // the persist↔in-memory 1:1 invariant `flush_push_queue` upholds.
        let drained = dequeued.lock().unwrap().clone();
        assert_eq!(
            drained,
            vec![999],
            "#325: the rehydrated row's db_id must be dequeued after the \
             explicit flush_pending — otherwise a third boot would replay it again"
        );
    }

    /// #325 round-2 P1 — `Enqueue`-arm persist-await race close.
    ///
    /// The race codex's round-2 review flagged:
    /// 1. Phase observed as `TurnRunning` → action fixed to `Enqueue`.
    /// 2. `persist_one` awaits the DB insert (real-world: tens of ms).
    /// 3. During (2), a `turn/completed` arrives; consumer task's
    ///    `flush_push_queue` runs, finds the queue empty (our row not yet
    ///    appended), walks phase Issuing→TurnCompleted with nothing to
    ///    flush — and no further `turn/completed` is expected.
    /// 4. `persist_one` returns; we append to the in-memory queue and
    ///    return `Ok(Enqueued)`. Without the round-2 fix, the row stays
    ///    stranded until an unrelated live event or restart.
    ///
    /// The fix: after the append, re-acquire status and — if phase is no
    /// longer in `{Issuing, TurnRunning}` — drive an idempotent
    /// `flush_pending`. This test:
    ///   * installs a slow `persist.enqueue` that blocks on a oneshot,
    ///     simulating the DB-insert window;
    ///   * starts the push (it blocks inside `persist_one`);
    ///   * walks the phase to `TurnCompleted` from outside (simulating
    ///     the consumer's flush-of-empty path);
    ///   * unblocks `persist_one`;
    ///   * asserts the row is FLUSHED via a `turn/start` on the wire —
    ///     not stranded in the queue.
    #[tokio::test]
    async fn enqueue_arm_flushes_when_persist_await_races_past_turn_completed() {
        use futures_util::{SinkExt, StreamExt};
        use std::sync::Mutex as StdMutex;
        use tokio::sync::oneshot;
        use tokio_tungstenite::tungstenite::Message;

        let (handle, mut server) = fake_handle().await;
        let handle = Arc::new(handle);
        // Force phase to `TurnRunning` so `push_observation`'s `decide` picks
        // `Enqueue` (the racy arm).
        {
            let mut g = handle.status.lock().await;
            g.phase = SpecPushPhase::TurnRunning;
        }

        // Slow persist: block on a oneshot so we control exactly when
        // `persist_one` returns. The same closure must also serve
        // dequeues (flush success path).
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let release_rx = Arc::new(StdMutex::new(Some(release_rx)));
        let next_db_id = Arc::new(std::sync::atomic::AtomicI64::new(0));
        let next_db_id_e = Arc::clone(&next_db_id);
        let release_rx_e = Arc::clone(&release_rx);
        let persist = QueuePersist {
            enqueue: Arc::new(move |_envelope_id, _text| {
                let next_db_id = Arc::clone(&next_db_id_e);
                let release_rx = Arc::clone(&release_rx_e);
                Box::pin(async move {
                    // Take the receiver (single-shot); if absent, no wait.
                    let rx_opt = release_rx.lock().unwrap().take();
                    if let Some(rx) = rx_opt {
                        let _ = rx.await;
                    }
                    Some(next_db_id.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1)
                })
            }),
            dequeue: Arc::new(|_| Box::pin(async {})),
            list: Arc::new(|| Box::pin(async { Vec::new() })),
        };
        handle.install_queue_persist(persist).await;

        // Step 1+2: start the push; it blocks inside `persist_one`.
        let push_handle = Arc::clone(&handle);
        let push =
            tokio::spawn(async move { push_handle.push_observation(42, "racy-observation").await });

        // Wait for `push_observation` to enter the persist await. We can't
        // observe the precise moment, so yield + brief sleep is sufficient
        // — `push_observation`'s status-lock + decide work is pure-CPU.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Step 3: simulate the consumer's `flush_push_queue` on an empty
        // queue — it walks phase Issuing→TurnCompleted but finds nothing
        // to flush. We just set the phase directly: the bug is "after this
        // walk, no `turn/completed` is expected to come back", and our
        // post-append recheck must detect that and force a flush.
        {
            let mut g = handle.status.lock().await;
            g.phase = SpecPushPhase::TurnCompleted;
        }

        // Step 4: release `persist_one` so `push_observation` continues.
        // The Enqueue arm appends to the queue, re-acquires status, sees
        // `TurnCompleted` (NOT in {Issuing, TurnRunning}), and drives
        // `flush_pending` → `flush_push_queue` → coalesced `turn/start`.
        let _ = release_tx.send(());

        // Server side: expect ONE `turn/start` with our observation text.
        // Ack it so the client's request resolves and `flush_pending`
        // returns.
        let req = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match server.next().await.expect("frame").expect("ws ok") {
                    Message::Text(t) => {
                        let v: Value = serde_json::from_str(&t).unwrap();
                        if v.get("method").and_then(Value::as_str) == Some("turn/start") {
                            break v;
                        }
                    }
                    _ => continue,
                }
            }
        })
        .await
        .expect(
            "#325 round-2 P1: the stranded row must be flushed via flush_pending — \
             no `turn/start` arrived within 3s, meaning the row is still sitting \
             in the queue undelivered",
        );

        let text = req
            .pointer("/params/input/0/text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            text, "racy-observation",
            "#325 round-2 P1: the forced flush must carry the just-appended observation"
        );

        // Ack so the push future resolves.
        let id = req.get("id").cloned().unwrap();
        server
            .send(Message::Text(
                serde_json::to_string(&serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "turn": { "id": "t-race-close" } }
                }))
                .unwrap(),
            ))
            .await
            .unwrap();

        let outcome = push.await.unwrap().unwrap();
        assert_eq!(
            outcome,
            PushOutcome::Enqueued,
            "#325 round-2 P1: outcome is still Enqueued (the contract holds — the \
             caller never sees the internal force-flush); the flush_pending is a \
             best-effort safety net on top, not a new outcome variant"
        );

        // Queue must be empty post-flush (the row was flushed, then dequeued).
        assert!(
            handle.queue.lock().await.is_empty(),
            "#325 round-2 P1: queue must drain after the forced flush"
        );

        let _server = server;
    }

    /// #325 round-2 P2 — `rehydrate_queue_from_persist` must drop rows
    /// whose `envelope_id <= watermark` (the prior process's flush
    /// succeeded and bumped the watermark, but the `dequeue` write didn't
    /// commit — or committed and the row is stale). Without filtering,
    /// those rows would be re-pushed into the in-memory queue and
    /// redelivered to codex, bypassing the durable watermark that
    /// `events_since(watermark)` itself correctly skips.
    ///
    /// Scenario: watermark=10; rehydrate sees rows with envelope_ids
    /// `[5, 7, 12]`. 5 and 7 are stale (already delivered) and must be
    /// physically dequeued; 12 is live and must be queued + returned.
    #[tokio::test]
    async fn rehydrate_filters_stale_rows_against_watermark() {
        use std::sync::Mutex as StdMutex;

        let (handle, _server) = fake_handle().await;

        // Stub persist: pre-seed three rows; one above watermark, two
        // below. Track dequeue ids so we can assert the stale rows are
        // physically removed.
        let stored: Arc<StdMutex<Vec<(i64, i64, String)>>> = Arc::new(StdMutex::new(vec![
            (501, 5, "stale-five".to_string()),
            (502, 7, "stale-seven".to_string()),
            (503, 12, "live-twelve".to_string()),
        ]));
        let dequeued: Arc<StdMutex<Vec<i64>>> = Arc::new(StdMutex::new(Vec::new()));
        let stored_for_list = Arc::clone(&stored);
        let dequeued_for_close = Arc::clone(&dequeued);
        let persist = QueuePersist {
            enqueue: Arc::new(|_, _| Box::pin(async { None })),
            dequeue: Arc::new(move |ids| {
                let dequeued = Arc::clone(&dequeued_for_close);
                Box::pin(async move {
                    dequeued.lock().unwrap().extend(ids);
                })
            }),
            list: Arc::new(move || {
                let snap = stored_for_list.lock().unwrap().clone();
                Box::pin(async move { snap })
            }),
        };
        handle.install_queue_persist(persist).await;

        // watermark=10 — rows with envelope_id <= 10 (i.e. 5, 7) are
        // already delivered and must be dropped + dequeued.
        let live_ids = handle.rehydrate_queue_from_persist(10).await;
        assert_eq!(
            live_ids,
            vec![12],
            "#325 round-2 P2: only rows with envelope_id > watermark must be \
             returned for the catch-up skip-set"
        );

        // In-memory queue must contain ONLY the live row.
        let q = handle.queue.lock().await;
        assert_eq!(
            q.len(),
            1,
            "#325 round-2 P2: stale rows (envelope_id <= watermark) must be \
             skipped during in-memory rehydrate"
        );
        assert_eq!(q[0].envelope_id, 12);
        assert_eq!(q[0].text, "live-twelve");
        assert_eq!(q[0].db_id, Some(503));
        drop(q);

        // Stale rows must be physically dequeued so the NEXT boot doesn't
        // see them again. Order in `dequeued` matches stored-row iteration
        // order (5 before 7) — the rehydrate code processes the list in
        // order and collects stale_db_ids in encounter order.
        let mut deq = dequeued.lock().unwrap().clone();
        deq.sort();
        assert_eq!(
            deq,
            vec![501, 502],
            "#325 round-2 P2: stale rows must be physically deleted via the \
             persist.dequeue callback so a future boot doesn't see them again"
        );
    }

    /// Shared B1 harness: build a fake handle whose server faithfully models
    /// codex's **silent-drop** — a `turn/start` that arrives while a turn is
    /// already active is OK-acked but produces NO `turn/started`/
    /// `turn/completed` and its observation is NEVER recorded as delivered.
    /// The `driver` runs the push/flush sequence under test; afterwards we
    /// let the consumer flush any deferred (enqueued) observation across a
    /// later cycle, then return the set of observation lines the server
    /// actually delivered. A correct (single-winner) implementation loses
    /// nothing; the pre-fix double-issue drops one.
    async fn b1_collect_delivered<F, Fut>(driver: F) -> std::collections::HashSet<String>
    where
        F: FnOnce(Arc<SpecPushHandle>) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        use futures_util::{SinkExt, StreamExt};
        use std::collections::HashSet;
        use tokio_tungstenite::tungstenite::Message;

        let (handle, server) = fake_handle().await;
        let handle = Arc::new(handle);
        // Phase = TurnCompleted (between turns) with A already queued. The
        // envelope ids on queue items are arbitrary in this test (delivery
        // is asserted on `text`, not id) — pick 1 for A, the driver picks
        // 2 for B.
        {
            let mut g = handle.status.lock().await;
            g.phase = SpecPushPhase::TurnCompleted;
            handle.queue.lock().await.push_back(QueuedObservation {
                envelope_id: 1,
                text: "A".to_string(),
                db_id: None,
            });
        }

        // Server task: model codex's silent-drop. A turn is "active" for a
        // short WINDOW (started → +TURN_RUN → completed). The read loop keeps
        // reading WHILE a turn is active (select! against the completion
        // timer), so a `turn/start` arriving during that window is genuinely
        // the dropped case. Without holding `active` across reads, two serial
        // turn/starts would never overlap and nothing would drop.
        let server_task = tokio::spawn(async move {
            const TURN_RUN: Duration = Duration::from_millis(60);
            let mut server = server;
            let mut delivered: HashSet<String> = HashSet::new();
            let mut turn_seq = 0u32;
            let mut active_until: Option<tokio::time::Instant> = None;
            let overall = tokio::time::Instant::now() + Duration::from_secs(3);
            loop {
                if tokio::time::Instant::now() >= overall {
                    break;
                }
                let next = if let Some(done_at) = active_until {
                    tokio::select! {
                        biased;
                        _ = tokio::time::sleep_until(done_at) => {
                            // Turn window elapsed → emit turn/completed, idle.
                            turn_seq += 1;
                            let turn_id = format!("turn-{turn_seq}");
                            server
                                .send(Message::Text(
                                    serde_json::to_string(&serde_json::json!({
                                        "jsonrpc": "2.0", "method": "turn/completed",
                                        "params": { "threadId": "thread-test", "turn": { "id": turn_id } }
                                    }))
                                    .unwrap(),
                                ))
                                .await
                                .ok();
                            active_until = None;
                            continue;
                        }
                        f = server.next() => f,
                    }
                } else {
                    match tokio::time::timeout(Duration::from_millis(200), server.next()).await {
                        Ok(f) => f,
                        Err(_) => continue, // idle read tick
                    }
                };
                let frame = match next {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(_)) => continue,
                    _ => break, // stream closed
                };
                let v: Value = match serde_json::from_str(&frame) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if v.get("method").and_then(Value::as_str) != Some("turn/start") {
                    continue;
                }
                let id = v.get("id").cloned().unwrap_or(Value::Null);
                let text = v
                    .pointer("/params/input/0/text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                turn_seq += 1;
                let turn_id = format!("turn-{turn_seq}");
                // Always OK-ack (codex acks even the silently-dropped turn).
                server
                    .send(Message::Text(
                        serde_json::to_string(&serde_json::json!({
                            "jsonrpc": "2.0", "id": id,
                            "result": { "turn": { "id": turn_id } }
                        }))
                        .unwrap(),
                    ))
                    .await
                    .ok();
                if active_until.is_some() {
                    // SILENT DROP — a turn is already active. No record, no
                    // lifecycle. (This is the codex behavior B1 guards.)
                    continue;
                }
                // This turn runs: record each coalesced line + emit
                // turn/started, and stay active for TURN_RUN.
                for line in text.split('\n') {
                    delivered.insert(line.to_string());
                }
                server
                    .send(Message::Text(
                        serde_json::to_string(&serde_json::json!({
                            "jsonrpc": "2.0", "method": "turn/started",
                            "params": { "threadId": "thread-test", "turn": { "id": turn_id } }
                        }))
                        .unwrap(),
                    ))
                    .await
                    .ok();
                active_until = Some(tokio::time::Instant::now() + TURN_RUN);
            }
            delivered
        });

        // Run the push/flush sequence under test.
        driver(Arc::clone(&handle)).await;

        // Let the consumer task flush any deferred (enqueued) observation on
        // the server's `turn/completed`. Poll until the queue drains or a
        // bounded budget elapses ("A then B across two cycles" settles here),
        // then a small grace so the final flush turn/start is recorded.
        let drain_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if handle.queue.lock().await.is_empty() {
                break;
            }
            if tokio::time::Instant::now() >= drain_deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
        drop(handle);

        tokio::time::timeout(Duration::from_secs(4), server_task)
            .await
            .expect("server task joins")
            .expect("server task ok")
    }

    /// B1 regression (DETERMINISTIC) — the canonical flush-vs-push interleave
    /// must NEVER drop an observation.
    ///
    /// Phase `TurnCompleted` with `[A]` queued; run `push_observation("B")`
    /// to completion, THEN `flush_push_queue`. This is the exact ordering the
    /// pre-fix code mishandled: pre-fix `push` flips `TurnRunning` and issues
    /// `turn/start("B")` WITHOUT draining the queue, so `A` is left for the
    /// flush, which then issues `turn/start("A")` while B's turn is active →
    /// codex silently drops it → **A is lost** (drained, OK-acked, never
    /// re-buffered). Post-fix the winning push drains `[A]` and appends `B`
    /// into ONE `turn/start("A\nB")`, and the flush sees `Issuing` and
    /// no-ops, so both survive. Deterministic: the ordering forces the bug,
    /// no scheduler luck required. (Verified to FAIL pre-fix / PASS post-fix.)
    #[tokio::test]
    async fn b1_push_then_flush_never_drops_observation() {
        let delivered = b1_collect_delivered(|handle| async move {
            // Winner push (drains [A] + appends B, post-fix). Pre-fix it
            // issues only "B", stranding A for the racy flush below.
            let _ = handle.push_observation(2, "B").await;
            // The flush that, pre-fix, issues the second (dropped) turn.
            flush_push_queue(
                &handle.thread_id,
                &handle.status,
                &handle.client,
                &handle.queue,
                &handle.watermark_sink,
                &handle.queue_persist,
            )
            .await;
        })
        .await;

        assert!(
            delivered.contains("A"),
            "observation A was DROPPED by the flush-vs-push race (B1) — delivered={delivered:?}"
        );
        assert!(
            delivered.contains("B"),
            "observation B was DROPPED (B1) — delivered={delivered:?}"
        );
    }

    /// B1 regression (CONCURRENT) — the same invariant under a genuinely
    /// concurrent drive: a `flush_push_queue` (what the consumer runs on
    /// `turn/completed`) and a dispatcher `push_observation` spawned and
    /// `join!`ed. Looped to shake out scheduling nondeterminism. Whichever
    /// issuer wins, neither A nor B may be lost (the loser only enqueues; the
    /// single winner coalesces / the deferred item flushes next cycle).
    #[tokio::test]
    async fn b1_concurrent_flush_and_push_never_drops_observation() {
        for iter in 0..40 {
            let delivered = b1_collect_delivered(|handle| async move {
                let h_flush = Arc::clone(&handle);
                let flush = tokio::spawn(async move {
                    flush_push_queue(
                        &h_flush.thread_id,
                        &h_flush.status,
                        &h_flush.client,
                        &h_flush.queue,
                        &h_flush.watermark_sink,
                        &h_flush.queue_persist,
                    )
                    .await;
                });
                let h_push = Arc::clone(&handle);
                let push = tokio::spawn(async move {
                    let _ = h_push.push_observation(2, "B").await;
                });
                let _ = tokio::join!(flush, push);
            })
            .await;

            assert!(
                delivered.contains("A"),
                "iter {iter}: observation A was DROPPED (B1 race) — delivered={delivered:?}"
            );
            assert!(
                delivered.contains("B"),
                "iter {iter}: observation B was DROPPED (B1 race) — delivered={delivered:?}"
            );
        }
    }

    #[tokio::test]
    async fn record_tracks_turn_lifecycle() {
        let status: SharedStatus = Arc::new(Mutex::new(SpecPushStatus::default()));

        record(
            &status,
            &Notification::TurnStarted {
                thread_id: "t1".into(),
                turn: serde_json::json!({ "id": "u1" }),
            },
        )
        .await;
        {
            let g = status.lock().await;
            assert_eq!(g.phase, SpecPushPhase::TurnRunning);
            assert_eq!(g.last_thread_id.as_deref(), Some("t1"));
            assert_eq!(g.last_turn_id.as_deref(), Some("u1"));
        }

        record(
            &status,
            &Notification::TurnCompleted {
                thread_id: "t1".into(),
                turn: serde_json::json!({ "id": "u1" }),
            },
        )
        .await;
        {
            let g = status.lock().await;
            assert_eq!(g.phase, SpecPushPhase::TurnCompleted);
        }
    }

    #[test]
    fn warn_on_approval_matches_request_shapes() {
        // These method names mirror the issue's approval/requestUserInput
        // server→client request shapes. `warn_on_approval` only logs, so
        // we assert the match predicate directly (no panic / no return
        // value) by reconstructing its condition.
        for method in [
            "item/commandExecution/requestApproval",
            "item/fileChange/requestApproval",
            "item/permissions/requestApproval",
            "item/tool/requestUserInput",
        ] {
            assert!(
                method.contains("requestApproval") || method.contains("requestUserInput"),
                "approval-shaped method should match the warn predicate: {method}"
            );
            // Exercise the function for coverage — it must not panic.
            warn_on_approval(&Notification::Item {
                method: method.to_string(),
                params: Value::Null,
            });
        }
        // A normal item must NOT match.
        let benign = "item/agentMessage/delta";
        assert!(!(benign.contains("requestApproval") || benign.contains("requestUserInput")));
    }

    /// B1 regression: a child spawned in its own process group is reaped by
    /// `kill(-pgid, …)`. This models the node-launcher/native-child shape
    /// with a `sh -c` launcher that itself spawns a long `sleep` grandchild
    /// in the SAME group; killing the group must reap BOTH (the bug was
    /// that a pid-only kill left the grandchild alive). We assert the
    /// grandchild is gone after a group SIGTERM.
    #[tokio::test]
    async fn kill_process_group_reaps_launcher_and_child() {
        use std::process::Stdio;
        // Launcher: print its child's pid, then exec a sleep so the
        // launcher itself stays alive too. The grandchild `sleep` inherits
        // the launcher's process group (we set `process_group(0)` on the
        // launcher, making it the group leader).
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 120 & echo $! ; wait")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(false)
            .spawn()
            .expect("spawn launcher");
        let pgid = i32::try_from(child.id().expect("launcher pid")).expect("pid fits i32");

        // Read the grandchild pid the launcher printed.
        use tokio::io::AsyncReadExt;
        let mut out = child.stdout.take().expect("stdout piped");
        let mut buf = Vec::new();
        // Bounded read: the launcher prints one line immediately.
        let grandchild_pid = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let mut b = [0u8; 64];
                let n = out.read(&mut b).await.expect("read launcher stdout");
                if n == 0 {
                    panic!("launcher closed stdout before printing child pid");
                }
                buf.extend_from_slice(&b[..n]);
                if let Some(nl) = buf.iter().position(|&c| c == b'\n') {
                    let line = String::from_utf8_lossy(&buf[..nl]);
                    break line.trim().parse::<i32>().expect("grandchild pid");
                }
            }
        })
        .await
        .expect("timed out reading grandchild pid");

        // Sanity: the grandchild shares the launcher's process group.
        // (`/proc/<pid>/stat` field 5 is pgrp.)
        let stat = std::fs::read_to_string(format!("/proc/{grandchild_pid}/stat"))
            .expect("grandchild /proc/.../stat readable while alive");
        // Field layout: pid (comm) state ppid pgrp ... ; comm can contain
        // spaces/parens, so split on the last ')'.
        let after = stat.rsplit_once(')').expect("stat has comm").1;
        let pgrp: i32 = after
            .split_whitespace()
            .nth(2)
            .expect("pgrp field")
            .parse()
            .expect("pgrp int");
        assert_eq!(
            pgrp, pgid,
            "grandchild must share the launcher process group"
        );

        // The load-bearing reap: signal the whole group.
        assert!(
            signal_process_group(pgid, libc::SIGTERM),
            "group SIGTERM should reach at least one process"
        );

        // Both launcher and grandchild must die. Poll briefly.
        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        let mut gone = false;
        for _ in 0..50 {
            // kill(pid, 0) probes existence without signaling.
            let alive = unsafe { libc::kill(grandchild_pid, 0) } == 0;
            if !alive {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            gone,
            "grandchild pid {grandchild_pid} must be reaped by the group kill (pid-only kill would have leaked it)"
        );
    }

    /// Refuse to signal a non-positive process group (persistence-corruption
    /// guard) — `kill(-1)`/`kill(0)` would hit far too much.
    #[test]
    fn signal_process_group_refuses_non_positive() {
        assert!(!signal_process_group(0, libc::SIGTERM));
        assert!(!signal_process_group(-5, libc::SIGTERM));
        assert!(!signal_process_group(1, libc::SIGTERM));
    }

    /// S1 — the overall boot deadline must be at least as large as the
    /// single largest inner per-step budget, otherwise the inner safety
    /// nets would never get a chance to fire under their own budget and
    /// the overall cap alone would govern (defeating "keep the per-step
    /// budgets as inner safety nets"). The chosen cap (45 s) is the hard
    /// ceiling on the whole sequence; this pins the relationship so a
    /// future tweak to either constant is a conscious change.
    #[test]
    fn overall_boot_budget_caps_the_sequence() {
        // Sanity: the overall cap is the binding ceiling and is at least
        // the largest single step (so a healthy step never trips the
        // overall deadline before its own budget).
        assert!(OVERALL_BOOT_BUDGET >= SOCKET_READY_BUDGET);
        assert!(OVERALL_BOOT_BUDGET >= TURN_STARTED_BUDGET);
        assert_eq!(OVERALL_BOOT_BUDGET, Duration::from_secs(45));
    }

    /// S1 — the rollback the overall-timeout path relies on. When the boot
    /// sequence is aborted (a `?` early return OR the
    /// `OVERALL_BOOT_BUDGET` timeout dropping the in-flight future), the
    /// still-armed [`SpawnRollback`] guard must (a) reap the child's whole
    /// process GROUP via `kill(-pgid)` — so no orphan `codex app-server`
    /// is leaked — and (b) clean the per-card socket dir. This test models
    /// the timeout/early-return teardown directly: spawn a group-leader
    /// child, drop an armed `SpawnRollback` pointed at its pgid + a socket
    /// inside a tempdir, and assert the child is gone and the socket dir
    /// removed. (The 45 s overall path can't be wall-clock-tested cheaply;
    /// this exercises the load-bearing cleanup it triggers.)
    #[tokio::test]
    async fn spawn_rollback_reaps_group_and_cleans_socket_dir_on_drop() {
        // A real group-leader child (mirrors the production spawn shape:
        // `process_group(0)` → pgid == pid).
        let mut child = Command::new("sleep")
            .arg("120")
            .process_group(0)
            .kill_on_drop(false)
            .spawn()
            .expect("spawn group-leader child");
        let pid = i32::try_from(child.id().expect("child pid")).expect("pid fits i32");

        // Per-card socket dir + a stale socket file inside it, as
        // `spawn_spec_appserver` would have created.
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock_dir = tmp.path().join("appserver").join("card-xyz");
        std::fs::create_dir_all(&sock_dir).expect("mkdir sock dir");
        let sock = sock_dir.join("app.sock");
        std::fs::write(&sock, b"").expect("touch sock file");

        // Arm the guard, then drop it — exactly what the overall-timeout
        // arm (and every `?` early return) does on the way out.
        {
            let _rollback = SpawnRollback::new(pid, &sock);
            // armed by default; dropping here fires the reap + cleanup.
        }

        // The guard's group SIGTERM terminates the child; `wait()` reaps
        // the zombie so the subsequent `kill(pid, 0)` existence probe sees
        // ESRCH rather than a still-addressable zombie. (Without the wait,
        // a terminated-but-unreaped child still answers signal-0 as alive.)
        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        let mut gone = false;
        for _ in 0..50 {
            // kill(pid, 0) probes existence without signaling.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if !alive {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            gone,
            "armed SpawnRollback drop must reap the child's process group (pid {pid})"
        );

        // The socket file + its now-empty per-card dir must be cleaned.
        assert!(
            !sock.exists(),
            "armed SpawnRollback drop must remove the listen socket file"
        );
        assert!(
            !sock_dir.exists(),
            "armed SpawnRollback drop must remove the now-empty per-card socket dir"
        );
    }

    /// S1 — a *disarmed* rollback (the success path, after the
    /// [`SpecPushHandle`] takes ownership of teardown) must NOT reap the
    /// group or touch the socket dir.
    #[tokio::test]
    async fn spawn_rollback_disarmed_is_a_noop() {
        let mut child = Command::new("sleep")
            .arg("120")
            .process_group(0)
            .kill_on_drop(true) // we own teardown here; ensure no leak.
            .spawn()
            .expect("spawn group-leader child");
        let pid = i32::try_from(child.id().expect("child pid")).expect("pid fits i32");

        let tmp = tempfile::tempdir().expect("tempdir");
        let sock_dir = tmp.path().join("appserver").join("card-disarm");
        std::fs::create_dir_all(&sock_dir).expect("mkdir sock dir");
        let sock = sock_dir.join("app.sock");
        std::fs::write(&sock, b"").expect("touch sock file");

        {
            let mut rollback = SpawnRollback::new(pid, &sock);
            rollback.disarm();
            // dropping a disarmed guard must do nothing.
        }

        // Child still alive (the handle, not the guard, would reap it).
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            0,
            "disarmed SpawnRollback must NOT reap the group"
        );
        // Socket dir untouched.
        assert!(
            sock.exists(),
            "disarmed rollback must not remove the socket"
        );
        assert!(
            sock_dir.exists(),
            "disarmed rollback must not remove the socket dir"
        );

        // Teardown: kill the child ourselves (kill_on_drop also covers it).
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    /// #313 problem #1 round-3 (B1) — `socket_owned_by_appserver` returns
    /// `false` when the persisted socket *path does not exist* (graceful
    /// teardown wiped it, or host reboot lost the tmpfs entry). The
    /// caller must NOT signal the persisted pgid in this case; the
    /// persisted pgid is presumably recycled.
    #[tokio::test]
    async fn socket_ownership_probe_missing_path_returns_false() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp
            .path()
            .join("appserver")
            .join("missing-card")
            .join("sock");
        // Note: we don't create the parent dirs — the path is entirely absent.
        assert!(
            !sock.exists(),
            "test precondition: socket path should be absent"
        );
        let ok = socket_owned_by_appserver(&sock).await;
        assert!(
            !ok,
            "socket_owned_by_appserver must return false for an absent path \
             (kill of persisted pgid would be unsafe — could hit unrelated process)"
        );
    }

    /// #313 problem #1 round-3 (B1) — `socket_owned_by_appserver` returns
    /// `false` when the socket FILE exists but no process is bound
    /// (stale dirent from a crashed launcher). `UnixStream::connect`
    /// returns `ECONNREFUSED` in that case; we treat it the same as
    /// missing: skip the kill, just `cleanup_sock_dir` and respawn.
    #[tokio::test]
    async fn socket_ownership_probe_stale_dirent_returns_false() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock_dir = tmp.path().join("appserver").join("card-stale");
        std::fs::create_dir_all(&sock_dir).expect("mkdir sock dir");
        let sock = sock_dir.join("sock");
        // Touch a regular file at the socket path — `UnixStream::connect`
        // on a non-socket file returns `ECONNREFUSED` (or similar
        // not-a-socket error mapped to `ConnectionRefused` on most
        // Unixes). Either way the probe must return false.
        std::fs::write(&sock, b"").expect("touch sock file");
        assert!(sock.exists(), "test precondition: socket path should exist");
        let ok = socket_owned_by_appserver(&sock).await;
        assert!(
            !ok,
            "socket_owned_by_appserver must return false for a stale dirent / \
             non-socket path (no live listener = no ownership = unsafe to kill)"
        );
    }

    /// #313 problem #1 round-3 (B1) — `socket_owned_by_appserver` returns
    /// `true` when a real listener is bound to the path. This is the
    /// "our app-server is still running" case: caller proceeds with the
    /// SIGTERM → grace → SIGKILL → cleanup_sock_dir sequence. We model
    /// the listener with a bare `tokio::net::UnixListener` — the probe
    /// only does `connect(2)`, not a codex JSON-RPC handshake, so any
    /// accept-loop is sufficient evidence of a bound listener.
    #[tokio::test]
    async fn socket_ownership_probe_live_listener_returns_true() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock_dir = tmp.path().join("appserver").join("card-live");
        std::fs::create_dir_all(&sock_dir).expect("mkdir sock dir");
        let sock = sock_dir.join("sock");
        let listener = tokio::net::UnixListener::bind(&sock).expect("bind listener");
        // Drive a single accept in the background so the probe's
        // connect resolves cleanly. We don't read or write — the probe
        // just needs to confirm a listener exists.
        let accept_task = tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        let ok = socket_owned_by_appserver(&sock).await;
        assert!(
            ok,
            "socket_owned_by_appserver must return true when a listener is bound \
             (the per-card path is UUID-scoped; a live listener is almost certainly ours)"
        );
        let _ = tokio::time::timeout(Duration::from_secs(1), accept_task).await;
    }

    /// #318 INV-4 (codex P1) — sanity on the budget constant: short enough
    /// that the boot-takeover catch-up delay is bounded, long enough that
    /// a healthy mid-turn server's in-flight `turn/completed` can land
    /// before we falsely promote and (potentially) silent-drop a flush.
    #[test]
    fn resumed_reconcile_budget_sane() {
        assert!(RESUMED_RECONCILE_BUDGET >= Duration::from_secs(1));
        assert!(RESUMED_RECONCILE_BUDGET <= Duration::from_secs(30));
        assert_eq!(RESUMED_RECONCILE_BUDGET, Duration::from_secs(5));
    }

    /// #318 INV-4 (codex P1) — idle-resume case: the reconcile timer fires
    /// (no `turn/started`/`turn/completed` ever arrives on the resumed
    /// stream because the prior boot's turn already completed before
    /// kernel crash), promotes `Resumed` -> `TurnCompleted`, and flushes a
    /// queued catch-up observation as a coalesced `turn/start`.
    ///
    /// Without this timer (pre-fix), the observation sits in the in-memory
    /// queue forever — codex never sends `turn/completed` on a truly-idle
    /// thread, so the consumer task's `flush_push_queue` never fires.
    ///
    /// Drives time with `tokio::time::pause()` / `advance` so the test is
    /// fully deterministic and doesn't depend on the wall-clock 5s budget.
    #[tokio::test(start_paused = true)]
    async fn resume_reconcile_flushes_queue_on_idle_resume() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let (handle, mut server) = fake_handle().await;
        // Seed the post-`thread/resume` state: phase = Resumed, with a
        // catch-up observation already buffered (the dispatcher's
        // `decide(Resumed) == Enqueue` placed it here, awaiting a
        // lifecycle signal that, in the idle-resume case, never arrives).
        {
            let mut g = handle.status.lock().await;
            g.phase = SpecPushPhase::Resumed;
            handle.queue.lock().await.push_back(QueuedObservation {
                envelope_id: 7,
                text: "catch-up obs".to_string(),
            });
        }

        // Spawn the reconciler the way `build_handle_after_spawn_resume`
        // does (via the extracted helper), with a tiny test budget so the
        // assertions about timing are explicit.
        let budget = Duration::from_secs(5);
        let reconciler = tokio::spawn(resume_reconcile_task(
            budget,
            handle.thread_id.clone(),
            handle.status.clone(),
            handle.client.clone(),
            handle.queue.clone(),
            handle.watermark_sink.clone(),
        ));

        // Before the budget elapses, NOTHING should happen — phase is
        // still Resumed and the queue is untouched. Advance just shy of
        // the budget and confirm.
        tokio::time::advance(budget - Duration::from_millis(50)).await;
        tokio::task::yield_now().await;
        assert_eq!(handle.status.lock().await.phase, SpecPushPhase::Resumed);
        assert_eq!(handle.queue.lock().await.len(), 1);

        // Advance past the budget — the timer fires, CAS-promotes to
        // TurnCompleted, then `flush_push_queue` claims `Issuing` and
        // issues a `turn/start` carrying the queued observation. Read it
        // off the wire to prove the flush actually happened.
        tokio::time::advance(Duration::from_millis(200)).await;

        // The flush will issue `turn/start` and await its response.
        // Resume real wall-clock so the WS round-trip doesn't deadlock on
        // paused time waiting for a frame that arrives via the IO runtime.
        tokio::time::resume();

        let req = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match server.next().await.expect("frame").expect("ws ok") {
                    Message::Text(t) => {
                        let v: Value = serde_json::from_str(&t).unwrap();
                        if v.get("method").and_then(Value::as_str) == Some("turn/start") {
                            break v;
                        }
                    }
                    _ => continue,
                }
            }
        })
        .await
        .expect("reconcile flush must issue a turn/start within budget");

        let input_text = req
            .pointer("/params/input/0/text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            input_text, "catch-up obs",
            "reconcile flush must deliver the queued catch-up observation"
        );

        // Answer the turn/start so the flush's await resolves cleanly and
        // the reconciler task completes (otherwise its outstanding request
        // would dangle on the fake server's WS pair).
        let id = req.get("id").cloned().unwrap();
        server
            .send(Message::Text(
                serde_json::to_string(&serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "turn": { "id": "t-reconcile-1" } }
                }))
                .unwrap(),
            ))
            .await
            .unwrap();

        reconciler
            .await
            .expect("reconcile task must complete after issuing the flush");

        // Post-conditions: queue drained, phase rolled forward off Resumed.
        assert!(handle.queue.lock().await.is_empty());
        let phase = handle.status.lock().await.phase;
        // After a successful turn/start, phase is `Issuing` (waiting on
        // server's `turn/started` to reconcile to `TurnRunning`). The key
        // invariant: it is NO LONGER `Resumed`, so a later push will not
        // route to `Enqueue` again for the wrong reason.
        assert_ne!(
            phase,
            SpecPushPhase::Resumed,
            "phase must advance past Resumed after the reconcile flush"
        );
        let _server = server;
    }

    /// #318 INV-4 (codex P1) — race-window: if a real `turn/started` (or
    /// `turn/completed`) lands DURING the reconcile budget, the consumer
    /// task's `record()` advances the phase off `Resumed` and the timer's
    /// CAS must NO-OP (no spurious `turn/start` issued). This guards the
    /// "mid-turn resume that emits its `turn/completed` within budget"
    /// case — we must not double-issue when the natural lifecycle is
    /// about to drive the flush itself.
    #[tokio::test(start_paused = true)]
    async fn resume_reconcile_no_ops_when_notification_arrives_first() {
        let (handle, server) = fake_handle().await;
        {
            let mut g = handle.status.lock().await;
            g.phase = SpecPushPhase::Resumed;
            // Queue intentionally empty — this test only asserts the CAS
            // no-op behavior, not the flush body.
        }

        let budget = Duration::from_secs(5);
        let reconciler = tokio::spawn(resume_reconcile_task(
            budget,
            handle.thread_id.clone(),
            handle.status.clone(),
            handle.client.clone(),
            handle.queue.clone(),
            handle.watermark_sink.clone(),
        ));

        // Within the budget, simulate the consumer task seeing a real
        // `turn/started` -> `record()` flips phase to TurnRunning.
        tokio::time::advance(Duration::from_secs(1)).await;
        record(
            &handle.status,
            &Notification::TurnStarted {
                thread_id: handle.thread_id.clone(),
                turn: serde_json::json!({ "id": "u-resume-1" }),
            },
        )
        .await;
        assert_eq!(handle.status.lock().await.phase, SpecPushPhase::TurnRunning);

        // Now advance past the budget. The timer fires, observes
        // `phase != Resumed`, and no-ops (does NOT issue a flush).
        tokio::time::advance(budget).await;
        reconciler
            .await
            .expect("reconcile task must complete (no-op branch)");

        // Phase must still be TurnRunning — the timer did not clobber it
        // back to TurnCompleted (which would be a correctness bug: a turn
        // really IS running on the server).
        assert_eq!(
            handle.status.lock().await.phase,
            SpecPushPhase::TurnRunning,
            "reconcile timer must not overwrite a real lifecycle-driven phase"
        );
        let _server = server;
    }
}
