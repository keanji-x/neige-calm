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
type PushQueue = Arc<Mutex<VecDeque<QueuedObservation>>>;

/// One queued observation: the envelope id (so the flush path can report
/// the max delivered id to the dispatcher) plus the rendered text codex
/// will receive.
#[derive(Debug, Clone)]
struct QueuedObservation {
    envelope_id: i64,
    text: String,
}

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
    ///     `turn/start` on the next `turn/completed`.
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
    /// (drain + append), releases it, then issues. [`flush_push_queue`]
    /// uses the same status-then-queue, never-nested order. Preserving this
    /// keeps the path deadlock-free (the review confirmed today's code is).
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
                    // `turn/completed` flush retries them.
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
                Ok(PushOutcome::Issued { max_envelope_id })
            }
            PushAction::Enqueue => {
                self.queue.lock().await.push_back(QueuedObservation {
                    envelope_id,
                    text: text.to_string(),
                });
                tracing::debug!(
                    thread_id = %self.thread_id,
                    envelope_id,
                    "spec push: turn active / being issued — enqueued observation for flush"
                );
                Ok(PushOutcome::Enqueued)
            }
        }
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
    pub fn insert(&self, wave_id: WaveId, handle: SpecPushHandle) -> Option<SpecPushHandle> {
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

    // Seed the consumer-tracked status with the resumed thread id. Phase
    // stays `Idle` until the notification stream tells us otherwise — the
    // next observation will start a turn if no turn is in flight; if one
    // is, the server's `turn/started`/`turn/completed` will reconcile the
    // phase before any push tries to issue.
    let status: SharedStatus = Arc::new(Mutex::new(SpecPushStatus {
        last_thread_id: Some(thread_id.to_string()),
        ..Default::default()
    }));

    Ok(park_handle(
        child,
        pgid,
        client,
        thread_id.to_string(),
        sock,
        notifs,
        status,
    ))
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
    let consumer_status = status.clone();
    let consumer_thread = thread_id.clone();
    let consumer_sink = watermark_sink.clone();
    let consumer = tokio::spawn(consume_notifications(
        notifs,
        consumer_thread,
        consumer_status,
        client.clone(),
        queue.clone(),
        consumer_sink,
    ));
    SpecPushHandle {
        child,
        pgid,
        client,
        thread_id,
        sock: sock.to_path_buf(),
        consumer,
        status,
        queue,
        watermark_sink,
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
) {
    while let Some(n) = notifs.recv().await {
        warn_on_approval(&n);
        record(&status, &n).await;
        // PR3b flush: a turn just finished — if observations piled up while
        // it ran, deliver them now as ONE coalesced turn so the spec sees
        // them between turns (codex silently drops a turn/start issued while
        // a turn is active, so we can only start one here, between turns).
        if matches!(n, Notification::TurnCompleted { .. }) {
            flush_push_queue(&thread_id, &status, &client, &queue, &watermark_sink).await;
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
    // sink is `None` only in test paths that don't exercise persistence
    // (the production dispatcher always installs one in
    // `register_handle_with_sink` before the first push can land).
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
        let client = Arc::new(client);
        let consumer = tokio::spawn(consume_notifications(
            notifs,
            "thread-test".to_string(),
            status.clone(),
            client.clone(),
            queue.clone(),
            watermark_sink.clone(),
        ));
        let handle = SpecPushHandle {
            child,
            pgid,
            client,
            thread_id: "thread-test".into(),
            sock: PathBuf::from("/tmp/test/app.sock"),
            consumer,
            status,
            queue,
            watermark_sink,
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
}
