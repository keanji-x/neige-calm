use std::collections::{HashSet, VecDeque};
#[cfg(feature = "fixtures")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use serde_json::Value;
#[cfg(feature = "fixtures")]
use std::collections::HashMap;
#[cfg(feature = "fixtures")]
use std::sync::OnceLock;
#[cfg(feature = "fixtures")]
use tokio::sync::Notify;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc, oneshot};
use tokio::task::AbortHandle;

use crate::card_role_cache::CardRoleCache;
use crate::codex_appserver::{InputItem, Notification};
use crate::db::{Repo, write_in_tx_typed};
use crate::error::{CalmError, Result};
use crate::event::{Event, EventBus, EventScope, HarnessQueueChange};
use crate::harness::config::HarnessConfig;
use crate::harness::observation::Observation;
use crate::harness::queue::{
    FoldOutcome, MutationApplied, MutationRefused, MutationResult, QueueEntry, QueueEntryId,
    QueueMutation, apply_mutation, input_segments_for_entries, locate_entry, try_fold_tail,
};
use crate::harness::snapshot::{HarnessPhaseTag, HarnessSnapshot, IssuedInputSegments};
use crate::harness::state::{HarnessState, IssuingKind, run_status_for};
use crate::harness::token_usage::TokenUsage;
use crate::ids::{ActorId, CardId, TrackId};
use crate::model::HarnessInputSegment;
use crate::planner_attachments::bind::BoundAttachment;
use crate::planner_model::{
    CardModelSelection, FailureKind, InstallationDefaults, TurnModelSelection,
    effective_model_for_catalog_lookup, resolve_turn_selection,
};
use crate::shared_codex_appserver::SharedCodexAppServer;
use crate::track_area_cache::TrackAreaCache;
use crate::track_vcs;

/// #1449 — park a runtime immediately before it can turn its pending queue into
/// a turn, so a test can order "the workspace is repointed" strictly *before*
/// "the first message drains".
///
/// The race this makes deterministic is real and silent: `PATCH
/// /api/tracks/{id}` supersedes every live runtime of the track and mints a
/// successor, and until #1449 the successor started with an empty queue. If the
/// drain lost the race, the sentence the user typed sat forever on a superseded
/// row that nothing reads. Under load the existing
/// `a_replay_of_a_success_that_happened_on_a_retry_key_survives_a_repoint`
/// catches it a few times out of six; this hook makes it every time.
///
/// # Why here and not at the drain itself
///
/// The queue is taken a few statements below, under `inner.issuance` — and
/// `PlannerHarness::shutdown_inner` takes that same lock. A hook parked while
/// holding it would deadlock the very `PATCH` the test is trying to order
/// against: the fence's `shutdown_fenced_harness` would wait for the run loop
/// that is waiting for the test that is waiting for the `PATCH`. Parking one
/// statement earlier keeps the property the test needs — the queue has not been
/// touched — while leaving the shutdown path free. The re-check of
/// `shutting_down` immediately after the lock is what stops the parked loop
/// from draining once it is released.
///
/// Same convention as `WorkspaceRepointRaceHook` in `routes/tracks.rs`, and the
/// same limit to it: the hook struct, the registry and the wait are
/// `fixtures`-only, so a release build compiles no map and no rendezvous. The
/// call site and `wait_at_planner_harness_drain_race_hook` itself are NOT
/// `cfg`-gated — in a release build the body collapses to
/// `let _ = worker_session_id;` and the call remains, taking that one
/// argument.
///
/// # Arming
///
/// [`ANY_RUNTIME`] parks whichever runtime reaches the drain FIRST, not a
/// runtime chosen by name — the two are the same thing only while the process
/// has exactly one harness that can drain. `nextest` gives every test its own
/// process, so no other test in this suite can steal the entry; a card that
/// starts a second harness within one test can. Arm by runtime id whenever the
/// id is knowable.
#[cfg(feature = "fixtures")]
#[derive(Clone)]
pub struct PlannerHarnessDrainRaceHook {
    pub entered: Arc<Notify>,
    pub release: Arc<Notify>,
}

#[cfg(feature = "fixtures")]
fn planner_harness_drain_race_hooks()
-> &'static StdMutex<HashMap<String, PlannerHarnessDrainRaceHook>> {
    static HOOKS: OnceLock<StdMutex<HashMap<String, PlannerHarnessDrainRaceHook>>> =
        OnceLock::new();
    HOOKS.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// Arm the hook for whichever runtime reaches the drain next, rather than for a
/// named one.
///
/// Needed because the id of the runtime under test cannot be known before it
/// exists: `POST /api/tracks` mints the runtime, starts its run loop and lets it
/// drain, all before the 201 is written. Arming after the response is a race
/// that the drain usually wins — which is precisely the race #1449 is about. The
/// entry is still one-shot, so a second runtime is unaffected unless the test
/// arms it again.
#[cfg(feature = "fixtures")]
pub const ANY_RUNTIME: &str = "#1449-any-runtime";

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn install_planner_harness_drain_race_hook_for_test(
    worker_session_id: &str,
    hook: PlannerHarnessDrainRaceHook,
) {
    planner_harness_drain_race_hooks()
        .lock()
        .expect("planner harness drain hook mutex")
        .insert(worker_session_id.to_string(), hook);
}

async fn wait_at_planner_harness_drain_race_hook(worker_session_id: &str) {
    #[cfg(feature = "fixtures")]
    {
        let hook = {
            let mut hooks = planner_harness_drain_race_hooks()
                .lock()
                .expect("planner harness drain hook mutex");
            hooks
                .remove(worker_session_id)
                .or_else(|| hooks.remove(ANY_RUNTIME))
        };
        if let Some(hook) = hook {
            hook.entered.notify_one();
            hook.release.notified().await;
        }
    }
    #[cfg(not(feature = "fixtures"))]
    let _ = worker_session_id;
}

const OBSERVATION_BUFFER: usize = 256;
/// Hard cap on queued observations. Public because it is a wire-visible
/// constant: at this length an incoming user message folds into the tail
/// instead of taking a slot, which is the one accepted way `POST
/// /planner/input` answers with `entry_id: null`. A test that hardcoded 256
/// would be restating this rather than checking it.
pub const MAX_PENDING_QUEUE_LEN: usize = 256;
const RECENT_HOOK_KEY_CACHE_LEN: usize = 256;
/// #615 F3 fold-in: upper bound on the size of a folded `UserMessage` tail
/// entry. Each individual `/planner/input` body is capped at 32_768 chars at the
/// route layer, but the fold path concatenates adjacent UserMessage
/// observations into one entry. Under sustained backpressure a stream of
/// max-size posts could otherwise grow the tail without bound and inflate every
/// snapshot rewrite. Once the folded text would exceed this cap, refuse to
/// fold; the eviction-fallback path in `enqueue_pending_observation` then drops
/// a non-hard-fire entry from the queue front and lets the incoming UserMessage
/// take a fresh slot.
const MAX_FOLDED_USER_MESSAGE_CHARS: usize = 4 * 32_768;

#[derive(Clone)]
pub struct PlannerHarness {
    inner: Arc<Inner>,
}

pub struct PlannerHarnessParams {
    pub worker_session_id: String,
    pub track_id: TrackId,
    pub card_id: CardId,
    pub thread_id: Option<String>,
    pub repo: Arc<dyn Repo>,
    pub events: EventBus,
    pub card_role_cache: CardRoleCache,
    pub track_area_cache: TrackAreaCache,
    pub daemon: Arc<SharedCodexAppServer>,
    pub config: HarnessConfig,
    pub snapshot: HarnessSnapshot,
}

pub(super) struct Inner {
    worker_session_id: String,
    track_id: TrackId,
    card_id: CardId,
    thread_id: RwLock<Option<String>>,
    repo: Arc<dyn Repo>,
    events: EventBus,
    card_role_cache: CardRoleCache,
    track_area_cache: TrackAreaCache,
    daemon: Arc<SharedCodexAppServer>,
    observations: ObservationIngress,
    state: Mutex<HarnessState>,
    last_phase: Mutex<HarnessPhaseTag>,
    /// #1505 PR1 — one queue, not a bundle of parallel arrays. `QueueEntry`
    /// carries the envelope id, the #1449 message ids and (for user input) the
    /// stable entry id, so there is no second array that can drift out of step
    /// with this one.
    pending_queue: Mutex<VecDeque<QueueEntry>>,
    recent_hook_keys: Mutex<VecDeque<String>>,
    recent_hook_key_set: Mutex<HashSet<String>>,
    push_watermark: Mutex<i64>,
    last_turn_id: Mutex<Option<String>>,
    issued_turn_id: Mutex<Option<String>>,
    issued_turn_head: Mutex<Option<track_vcs::CommitHash>>,
    /// See `HarnessSnapshot::projection_client_id`. Live copy of the slot;
    /// `maybe_issue_turn` is its only writer.
    projection_client_id: Mutex<Option<QueueEntryId>>,
    /// See `HarnessSnapshot::issued_input_segments`: the segments of the
    /// turn a pre-#1625-P2 binary left in flight, read from its snapshot at
    /// boot and never written back. Consumed by that turn's completed
    /// `userMessage` echo (`on_notification`), or superseded by the next
    /// drain (`maybe_issue_turn`), whichever comes first — exactly the
    /// lifetime the pre-P2 slot had.
    legacy_issued_input_segments: Mutex<Option<IssuedInputSegments>>,
    last_report_body_sha256: Mutex<Option<String>>,
    last_seen_head: Mutex<Option<track_vcs::CommitHash>>,
    /// #1255 S3 — latest context-window reading from `thread/tokenUsage/updated`.
    /// Latest-wins: every frame replaces this whole value (modulo the sticky
    /// window in [`TokenUsage::sticky_merge`]), and it rides the runtime
    /// snapshot out to `worker_sessions.handle_state` on the next persist.
    token_usage: Mutex<Option<TokenUsage>>,
    debounce: Mutex<DebounceState>,
    interrupt_deadline: Mutex<Option<(String, Instant)>>,
    /// #1505 S4 review — do not re-attempt turn issuance before this instant.
    ///
    /// Set when model selection, briefing preparation or `turn/start` fails.
    ///
    /// A resolution failure re-buffers, which arms `hard_fire`, which means
    /// the very next 50 ms tick would try again — and each attempt costs a
    /// transcript-refresh WRITE transaction before it gets far enough to fail.
    /// While codex is unreachable that failure is instant and the loop would
    /// spin at twenty write transactions a second for as long as the outage
    /// lasts. This paces it. Nothing else is delayed: the queue is untouched,
    /// and the only cost of the pause is that a selection repaired inside it
    /// waits out the remainder.
    issuance_retry_after: Mutex<Option<Instant>>,
    /// #1505 S4 review — what to tell the reader about why their message has
    /// not been sent, or `None` when there is nothing worth saying.
    ///
    /// Live-only, exactly like `phase`: it is re-derived by the next attempt,
    /// so a restart or a supersede loses nothing that will not come straight
    /// back. It is deliberately NOT on `HarnessSnapshot` — a field there would
    /// be dropped by the adapter's three-key copy on supersede and would have
    /// to be kept in step by hand forever, for a value whose whole lifetime is
    /// one retry interval.
    ///
    /// These failures fill it, and the wording differs because the reader's
    /// situation does:
    ///
    ///  * a briefing read failed — says input is retained and preparation will retry;
    ///  * a selection nobody can determine — names the choice that fixes it;
    ///  * a turn codex refused — says the message was NOT sent;
    ///  * a run of retryable failures that has lasted past
    ///    [`HarnessConfig::transient_silence_budget`] — says the message is
    ///    still coming.
    ///
    /// A brief retryable failure fills nothing. A codex restart is nobody's
    /// problem to act on, and a notice for every one of them would train the
    /// reader to ignore the field.
    ///
    /// **It is therefore not "only failures that cannot clear themselves"** —
    /// that was true when only the first case existed, and stopped being true
    /// when the third was added without this sentence being revisited.
    issuance_block: Mutex<Option<String>>,
    /// When the current run of consecutive refusals began, or `None` when the
    /// last attempt succeeded. Feeds
    /// [`HarnessConfig::transient_silence_budget`].
    refusing_since: Mutex<Option<Instant>>,
    /// #1505 S4 review round 2 — how many issuance attempts have been refused.
    ///
    /// Exists so a test can assert the retry is PACED without waiting on a
    /// wall clock: an interval smaller than a test's own deadline is invisible
    /// to any assertion that merely waits for an outcome, which is how the
    /// first cut of the pacing shipped untested.
    #[cfg(feature = "fixtures")]
    refused_issuances: AtomicU64,
    shutdown: broadcast::Sender<()>,
    shutting_down: Arc<AtomicBool>,
    /// Synchronous ingress/shutdown linearization for the non-awaiting event
    /// producers. `observe_delivery` holds this through `try_send`; shutdown
    /// closes it under the same mutex before notifying the run loop.
    observations_closed: StdMutex<bool>,
    /// User input has no replay payload, so it is folded and persisted before
    /// the HTTP request returns. Shutdown waits behind this lock before closing.
    durable_observation: Mutex<()>,
    /// Linearizes `turn/start` with shutdown. A request may have reached Codex
    /// before the daemon returns a turn id; shutdown waits for that response,
    /// then interrupts the now-known turn before aborting the run loop.
    issuance: Mutex<()>,
    /// Issue #682 review — issuance kill-switch for dev-forced harnesses.
    /// Checked at the top of [`maybe_issue_turn`]; observations still
    /// enqueue normally, the harness just never calls `turn_start`. Only
    /// the fixtures-gated [`PlannerHarness::pause_issuance_for_dev`] sets it,
    /// so production harnesses never pause.
    issuance_paused: AtomicBool,
    /// #1505 PR2b — queue entries the load-time truncation discarded whose
    /// `harness.queue.changed { dropped }` row does not exist yet.
    ///
    /// A `tokio::Mutex`: `flush_dropped_announcements` holds it across the
    /// event inserts so two racing flushers cannot both take the same id.
    ///
    /// Held here rather than passed to the run loop alone because the loss and
    /// its announcement have to share a fate, and the run loop is not the
    /// first thing that can make the loss durable:
    /// `planner_harness_start_adapter` calls `handle.persist_snapshot()` on
    /// its OWN task immediately after `PlannerHarness::run` returns, which can
    /// run before the spawned loop is ever polled. `persist_snapshot_inner`
    /// therefore drains this first and refuses to write if it cannot — so the
    /// truncated queue reaches the row only once the record of what it lost
    /// is already there, and a failure leaves the untruncated row intact for
    /// the next boot to retry.
    unannounced_drops: Mutex<Vec<QueueEntryId>>,
    /// #1625 P3 review round 1 — the entries codex accepted through
    /// `turn/steer` into the turn that is running, in the order they were
    /// steered, held until that turn completes. See [`SteeredEntry`] for why
    /// a completion has to look at them: codex records a steered input only
    /// at its next model request, and an interrupt before that clears it.
    ///
    /// Live-only, like `issuance_block`: the list is about a turn this
    /// process is watching, and a restart loses the turn with it — the
    /// snapshot restores as `Resumed`, and no completion for that turn will
    /// ever reach this loop, so nothing here could be acted on.
    steered_into_running_turn: Mutex<Vec<SteeredEntry>>,
    abort_handle: StdMutex<Option<AbortHandle>>,
    config: HarnessConfig,
}

/// #1625 P3 review round 1 — one entry codex has accepted into the running
/// turn, remembered until that turn ends.
///
/// Accepted is not recorded. Vendored codex (`external/codex` @ `5a440c0`):
/// `turn/steer` only pushes the input into the turn's `pending_input`
/// (`core/src/session/mod.rs`, `steer_input`); it is written to history and
/// echoed as `item/started` + `item/completed` at the turn's NEXT model
/// request (`session/turn.rs`, `run_hooks_and_record_inputs`); and an
/// interrupt in between — the Stop button, or `max_turn_duration` — goes
/// through `abort_all_tasks` → `input_queue.clear_pending`, which empties
/// `pending_input` (`core/src/session/input_queue.rs`). The turn then ends
/// with `status: interrupted` and the sentence is in nobody's record: not in
/// codex's history, not in the queue, and its transcript row still says it
/// was sent.
///
/// The completion sweep (`restore_steered_entries_codex_dropped`) tells the
/// two outcomes apart by the row: the echo upgrades the projection in place
/// (`transcript_projection_upgrade` stamps the turn id), and on the ordered
/// notification stream the echo precedes the completion, so a row that is
/// STILL a projection when the completion arrives is a sentence codex never
/// recorded. That row is deleted and the entry goes back to the head of the
/// queue with its id and rev, exactly as a refused steer's does.
struct SteeredEntry {
    /// The turn `turn/steer` named as `expectedTurnId` and codex confirmed.
    turn_id: String,
    entry: QueueEntry,
    /// The transcript projection written once codex accepted, or `None` if
    /// that write failed (logged at the time). Without a row the sweep has
    /// nothing to consult and leaves the entry alone: it cannot tell a
    /// delivery from a drop, and re-delivering a sentence codex has is the
    /// worse error of the two.
    row_id: Option<i64>,
}

pub(super) struct IssueTurnHandle<'a> {
    daemon: &'a Arc<SharedCodexAppServer>,
}

impl<'a> IssueTurnHandle<'a> {
    pub(super) fn from_reconciliation(inner: &'a Inner) -> Self {
        Self {
            daemon: &inner.daemon,
        }
    }

    /// `selection` is required, not defaulted: #1505 S4-3 makes "which model
    /// runs this turn" part of what issuance means, and a default here would
    /// be a silent answer to it.
    ///
    /// `client_user_message_id` is the key of the projection row the drain
    /// wrote for this batch (#1625 P2); codex hands it back as
    /// `item.clientId` on the echoed `userMessage`.
    pub(super) async fn issue(
        &self,
        thread_id: &str,
        input: Vec<InputItem>,
        selection: &TurnModelSelection,
        client_user_message_id: Option<&str>,
    ) -> Result<String> {
        self.daemon
            .turn_start(thread_id, input, selection, client_user_message_id)
            .await
    }
}

#[derive(Clone, Debug)]
pub struct HarnessObservationDelivery {
    pub entry: QueueEntry,
}

enum HarnessObservationCommand {
    Delivery(HarnessObservationDelivery),
    Durable {
        deliveries: Vec<HarnessObservationDelivery>,
        persisted: oneshot::Sender<Result<DurableAck>>,
    },
    /// #1505 PR2 — a human edit or delete against one queue entry.
    ///
    /// It rides the same mpsc as every other command and is handled in the
    /// same `observations.recv()` arm, so it never runs part-way through a
    /// tick's `watchdog_tick` / `maybe_issue_turn`.
    ///
    /// That is NOT what makes "a delete racing a drain has two outcomes, never
    /// three" true, and the earlier draft of this comment said it was.
    /// `pending_queue` is a `tokio::Mutex`; the disjunction comes from
    /// `queue::apply_mutation` doing its whole compare-and-swap under one hold
    /// of it, and from `maybe_issue_turn` emptying the queue under the same
    /// lock before it calls `turn/start`. Running this on the caller's task
    /// instead was tried as a mutation and reddened nothing, which is correct.
    ///
    /// What the single arm does buy: a mutation cannot be interleaved with the
    /// rest of a tick, and it cannot be starved by one either.
    ///
    /// Unlike `Durable`, the sender does NOT hold `durable_observation` while
    /// it waits. That mutex is held by `observe_durable_observations` across
    /// its `confirmation.await`, so a POST stuck behind a slow issuance blocks
    /// every later POST; a mutation must not be able to join that queue behind
    /// an unrelated send. What this buys is concurrent WAITING, not lower
    /// latency: every one of these still waits for the same `select!`.
    Mutate {
        mutation: QueueMutation,
        actor: ActorId,
        applied: oneshot::Sender<Result<MutationResult>>,
    },
    /// #1625 P3 — a human asking for one queued entry to go into the turn
    /// that is running right now, instead of waiting for the next one.
    ///
    /// What rules out the double delivery the design named (the turn ends
    /// under the steer, the tick drains the entry into the next turn, codex
    /// delivers it in both) is take-before-ask under the queue lock: the
    /// entry leaves the queue BEFORE codex is asked — the drain's own order,
    /// which empties the queue before `turn/start` — and every way back in
    /// (`rebuffer_head`) takes the same lock the drain reads under. So at no
    /// instant does the queue list a sentence codex may already hold, and a
    /// drain that runs at any point during the RPC finds nothing of it. A
    /// refused steer awaited off this task would hold just the same:
    /// `planner_steer.rs`'s completion-race test holds the RPC open,
    /// completes the turn under it, and shows one delivery.
    ///
    /// What the same channel and the same `select!` arm as `Mutate` DO buy
    /// is ordering with the completion: `handle_steer` awaits `turn/steer`
    /// inside the arm, so no notification is processed until it returns —
    /// the `TurnCompleted` sweep over `steered_into_running_turn` therefore
    /// always sees an accepted entry registered, and the echo that names its
    /// row can never be processed before the row is written. Off this task
    /// a completion could run its sweep between codex accepting and the
    /// steer registering the entry, and a dropped sentence would go
    /// unnoticed.
    ///
    /// The cost is the mirror image: a slow `turn/steer` holds the loop for
    /// its duration, exactly as a slow `turn/start` already does in the tick
    /// arm, and the same client request timeout bounds both.
    Steer {
        entry_id: QueueEntryId,
        if_entry_rev: u32,
        actor: ActorId,
        applied: oneshot::Sender<Result<SteerResult>>,
    },
}

/// #1625 P3 — a steer that took effect: codex has the entry inside `turn_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteerApplied {
    pub entry_id: QueueEntryId,
    /// The rev the entry carried when it left the queue.
    pub rev: u32,
    /// The turn codex put the message into — the one that was running.
    pub turn_id: String,
}

/// #1625 P3 — why a steer delivered nothing. In every arm the entry is still
/// in the queue, with the id and rev the client read, and drains into the
/// next turn the ordinary way.
#[derive(Debug, Clone, PartialEq)]
pub enum SteerRefused {
    /// The queue's own answer — not there, not the rev you read, or not
    /// uniquely named — given before the phase is looked at, because it is
    /// about the message the person pointed at rather than the moment they
    /// pressed.
    Queue(MutationRefused),
    /// No turn is running at this moment: nothing was taken out of the queue
    /// and codex was not asked.
    NoRunningTurn { phase: HarnessPhaseTag },
    /// Codex was asked and said no — the turn had ended, or another one was
    /// running (`CalmError::CodexRefused`, codex's own `-32600` sentence in
    /// `message`). The entry is back at the head of the queue; no transcript
    /// row was written, because the row is written only once codex has said
    /// yes. `phase` is the harness's own phase at the moment it answered,
    /// which is still `TurnRunning` until the completion codex has already
    /// seen reaches this loop.
    NotTaken {
        message: String,
        phase: HarnessPhaseTag,
    },
    /// Codex was asked and did not answer: the request timed out, or the
    /// connection dropped before the reply, or it could not be reached at
    /// all (any error that is not `CodexRefused`). Whether codex took the
    /// message is NOT known on this side. The entry is back at the head of
    /// the queue and goes with the next turn — if codex did take it, its
    /// echo will still arrive and the sentence reaches the model twice; the
    /// alternative is losing it. The route gives this its own `code` so the
    /// person is told "unknown", not "nothing happened".
    Unanswered {
        message: String,
        phase: HarnessPhaseTag,
    },
}

/// The domain answer to a steer, in the same two layers as [`MutationResult`]:
/// the outer `Result` is transport, the inner is the loop's own answer.
pub type SteerResult = std::result::Result<SteerApplied, SteerRefused>;

enum ObservationIngress {
    Running(mpsc::Sender<HarnessObservationCommand>),
    #[cfg(feature = "fixtures")]
    Unstarted(mpsc::Sender<HarnessObservationDelivery>),
}

#[derive(Clone, Copy, Debug, Default)]
struct DebounceState {
    first_pending_at: Option<Instant>,
    last_pending_at: Option<Instant>,
    hard_fire: bool,
}

struct DurableUserMessageCheckpoint {
    pending_queue: VecDeque<QueueEntry>,
    debounce: DebounceState,
}

async fn checkpoint_durable_user_message(inner: &Inner) -> DurableUserMessageCheckpoint {
    DurableUserMessageCheckpoint {
        pending_queue: inner.pending_queue.lock().await.clone(),
        debounce: *inner.debounce.lock().await,
    }
}

async fn restore_durable_user_message(inner: &Inner, checkpoint: DurableUserMessageCheckpoint) {
    *inner.pending_queue.lock().await = checkpoint.pending_queue;
    *inner.debounce.lock().await = checkpoint.debounce;
}

/// #1505 PR1 — what a durable enqueue tells the caller about the entry it
/// created.
///
/// A process-internal channel type, not a wire type: the HTTP layer maps it
/// into `SendPlannerInputResponse.entry_id`.
///
/// `entry_id` is `None` in exactly one accepted case — the incoming message
/// folded into a [`QueueEntry::LegacyUser`] tail, which never gains an id.
/// The other `None` paths the client sees are refusals, not acks: a dormant
/// harness (no runtime at all), a 503 from a saturated observation channel,
/// and a 409 from a harness that is shutting down.
///
/// A batch carries at most one user-authored delivery today
/// (`observe_user_message_durable` sends exactly one), so the ack is exact.
/// The rule if that ever changes is written into the loops that fill this in:
/// the LAST user-authored delivery wins, `None` included. Skipping the
/// assignment when the id happens to be `None` would be worse than arbitrary —
/// a batch whose final message folded onto a legacy tail would report the
/// id of an EARLIER message, i.e. name the wrong entry rather than admit to
/// naming none.
#[derive(Debug, Clone, PartialEq)]
pub struct DurableAck {
    pub entry_id: Option<QueueEntryId>,
}

/// Result of offering one entry to the pending queue.
enum EnqueueOutcome {
    /// The queue was full of hard-fire entries and nothing could be evicted.
    Rejected,
    Accepted {
        entry_id: Option<QueueEntryId>,
    },
}

impl PlannerHarness {
    pub fn track_id(&self) -> &TrackId {
        &self.inner.track_id
    }

    pub fn run(params: PlannerHarnessParams) -> Self {
        params.snapshot.assert_known_schema();
        let (obs_tx, obs_rx) = mpsc::channel(OBSERVATION_BUFFER);
        let (shutdown_tx, shutdown_rx) = broadcast::channel(4);
        let notifications = params.daemon.subscribe_notifications();
        let (inner, announce_dropped_first) =
            inner_from_params(params, ObservationIngress::Running(obs_tx), shutdown_tx);
        let handle = Self {
            inner: Arc::clone(&inner),
        };
        let task = tokio::spawn(run_loop(
            inner,
            obs_rx,
            shutdown_rx,
            notifications,
            announce_dropped_first,
        ));
        let abort = task.abort_handle();
        *handle
            .inner
            .abort_handle
            .lock()
            .expect("planner harness abort handle mutex poisoned") = Some(abort);
        tokio::spawn(async move {
            let _ = task.await;
        });
        handle
    }

    #[cfg(feature = "fixtures")]
    pub fn run_unstarted_for_test(
        params: PlannerHarnessParams,
        observation_buffer: usize,
    ) -> (Self, mpsc::Receiver<HarnessObservationDelivery>) {
        params.snapshot.assert_known_schema();
        let (obs_tx, obs_rx) = mpsc::channel(observation_buffer);
        let (shutdown_tx, _shutdown_rx) = broadcast::channel(4);
        // No run loop on this path to flush the drop announcements early; the
        // `persist_snapshot_inner` drain still covers them, which is the half
        // that carries the guarantee.
        let (inner, _announce_dropped_first) =
            inner_from_params(params, ObservationIngress::Unstarted(obs_tx), shutdown_tx);
        (Self { inner }, obs_rx)
    }

    pub fn observe(&self, obs: Observation) -> Result<()> {
        self.observe_delivery(HarnessObservationDelivery {
            entry: QueueEntry::system(obs, None)?,
        })
    }

    pub fn observe_envelope(&self, obs: Observation, envelope_id: i64) -> Result<()> {
        self.observe_delivery(HarnessObservationDelivery {
            entry: QueueEntry::system(obs, Some(envelope_id))?,
        })
    }

    fn observe_delivery(&self, delivery: HarnessObservationDelivery) -> Result<()> {
        let closed = self
            .inner
            .observations_closed
            .lock()
            .expect("planner harness observation gate mutex poisoned");
        if *closed {
            return Err(CalmError::Conflict(
                "planner harness is shutting down; refusing new observation".into(),
            ));
        }
        let result = match &self.inner.observations {
            ObservationIngress::Running(sender) => sender
                .try_send(HarnessObservationCommand::Delivery(delivery))
                .map_err(map_observation_send_error),
            #[cfg(feature = "fixtures")]
            ObservationIngress::Unstarted(sender) => sender
                .try_send(delivery)
                .map_err(map_observation_send_error),
        };
        drop(closed);
        result
    }

    /// Fold and persist non-replayable user intent before acknowledging it.
    ///
    /// The returned [`DurableAck`] names the entry the text ended up in — which
    /// is NOT always an entry minted for this call: under backpressure the text
    /// folds into the queue tail and the ack names the survivor.
    pub async fn observe_user_message_durable(
        &self,
        text: String,
        attachments: Vec<BoundAttachment>,
    ) -> Result<DurableAck> {
        self.observe_durable_entries(vec![QueueEntry::user_message(text, None, attachments)])
            .await
    }

    async fn observe_durable_entries(&self, entries: Vec<QueueEntry>) -> Result<DurableAck> {
        let _durable_guard = self.inner.durable_observation.lock().await;
        if self.inner.shutting_down.load(Ordering::SeqCst) {
            return Err(CalmError::Conflict(
                "planner harness is shutting down; refusing new observation".into(),
            ));
        }
        let deliveries = entries
            .into_iter()
            .map(|entry| HarnessObservationDelivery { entry })
            .collect::<Vec<_>>();
        match &self.inner.observations {
            ObservationIngress::Running(sender) => {
                let (persisted, confirmation) = oneshot::channel();
                sender
                    .try_send(HarnessObservationCommand::Durable {
                        deliveries,
                        persisted,
                    })
                    .map_err(map_observation_send_error)?;
                confirmation.await.map_err(|_| {
                    CalmError::Conflict(
                        "planner harness runtime shut down before persistence".into(),
                    )
                })?
            }
            #[cfg(feature = "fixtures")]
            ObservationIngress::Unstarted(_) => {
                let checkpoint = checkpoint_durable_user_message(&self.inner).await;
                let mut ack = DurableAck { entry_id: None };
                for delivery in deliveries {
                    let user_authored = delivery.entry.is_user_authored();
                    match on_observation(&self.inner, delivery.entry).await {
                        EnqueueOutcome::Accepted { entry_id } => {
                            if user_authored {
                                ack.entry_id = entry_id;
                            }
                        }
                        EnqueueOutcome::Rejected => {
                            restore_durable_user_message(&self.inner, checkpoint).await;
                            return Err(CalmError::ServiceUnavailable(
                                "planner harness pending queue full, retry shortly".into(),
                            ));
                        }
                    }
                }
                if let Err(error) = persist_snapshot_for_durable_send(&self.inner).await {
                    restore_durable_user_message(&self.inner, checkpoint).await;
                    return Err(error);
                }
                Ok(ack)
            }
        }
    }

    /// #1505 PR2 — edit or delete one queue entry, from the REST write port.
    ///
    /// The outer `Result` is transport: the harness is shutting down, or its
    /// command channel is saturated. The inner [`MutationResult`] is the
    /// queue's own answer — applied, not found, stale, or ambiguous.
    ///
    /// `actor` is only ever [`ActorId::User`] today, because both routes
    /// refuse anything else before calling this. It is a parameter rather than
    /// a hardcoded constant so the event says who asked rather than restating
    /// what the route guard happens to permit; PR2b's kernel-authored `Dropped`
    /// is the second caller.
    pub async fn mutate_pending_entry(
        &self,
        mutation: QueueMutation,
        actor: ActorId,
    ) -> Result<MutationResult> {
        // Deliberately NOT taking `durable_observation`: see the doc comment on
        // `HarnessObservationCommand::Mutate`.
        if self.inner.shutting_down.load(Ordering::SeqCst) {
            return Err(CalmError::Conflict(
                "planner harness is shutting down; refusing queue mutation".into(),
            ));
        }
        match &self.inner.observations {
            ObservationIngress::Running(sender) => {
                let (applied, answer) = oneshot::channel();
                sender
                    .try_send(HarnessObservationCommand::Mutate {
                        mutation,
                        actor,
                        applied,
                    })
                    .map_err(map_observation_send_error)?;
                answer.await.map_err(|_| {
                    CalmError::Conflict(
                        "planner harness runtime shut down before the queue mutation was applied"
                            .into(),
                    )
                })?
            }
            #[cfg(feature = "fixtures")]
            ObservationIngress::Unstarted(_) => {
                handle_queue_mutation(&self.inner, &mutation, &actor).await
            }
        }
    }

    /// #1625 P3 — send one queued entry into the turn that is running now,
    /// from the REST write port.
    ///
    /// Two layers, as with [`Self::mutate_pending_entry`]: the outer `Result`
    /// is transport, the inner [`SteerResult`] is the loop's answer. Every
    /// inner refusal leaves the entry queued under the id and rev the client
    /// read.
    pub async fn steer_pending_entry(
        &self,
        entry_id: QueueEntryId,
        if_entry_rev: u32,
        actor: ActorId,
    ) -> Result<SteerResult> {
        if self.inner.shutting_down.load(Ordering::SeqCst) {
            return Err(CalmError::Conflict(
                "planner harness is shutting down; refusing to steer".into(),
            ));
        }
        match &self.inner.observations {
            ObservationIngress::Running(sender) => {
                let (applied, answer) = oneshot::channel();
                sender
                    .try_send(HarnessObservationCommand::Steer {
                        entry_id,
                        if_entry_rev,
                        actor,
                        applied,
                    })
                    .map_err(map_observation_send_error)?;
                answer.await.map_err(|_| {
                    CalmError::Conflict(
                        "planner harness runtime shut down before the steer was answered".into(),
                    )
                })?
            }
            #[cfg(feature = "fixtures")]
            ObservationIngress::Unstarted(_) => {
                handle_steer(&self.inner, &entry_id, if_entry_rev, &actor).await
            }
        }
    }

    pub async fn interrupt(&self, reason: String) -> Result<()> {
        issue_interrupt(&self.inner, reason).await
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.shutdown_inner(false, false).await
    }

    /// Quiesce an owner that is preparing for deletion and return its retained
    /// thread seal. An error/panic releases the seal through the local guard;
    /// the caller owns it only after strict interruption succeeds.
    pub async fn shutdown_for_deletion(&self) -> Result<Option<String>> {
        let thread_id = self.inner.thread_id.read().await.clone();
        let mut seals =
            crate::shared_codex_appserver::DeletionThreadSeals::new(self.inner.daemon.clone());
        if let Some(thread_id) = thread_id.clone() {
            seals.seal(thread_id);
        }
        self.shutdown_inner(false, true).await?;
        Ok(seals.retain().pop())
    }

    async fn shutdown_inner(&self, seal_thread: bool, strict_interrupt: bool) -> Result<()> {
        let _durable_guard = self.inner.durable_observation.lock().await;
        {
            let mut closed = self
                .inner
                .observations_closed
                .lock()
                .expect("planner harness observation gate mutex poisoned");
            *closed = true;
            self.inner.shutting_down.store(true, Ordering::SeqCst);
        }
        let thread_id = self.inner.thread_id.read().await.clone();
        if seal_thread && let Some(thread_id) = thread_id.as_deref() {
            self.inner.daemon.seal_turn_thread_for_deletion(thread_id);
        }
        let _ = self.inner.shutdown.send(());
        // If turn/start is already in flight, wait until its id is recorded in
        // the shared daemon cache. If shutdown won first, maybe_issue_turn sees
        // `shutting_down` under this same mutex and never calls the daemon.
        let _issuance_guard = self.inner.issuance.lock().await;
        self.persist_snapshot().await?;
        let mut interrupt_error = None;
        if let Some(thread_id) = thread_id {
            let last_turn_id = self.inner.last_turn_id.lock().await.clone();
            let active_turn_id = self.inner.daemon.active_turn_id_for_thread(&thread_id);
            if let Err(e) = self.inner.daemon.interrupt_active_turn(&thread_id).await {
                tracing::warn!(
                    thread_id,
                    error = %e,
                    "planner harness shutdown thread interrupt failed"
                );
                interrupt_error = Some(e);
            }
            if active_turn_id.is_none()
                && let Some(last_turn_id) = last_turn_id
                && let Err(e) = self
                    .inner
                    .daemon
                    .turn_interrupt(&thread_id, &last_turn_id)
                    .await
            {
                tracing::warn!(
                    thread_id,
                    turn_id = %last_turn_id,
                    error = %e,
                    "planner harness shutdown last-known turn interrupt failed"
                );
                interrupt_error = Some(e);
            }
        }
        let abort = self
            .inner
            .abort_handle
            .lock()
            .expect("planner harness abort handle mutex poisoned")
            .take();
        if let Some(abort) = abort {
            abort.abort();
        }
        if strict_interrupt && let Some(error) = interrupt_error {
            return Err(error);
        }
        Ok(())
    }

    pub async fn snapshot(&self) -> HarnessSnapshot {
        snapshot_for(&self.inner).await
    }

    /// Why this conversation's queue is not draining, or `None` when there is
    /// nothing worth telling the reader.
    ///
    /// `None` does NOT mean "waiting is the right answer": a long-running
    /// outage is exactly the case where waiting is right and the reader is
    /// told anyway, because silence and a hang are indistinguishable from
    /// their side. See [`Inner::issuance_block`] for the producers, and
    /// for why this is live-only rather than a snapshot field.
    pub async fn issuance_block(&self) -> Option<String> {
        self.inner.issuance_block.lock().await.clone()
    }

    /// See [`Inner::refused_issuances`].
    #[cfg(feature = "fixtures")]
    pub fn refused_issuances_for_test(&self) -> u64 {
        self.inner.refused_issuances.load(Ordering::SeqCst)
    }

    /// Forget a refusal so the next tick re-attempts immediately.
    ///
    /// Called when a person changes the selection. Without it they would fix
    /// the thing the message told them to fix and then watch their sentence sit
    /// there for the rest of the 30 s interval, which is exactly long enough to
    /// conclude that nothing happened.
    pub async fn retry_issuance_now(&self) {
        *self.inner.issuance_retry_after.lock().await = None;
        *self.inner.issuance_block.lock().await = None;
    }

    pub async fn persist_snapshot(&self) -> Result<()> {
        persist_snapshot(&self.inner).await
    }

    pub async fn state_for_test(&self) -> HarnessState {
        self.inner.state.lock().await.clone()
    }

    pub async fn thread_id_for_test(&self) -> Option<String> {
        self.inner.thread_id.read().await.clone()
    }

    pub async fn pending_len_for_test(&self) -> usize {
        self.inner.pending_queue.lock().await.len()
    }

    #[cfg(feature = "fixtures")]
    pub async fn pending_queue_for_test(&self) -> Vec<Observation> {
        self.inner
            .pending_queue
            .lock()
            .await
            .iter()
            .map(QueueEntry::observation)
            .collect()
    }

    #[cfg(feature = "fixtures")]
    pub async fn pending_entries_for_test(&self) -> Vec<QueueEntry> {
        self.inner
            .pending_queue
            .lock()
            .await
            .iter()
            .cloned()
            .collect()
    }

    #[cfg(feature = "fixtures")]
    pub async fn observe_for_test(&self, obs: Observation, envelope_id: Option<i64>) {
        let entry = match obs {
            Observation::UserMessage { text } => {
                QueueEntry::user_message(text, envelope_id, Vec::new())
            }
            other => QueueEntry::system(other, envelope_id)
                .expect("non-user observation wraps as a system entry"),
        };
        let _ = on_observation(&self.inner, entry).await;
    }

    /// Issue #682 — dev-only seam for the replay binary's
    /// `POST /dev/force-planner-phase`. Forces the harness FSM into the state
    /// matching `tag` (synthesized with `"dev-forced"` sentinel ids) and
    /// runs the regular [`persist_snapshot`] path — the single write point
    /// that updates the persisted snapshot (`session_set_handle_state_tx`),
    /// the worker-session status, and emits `HarnessPhaseChanged` when the
    /// phase actually changed. All three read surfaces (`GET /planner/run`,
    /// the WS event stream, the DB snapshot) stay consistent by
    /// construction. Forcing the same phase twice emits no duplicate event
    /// (persist only emits on `last_phase != new_phase`).
    ///
    /// Live-watchdog interactions a caller (read: PR-2 e2e specs) must know:
    /// - forcing `resumed` is not sticky — `watchdog_tick` decays `Resumed`
    ///   to `Idle` after `config.resumed_reconcile_budget` (default 5s),
    ///   emitting one more `HarnessPhaseChanged`;
    /// - `wedged` is rejected (`BadRequest`): persisting it writes
    ///   `WorkerSessionState::Failed` via `run_status_for`, and
    ///   `session_projection_active_for_card` filters failed rows, so `GET
    ///   /planner/run` would instantly report dormant and the next force would
    ///   mint a second runtime. The dev endpoint
    ///   (`replay::force_planner_phase`) 400s before ever reaching here;
    /// - any armed `interrupt_deadline` (a prior `/planner/interrupt`) and
    ///   `issued_turn_id` are cleared before persisting, so the interrupt
    ///   watchdog can't asynchronously flip a freshly forced phase to
    ///   `Wedged` mid-test.
    ///
    /// Returns `(old_phase, new_phase)` so the dev endpoint can report
    /// what it did.
    #[cfg(feature = "fixtures")]
    pub async fn force_phase_for_dev(
        &self,
        tag: HarnessPhaseTag,
    ) -> Result<(HarnessPhaseTag, HarnessPhaseTag)> {
        const DEV_FORCED_TURN_ID: &str = "dev-forced";
        let now = Instant::now();
        let state = match tag {
            HarnessPhaseTag::PendingThreadStart => HarnessState::PendingThreadStart,
            HarnessPhaseTag::Idle => HarnessState::Idle,
            HarnessPhaseTag::IssuingTurn => HarnessState::Issuing {
                since: now,
                kind: IssuingKind::TurnStart,
            },
            HarnessPhaseTag::IssuingInterrupt => HarnessState::Issuing {
                since: now,
                kind: IssuingKind::Interrupt {
                    target_turn_id: DEV_FORCED_TURN_ID.into(),
                    reason: "dev-forced".into(),
                },
            },
            HarnessPhaseTag::TurnRunning => HarnessState::TurnRunning {
                turn_id: DEV_FORCED_TURN_ID.into(),
                started_at: now,
            },
            HarnessPhaseTag::TurnCompleted => HarnessState::TurnCompleted {
                last_turn_id: DEV_FORCED_TURN_ID.into(),
            },
            HarnessPhaseTag::Resumed => HarnessState::Resumed { resumed_at: now },
            // See doc-comment: a forced Wedged would persist as
            // `WorkerSessionState::Failed`, which the active-runtime read path
            // filters out. `replay::force_planner_phase` rejects the tag with
            // the client-facing message; this arm is defense in depth for
            // any future direct caller.
            HarnessPhaseTag::Wedged => {
                return Err(CalmError::BadRequest(
                    "force_phase_for_dev does not support `wedged` (a failed runtime row \
                     is no longer projectable by GET /planner/run)"
                        .into(),
                ));
            }
        };
        let old_phase = *self.inner.last_phase.lock().await;
        *self.inner.state.lock().await = state;
        // Phases that imply a known turn need `last_turn_id` populated so
        // `persist_snapshot` can derive `active_turn_id` (TurnRunning /
        // IssuingInterrupt) and the snapshot round-trips through
        // `state_from_snapshot` recovery. Keep a real id if one exists.
        if matches!(
            tag,
            HarnessPhaseTag::TurnRunning
                | HarnessPhaseTag::IssuingInterrupt
                | HarnessPhaseTag::TurnCompleted
        ) {
            let mut last_turn_id = self.inner.last_turn_id.lock().await;
            if last_turn_id.is_none() {
                *last_turn_id = Some(DEV_FORCED_TURN_ID.into());
            }
        }
        // Issue #682 review — disarm async followers of the *previous*
        // state before persisting the forced one: a `/planner/interrupt`
        // issued earlier arms `interrupt_deadline` (30s), after which
        // `watchdog_tick` would flip the harness to `Wedged` mid-test and
        // emit an unexpected phase event. `issued_turn_id` likewise belongs
        // to the superseded state.
        *self.inner.issued_turn_id.lock().await = None;
        *self.inner.legacy_issued_input_segments.lock().await = None;
        *self.inner.interrupt_deadline.lock().await = None;
        persist_snapshot(&self.inner).await?;
        Ok((old_phase, tag))
    }

    /// Issue #682 review — permanently stop this harness from issuing
    /// turns. `replay::force_planner_phase` calls this on every harness it
    /// hands out: in replay mode the shared codex app-server is a
    /// non-running stub, so `turn_start` always fails and the run loop
    /// would otherwise churn (`issuing_turn` → fail → re-buffer with
    /// `hard_fire` → retry) on every 50ms tick once an issuable phase
    /// holds a pending observation. Observations (`/planner/input`) still
    /// enqueue normally — the harness just never issues.
    #[cfg(feature = "fixtures")]
    pub fn pause_issuance_for_dev(&self) {
        self.inner.issuance_paused.store(true, Ordering::SeqCst);
    }

    /// #1505 PR2 — the debounce arming, for the tests that pin §4.5.
    ///
    /// Read directly rather than inferred from whether a turn fired: inferring
    /// it would make the assertion depend on the 50ms tick, and a rule about
    /// what the queue is armed with is not a rule about when.
    #[cfg(feature = "fixtures")]
    pub async fn debounce_hard_fire_for_test(&self) -> bool {
        self.inner.debounce.lock().await.hard_fire
    }

    /// Whether `(first_pending_at, last_pending_at)` are set. The values are
    /// `Instant`s and mean nothing outside this process; whether they are
    /// present is the whole of what §4.5 says about them.
    #[cfg(feature = "fixtures")]
    pub async fn debounce_timestamps_set_for_test(&self) -> (bool, bool) {
        let debounce = self.inner.debounce.lock().await;
        (
            debounce.first_pending_at.is_some(),
            debounce.last_pending_at.is_some(),
        )
    }

    /// #1667 A3 — age the pending window by `by` without sleeping: both
    /// debounce timestamps move into the past, so the next 50 ms tick sees
    /// a window that has been idle for `by` longer than it really has. The
    /// run loop reads `std::time::Instant`, which `tokio::time::pause`
    /// cannot move, so this is the fixture clock.
    #[cfg(feature = "fixtures")]
    pub async fn rewind_debounce_for_test(&self, by: Duration) {
        let mut debounce = self.inner.debounce.lock().await;
        let rewind = |at: Instant| at.checked_sub(by).unwrap_or(at);
        debounce.first_pending_at = debounce.first_pending_at.map(rewind);
        debounce.last_pending_at = debounce.last_pending_at.map(rewind);
    }

    /// How long the current pending window has been open, in milliseconds.
    /// Zero when there is no window.
    #[cfg(feature = "fixtures")]
    pub async fn debounce_first_pending_elapsed_ms_for_test(&self) -> u128 {
        self.inner
            .debounce
            .lock()
            .await
            .first_pending_at
            .map(|at| at.elapsed().as_millis())
            .unwrap_or(0)
    }

    pub async fn set_state_for_test(&self, state: HarnessState) {
        *self.inner.state.lock().await = state;
    }

    pub async fn set_issued_turn_id_for_test(&self, turn_id: Option<String>) {
        *self.inner.issued_turn_id.lock().await = turn_id;
    }

    pub async fn set_last_seen_head_for_test(&self, head: Option<String>) {
        *self.inner.last_seen_head.lock().await = head;
    }

    pub async fn last_seen_head_for_test(&self) -> Option<String> {
        self.inner.last_seen_head.lock().await.clone()
    }
}

fn map_observation_send_error<T>(e: mpsc::error::TrySendError<T>) -> CalmError {
    match e {
        // Backpressure: server is temporarily saturated, client should retry.
        mpsc::error::TrySendError::Full(_) => CalmError::ServiceUnavailable(
            "planner harness observation queue full, retry shortly".into(),
        ),
        // Lifecycle race: the runtime is going away mid-request. State has
        // changed since the caller's runtime lookup; client should re-poll
        // or accept the runtime as gone.
        mpsc::error::TrySendError::Closed(_) => {
            CalmError::Conflict("planner harness runtime shutting down".into())
        }
    }
}

fn inner_from_params(
    params: PlannerHarnessParams,
    observations: ObservationIngress,
    shutdown: broadcast::Sender<()>,
) -> (Arc<Inner>, bool) {
    let mut snapshot = params.snapshot;
    let dropped_on_load = truncate_snapshot_pending_queue(&mut snapshot);
    let announce_first = !dropped_on_load.is_empty();
    let pending_queue: VecDeque<_> = snapshot.pending_entries().into_iter().collect();
    let debounce = debounce_from_initial_queue(&pending_queue);
    let state = state_from_snapshot(&snapshot);
    let last_phase = snapshot.phase;
    let (recent_hook_keys, recent_hook_key_set) =
        recent_hook_keys_from_pending_queue(&pending_queue);
    let inner = Arc::new(Inner {
        worker_session_id: params.worker_session_id,
        track_id: params.track_id,
        card_id: params.card_id,
        thread_id: RwLock::new(params.thread_id.or(snapshot.last_thread_id.clone())),
        repo: params.repo,
        events: params.events,
        card_role_cache: params.card_role_cache,
        track_area_cache: params.track_area_cache,
        daemon: params.daemon,
        observations,
        state: Mutex::new(state),
        last_phase: Mutex::new(last_phase),
        pending_queue: Mutex::new(pending_queue),
        recent_hook_keys: Mutex::new(recent_hook_keys),
        recent_hook_key_set: Mutex::new(recent_hook_key_set),
        push_watermark: Mutex::new(snapshot.push_watermark),
        last_turn_id: Mutex::new(snapshot.last_turn_id),
        issued_turn_id: Mutex::new(None),
        issued_turn_head: Mutex::new(snapshot.issued_turn_head),
        projection_client_id: Mutex::new(snapshot.projection_client_id),
        legacy_issued_input_segments: Mutex::new(snapshot.issued_input_segments),
        last_report_body_sha256: Mutex::new(snapshot.last_report_body_sha256),
        last_seen_head: Mutex::new(snapshot.last_seen_head),
        // Round-trips through the snapshot so the reading survives a reboot
        // and the lazy-recovery respawn in `ensure_live_planner_harness`. Without
        // this line the value would be written to disk and then silently
        // dropped on the way back in — codex only re-pushes it on the next
        // model response, so a resumed-but-idle thread would read as having no
        // context usage at all.
        token_usage: Mutex::new(snapshot.token_usage),
        debounce: Mutex::new(debounce),
        interrupt_deadline: Mutex::new(None),
        issuance_retry_after: Mutex::new(None),
        issuance_block: Mutex::new(None),
        refusing_since: Mutex::new(None),
        #[cfg(feature = "fixtures")]
        refused_issuances: AtomicU64::new(0),
        shutdown,
        shutting_down: Arc::new(AtomicBool::new(false)),
        observations_closed: StdMutex::new(false),
        durable_observation: Mutex::new(()),
        issuance: Mutex::new(()),
        issuance_paused: AtomicBool::new(false),
        unannounced_drops: Mutex::new(dropped_on_load),
        steered_into_running_turn: Mutex::new(Vec::new()),
        abort_handle: StdMutex::new(None),
        config: params.config,
    });
    (inner, announce_first)
}

fn harness_event_scope(inner: &Inner, event_name: &'static str) -> EventScope {
    let card = inner.card_id.clone();
    let track = inner.track_id.clone();
    match inner.track_area_cache.area_of(&track) {
        Some(area) => EventScope::Card { card, track, area },
        None => {
            tracing::warn!(
                runtime_id = %inner.worker_session_id,
                card_id = %card,
                track_id = %track,
                event_name,
                "planner harness event missing track area cache entry; using system scope"
            );
            EventScope::System
        }
    }
}

fn debounce_from_initial_queue(queue: &VecDeque<QueueEntry>) -> DebounceState {
    if queue.is_empty() {
        return DebounceState::default();
    }
    let now = Instant::now();
    DebounceState {
        first_pending_at: Some(now),
        last_pending_at: Some(now),
        hard_fire: queue.iter().any(QueueEntry::is_hard_fire),
    }
}

/// Seed hook-stop dedupe from the restored pending queue.
///
/// Snapshot recovery has already accepted these `WorkerHookStop` observations, so
/// their non-empty `idempotency_key` values must populate the recent-key LRU
/// before fallback replay or bridge retry can deliver the same hook again. Empty
/// keys are skipped because old snapshot rows deserialize them from the default.
fn recent_hook_keys_from_pending_queue(
    pending_queue: &VecDeque<QueueEntry>,
) -> (VecDeque<String>, HashSet<String>) {
    let mut keys = VecDeque::with_capacity(RECENT_HOOK_KEY_CACHE_LEN);
    let mut set = HashSet::with_capacity(RECENT_HOOK_KEY_CACHE_LEN);
    for entry in pending_queue {
        let Some(idempotency_key) = entry.hook_idempotency_key() else {
            continue;
        };
        if !set.insert(idempotency_key.to_string()) {
            continue;
        }
        keys.push_back(idempotency_key.to_string());
        while keys.len() > RECENT_HOOK_KEY_CACHE_LEN {
            if let Some(evicted) = keys.pop_front() {
                set.remove(&evicted);
            }
        }
    }
    (keys, set)
}

/// Cadence timer for the harness run loop's periodic maintenance branch.
///
/// The loop's `select!` can be parked for a long bounded stretch inside
/// `maybe_issue_turn` (transcript refresh + diff + head fallback + a
/// `turn/start` round trip, ~41s worst case). With tokio's default
/// [`MissedTickBehavior::Burst`] a 50ms interval would then hand back roughly
/// 800 immediately-ready ticks, and every one of them competes with the
/// observation, notification and shutdown branches of the same `select!`.
/// [`MissedTickBehavior::Skip`] collapses that backlog into a single tick on
/// the next aligned deadline.
fn harness_tick() -> tokio::time::Interval {
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tick
}

async fn run_loop(
    inner: Arc<Inner>,
    mut observations: mpsc::Receiver<HarnessObservationCommand>,
    mut shutdown: broadcast::Receiver<()>,
    mut notifications: broadcast::Receiver<Notification>,
    announce_dropped_first: bool,
) {
    // #1505 PR2b — before the first command is served, so a reader who has
    // seen ANY of this harness's work has seen its drop announcements too, and
    // "nothing was dropped" is a fact rather than "not yet".
    //
    // Correctness does not rest on this call. `persist_snapshot_inner` drains
    // the same list and refuses to write without it, so the loss cannot become
    // durable unannounced even if this task is never polled, is aborted
    // part-way, or fails right here. This is the early flush, not the
    // guarantee.
    if announce_dropped_first && let Err(error) = flush_dropped_announcements(&inner).await {
        tracing::error!(
            card_id = %inner.card_id,
            error = %error,
            "planner queue drop announcements failed on load; the truncated queue will not be \
             persisted until they land"
        );
    }
    let mut tick = harness_tick();
    loop {
        tokio::select! {
            command = observations.recv() => {
                let Some(command) = command else { break };
                match command {
                    HarnessObservationCommand::Delivery(delivery) => {
                        let _accepted = on_observation(&inner, delivery.entry).await;
                        if let Err(e) = persist_snapshot(&inner).await {
                            tracing::warn!(error = %e, "planner harness snapshot persist failed after observation");
                        }
                    }
                    HarnessObservationCommand::Durable { deliveries, persisted } => {
                        let checkpoint = checkpoint_durable_user_message(&inner).await;
                        let mut accepted = true;
                        let mut ack = DurableAck { entry_id: None };
                        for delivery in deliveries {
                            let user_authored = delivery.entry.is_user_authored();
                            match on_observation(&inner, delivery.entry).await {
                                EnqueueOutcome::Accepted { entry_id } => {
                                    if user_authored {
                                        ack.entry_id = entry_id;
                                    }
                                }
                                EnqueueOutcome::Rejected => {
                                    accepted = false;
                                    break;
                                }
                            }
                        }
                        let result = if accepted {
                            match persist_snapshot_for_durable_send(&inner).await {
                                Ok(()) => Ok(ack),
                                Err(error) => {
                                    restore_durable_user_message(&inner, checkpoint).await;
                                    Err(error)
                                }
                            }
                        } else {
                            restore_durable_user_message(&inner, checkpoint).await;
                            Err(CalmError::ServiceUnavailable(
                                "planner harness pending queue full, retry shortly".into(),
                            ))
                        };
                        let _ = persisted.send(result);
                    }
                    HarnessObservationCommand::Mutate { mutation, actor, applied } => {
                        let outcome = handle_queue_mutation(&inner, &mutation, &actor).await;
                        let _ = applied.send(outcome);
                    }
                    HarnessObservationCommand::Steer { entry_id, if_entry_rev, actor, applied } => {
                        let outcome = handle_steer(&inner, &entry_id, if_entry_rev, &actor).await;
                        let _ = applied.send(outcome);
                    }
                }
            }
            notif = notifications.recv() => {
                match notif {
                    Ok(notif) => {
                        if let Err(e) = on_notification(&inner, notif).await {
                            tracing::warn!(error = %e, "planner harness notification handling failed");
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "planner harness notification receiver lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = tick.tick() => {
                if let Err(e) = watchdog_tick(&inner).await {
                    tracing::warn!(error = %e, "planner harness watchdog tick failed");
                }
                if let Err(e) = maybe_issue_turn(&inner).await {
                    tracing::warn!(error = %e, "planner harness turn issuance failed");
                }
            }
            _ = shutdown.recv() => {
                break;
            }
        }
    }
}

/// #1505 PR2 — the whole of what `HarnessObservationCommand::Mutate` is allowed
/// to do: take the queue lock, apply the mutation, re-arm the debounce, persist,
/// emit.
///
/// It runs on the run-loop task (or, under the fixtures ingress, on the
/// caller's — there is no loop there to hand it to). An edit or a delete is
/// answered from memory and the database alone; codex is not asked, so
/// neither holds the loop for longer than a snapshot write and an event
/// insert. A steer is the
/// command on this channel that DOES await codex, and it does so on purpose —
/// see `HarnessObservationCommand::Steer` and `handle_steer`, which is why a
/// `QueueMutation::Steer` handed to this function is refused rather than
/// applied: applying it here would take the entry out and deliver it nowhere.
async fn handle_queue_mutation(
    inner: &Arc<Inner>,
    mutation: &QueueMutation,
    actor: &ActorId,
) -> Result<MutationResult> {
    if matches!(mutation, QueueMutation::Steer { .. }) {
        return Err(CalmError::Internal(
            "a steer is not a queue mutation: it goes through steer_pending_entry, which \
             delivers what it takes"
                .into(),
        ));
    }
    let (outcome, checkpoint) = {
        let mut queue = inner.pending_queue.lock().await;
        let before = queue.clone();
        (apply_mutation(&mut queue, mutation), before)
    };
    let applied = match outcome {
        Ok(applied) => applied,
        // A refusal changed nothing, so there is nothing to persist and nothing
        // to announce. In particular a 404 does NOT mean the entry was
        // delivered — `rebuffer_head` can put a drained batch back — so
        // inventing an event here would put a false sentence in the audit log.
        Err(refused) => return Ok(Err(refused)),
    };

    if applied.change == HarnessQueueChange::Deleted {
        rearm_debounce_after_departure(inner, &applied).await;
    }

    if let Err(error) = persist_snapshot(inner).await {
        // Same shape as the durable enqueue path: memory is rolled back to the
        // exact queue the mutation started from, so a client that gets a 500
        // and re-reads sees the entry it tried to change, unchanged.
        *inner.pending_queue.lock().await = checkpoint;
        return Err(error);
    }

    if let Err(error) = emit_queue_changed(inner, actor, &applied.entry_id, applied.change).await {
        // The snapshot above is already committed, so the change has happened
        // whatever this says. Reporting failure here would invite a retry of a
        // delete that already succeeded, and the retry would answer 404 —
        // telling the user their entry was never there. Surface it
        // operationally instead. The visible cost is real: the frontend's queue
        // region is invalidated BY this event, so a client that is not the one
        // that issued the mutation will not refresh until something else moves.
        tracing::error!(
            card_id = %inner.card_id,
            entry_id = %applied.entry_id,
            change = ?applied.change,
            error = %error,
            "planner queue mutation was applied but its audit event failed"
        );
    }
    Ok(Ok(applied))
}

/// §4.5 — one rule for every departure from the queue. `hard_fire` is
/// recomputed over what is left, so deleting (or steering away) the only user
/// message does not leave a queue of soft observations falsely armed. The
/// timestamps are NOT touched unless the queue emptied: the observations still
/// waiting keep the arming they were enqueued with, and a user taking a
/// message out must not postpone somebody else's turn.
async fn rearm_debounce_after_departure(inner: &Inner, applied: &MutationApplied) {
    let mut debounce = inner.debounce.lock().await;
    debounce.hard_fire = applied.remaining_hard_fire;
    if applied.queue_now_empty {
        debounce.first_pending_at = None;
        debounce.last_pending_at = None;
    }
}

/// #1625 P3 — the whole of what `HarnessObservationCommand::Steer` does, in
/// this order and no other:
///
///  1. answer the queue's own refusals (not there, stale, ambiguous) — the
///     entry is untouched;
///  2. refuse unless a turn is running — the entry is untouched;
///  3. take the entry OUT of the queue (`QueueMutation::Steer`), re-arm the
///     debounce over what is left;
///  4. `turn/steer` with `expectedTurnId` = the running turn and
///     `clientUserMessageId` = the entry id;
///  5. on yes: write its transcript row, keyed by the entry id, `turn_id`
///     NULL, exactly the projection the drain writes (`write_projection_row`);
///     remember the entry for the turn's completion (`SteeredEntry`);
///     persist the queue without it; announce the row and the departure
///     (`Steered`); answer 200. On anything else: put the entry back at the
///     head with its id and rev (`rebuffer_head`), announce the return
///     (`Restored`), and refuse — `NotTaken` when codex said no,
///     `Unanswered` when it said nothing.
///
/// Step 3 before step 4 is the pinned order, and `HarnessObservationCommand::Steer`
/// says what it rests on. The input is the entry's own text plus one
/// `localImage` per attachment — the same segment the drain would build for
/// it — and nothing the drain prepends: no track diff, no recovery
/// briefing, no result receipts. Those are context for a turn that is
/// starting; this turn already has its context.
///
/// The row is written AFTER codex has said yes (review round 1), unlike the
/// drain's, which precedes `turn/start`. Written before, a refusal had a row
/// to delete and a crash between the write and the RPC left an orphan keyed
/// by the entry id; written after, neither exists, and nothing is lost by
/// the reordering: this function runs on the loop task, so codex's echo —
/// `item/started`, then `item/completed`, both carrying `clientId` = the
/// entry id — cannot be processed before it returns, and the upgrade through
/// `transcript_projection_upgrade` finds the row exactly as the drain's echo
/// finds its. A reader on another device sees the sentence when the 200's
/// `harness.item.added` reaches them, one RPC later than before.
///
/// What a refusal means for the person: nothing was lost. The entry is in
/// the queue — untouched for steps 1 and 2, at the head after step 5 — with
/// the id and rev they read, and it goes with the next turn. `Restored` is
/// what tells the OTHER clients: the entry was out of the queue for the
/// length of the RPC, and one that read `/planner/run` inside that window
/// would otherwise keep showing it as sent.
async fn handle_steer(
    inner: &Arc<Inner>,
    entry_id: &QueueEntryId,
    if_entry_rev: u32,
    actor: &ActorId,
) -> Result<SteerResult> {
    // Read on this task. The phase leaves `TurnRunning` through the
    // notification arm of the same `select!`, which cannot run while this
    // function does; the one writer on another task is `issue_interrupt`
    // (a person pressing Stop). A Stop landing between here and the RPC is
    // answered by codex itself (`SteerRefused::NotTaken`); a Stop landing
    // after codex accepted is what `SteeredEntry` exists for.
    let running = inner.state.lock().await.clone();
    let phase = HarnessPhaseTag::from(&running);
    let turn_id = match running {
        HarnessState::TurnRunning { turn_id, .. } => Some(turn_id),
        _ => None,
    };
    let (applied, turn_id) = {
        let mut queue = inner.pending_queue.lock().await;
        let Some(turn_id) = turn_id else {
            return Ok(Err(match locate_entry(&queue, entry_id, if_entry_rev) {
                Err(refused) => SteerRefused::Queue(refused),
                Ok(_) => SteerRefused::NoRunningTurn { phase },
            }));
        };
        let mutation = QueueMutation::Steer {
            entry_id: entry_id.clone(),
            if_entry_rev,
        };
        match apply_mutation(&mut queue, &mutation) {
            Ok(applied) => (applied, turn_id),
            Err(refused) => return Ok(Err(SteerRefused::Queue(refused))),
        }
    };
    let entry = applied
        .removed
        .clone()
        .expect("QueueMutation::Steer hands the removed entry back");
    rearm_debounce_after_departure(inner, &applied).await;

    let Some(thread_id) = inner.thread_id.read().await.clone() else {
        // `TurnRunning` is only ever entered from a `turn/started` on this
        // thread, so this is unreachable in practice; refusing rather than
        // panicking keeps the entry.
        rebuffer_head(inner, vec![entry]).await;
        return Ok(Err(SteerRefused::NoRunningTurn { phase }));
    };
    let segments = input_segments_for_entries(&inner.card_id, std::slice::from_ref(&entry));
    let text = segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let mut items = vec![InputItem::text(text)];
    items.extend(
        entry
            .attachments()
            .iter()
            .map(|attachment| InputItem::local_image(attachment.path.clone())),
    );

    tracing::debug!(
        target: "calm_server::planner_harness_issue",
        worker_session_id = %inner.worker_session_id,
        card_id = %inner.card_id,
        thread_id = %thread_id,
        turn_id = %turn_id,
        entry_id = %entry_id,
        "calling daemon.turn_steer"
    );
    let steered = inner
        .daemon
        .turn_steer(&thread_id, &turn_id, items, Some(entry_id.as_str()))
        .await;
    let error = match steered {
        Ok(taken_by) => {
            // Codex has the sentence. The row that shows it on every screen
            // is written now, and a failed write is logged rather than
            // reported: the delivery has happened whatever this says, the
            // echo will insert a row of its own if there is none to upgrade,
            // and a 500 here would invite a retry of a delivery that already
            // happened. `SteeredEntry::row_id` records the miss for the
            // completion sweep.
            let row_id = match insert_projection_row(
                inner,
                &thread_id,
                entry_id.as_str(),
                &segments,
            )
            .await
            {
                Ok(row_id) => Some(row_id),
                Err(error) => {
                    tracing::error!(
                        worker_session_id = %inner.worker_session_id,
                        card_id = %inner.card_id,
                        entry_id = %entry_id,
                        error = %error,
                        "planner harness steered an entry but could not write its transcript row"
                    );
                    None
                }
            };
            inner
                .steered_into_running_turn
                .lock()
                .await
                .push(SteeredEntry {
                    turn_id: taken_by.clone(),
                    entry,
                    row_id,
                });
            // The queue without the entry is the truth to persist. A failed
            // write here is logged and the steer still answers 200, for the
            // reason above; the next successful snapshot (the turn's
            // completion at the latest) writes the same queue. The window it
            // leaves — a restart before that write re-drains the entry — is
            // the same window the drain's own `persist_issuance_outcome`
            // has, and is declared in the PR.
            if let Err(error) = persist_snapshot(inner).await {
                tracing::error!(
                    worker_session_id = %inner.worker_session_id,
                    card_id = %inner.card_id,
                    entry_id = %entry_id,
                    error = %error,
                    "planner harness steered an entry but could not persist the queue without it"
                );
            }
            if let Some(row_id) = row_id
                && let Err(error) = emit_item_added(
                    inner,
                    row_id,
                    Some(entry_id.as_str().to_string()),
                    Some("userMessage".to_string()),
                    None,
                    "item/completed".to_string(),
                )
                .await
            {
                tracing::error!(
                    worker_session_id = %inner.worker_session_id,
                    card_id = %inner.card_id,
                    entry_id = %entry_id,
                    error = %error,
                    "planner harness steered an entry but could not announce its transcript row"
                );
            }
            if let Err(error) =
                emit_queue_changed(inner, actor, entry_id, HarnessQueueChange::Steered).await
            {
                // Same reasoning as the mutation path: the delivery has
                // happened whatever this says, so it is surfaced operationally
                // rather than reported as a failure that invites a retry.
                tracing::error!(
                    worker_session_id = %inner.worker_session_id,
                    card_id = %inner.card_id,
                    entry_id = %entry_id,
                    error = %error,
                    "planner harness steered an entry but its audit event failed"
                );
            }
            return Ok(Ok(SteerApplied {
                entry_id: entry_id.clone(),
                rev: applied.rev,
                turn_id: taken_by,
            }));
        }
        Err(error) => error,
    };

    // Refused, unreachable, or timed out: the sentence goes back where it
    // was. No row was written, so there is none to delete. The snapshot on
    // disk never stopped listing the entry (it is persisted only once codex
    // has said yes), so the persist here changes nothing durable and its
    // failure is a warning, not a 500 over a queue that is already right in
    // memory and on disk.
    rebuffer_head(inner, vec![entry]).await;
    if let Err(persist_error) = persist_snapshot(inner).await {
        tracing::warn!(
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            entry_id = %entry_id,
            error = %persist_error,
            "planner harness could not persist the queue after a refused steer"
        );
    }
    if let Err(event_error) =
        emit_queue_changed(inner, actor, entry_id, HarnessQueueChange::Restored).await
    {
        // The entry IS back; a client that missed this refetches on the
        // next event that touches the queue. Reporting failure would turn a
        // typed "still queued" into an untyped 500 over the same queue.
        tracing::error!(
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            entry_id = %entry_id,
            error = %event_error,
            "planner harness re-buffered a refused steer but its audit event failed"
        );
    }
    tracing::warn!(
        worker_session_id = %inner.worker_session_id,
        card_id = %inner.card_id,
        entry_id = %entry_id,
        turn_id = %turn_id,
        error = %error,
        "planner harness could not steer the entry into the running turn; re-buffered it"
    );
    let phase = HarnessPhaseTag::from(&*inner.state.lock().await);
    // `CodexRefused` is the one error shape in which codex is known to have
    // seen the request and declined it (`codex_appserver::request_until`);
    // every other error — timeout, connection closed before the reply, no
    // client — leaves the outcome unknown, and is answered as such. A send
    // that never left this process is folded into "unknown" too: an honest
    // superset, at the price of one over-cautious sentence.
    Ok(Err(match error {
        CalmError::CodexRefused(message) => SteerRefused::NotTaken { message, phase },
        other => SteerRefused::Unanswered {
            message: other.to_string(),
            phase,
        },
    }))
}

/// #1625 P3 review round 1 — the `TurnCompleted` arm's sweep over
/// `steered_into_running_turn`: every entry codex accepted into `turn_id`
/// whose transcript row is still a projection (see [`SteeredEntry`]) was
/// dropped by codex before it was recorded — the interrupt cleared its
/// pending input — so the row is deleted and the entry goes back to the head
/// of the queue, in steer order, with its id and one rev up
/// (`QueueEntry::bump_rev_for_restore`, review round 2), and its return is
/// announced (`Restored`, actor `Kernel`). Entries whose row was upgraded by
/// the echo are delivered and are simply forgotten.
///
/// Called AFTER the arm's non-target and stale-completion gates (a completion
/// the FSM ignores sweeps nothing) and BEFORE
/// `persist_snapshot_stamping_issued_head`, so the `HarnessPhaseChanged`
/// that persist emits — whose plan refetches both the queue and the
/// transcript — already finds the rows gone and the queue refilled. The
/// re-buffer arms `hard_fire`, so the restored entry drains into the next
/// turn on the tick that follows, as any queued message does once a turn
/// has ended.
///
/// Entries recorded under a different turn than the one completing cannot
/// occur (a steer names the running turn, and the arm accepts completions
/// for the running turn only); they are swept all the same rather than left
/// to wait for a completion that will never come, and the mismatch is logged.
async fn restore_steered_entries_codex_dropped(inner: &Arc<Inner>, turn_id: &str) {
    let steered = std::mem::take(&mut *inner.steered_into_running_turn.lock().await);
    if steered.is_empty() {
        return;
    }
    let mut restored = Vec::new();
    for steered in steered {
        if steered.turn_id != turn_id {
            tracing::warn!(
                worker_session_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                steered_turn_id = %steered.turn_id,
                completed_turn_id = %turn_id,
                "planner harness sweeping a steered entry recorded under another turn"
            );
        }
        let Some(entry_id) = steered.entry.id().cloned() else {
            continue;
        };
        if steered.row_id.is_none() {
            tracing::warn!(
                worker_session_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                entry_id = %entry_id,
                "planner harness cannot tell whether codex recorded a steered entry: its \
                 transcript row was never written; leaving it delivered"
            );
            continue;
        }
        let still_projection = match inner
            .repo
            .transcript_projection_id(inner.card_id.as_str(), entry_id.as_str())
            .await
        {
            Ok(row) => row.is_some(),
            Err(error) => {
                tracing::warn!(
                    worker_session_id = %inner.worker_session_id,
                    card_id = %inner.card_id,
                    entry_id = %entry_id,
                    error = %error,
                    "planner harness could not read a steered entry's transcript row; leaving it \
                     delivered"
                );
                false
            }
        };
        if !still_projection {
            continue;
        }
        if let Err(error) = inner
            .repo
            .transcript_projection_delete(inner.card_id.as_str(), entry_id.as_str())
            .await
        {
            tracing::warn!(
                worker_session_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                entry_id = %entry_id,
                error = %error,
                "planner harness could not delete the transcript row of a steered entry codex \
                 dropped"
            );
        }
        tracing::warn!(
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            entry_id = %entry_id,
            turn_id,
            "planner harness restoring a steered entry the turn ended without recording"
        );
        restored.push(steered.entry);
    }
    if restored.is_empty() {
        return;
    }
    let ids = restored
        .iter()
        .filter_map(|entry| entry.id().cloned())
        .collect::<Vec<_>>();
    // #1625 P3 review round 2 — one rev up before it goes back. The client
    // whose steer answered 200 is hiding the entry; the page it reads after
    // this restore lists the same id again, and without the bump that page
    // is indistinguishable from the one it read before the steer — a client
    // whose refetch lands only after the restore would hide the entry for
    // good. The refused-steer restore in `handle_steer` does NOT bump, and
    // `bump_rev_for_restore` says why.
    for entry in &mut restored {
        entry.bump_rev_for_restore();
    }
    rebuffer_head(inner, restored).await;
    for entry_id in ids {
        if let Err(error) = emit_queue_changed(
            inner,
            &ActorId::Kernel,
            &entry_id,
            HarnessQueueChange::Restored,
        )
        .await
        {
            tracing::error!(
                worker_session_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                entry_id = %entry_id,
                error = %error,
                "planner harness restored a steered entry but its audit event failed"
            );
        }
    }
}

/// One `harness.queue.changed` row for `entry_id`, with `actor` as the
/// event's own actor field (the websocket frame carries no envelope actor).
/// The caller decides what a failure means: the queue change it announces
/// has already happened by the time this is called, on every path.
async fn emit_queue_changed(
    inner: &Arc<Inner>,
    actor: &ActorId,
    entry_id: &QueueEntryId,
    change: HarnessQueueChange,
) -> Result<()> {
    let scope = harness_event_scope(inner, "harness.queue.changed");
    inner
        .repo
        .log_pure_event(
            actor.clone(),
            scope,
            None,
            &inner.events,
            &inner.card_role_cache,
            &inner.track_area_cache,
            Event::HarnessQueueChanged {
                worker_session_id: inner.worker_session_id.clone(),
                card_id: inner.card_id.clone(),
                track_id: inner.track_id.clone(),
                entry_id: entry_id.as_str().to_string(),
                change,
                actor: actor.clone(),
            },
        )
        .await?;
    Ok(())
}

async fn on_observation(inner: &Arc<Inner>, entry: QueueEntry) -> EnqueueOutcome {
    if let Some(envelope_id) = entry.envelope_id() {
        let mut watermark = inner.push_watermark.lock().await;
        *watermark = (*watermark).max(envelope_id);
    }
    if suppress_duplicate_hook_stop(inner, &entry).await {
        return EnqueueOutcome::Rejected;
    }
    let hard_fire = entry.is_hard_fire();
    let report_sha256 = entry.report_sha256().map(str::to_string);
    let outcome = enqueue_pending_observation(inner, entry).await;
    if matches!(outcome, EnqueueOutcome::Rejected) {
        return outcome;
    }
    if let Some(hash) = report_sha256 {
        *inner.last_report_body_sha256.lock().await = Some(hash);
    }
    let now = Instant::now();
    let mut debounce = inner.debounce.lock().await;
    if debounce.first_pending_at.is_none() {
        debounce.first_pending_at = Some(now);
    }
    debounce.last_pending_at = Some(now);
    debounce.hard_fire |= hard_fire;
    outcome
}

/// KNOWN GAP (#1449): the harvest appends to the successor's queue without
/// consulting `MAX_PENDING_QUEUE_LEN` — the constant is private to this module
/// — and the cap is applied here afterwards, from the OLD end. The harvested
/// human sentences are the oldest entries, so they are the ones dropped, and
/// the source row is stamped and emptied by then: the only trace is the warn
/// below. It needs a predecessor holding more than `MAX_PENDING_QUEUE_LEN`
/// undelivered entries, and it became reachable when the transfer became a
/// move.
///
/// #1505 PR2b — the warn is no longer the only trace. Returns the
/// [`QueueEntryId`]s of the addressable user entries this discarded, so the
/// caller can announce each one as `harness.queue.changed { change: dropped }`.
/// The announcement is an audit row and the input a future frontend slice
/// needs; it is NOT, today, something that stops a stale client placeholder,
/// because nothing in either frontend tree reads the `dropped` variant — the
/// event maps to query invalidation only (`fe/core/events/invalidation-plan.ts`).
/// A browser holding the echo for a discarded entry still shows it after the
/// entry leaves `pending`. Closing that needs a per-entry retirement channel
/// into the router — a new effect in `fe/core/events`'s reducer and an arm in
/// its adapter — and no slice is scheduled for it, so this is a live gap and
/// not a handoff.
///
/// Only `User` entries are named. A `LegacyUser` has no id (so nothing can be
/// said about it that a client could act on) and a `System` entry was never a
/// person's message; both are still counted in the warn's arithmetic.
fn truncate_snapshot_pending_queue(snapshot: &mut HarnessSnapshot) -> Vec<QueueEntryId> {
    let len = snapshot.pending_len();
    if len <= MAX_PENDING_QUEUE_LEN {
        return Vec::new();
    }
    let drop_count = len - MAX_PENDING_QUEUE_LEN;
    let mut entries = snapshot.pending_entries();
    let dropped = entries
        .drain(..drop_count)
        .filter_map(|entry| entry.id().cloned())
        .collect::<Vec<_>>();
    snapshot.set_pending_entries(entries);
    tracing::warn!(
        target: "planner.harness.backpressure",
        original_len = len,
        retained_len = snapshot.pending_len(),
        addressable_dropped = dropped.len(),
        "snapshot pending_queue truncated to newest observations"
    );
    dropped
}

/// #1505 PR2b — drain the `harness.queue.changed { change: dropped }` rows the
/// load-time truncation still owes, and report a failure to the caller.
///
/// This is the announcement's whole guarantee, and it is a fail-closed one:
/// `persist_snapshot_inner` calls it before every write and refuses the write
/// if it returns `Err`, so a truncated queue reaches the row only after the
/// record of what it discarded is already committed.
///
/// **The lock is held across the inserts, and that is what stops two flushers
/// IN THIS PROCESS announcing the same id.** Two callers really do race: the
/// run loop's early flush runs on the task spawned by `PlannerHarness::run`,
/// while `planner_harness_start_adapter` calls `handle.persist_snapshot()` on
/// its own task straight afterwards — and `persist_snapshot` yields at its
/// first database await, which is when the spawned loop first gets polled, so
/// this interleaves on a current-thread runtime too. Reading the head under
/// the lock and then dropping it before the insert let both callers take the
/// same id and announce it twice. The second caller now waits and finds the
/// list empty.
///
/// **It is not a claim across boots, and one row per entry is not guaranteed
/// there.** The list lives in memory. Give `[A, B, C]`: A's row commits and A
/// leaves the list, B's insert fails, the write is refused, and the process
/// restarts — boot 2 reads the still-untruncated `handle_state_json`, derives
/// the same three ids, and nothing compares them against the rows already in
/// `events`, so A is announced a second time. Aborting the run-loop task
/// between the commit inside `log_pure_event` and the `retain` below has the
/// same shape. Making it true across boots needs the announcement and the
/// truncation to share a transaction, or the reader to dedupe on `entry_id`;
/// neither is here.
///
/// Ids are removed one at a time, as each row commits, so a failure part-way
/// through leaves exactly the un-announced remainder behind. That also makes
/// the drain safe to abandon: `shutdown_inner` can abort the run-loop task
/// mid-flush without turning the leftovers into a silent loss, because nothing
/// can persist the truncation without draining them first.
///
/// A failure does not overwrite the untruncated row, so nothing is lost *by
/// this write*. Whether those entries are ever read again depends on what
/// becomes of the runtime: a boot that reaches this row re-truncates and
/// retries, but a mint that fails here is compensated to `failed`, and neither
/// the harvest (which reads `superseded` rows) nor `restore_old_runtime` reads
/// a failed one — so entries that originated on this row, as opposed to ones
/// recorded in the `harvested_from` journal, can still be stranded there.
async fn flush_dropped_announcements(inner: &Arc<Inner>) -> Result<()> {
    // One flusher at a time, for the whole drain. A `tokio::Mutex` because the
    // guard is held across the `log_pure_event` await below — which is the
    // point, not an oversight.
    let mut outstanding = inner.unannounced_drops.lock().await;
    loop {
        let Some(entry_id) = outstanding.first().cloned() else {
            return Ok(());
        };
        let scope = harness_event_scope(inner, "harness.queue.changed");
        inner
            .repo
            .log_pure_event(
                ActorId::Kernel,
                scope,
                None,
                &inner.events,
                &inner.card_role_cache,
                &inner.track_area_cache,
                Event::HarnessQueueChanged {
                    worker_session_id: inner.worker_session_id.clone(),
                    card_id: inner.card_id.clone(),
                    track_id: inner.track_id.clone(),
                    entry_id: entry_id.as_str().to_string(),
                    change: HarnessQueueChange::Dropped,
                    actor: ActorId::Kernel,
                },
            )
            .await?;
        outstanding.retain(|pending| pending != &entry_id);
    }
}

/// #1667 D1 — a `ReportEdited` system entry: the one shape that folds into
/// an adjacent same-track predecessor on EVERY enqueue, not only under
/// backpressure.
fn is_report_edit(entry: &QueueEntry) -> bool {
    matches!(
        entry,
        QueueEntry::System {
            observation: Observation::ReportEdited { .. },
            ..
        }
    )
}

async fn enqueue_pending_observation(inner: &Arc<Inner>, entry: QueueEntry) -> EnqueueOutcome {
    let mut queue = inner.pending_queue.lock().await;
    // #1667 D1 — one edit session is many saves. Adjacent report edits of the
    // same track fold unconditionally (first `body_before`, newest `body`),
    // so the planner reads ONE diff from the version it last knew to the
    // newest body instead of one bounded diff per save. Only this shape
    // folds early: user text keeps its own slot (ids, attachments,
    // separate `User says:` blocks) until the cap below forces a fold.
    if is_report_edit(&entry)
        && queue.back().is_some_and(is_report_edit)
        && let FoldOutcome::Folded { entry_id } =
            try_fold_tail(&mut queue, &entry, MAX_FOLDED_USER_MESSAGE_CHARS)
    {
        return EnqueueOutcome::Accepted { entry_id };
    }
    if queue.len() >= MAX_PENDING_QUEUE_LEN {
        match try_fold_tail(&mut queue, &entry, MAX_FOLDED_USER_MESSAGE_CHARS) {
            FoldOutcome::Folded { entry_id } => {
                return EnqueueOutcome::Accepted { entry_id };
            }
            FoldOutcome::NotFolded => {}
        }
        let hard = entry.is_hard_fire();
        // Eviction can only ever take a non-hard-fire entry, and every one of
        // those is a `System` entry (`QueueEntry::User` / `LegacyUser` report
        // hard-fire unconditionally). So neither fold nor eviction can destroy
        // an id a client has already been shown.
        if let Some(drop_idx) = queue.iter().position(|queued| !queued.is_hard_fire()) {
            queue.remove(drop_idx);
        } else {
            tracing::warn!(
                target: "planner.harness.backpressure",
                queue_len = queue.len(),
                hard,
                variant = ?entry,
                "pending_queue full, incoming observation dropped"
            );
            return EnqueueOutcome::Rejected;
        }
    }
    let entry_id = entry.id().cloned();
    queue.push_back(entry);
    EnqueueOutcome::Accepted { entry_id }
}

async fn suppress_duplicate_hook_stop(inner: &Arc<Inner>, entry: &QueueEntry) -> bool {
    let Some(idempotency_key) = entry.hook_idempotency_key() else {
        return false;
    };
    let mut set = inner.recent_hook_key_set.lock().await;
    if set.contains(idempotency_key) {
        tracing::warn!(
            target: "planner.harness.dedupe",
            key = %idempotency_key,
            "duplicate WorkerHookStop suppressed"
        );
        return true;
    }
    set.insert(idempotency_key.to_string());
    let mut keys = inner.recent_hook_keys.lock().await;
    keys.push_back(idempotency_key.to_string());
    while keys.len() > RECENT_HOOK_KEY_CACHE_LEN {
        if let Some(evicted) = keys.pop_front() {
            set.remove(&evicted);
        }
    }
    false
}

async fn on_notification(inner: &Arc<Inner>, notif: Notification) -> Result<()> {
    let current_thread = inner.thread_id.read().await.clone();
    if notif.thread_id() != current_thread.as_deref() {
        return Ok(());
    }

    if let Notification::Other { method, .. } = &notif
        && method.starts_with("approval/")
    {
        tracing::warn!(
            method,
            "planner harness ignoring approval-shaped notification under approval_policy=never"
        );
        return Ok(());
    }

    match notif {
        Notification::ThreadStarted { params } => {
            if let Some(thread_id) = crate::shared_codex_appserver::thread_id_from_started(&params)
            {
                *inner.thread_id.write().await = Some(thread_id.to_string());
            }
            let mut state = inner.state.lock().await;
            if matches!(
                *state,
                HarnessState::PendingThreadStart | HarnessState::Resumed { .. }
            ) {
                *state = HarnessState::Idle;
            }
        }
        Notification::ThreadStatusChanged { status, .. } => {
            if status.get("type").and_then(Value::as_str) == Some("systemError") {
                *inner.state.lock().await = HarnessState::Wedged {
                    since: Instant::now(),
                    reason: "system_error".into(),
                };
                *inner.issued_turn_id.lock().await = None;
            } else if status.get("type").and_then(Value::as_str) == Some("idle") {
                let mut state = inner.state.lock().await;
                if matches!(*state, HarnessState::Resumed { .. }) {
                    *state = HarnessState::Idle;
                }
            }
        }
        Notification::TurnStarted { turn, .. } => {
            let Some(turn_id) = turn.get("id").and_then(Value::as_str).map(str::to_string) else {
                tracing::debug!(?turn, "planner harness ignoring TurnStarted without id");
                return persist_snapshot(inner).await;
            };
            let state_snap = inner.state.lock().await.clone();
            let last_seen = inner.last_turn_id.lock().await.clone();
            let issued = inner.issued_turn_id.lock().await.clone();
            let accept = match &state_snap {
                HarnessState::Issuing {
                    kind: IssuingKind::TurnStart,
                    ..
                } => issued.as_deref() == Some(turn_id.as_str()),
                HarnessState::TurnRunning {
                    turn_id: active, ..
                } => active == &turn_id,
                HarnessState::Idle => last_seen.is_none(),
                HarnessState::Resumed { .. } => last_seen.as_deref() == Some(turn_id.as_str()),
                _ => false,
            };
            if !accept {
                tracing::debug!(
                    observed = %turn_id,
                    last_seen = ?last_seen,
                    issued = ?issued,
                    state = ?state_snap,
                    "planner harness ignoring TurnStarted that does not match expected turn"
                );
                return persist_snapshot(inner).await;
            }
            let already_running_same = matches!(
                &state_snap,
                HarnessState::TurnRunning { turn_id: active, .. } if active == &turn_id
            );
            *inner.last_turn_id.lock().await = Some(turn_id.clone());
            if !already_running_same {
                *inner.state.lock().await = HarnessState::TurnRunning {
                    turn_id,
                    started_at: Instant::now(),
                }
            }
            *inner.issued_turn_id.lock().await = None;
            *inner.interrupt_deadline.lock().await = None;
        }
        Notification::TurnCompleted { turn, .. } => {
            let fallback_turn_id = inner.last_turn_id.lock().await.clone();
            let turn_id = turn
                .get("id")
                .and_then(Value::as_str)
                .or(fallback_turn_id.as_deref())
                .unwrap_or("unknown-turn")
                .to_string();
            let interrupt_target = {
                let state = inner.state.lock().await;
                match &*state {
                    HarnessState::Issuing {
                        kind: IssuingKind::Interrupt { target_turn_id, .. },
                        ..
                    } => Some(target_turn_id.clone()),
                    _ => None,
                }
            };
            if let Some(target_turn_id) = interrupt_target {
                if turn_id != target_turn_id {
                    tracing::debug!(
                        observed_turn_id = %turn_id,
                        target_turn_id = %target_turn_id,
                        status = ?turn.get("status"),
                        "planner harness ignoring non-target completion while interrupt is pending"
                    );
                    return persist_snapshot(inner).await;
                }
                *inner.last_turn_id.lock().await = Some(target_turn_id.clone());
                *inner.state.lock().await = HarnessState::TurnCompleted {
                    last_turn_id: target_turn_id,
                };
                *inner.interrupt_deadline.lock().await = None;
                persist_turn_outcome(inner, &turn_id, &turn).await;
                // The Stop path is the one that drops steered input (see
                // `SteeredEntry`); the sweep runs before the phase persist so
                // the phase event carries the restored queue.
                restore_steered_entries_codex_dropped(inner, &turn_id).await;
                return persist_snapshot_stamping_issued_head(inner).await;
            }
            let state = inner.state.lock().await.clone();
            let active = state.active_turn_id();
            if !matches!(state, HarnessState::TurnRunning { .. })
                || active.as_deref() != Some(turn_id.as_str())
            {
                tracing::debug!(
                    observed = %turn_id,
                    active = ?active,
                    state = ?state,
                    "planner harness ignoring stale TurnCompleted"
                );
                return persist_snapshot(inner).await;
            }
            *inner.last_turn_id.lock().await = Some(turn_id.clone());
            *inner.state.lock().await = HarnessState::TurnCompleted {
                last_turn_id: turn_id.clone(),
            };
            *inner.interrupt_deadline.lock().await = None;
            persist_turn_outcome(inner, &turn_id, &turn).await;
            // A turn can end without a model request after the steer on
            // this branch too (a model error, `max_turn_duration` when the
            // interrupt was issued elsewhere); same sweep, same order.
            restore_steered_entries_codex_dropped(inner, &turn_id).await;
            return persist_snapshot_stamping_issued_head(inner).await;
        }
        // #1625 P1: the turn-outcome row is written only from `TurnCompleted`
        // above — codex 0.153.4 has no `turn/aborted` notification; an
        // interrupt arrives as `turn/completed` with `status: "interrupted"`.
        Notification::Other { method, params } if method == "turn/aborted" => {
            let Some(aborted_turn_id) = other_turn_id(&params).map(ToOwned::to_owned) else {
                tracing::debug!("planner harness ignoring turn/aborted without a turn id");
                return persist_snapshot(inner).await;
            };
            let interrupt_target = {
                let state = inner.state.lock().await;
                match &*state {
                    HarnessState::Issuing {
                        kind: IssuingKind::Interrupt { target_turn_id, .. },
                        ..
                    } => Some(target_turn_id.clone()),
                    _ => None,
                }
            };
            let Some(target_turn_id) = interrupt_target else {
                tracing::debug!(
                    turn_id = %aborted_turn_id,
                    "planner harness ignoring turn/aborted outside interrupt issuance"
                );
                return persist_snapshot(inner).await;
            };
            if aborted_turn_id != target_turn_id {
                tracing::debug!(
                    observed_turn_id = %aborted_turn_id,
                    target_turn_id = %target_turn_id,
                    "planner harness ignoring non-target aborted turn while interrupt is pending"
                );
                return persist_snapshot(inner).await;
            }
            *inner.last_turn_id.lock().await = Some(target_turn_id.clone());
            *inner.state.lock().await = HarnessState::TurnCompleted {
                last_turn_id: target_turn_id,
            };
            *inner.interrupt_deadline.lock().await = None;
            restore_steered_entries_codex_dropped(inner, &aborted_turn_id).await;
            return persist_snapshot_stamping_issued_head(inner).await;
        }
        Notification::Item { method, params } if should_persist_item_method(&method) => {
            let Some(item) = params.get("item") else {
                tracing::debug!(
                    method,
                    "planner harness ignoring item notification without item"
                );
                return persist_snapshot(inner).await;
            };
            let Some(thread_id) = inner.thread_id.read().await.clone() else {
                tracing::warn!(
                    runtime_id = %inner.worker_session_id,
                    card_id = %inner.card_id,
                    method,
                    "planner harness item notification arrived before thread id was known"
                );
                return persist_snapshot(inner).await;
            };

            let item_uuid = item
                .get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let item_type = item
                .get("type")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let turn_id = item_turn_id(&params).map(ToOwned::to_owned);
            let params_json = serde_json::to_string(&params)?;
            // Review round 2 — the one turn a PRE-P2 binary can have left in
            // flight across the upgrade (`HarnessSnapshot::issued_input_segments`).
            // Its drain wrote no projection row, so its echo takes the
            // segments from the legacy slot, exactly as the pre-P2 echo arm
            // did: matched by turn, attached to the row this arm inserts,
            // and consumed by the completed echo. A turn issued by THIS
            // binary never matches — its segments are on its projection row
            // and the slot holds nothing of its own.
            let legacy_segments_json = if is_user_message_type(item_type.as_deref()) {
                let legacy = inner.legacy_issued_input_segments.lock().await;
                legacy
                    .as_ref()
                    .filter(|legacy| turn_id.as_deref() == Some(legacy.turn_id.as_str()))
                    .map(|legacy| serde_json::to_string(&legacy.segments))
                    .transpose()?
            } else {
                None
            };
            // #1625 P2 — a `userMessage` echo carrying `item.clientId` is
            // codex handing back the id the drain sent as
            // `clientUserMessageId`, and that id names the projection row the
            // drain wrote before `turn/start` went out (`write_projection_row`).
            // That row is the person's sentence, already on every screen; the
            // echo upgrades it in place (turn, codex item id, verbatim params)
            // and must never become a second copy of it. The `input_segments`
            // column stays as the drain wrote it — it is the kernel's record
            // of what was sent, which the echo cannot improve on.
            //
            // Codex announces the same item twice, `item/started` then
            // `item/completed`, and only a completed `userMessage` renders
            // (`fe/core/domain/conversation.ts` reads user messages from
            // completed rows alone). The
            // projection is already a completed row, so a started echo that
            // names it is not stored at all: stored, it would stand beside
            // the projection as a row the frontend pairs with nothing — the
            // pairing key is `item_uuid`, and the projection's is the client
            // id until the completed echo arrives — and a started row for a
            // user message has never carried anything the completed one does
            // not. An echo whose client id names no projection (the row was
            // cleared under it, or codex invented one) is stored exactly as
            // before this slice.
            //
            // Exactly ONE row renders per drained turn, before the echo and
            // after it: that is the invariant this arm keeps.
            let projection_client_id = if is_user_message_type(item_type.as_deref()) {
                item.get("clientId")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            } else {
                None
            };
            let item_db_id = match (projection_client_id.as_deref(), method.as_str()) {
                (Some(client_id), "item/completed") => {
                    let upgraded = match item_uuid.as_deref() {
                        Some(codex_item_id) => {
                            inner
                                .repo
                                .transcript_projection_upgrade(
                                    inner.card_id.as_str(),
                                    client_id,
                                    turn_id.as_deref(),
                                    codex_item_id,
                                    &params_json,
                                )
                                .await?
                        }
                        None => None,
                    };
                    match upgraded {
                        Some(row_id) => row_id,
                        None => {
                            insert_item_row(
                                inner,
                                &thread_id,
                                turn_id.as_deref(),
                                item_uuid.as_deref(),
                                item_type.as_deref(),
                                &method,
                                &params_json,
                                legacy_segments_json.as_deref(),
                            )
                            .await?
                        }
                    }
                }
                (Some(client_id), "item/started")
                    if inner
                        .repo
                        .transcript_projection_id(inner.card_id.as_str(), client_id)
                        .await?
                        .is_some() =>
                {
                    tracing::debug!(
                        worker_session_id = %inner.worker_session_id,
                        card_id = %inner.card_id,
                        client_id,
                        "planner harness skipping item/started echo of a projected user message"
                    );
                    return persist_snapshot(inner).await;
                }
                _ => {
                    insert_item_row(
                        inner,
                        &thread_id,
                        turn_id.as_deref(),
                        item_uuid.as_deref(),
                        item_type.as_deref(),
                        &method,
                        &params_json,
                        legacy_segments_json.as_deref(),
                    )
                    .await?
                }
            };
            if method == "item/completed" && legacy_segments_json.is_some() {
                *inner.legacy_issued_input_segments.lock().await = None;
            }
            emit_item_added(inner, item_db_id, item_uuid, item_type, turn_id, method).await?;
        }
        // `turn/plan/updated` — codex's own TODO checklist for the running
        // turn (`{ threadId, turnId, explanation, plan: [{ step, status }] }`,
        // status spelled `pending` | `inProgress` | `completed` on the wire).
        // Each notification carries the *whole* checklist and supersedes the
        // previous one for that turn; we kept dropping it into the catch-all
        // below, so the shape has never been observable from real data. This
        // arm only persists it (#1255) — no UI reads it yet, deliberately:
        // how often codex revises a plan inside one turn decides the UI shape,
        // and only stored rows can answer that.
        Notification::Other { method, params } if method == "turn/plan/updated" => {
            // Structurally required, not defensive: `harness_items.thread_id`
            // is NOT NULL, so there is no row to write without one.
            //
            // What this branch actually catches is narrow: a *malformed* plan,
            // one carrying no `threadId` at all (upstream marks it required)
            // while the harness has no thread either. It is NOT the early-turn
            // case. A plan that does carry a `threadId` while `inner.thread_id`
            // is still `None` never reaches this arm — `on_notification` opens
            // by comparing `notif.thread_id()` (which for `Other` reads
            // `params.threadId`) against `inner.thread_id` and returns at the
            // top of this function. That prologue is the real silent-loss path
            // for an early plan, and it logs nothing at all. #1255 leaves it
            // alone on purpose: it is the shared prologue for every
            // notification type, so instrumenting it is its own change. If plan
            // loss ever needs to be observable, that is where to look.
            let Some(thread_id) = inner.thread_id.read().await.clone() else {
                tracing::warn!(
                    runtime_id = %inner.worker_session_id,
                    card_id = %inner.card_id,
                    method,
                    "planner harness dropping turn/plan/updated: the frame carries no threadId \
                     and no thread is known yet"
                );
                return persist_snapshot(inner).await;
            };
            // `turnId` is top-level on a plan; `item_turn_id` already falls
            // back to it — and, unlike `other_turn_id`, it also accepts the
            // snake_case `turn_id` spelling, which is why it is the one used
            // here (pinned by `turn_plan_updated_persists_rows_without_events`).
            let turn_id = item_turn_id(&params).map(ToOwned::to_owned);
            let params_json = serde_json::to_string(&params)?;
            inner
                .repo
                .harness_item_insert(
                    &inner.worker_session_id,
                    inner.card_id.as_str(),
                    inner.track_id.as_str(),
                    &thread_id,
                    turn_id.as_deref(),
                    // No `item_uuid`, and no `item_type`: a plan is not an item.
                    // It has no id and no item type, and writing either would
                    // state something untrue about the row. (It has no rendering
                    // consequence either way — `harnessItemToActivity` needs an
                    // `item/*` method *and* an `item` object in `params`, and a
                    // plan frame has neither.)
                    None,
                    None,
                    &method,
                    &params_json,
                    None,
                )
                .await?;
            // Deliberately NO `Event::HarnessItemAdded` for a plan row (#1255),
            // and the absence is the contract, not an oversight:
            //
            // - Nothing reads plan rows. No UI renders them, so there is nothing
            //   to invalidate. `harness.item.added` invalidates
            //   `['harness-items', cardId]` (fe/core/events/invalidation-plan.ts),
            //   which refetches a 300-row page — per plan frame, for data nobody
            //   renders.
            // - It is not free on the truth side either: `HarnessItemAdded` is
            //   *not* in the skip list in `calm-truth/src/track_vcs/commit.rs`,
            //   and `track_vcs/delta.rs` maps it to `add_card_runtime_paths`, so
            //   every plan frame would append a track-vcs commit re-rendering
            //   `cards/<id>/.payload.json` + `runtime.json`.
            // - Skipping it is not a truth-spine violation: `harness_items` is
            //   out-of-domain storage written directly, not event-sourced, so a
            //   row without an event is a legal state here.
            //
            // The UI slice MUST revisit this and choose knowingly between
            // (a) emitting `HarnessItemAdded` per plan update at the cost above,
            // and (b) letting plan rows ride the refresh that real item rows
            // already trigger.
            //
            // It will also need a way to *read* these rows: the transcript feed
            // (`GET /api/cards/:id/harness/items`) now narrows to `item/*` in
            // SQL, because its `limit` is the page budget of a reader that
            // renders only those. Give plans a read path of their own; do not
            // widen that query back to unfiltered
            // (`RepoRead::harness_item_list_transcript_by_card` says the same
            // where the filter lives).
        }
        // `thread/tokenUsage/updated` — how full the model's context is
        // (#1255 S3). Pushed after every upstream response; wire shape and the
        // `total` vs `last` trap are documented in `harness/token_usage.rs`,
        // which is where the parse and the arithmetic live. The one-line
        // version, because it is the mistake this arm exists to prevent:
        // `tokenUsage.total` is a LIFETIME sum over every response in the
        // thread and routinely exceeds the window; `tokenUsage.last` is the
        // occupancy proxy.
        //
        // Storage is the runtime snapshot, not the transcript table. The reading is
        // latest-wins — one value per runtime, superseded on every response —
        // which is exactly what `worker_sessions.handle_state` already is:
        // rewritten in place by `persist_snapshot_inner`, no event, no track-vcs
        // commit. Appending a row per response instead would need either its
        // own `Event` (a track-vcs commit plus a 300-row transcript refetch, per
        // model response) or no event at all, in which case nothing would ever
        // invalidate and no reader would see it. S2 appended to `harness_items`
        // because it was gathering evidence for a UI it could not yet design;
        // that reason does not transfer to a value whose whole content is
        // "the current number".
        //
        // CROSS-THREAD GATE. `PlannerHarness::run` subscribes to the daemon's
        // *global* notification broadcast, so every harness on this box sees
        // every `thread/tokenUsage/updated` frame from every thread. The only
        // thing keeping card A's meter from showing card B's context is
        // `on_notification`'s prologue — `notif.thread_id() != current_thread`
        // — and its failure mode is a plausible-looking wrong number, never an
        // error. `token_usage_from_a_foreign_thread_is_ignored` in
        // `tests/cases/planner_harness_token_usage.rs` is the test that holds it.
        //
        // One lenient edge, recorded because it is real and NOT worth building
        // machinery for: `other_thread_id` returns `None` for a frame with no
        // `threadId`, so a frame lacking the key compares equal to a harness
        // whose `inner.thread_id` is still `None` (pre-`thread/started`) and
        // would be ingested by an unrelated harness. `threadId` is REQUIRED in
        // the generated schema (see `harness/token_usage.rs` for the command
        // that prints it), so reaching this needs upstream protocol drift.
        // Note it; do not guard it.
        //
        // Note the deliberate absence of a `persist_snapshot` call in this arm:
        // the terminal `persist_snapshot(inner)` below runs for every
        // notification and serialises the whole snapshot, this field included.
        // Calling it here as well would write the same row twice per frame.
        Notification::Other { method, params } if method == "thread/tokenUsage/updated" => {
            match TokenUsage::from_params(&params, crate::model::now_ms()) {
                Some(incoming) => {
                    let mut slot = inner.token_usage.lock().await;
                    let merged = incoming.sticky_merge(slot.as_ref());
                    // Logged at ingest rather than inside `TokenUsage::percent`
                    // on purpose. `percent` is called once per `GET /planner/run`,
                    // i.e. once per client poll, so warning there would emit
                    // the same line forever for one bad frame. Here it fires
                    // once per frame that is actually anomalous, and the frame
                    // is still in hand to log against.
                    //
                    // This is not a formality: it is the alarm for our
                    // occupancy proxy being wrong, and it is calibrated. In
                    // 181_344 real usage frames on this box, `last` exceeded
                    // the window in 4 (0.002%, one session) — so this line
                    // firing is genuinely news, not noise. `percent` withholds
                    // the percentage in that case (it does NOT clamp to 100% —
                    // see its docs); this line is how anyone finds out.
                    if merged.exceeds_window() {
                        tracing::warn!(
                            target: "planner.harness.token_usage",
                            runtime_id = %inner.worker_session_id,
                            card_id = %inner.card_id,
                            used_tokens = merged.used_tokens,
                            context_window = ?merged.context_window,
                            "planner harness context usage exceeds the model context window; \
                             reporting the raw count with no percentage. The occupancy proxy \
                             (tokenUsage.last.totalTokens) may be wrong across compaction"
                        );
                    }
                    *slot = Some(merged);
                }
                // `last.totalTokens` is the only required part of the frame,
                // and a frame without a usable one — absent, non-integer, or
                // negative — yields no reading at all. Storing a zero would
                // claim an empty context, which is a stronger and possibly
                // false statement than "unknown"; the previous reading is left
                // in place instead. See `TokenUsage::from_params`.
                None => tracing::warn!(
                    target: "planner.harness.token_usage",
                    runtime_id = %inner.worker_session_id,
                    card_id = %inner.card_id,
                    method,
                    "planner harness dropping thread/tokenUsage/updated: no usable \
                     (non-negative integer) tokenUsage.last.totalTokens in the frame"
                ),
            }
        }
        Notification::Item { .. } | Notification::Other { .. } => {}
    }
    persist_snapshot(inner).await
}

fn other_turn_id(params: &Value) -> Option<&str> {
    params
        .get("turn")
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .or_else(|| params.get("turnId").and_then(Value::as_str))
}

fn item_turn_id(params: &Value) -> Option<&str> {
    params
        .get("turn")
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .or_else(|| params.get("turn_id").and_then(Value::as_str))
        .or_else(|| params.get("turnId").and_then(Value::as_str))
}

fn should_persist_item_method(method: &str) -> bool {
    matches!(method, "item/started" | "item/completed")
}

/// Both spellings, for the same reason `fe/core/domain/conversation.ts`
/// accepts both: live codex sends `userMessage`; the kernel stores
/// `item.type` verbatim and its own tests have used the snake case.
fn is_user_message_type(item_type: Option<&str>) -> bool {
    matches!(item_type, Some("userMessage" | "user_message"))
}

/// One transcript row for a codex `item/*` notification, exactly as this
/// file inserted it before #1625 P2. `input_segments` is NULL here for
/// every turn this binary issued: the segments of a batch live on the
/// projection row the drain wrote, and an echo that reaches this insert is
/// one no projection claims. The one exception is `legacy_segments_json`,
/// the pre-P2 slot's segments for the turn the previous binary left in
/// flight (`HarnessSnapshot::issued_input_segments`), which ride on the
/// echo row the way they did before this slice.
#[allow(clippy::too_many_arguments)]
async fn insert_item_row(
    inner: &Arc<Inner>,
    thread_id: &str,
    turn_id: Option<&str>,
    item_uuid: Option<&str>,
    item_type: Option<&str>,
    method: &str,
    params_json: &str,
    legacy_segments_json: Option<&str>,
) -> Result<i64> {
    Ok(inner
        .repo
        .harness_item_insert(
            &inner.worker_session_id,
            inner.card_id.as_str(),
            inner.track_id.as_str(),
            thread_id,
            turn_id,
            item_uuid,
            item_type,
            method,
            params_json,
            legacy_segments_json,
        )
        .await?)
}

async fn emit_item_added(
    inner: &Arc<Inner>,
    item_db_id: i64,
    item_uuid: Option<String>,
    item_type: Option<String>,
    turn_id: Option<String>,
    method: String,
) -> Result<()> {
    let scope = harness_event_scope(inner, "harness.item.added");
    inner
        .repo
        .log_pure_event(
            ActorId::Kernel,
            scope,
            None,
            &inner.events,
            &inner.card_role_cache,
            &inner.track_area_cache,
            Event::HarnessItemAdded {
                worker_session_id: inner.worker_session_id.clone(),
                card_id: inner.card_id.clone(),
                track_id: inner.track_id.clone(),
                item_db_id,
                item_uuid,
                item_type,
                turn_id,
                method,
            },
        )
        .await?;
    Ok(())
}

/// #1625 P2 (#1475) — the drained batch, written to the transcript BEFORE
/// `turn/start` goes out.
///
/// Until this row exists the person's own sentence is readable from nowhere
/// but the tab that sent it: the queue read (`GET /planner/run`) stops
/// listing an entry the moment it drains, and the transcript used to gain a
/// row only when codex echoed the turn back. Reload, or a second device, saw
/// an empty thread beside a `Working` dot for as long as codex took — or for
/// ever, when the turn never came back.
///
/// The row is a `userMessage` in the shape codex will echo (schema:
/// `UserMessageThreadItem { id, clientId, type, content: [TextUserInput] }`),
/// keyed so the echo can find it: `item_uuid` = `client_id`, which is what
/// the drain sends codex as `clientUserMessageId` and codex sends back as
/// `item.clientId`; `turn_id` NULL, because there is no turn yet; `method`
/// `item/completed`, because that is the only method a user message renders
/// under. `_projection: true` marks the params as kernel-written until the
/// echo replaces them. `input_segments` carries the per-entry segments, with
/// attachments — the column the transcript renders a user row from.
///
/// One row per drained turn, not per entry: the drain joins every entry into
/// ONE `InputItem::Text` (#1505 GAP-A3), so codex echoes ONE `userMessage`
/// per turn with ONE `clientId`. `client_id` is the key `maybe_issue_turn`
/// decided for the batch (the first user entry's id on a first issuance);
/// the other entries are readable through `input_segments`.
///
/// A projection with this key may already stand. The snapshot persisted at
/// the top of `maybe_issue_turn` still lists the batch AND carries this key
/// (`HarnessSnapshot::projection_client_id`), so a harness restarted between
/// that write and `persist_issuance_outcome` drains the same entries again
/// under the same key — the slot outranks the queue, so a sentence that
/// arrived after the restart and before the re-drain does not re-key the
/// batch (`state_from_snapshot`: an `IssuingTurn` phase comes back as
/// `TurnCompleted`/`Resumed`, and the queue is intact); a failed
/// `turn/start` whose delete failed leaves one too. Either way it is stale
/// — this drain is the batch's current issuance — so it is replaced, not
/// joined. That is also why the delete in the failure arm of
/// `maybe_issue_turn` only warns when it fails.
async fn write_projection_row(
    inner: &Arc<Inner>,
    thread_id: &str,
    client_id: &str,
    segments: &[HarnessInputSegment],
) -> Result<i64> {
    let item_db_id = insert_projection_row(inner, thread_id, client_id, segments).await?;
    // The existing per-row event, so every client refetches the transcript
    // now rather than at the echo (`fe/core/events/invalidation-plan.ts`
    // maps `harness.item.added` to `['harness-items', card_id]`). No new
    // event kind, no change to the invalidation plan.
    emit_item_added(
        inner,
        item_db_id,
        Some(client_id.to_string()),
        Some("userMessage".to_string()),
        None,
        "item/completed".to_string(),
    )
    .await?;
    Ok(item_db_id)
}

/// The row half of [`write_projection_row`], without the announcement.
///
/// #1625 P3 splits it out because a steer writes the same row at a different
/// moment — once codex has taken the input (`handle_steer`), where the drain
/// writes before `turn/start` — and announces it together with the
/// departure rather than through this function. The drain announces
/// immediately (`write_projection_row`), since its row stands until
/// `turn/start` fails, and that failure has a phase change to carry the
/// retraction.
async fn insert_projection_row(
    inner: &Arc<Inner>,
    thread_id: &str,
    client_id: &str,
    segments: &[HarnessInputSegment],
) -> Result<i64> {
    let stale = inner
        .repo
        .transcript_projection_delete(inner.card_id.as_str(), client_id)
        .await?;
    if stale > 0 {
        tracing::debug!(
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            client_id,
            stale,
            "planner harness replaced a stale user-message projection"
        );
    }
    // Without the diff block: that block is context the kernel prepends for
    // codex, and no transcript reader wants it (both frontends strip it from
    // the echo; here there is nothing to strip).
    let text = segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let params = serde_json::json!({
        "item": {
            "id": client_id,
            "clientId": client_id,
            "type": "userMessage",
            "content": [{ "type": "text", "text": text }],
        },
        "_projection": true,
    });
    let params_json = serde_json::to_string(&params)?;
    let input_segments = serde_json::to_string(segments)?;
    let item_db_id = inner
        .repo
        .harness_item_insert(
            &inner.worker_session_id,
            inner.card_id.as_str(),
            inner.track_id.as_str(),
            thread_id,
            None,
            Some(client_id),
            Some("userMessage"),
            "item/completed",
            &params_json,
            Some(&input_segments),
        )
        .await?;
    Ok(item_db_id)
}

/// 5s defensive cap on the track-vcs diff-block fetch inside `maybe_issue_turn`.
/// The diff block is a context augmentation prepended to planner turn
/// observations (#595 PR2); it is never a correctness requirement. If the
/// underlying sqlite SELECT chain stalls (issue #639 — silent stuck-turn
/// hypothesis), this ceiling converts an unobservable hang into a logged
/// warn + a degraded-but-functional turn issuance.
const SINCE_LAST_TURN_DIFF_TIMEOUT: Duration = Duration::from_secs(5);
const TRANSCRIPT_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);
const SINCE_LAST_TURN_HEAD_FALLBACK_TIMEOUT: Duration = Duration::from_secs(1);

/// How long the two codex reads that a model resolution may need are allowed
/// to take, together.
///
/// One budget for the pair, not one each, so the number below is the number a
/// stalled daemon can add to a turn. It is generous next to
/// `GET /api/models`'s eight seconds because nobody is watching a spinner
/// here — the cost of elapsing is a refused turn the person then has to
/// retry, which is worse than waiting a little longer for an answer.
///
/// Both reads only happen on the rare branch: a card that once had an
/// explicit model or effort and has since been set back to "follow the
/// default". A card that has never touched the picker resolves from its own
/// payload and never reaches this constant.
const MODEL_RESOLUTION_BUDGET: Duration = Duration::from_secs(15);

/// How long to leave a card alone after an attempt that could still succeed on
/// its own — briefing read failure, codex unreachable, or `turn/start` refused.
///
/// See [`Inner::issuance_retry_after`]. Short enough that a codex restart costs
/// the reader a pause rather than a stall, long enough that an outage does not
/// turn the 50 ms tick into a write-transaction storm. Without it a
/// re-buffered batch re-arms `hard_fire` and the next tick tries again 50 ms
/// later, which is roughly twenty RPCs and forty persist writes a second for
/// as long as the condition lasts.
const TRANSIENT_RETRY_DELAY: Duration = Duration::from_secs(2);

/// The same, for an attempt that CANNOT succeed until a person changes
/// something.
///
/// Polling fast buys nothing here — no amount of waiting makes codex's config
/// name a model — so the interval is long and the reader is told instead
/// ([`Inner::issuance_block`]). It stays a poll rather than a full stop
/// because the fix may arrive from outside this process, and because a harness
/// that stops trying is the wedge whose absence of an exit was the last
/// round's BLOCKER. `PUT /planner/model` clears the pause, so a person who
/// acts on the message does not then wait out this interval.
const NEEDS_A_CHOICE_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Read the card as it stands *now* and work out what the turn about to be
/// issued must tell codex about the model.
///
/// Separate from [`resolve_model_selection`] only so the read and the rule sit
/// together at the one call site that may perform them: this is the last
/// moment before the frame is built, and reading any earlier is the staleness
/// bug this function exists to prevent.
///
/// A card that has vanished between the top of `maybe_issue_turn` and here is
/// an `Err`, not an empty selection: there is no conversation left to answer
/// and no payload to answer it from.
async fn resolve_model_selection_for_issue(
    inner: &Arc<Inner>,
) -> std::result::Result<TurnModelSelection, IssuanceRefusal> {
    let card = match inner.repo.card_get(inner.card_id.as_str()).await {
        // A read that failed is a read that can succeed next time.
        Err(e) => {
            return Err(IssuanceRefusal::retryable(format!(
                "could not re-read the card to resolve its model: {e}"
            )));
        }
        // A card that is gone is not coming back, but there is also nobody
        // left to tell — the conversation it belonged to is gone with it.
        // Transient keeps the loop cheap without putting a message on a
        // surface no one is looking at.
        Ok(None) => {
            return Err(IssuanceRefusal::retryable(
                "the card this conversation belongs to no longer exists".into(),
            ));
        }
        Ok(Some(card)) => card,
    };
    resolve_model_selection(inner, &card.payload).await
}

/// A refusal to issue, and what waiting will do about it.
#[derive(Debug, Clone)]
struct IssuanceRefusal {
    kind: FailureKind,
    /// For the log. No advice, no audience.
    log: String,
    /// For the reader. Used by every kind except [`FailureKind::Retryable`],
    /// which supplies its own text on its own schedule — see `apply_refusal`
    /// and [`Inner::issuance_block`].
    ///
    /// It said "only `NeedsAChoice`" until `Rejected` was added and read it
    /// too, 145 lines below the sentence. Left empty for `Retryable`, whose
    /// `apply_refusal` arm calls `transient_notice` instead and never looks at
    /// this field — so nothing reads that emptiness and it carries no meaning.
    reader: String,
}

/// Classify a codex call that failed: codex ANSWERING with a refusal becomes
/// `refused(log)`, and every other failure is retryable.
///
/// **Every codex call on the issuance path goes through this** — `config/read`,
/// `model/list` and `turn/start`. It is written as a universal because it was
/// briefly false: `CodexRefused` was minted so `turn/start` could stop
/// promising delivery it could not make, and then only `turn/start` consulted
/// it, so a refused `config/read` or `model/list` still produced "your message
/// is still queued and will be sent when it answers" about a turn that could
/// not go out. Routing all three here is what stops the next call site
/// forgetting.
///
/// What each passes as `refused`:
///
/// | call | refused (`CodexRefused`) | could not ask |
/// |---|---|---|
/// | `config/read` | `NeedsAChoice`, naming the half that forced the read | `Retryable` |
/// | `model/list` | `NeedsAChoice` — only the effort can want the catalog | `Retryable` |
/// | `turn/start` | `Rejected` — no choice is KNOWN to remove the need | `Retryable` |
///
/// The first two are `NeedsAChoice` because a choice can genuinely remove the
/// need for the read — but WHICH choice depends on why the read happened, so
/// `config/read`'s sentence is derived from
/// [`CardModelSelection::defaults_needed_for`] rather than fixed. An explicit
/// model does NOT by itself skip `config/read`: the read is entered by a
/// disjunction, and a card with an explicit model and an effort following the
/// default still enters it.
///
/// Non-codex reads on this path (`card_get`, `track_get`) cannot be refused —
/// they have no peer to refuse them — and are `Retryable` on any failure.
fn classify_codex_failure(
    e: &CalmError,
    log: String,
    refused: impl FnOnce(String) -> IssuanceRefusal,
) -> IssuanceRefusal {
    if matches!(e, CalmError::CodexRefused(_)) {
        refused(log)
    } else {
        IssuanceRefusal::retryable(log)
    }
}

impl IssuanceRefusal {
    fn retryable(log: String) -> Self {
        Self {
            kind: FailureKind::Retryable,
            log,
            reader: String::new(),
        }
    }

    /// Codex answered and refused. Says so, and promises nothing.
    fn rejected(log: String) -> Self {
        Self {
            kind: FailureKind::Rejected,
            log,
            reader: "codex refused to start a turn for this conversation, so your message has \
                     not been sent. If you changed the model recently, it may not be one this \
                     account can run — try another."
                .into(),
        }
    }

    fn needs_a_choice(log: String, reader: String) -> Self {
        Self {
            kind: FailureKind::NeedsAChoice,
            log,
            reader,
        }
    }
}

/// Work out what this turn must tell codex about the model, from the card's
/// payload plus — only where the payload cannot answer alone — codex's own
/// effective config and catalog.
///
/// Returns `Err` when the answer cannot be established. The caller does not
/// send the turn on that: see [`crate::planner_model`]'s header for why
/// running under an unknown model is worse than not running.
///
/// The refusal carries BOTH a log line and, for the kinds a person has to act
/// on, a sentence for the reader that reaches them as
/// `GET /planner/run`'s `blocked_reason`. This doc previously said the reason
/// was "a LOG line, not advice to a reader … there is nothing for anyone to be
/// told to do" — written when every refusal was a silent retry, and left
/// standing when `blocked_reason` was added, so it denied the existence of the
/// field two commits of this PR are about.
async fn resolve_model_selection(
    inner: &Arc<Inner>,
    payload: &Value,
) -> std::result::Result<TurnModelSelection, IssuanceRefusal> {
    // A payload we cannot read does not start reading itself. Somebody has to
    // write a selection over it, and `PUT /planner/model` does exactly that.
    let card = CardModelSelection::from_payload(payload).map_err(|e| {
        IssuanceRefusal::needs_a_choice(
            e.to_string(),
            "This conversation's saved model selection cannot be read. Pick a model to replace it."
                .into(),
        )
    })?;
    if !card.needs_installation_defaults() {
        // The overwhelmingly common path: the payload is the whole answer.
        return resolve_turn_selection(&card, None, None).map_err(unresolved);
    }

    let deadline = tokio::time::Instant::now() + MODEL_RESOLUTION_BUDGET;
    let cwd = installation_cwd(inner).await?;
    // Codex not answering and codex answering "no model" are different facts
    // and must not collapse into one. Only the first is worth waiting out, so
    // the read's failure returns here rather than degrading to `None` and
    // being mistaken for an answer further down.
    let config = inner
        .daemon
        .config_read(Some(cwd.as_str()), deadline)
        .await
        .map_err(|e| {
            // The sentence is DERIVED. This branch is entered by a disjunction
            // — the model follows the default, or the effort does, or both —
            // and a fixed string is right for at most one of them. It said
            // "Pick a model explicitly" for a card whose model was already
            // explicit and whose EFFORT followed the default: the reader
            // re-picked what they already had, the disjunct stayed true, and
            // the next tick refused identically.
            let needed = card.defaults_needed_for();
            let reader = match (needed.subject(), needed.choice_to_make()) {
                (Some(subject), Some(choice)) => format!(
                    "codex will not report this conversation's configuration, so the default \
                     {subject} cannot be resolved and your message has not been sent. Pick \
                     {choice} explicitly to send it."
                ),
                // Unreachable: this read only happens when something is
                // needed. Fail closed with no advice rather than invent some.
                _ => "codex will not report this conversation's configuration, so your message \
                      has not been sent."
                    .to_string(),
            };
            classify_codex_failure(
                &e,
                format!("config/read failed while resolving this conversation's defaults: {e}"),
                |log| IssuanceRefusal::needs_a_choice(log, reader),
            )
        })?;
    let defaults = Some(InstallationDefaults {
        model: config.model,
        reasoning_effort: config.model_reasoning_effort,
    });

    // Only the effort's last fallback wants the catalog, and only when the
    // config did not already answer it.
    let catalog_effort = if card.needs_catalog()
        && defaults
            .as_ref()
            .is_none_or(|d| d.reasoning_effort.is_none())
    {
        catalog_default_effort(inner, &card, defaults.as_ref(), deadline).await?
    } else {
        None
    };

    resolve_turn_selection(&card, defaults.as_ref(), catalog_effort.as_deref()).map_err(unresolved)
}

/// Every refusal `resolve_turn_selection` can produce is one a person has to
/// act on: it is only reached once codex has answered, so getting here means
/// the answer did not name a model.
fn unresolved(e: crate::planner_model::UnresolvedSelection) -> IssuanceRefusal {
    IssuanceRefusal::needs_a_choice(e.log_reason().to_string(), e.reason().to_string())
}

/// Record a refusal: how long before the next attempt, and what (if anything)
/// the reader is told.
///
/// One place, because the arms that refuse must not drift into different
/// answers for the same fact — they already had, once, when only one of them
/// was paced.
async fn apply_refusal(inner: &Arc<Inner>, failure: &IssuanceRefusal) {
    let (delay, notice) = match failure.kind {
        // Nobody can act, and repeating may work. Silent while that is
        // plausibly still true; see `transient_notice`.
        FailureKind::Retryable => (TRANSIENT_RETRY_DELAY, transient_notice(inner).await),
        // Codex saw the input and said no. Retried slowly rather than not at
        // all — the person may change the model from another tab, and a
        // harness that stops trying is the wedge with no exit — but the reader
        // is told now, and told the truth: nothing is on its way.
        FailureKind::Rejected => (NEEDS_A_CHOICE_RETRY_DELAY, Some(failure.reader.clone())),
        FailureKind::NeedsAChoice => (NEEDS_A_CHOICE_RETRY_DELAY, Some(failure.reader.clone())),
    };
    *inner.issuance_retry_after.lock().await = Some(Instant::now() + delay);
    *inner.issuance_block.lock().await = notice;
}

/// What to tell the reader about a run of RETRYABLE refusals — nothing at
/// first, and then that the conversation is waiting.
///
/// Only this arm is silent at first. A refusal codex actually answered, and a
/// selection nobody can determine, are said immediately: neither gets better
/// by itself, so there is no brief window in which saying nothing is honest.
///
/// Silence is right for a codex restart: it lasts seconds, nobody can act on
/// it, and a notice for it would train the reader to ignore the field. Silence
/// stops being right when it stops being brief. Past
/// [`HarnessConfig::transient_silence_budget`] a message that has gone nowhere
/// for that long is indistinguishable, from the reader's side, from one that
/// will never go anywhere — so the conversation says it is waiting.
///
/// Pacing the retry bounded its RATE; this bounds its SILENCE. The two are
/// different guarantees, and the first was mistaken for the second.
///
/// It names no action, because there is none to name; it exists so that
/// "queued" stops being the only thing on screen. The retry continues
/// underneath, so the notice clears itself the moment codex answers.
async fn transient_notice(inner: &Arc<Inner>) -> Option<String> {
    let now = Instant::now();
    let began = {
        let mut since = inner.refusing_since.lock().await;
        *since.get_or_insert(now)
    };
    (now.duration_since(began) >= inner.config.transient_silence_budget).then(|| {
        "Waiting for codex — it has not accepted this conversation's last few turns. Your \
         message is still queued and will be sent when it answers."
            .to_string()
    })
}

/// #1505 S4 review r4 — park between the card read and the track read, so a
/// test can order "the track is deleted" strictly between them.
///
/// The window is real and is NOT forbidden by the schema, which is the
/// argument an earlier round got backwards. `cards` holds a foreign key to
/// `tracks`, so "card row present, track row absent" cannot exist at any
/// single database INSTANT — but this path reads at two instants with an
/// `.await` between them, and the foreign key says nothing about that. The
/// card read succeeds, `track_delete_tx` commits and takes the card with it,
/// and the track read then answers `Ok(None)` to a harness whose
/// `inner.track_id` names a track that is gone. The earlier claim that the
/// card-existence check "has already returned" by then was backwards: its
/// having returned IS the window.
///
/// The general form, because it will recur: a foreign key is an invariant over
/// one transaction, never over a read-then-read across an await.
///
/// Same convention as [`PlannerHarnessDrainRaceHook`]: the struct, the
/// registry and the wait are `fixtures`-only, and in a release build the call
/// site collapses to `let _ = worker_session_id;`.
#[cfg(feature = "fixtures")]
#[derive(Clone)]
pub struct PlannerHarnessCwdRaceHook {
    pub entered: Arc<Notify>,
    pub release: Arc<Notify>,
}

#[cfg(feature = "fixtures")]
fn planner_harness_cwd_race_hooks() -> &'static StdMutex<HashMap<String, PlannerHarnessCwdRaceHook>>
{
    static HOOKS: OnceLock<StdMutex<HashMap<String, PlannerHarnessCwdRaceHook>>> = OnceLock::new();
    HOOKS.get_or_init(|| StdMutex::new(HashMap::new()))
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn install_planner_harness_cwd_race_hook_for_test(
    worker_session_id: &str,
    hook: PlannerHarnessCwdRaceHook,
) {
    planner_harness_cwd_race_hooks()
        .lock()
        .expect("planner harness cwd hook mutex")
        .insert(worker_session_id.to_string(), hook);
}

async fn wait_at_planner_harness_cwd_race_hook(worker_session_id: &str) {
    #[cfg(feature = "fixtures")]
    {
        let hook = planner_harness_cwd_race_hooks()
            .lock()
            .expect("planner harness cwd hook mutex")
            .remove(worker_session_id);
        if let Some(hook) = hook {
            hook.entered.notify_one();
            hook.release.notified().await;
        }
    }
    #[cfg(not(feature = "fixtures"))]
    let _ = worker_session_id;
}

/// The workspace whose config layers apply to this conversation's thread.
///
/// It must be the path `thread/start` was given, or `config/read` folds in a
/// different set of project layers and answers a question we did not ask.
///
/// A track that cannot be read is therefore an `Err`, not a `None`. It used to
/// be a `None`, which `config_read` accepts by reading only the layers that
/// apply everywhere — a global answer handed back as this card's. That
/// sentence survived the fix that removed the behaviour and sat here
/// describing it as current; it is spelled out in the past tense now because
/// re-reading it as an instruction is how the bug comes back.
async fn installation_cwd(inner: &Arc<Inner>) -> std::result::Result<String, IssuanceRefusal> {
    // Deterministic card-read-then-track-read window. No-op in production.
    wait_at_planner_harness_cwd_race_hook(inner.worker_session_id.as_str()).await;
    match inner.repo.track_get(inner.track_id.as_str()).await {
        Ok(Some(track)) => Ok(track.workspace.path),
        // Both of these used to return `None`, which `config_read` accepts and
        // answers WITHOUT the project layers — a global answer handed back as
        // if it were this card's. That is the same collapse as the one below:
        // "there is no workspace" and "we could not read the workspace" are
        // different facts, and neither of them means "read the global layers
        // instead". A card whose track cannot be read is retried, not answered
        // from the wrong scope.
        //
        // `Ok(None)` is REACHABLE, and an earlier round of this PR argued
        // the opposite from a foreign key. See this function's header: the FK
        // constrains one transaction, this path reads at two instants, and a
        // `track_delete_tx` committing between them produces exactly this.
        Ok(None) => Err(IssuanceRefusal::retryable(format!(
            "track {} is not readable, so this conversation's config scope is unknown",
            inner.track_id
        ))),
        Err(e) => Err(IssuanceRefusal::retryable(format!(
            "could not read the track workspace for a config/read cwd: {e}"
        ))),
    }
}

/// Codex's own preset effort for the model that will actually run.
///
/// `UnresolvedSelection::Effort` is the alternative, so this is the last thing
/// standing between "the person set an effort once and has since chosen the
/// default" and a stalled conversation. It is codex's number, never one we
/// picked.
///
/// **`Ok(None)` means the catalog genuinely has no answer; a read that failed
/// is an `Err`.** Collapsing the two into one `None` is what let a `model/list`
/// timeout be reported to the reader as "Pick a reasoning effort to start it
/// again" — advice for a hiccup that would have cleared itself, and a retry
/// interval fifteen times longer than the one it deserved. `config/read` had
/// the same collapse one call above and was fixed alone; this is the rest of
/// the class.
async fn catalog_default_effort(
    inner: &Arc<Inner>,
    card: &CardModelSelection,
    defaults: Option<&InstallationDefaults>,
    deadline: tokio::time::Instant,
) -> std::result::Result<Option<String>, IssuanceRefusal> {
    let Some(slug) = effective_model_for_catalog_lookup(card, defaults) else {
        // Nothing names a model, so there is no catalog entry to look up. A
        // real absence, not a failed read.
        return Ok(None);
    };
    match inner.daemon.model_list(deadline).await {
        Ok(models) => Ok(models
            .into_iter()
            .find(|m| m.model == slug)
            .map(|m| m.default_reasoning_effort)),
        // Only the effort can want the catalog (`needs_catalog`), so unlike
        // `config/read` this one has a single reason and a fixed sentence is
        // honest.
        Err(e) => Err(classify_codex_failure(
            &e,
            format!("model/list failed while resolving this conversation's default effort: {e}"),
            |log| {
                IssuanceRefusal::needs_a_choice(
                    log,
                    "codex will not list its models, so the default reasoning effort cannot be \
                     resolved and your message has not been sent. Pick a reasoning effort \
                     explicitly to send it."
                        .to_string(),
                )
            },
        )),
    }
}

/// Consume only successful bookkeeping for a Done Track. Both live delivery
/// and persisted replay reach this boundary. The event log and already accepted
/// push watermark stay intact; failures, user intent and other observations
/// retain their queue order and normal delivery semantics.
async fn consume_completed_worktree_commits(inner: &Arc<Inner>) -> Result<()> {
    let is_commit = |entry: &QueueEntry| {
        matches!(entry, QueueEntry::System {
            observation: Observation::WorktreeCommitted { track_id, .. }, ..
        } if track_id == &inner.track_id)
    };
    if !inner.pending_queue.lock().await.iter().any(is_commit) {
        return Ok(());
    }
    if !inner
        .repo
        .track_get(inner.track_id.as_str())
        .await?
        .is_some_and(|track| track.lifecycle == crate::model::TrackLifecycle::Done)
    {
        return Ok(());
    }
    let checkpoint = checkpoint_durable_user_message(inner).await;
    let consumed = {
        let mut queue = inner.pending_queue.lock().await;
        let before = queue.len();
        queue.retain(|entry| !is_commit(entry));
        let mut debounce = inner.debounce.lock().await;
        debounce.hard_fire = queue.iter().any(QueueEntry::is_hard_fire);
        if queue.is_empty() {
            *debounce = DebounceState::default();
        }
        before - queue.len()
    };
    // Persist consumption even when no turn remains to write a later snapshot.
    // A failed write must leave the original queue available for retry.
    if let Err(error) = persist_snapshot(inner).await {
        restore_durable_user_message(inner, checkpoint).await;
        return Err(error);
    }
    tracing::debug!(
        track_id = %inner.track_id,
        consumed,
        "consumed successful worktree commit observations for a Done Track"
    );
    Ok(())
}

/// #1667 D2 — true iff the queue is non-empty and EVERY entry is a
/// `ReportEdited` observation. A single user message, task receipt or any
/// other observation in the queue makes this false, and the ordinary
/// debounce pair applies to the whole batch.
fn queue_is_only_report_edits(queue: &VecDeque<QueueEntry>) -> bool {
    !queue.is_empty()
        && queue.iter().all(|entry| {
            matches!(
                entry,
                QueueEntry::System {
                    observation: Observation::ReportEdited { .. },
                    ..
                }
            )
        })
}

async fn maybe_issue_turn(inner: &Arc<Inner>) -> Result<()> {
    // Issue #682 review — dev-forced harnesses run against the replay
    // binary's stub app-server; see `PlannerHarness::pause_issuance_for_dev`.
    if inner.issuance_paused.load(Ordering::SeqCst) {
        return Ok(());
    }
    // Most ticks find the queue empty; bail before any logging so the 50ms
    // tick cadence does not flood the log with one entry line per tick.
    let (queue_len, only_report_edits) = {
        let queue = inner.pending_queue.lock().await;
        (queue.len(), queue_is_only_report_edits(&queue))
    };
    if queue_len == 0 {
        return Ok(());
    }
    // #1505 S4 review — see `Inner::model_resolution_retry_after`. Checked
    // here, before any of the work below, because the point is to skip that
    // work and not merely to skip the codex call at the end of it.
    if let Some(retry_after) = *inner.issuance_retry_after.lock().await
        && Instant::now() < retry_after
    {
        return Ok(());
    }
    let (hard_fire, first_pending_at, last_pending_at) = {
        let debounce = inner.debounce.lock().await;
        (
            debounce.hard_fire,
            debounce.first_pending_at,
            debounce.last_pending_at,
        )
    };
    // No state snapshot here: the gating-reason logs below already cover the
    // state-blocked case, and the happy path logs the state implicitly through
    // the "calling daemon.turn_start" → "daemon.turn_start ok" pair.
    tracing::debug!(
        target: "calm_server::planner_harness_issue",
        runtime_id = %inner.worker_session_id,
        card_id = %inner.card_id,
        track_id = %inner.track_id,
        queue_len,
        hard_fire,
        "maybe_issue_turn entry (queue non-empty)"
    );

    let now = Instant::now();
    // #1667 D2 — a queue that is nothing but report edits waits for the
    // editor to go quiet (one wake per edit, not per save); any other
    // soft entry keeps the ordinary pair. `hard_fire` is checked first
    // and wins outright, so this only ever lengthens a wait.
    let (min_idle, max_wait) = if only_report_edits {
        (
            inner.config.report_edit_min_idle,
            inner.config.report_edit_max_wait,
        )
    } else {
        (
            inner.config.debounce_min_idle,
            inner.config.debounce_max_wait,
        )
    };
    let should_issue = if hard_fire {
        true
    } else {
        let Some(first) = first_pending_at else {
            tracing::debug!(
                target: "calm_server::planner_harness_issue",
                runtime_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                track_id = %inner.track_id,
                hard_fire,
                "debounce gating turn issuance (no first_pending_at)"
            );
            return Ok(());
        };
        let Some(last) = last_pending_at else {
            tracing::debug!(
                target: "calm_server::planner_harness_issue",
                runtime_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                track_id = %inner.track_id,
                hard_fire,
                "debounce gating turn issuance (no last_pending_at)"
            );
            return Ok(());
        };
        now.duration_since(last) >= min_idle || now.duration_since(first) >= max_wait
    };
    if !should_issue {
        tracing::debug!(
            target: "calm_server::planner_harness_issue",
            runtime_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            track_id = %inner.track_id,
            hard_fire,
            first_pending_ms = first_pending_at
                .map(|t| now.duration_since(t).as_millis() as u64),
            last_pending_ms = last_pending_at
                .map(|t| now.duration_since(t).as_millis() as u64),
            "debounce gating turn issuance"
        );
        return Ok(());
    }

    {
        let state = inner.state.lock().await;
        if !state.can_issue_turn() {
            tracing::debug!(
                target: "calm_server::planner_harness_issue",
                runtime_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                track_id = %inner.track_id,
                state = ?*state,
                "state gating turn issuance"
            );
            return Ok(());
        }
    }
    // #1449 — the DURABLE half of "may I still speak for this card", and it is
    // not redundant with the `shutting_down` flag consulted further down.
    //
    // `shutting_down` is process memory, set by `PlannerHarness::shutdown`. A
    // runtime can be retired in the DATABASE with its run loop perfectly
    // healthy and unaware: `prepare_tx` supersedes the card's live predecessor
    // and takes its pending queue, and nothing stops the predecessor's handle
    // until a later step of the same operation tears it down. In that window
    // the predecessor would issue a turn for a queue its successor is also
    // carrying.
    //
    // Placed HERE, above the work, rather than beside the drain: below this
    // point every tick pays a card/role lookup, a track-level transcript WRITE
    // transaction and a diff. A runtime refused at the drain kept paying all
    // of that, every 50ms, for as long as it lived. Above it, a refused
    // runtime pays one indexed read by id per tick and nothing else.
    //
    // The refusal declines to issue and returns; it does NOT wind the handle
    // down. A handle that stopped would still be registered, and
    // `ensure_live_planner_harness` does not health-check a registered handle,
    // so a predecessor a failed mint later restored would answer `Conflict` on
    // every send with no way back but `/planner/reset`. Stopping also raced
    // the durable-observation path, which reads `shutting_down` under a lock
    // this had no reason to hold.
    if !runtime_is_still_the_live_carrier(inner).await? {
        tracing::debug!(
            target: "calm_server::planner_harness_issue",
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            track_id = %inner.track_id,
            "runtime is no longer the card's live carrier; leaving the queue for its successor"
        );
        return Ok(());
    }
    // Two independent per-turn decisions that used to ride on one boolean
    // (#1189 review A6). Splitting them is the whole point:
    //
    // * `skip_transcript_refresh` — skip the track-level
    //   `snapshot_transcripts_for_cards_in_track` WRITE transaction below.
    // * `skip_track_diff` — issue the turn with no "track state changes since
    //   your last turn" block at all.
    //
    // An area chat skips both: it lives alone on a hidden scaffolding track, so
    // there is nothing to snapshot and nothing to diff.
    //
    // A track assistant skips only the first, and the asymmetry is deliberate.
    //   - Skipping the WRITE: the refresh commits a track-scoped track-vcs
    //     commit before every turn, and #1189's premise is N conversations on
    //     one track, so keeping it would multiply that write by N and make every
    //     assistant turn contend for the same sqlite write lock as the planner
    //     harness's own per-turn refresh.
    //
    //     This is a REAL, BOUNDED degradation — not "nothing is lost" — and the
    //     boundary is written out here because widening the skip is only safe
    //     inside it:
    //       * `cards/<id>/events.json` and `cards/<id>/conversation.md` are
    //         dirtied by exactly two places in the tree, both via
    //         `track_vcs::delta::add_card_event_paths`: `add_card_paths`
    //         (reachable only from `CardAdded` / `CardUpdated`, i.e. card
    //         creation) and `snapshot_transcripts_for_cards_in_track` — this
    //         very refresh. The ordinary event-driven commit path does NOT keep
    //         them current: `HarnessItemAdded` / `HarnessPhaseChanged` /
    //         `HarnessTranscriptCleared` / `HarnessUserMessageEnqueued` dirty
    //         only `.payload.json` + `runtime.json`, and `CodexHook` /
    //         `ClaudeHook` produce an EMPTY delta (`track_vcs/delta.rs`).
    //         `planner_harness_track_vcs.rs::
    //         since_last_turn_override_fences_post_refresh_hook_commit` pins
    //         that fact directly: a post-refresh hook commit advances HEAD and
    //         still does not contain its own transcript.
    //       * So on this track the freshness of BOTH transcript paths is
    //         maintained solely by the planner harness's own per-turn refresh. The
    //         event-driven path still keeps `report.md`, `runs/*`,
    //         `cards/<id>/.payload.json`, `cards/<id>/runtime.json` and newly
    //         added cards current; the skip degrades transcripts and nothing
    //         else.
    //
    //     Why that degradation is acceptable for THIS role and only this role:
    //     an assistant cannot read those paths at all. `track_file` (ls/cat) and
    //     `track_history` are `require_role_any([Planner, Worker])`, so an
    //     Assistant card is rejected by role; its track-fs surface is
    //     `track_report*` (`[Planner, Assistant]`), and `report.md` IS kept fresh by
    //     the event-driven path. The collaboration channel this design gives the
    //     assistant is the report block, not the transcript.
    //
    //     Consequences for whoever touches this next: (a) do NOT extend the skip
    //     to Planner or Worker cards — they can `track_file cat`
    //     `conversation.md`/`events.json` and would read a stale HEAD; (b) if
    //     the planner harness of this track ever stops refreshing per turn, these
    //     two paths have no writer left and go stale for everyone. The root fix
    //     is to make the hook / harness-item event transactions dirty the
    //     transcript paths too; that changes `track_vcs` delta semantics for all
    //     cards and is deliberately out of scope here.
    //   - Keeping the DIFF: this is what the assistant must not lose. It is the
    //     track's report patch plus the paths that changed since this
    //     conversation's last turn — for a card whose entire job is answering
    //     questions about the track and editing its report, that block is the
    //     context, not decoration. `since_last_turn_block` with no
    //     `current_override` simply reads the track's current head, so dropping
    //     the refresh costs at most the freshness a concurrent refresh would
    //     have added, never the block itself.
    //
    // #1505 S4-3's card-existence check rides on this same read. The model
    // SELECTION deliberately does not: it is read again, much later, at the
    // moment the batch is handed to codex — see `resolve_model_selection`'s
    // call site for why this row is too old to decide it.
    let Some(card) = inner.repo.card_get(inner.card_id.as_str()).await? else {
        // #1505 S4-3 narrows the old `(false, false)` here. A card that is
        // gone has no model selection to resolve and no conversation left to
        // answer, so issuing a turn for it spends a model call on nobody. The
        // queue is not drained and the state is not touched: a card that
        // reappears (a racing delete-then-restore, a read against a replica
        // mid-write) issues on the next tick exactly as before.
        tracing::warn!(
            target: "calm_server::planner_harness_issue",
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            "planner harness card is gone; not issuing a turn for it"
        );
        return Ok(());
    };
    let (skip_transcript_refresh, skip_track_diff) = {
        let role = inner.repo.card_role_get(card.id.as_str()).await?;
        if crate::plain_chat::card_is_plain_chat(&card, role, true) {
            (true, true)
        } else if crate::plain_chat::card_is_track_assistant(&card, role, true) {
            (true, false)
        } else {
            (false, false)
        }
    };
    let last_seen_head_snapshot = inner.last_seen_head.lock().await.clone();
    let refresh_head = if skip_transcript_refresh {
        None
    } else {
        let refresh_repo = Arc::clone(&inner.repo);
        let refresh_track_id = inner.track_id.clone();
        transcript_refresh_with_timeout(
            write_in_tx_typed::<track_vcs::CommitHash, _>(refresh_repo.as_ref(), move |tx| {
                Box::pin(async move {
                    track_vcs::snapshot_transcripts_for_cards_in_track(
                        tx,
                        &refresh_track_id,
                        None,
                        track_vcs::MANIFEST_SCHEMA_VERSION,
                    )
                    .await
                    .map_err(CalmError::from)
                })
            }),
            TRANSCRIPT_REFRESH_TIMEOUT,
            &inner.worker_session_id,
            inner.card_id.as_str(),
            inner.track_id.as_str(),
        )
        .await
    };
    tracing::debug!(
        target: "calm_server::planner_harness_issue",
        runtime_id = %inner.worker_session_id,
        card_id = %inner.card_id,
        track_id = %inner.track_id,
        last_seen_head = ?last_seen_head_snapshot,
        refresh_head = ?refresh_head.as_deref(),
        "fetching since-last-turn diff"
    );
    let diff = if skip_track_diff {
        track_vcs::SinceLastTurnBlock::empty()
    } else {
        diff_with_timeout(inner, refresh_head.as_ref()).await
    };
    // Deterministic drain-vs-supersede window for #1449. No-op in production.
    wait_at_planner_harness_drain_race_hook(&inner.worker_session_id).await;
    let _issuance_guard = inner.issuance.lock().await;
    if inner.shutting_down.load(Ordering::SeqCst) {
        return Ok(());
    }
    // #1449 — asked a SECOND time, here, and the two are not redundant.
    //
    // The check above runs before the transcript refresh and the diff so a
    // retired runtime does not pay for them; but that leaves the
    // whole of that work between the answer and the queue being taken, and a
    // fence landing inside that gap is exactly the case this is about. This
    // one is immediately before the drain and costs one indexed read per turn
    // actually being issued.
    if !runtime_is_still_the_live_carrier(inner).await? {
        tracing::debug!(
            target: "calm_server::planner_harness_issue",
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            track_id = %inner.track_id,
            "runtime was retired while this turn was being prepared; leaving the queue"
        );
        return Ok(());
    }
    tracing::debug!(
        target: "calm_server::planner_harness_issue",
        runtime_id = %inner.worker_session_id,
        card_id = %inner.card_id,
        track_id = %inner.track_id,
        block_some = diff.block.is_some(),
        current_head = ?diff.current_head.as_deref(),
        "since-last-turn diff resolved"
    );

    // A successful commit may have queued while the preceding Planner turn
    // was still accepting results. Re-evaluate at delivery, after that turn
    // can have marked Done; enqueue-time filtering would miss this case.
    consume_completed_worktree_commits(inner).await?;
    if inner.pending_queue.lock().await.is_empty() {
        return Ok(());
    }

    // The removed commit may have been the only hard-fire entry. Let the
    // next tick apply the existing debounce to any remaining soft entries;
    // this invocation was admitted using the queue's pre-consumption state.
    if hard_fire && !inner.debounce.lock().await.hard_fire {
        return Ok(());
    }

    let prior_turn = {
        let mut state = inner.state.lock().await;
        if !state.can_issue_turn() {
            tracing::debug!(
                target: "calm_server::planner_harness_issue",
                runtime_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                track_id = %inner.track_id,
                state = ?*state,
                "state gating turn issuance post-diff"
            );
            return Ok(());
        }
        let prior = match &*state {
            HarnessState::TurnCompleted { last_turn_id } => Some(last_turn_id.clone()),
            _ => None,
        };
        *state = HarnessState::Issuing {
            since: Instant::now(),
            kind: IssuingKind::TurnStart,
        };
        prior
    };
    *inner.issued_turn_id.lock().await = None;
    *inner.issued_turn_head.lock().await = None;
    // A new batch supersedes the pre-P2 slot, as it did before this slice: a
    // turn can only be issued once the previous one completed, and by then
    // that turn's echo has either consumed the slot or is not coming.
    *inner.legacy_issued_input_segments.lock().await = None;
    // #1625 P2 — the key shared by the projection row and `turn/start`'s
    // `clientUserMessageId`, decided HERE so that the snapshot written next
    // carries it: a harness restarted between that write and
    // `persist_issuance_outcome` re-drains the same batch (the queue is
    // intact in that snapshot) and must project it under the SAME key, or
    // the stale-row replacement in `write_projection_row` matches nothing and
    // the batch stands on the transcript twice.
    //
    // In order: the key an earlier issuance of this batch already used — the
    // slot survives a re-buffer and a restart precisely so the batch is not
    // re-keyed; else the first entry that has an id — a person's sentence
    // keeps its own id as the row's `item_uuid`, which the queue already
    // persists; else a fresh mint, for a batch of system entries alone (a
    // commit notification, a task result) on its first issuance. Read from
    // the queue rather than from `drained` because the queue does not change
    // between here and the drain below: every enqueue and every re-buffer
    // runs on this task.
    //
    // The slot outranks the queue (review round 2). The slot is `Some` only
    // from a drain's key decision until that batch's issuance outcome clears
    // it, and the batch stays at the head of the queue for that whole span
    // (each failure exit past this point re-buffers what it drained), so a
    // set slot always names the batch about to be drained, and the row its
    // predecessor may have written under it.
    // The queue can have GROWN by then: a sentence enqueued after a restart
    // and before this re-drain sits behind the recovered batch, and keying
    // by that sentence's id would leave the predecessor's row standing
    // beside the new one, because `write_projection_row` replaces under one
    // key only. What the recovered key names is the batch, not any entry
    // still in it: a person who deletes the sentence a re-buffered key came
    // from leaves the next drain keyed by an id no entry carries, which is
    // an identity and nothing more (no row stands under it — the failure
    // arm below deleted it).
    let client_id = {
        let from_queue = inner
            .pending_queue
            .lock()
            .await
            .iter()
            .find_map(QueueEntry::id)
            .cloned();
        let mut slot = inner.projection_client_id.lock().await;
        let key = slot
            .clone()
            .or(from_queue)
            .unwrap_or_else(QueueEntryId::mint);
        *slot = Some(key.clone());
        key
    };
    persist_snapshot(inner).await?;

    let drained = {
        let mut queue = inner.pending_queue.lock().await;
        queue.drain(..).collect::<Vec<_>>()
    };
    if drained.is_empty() {
        *inner.state.lock().await = prior_turn
            .map(|last_turn_id| HarnessState::TurnCompleted { last_turn_id })
            .unwrap_or(HarnessState::Idle);
        *inner.issued_turn_id.lock().await = None;
        return Ok(());
    }
    *inner.debounce.lock().await = DebounceState::default();
    // #1505 S6 — segments are built from the ENTRIES, not from observations.
    // An `Observation` cannot carry an attachment; the queue entry can, and
    // the bind put them there. `input_segments_for_entries` still delegates
    // the presentation and the rendered text to
    // `Observation::input_segments_for`, so this is not a second copy of that
    // table.
    let prepared = async {
        let semantic = match inner.thread_id.read().await.clone() {
            Some(thread) => {
                crate::semantic_recovery::registered(
                    inner.repo.as_ref(),
                    inner.card_id.as_str(),
                    &thread,
                )
                .await?
            }
            None => false,
        };
        let mut prepared = super::recovery_briefing::input_segments(
            inner.repo.as_ref(),
            &inner.card_id,
            &inner.track_id,
            &inner.worker_session_id,
            &drained,
            semantic,
        )
        .await?;
        super::result_receipt::enrich(
            inner.repo.as_ref(),
            &crate::state::WriteContext::new(
                inner.card_role_cache.clone(),
                inner.track_area_cache.clone(),
            ),
            &inner.track_id,
            &drained,
            &mut prepared.segments,
        )
        .await?;
        crate::error::Result::Ok(prepared)
    }
    .await;
    let prepared = match prepared {
        Ok(segments) => segments,
        Err(error) => {
            // Briefing reads must not lose the drained notifications or leave
            // the harness stuck Issuing. Rebuffering arms hard_fire, so use the
            // existing pacing guard before another tick repeats reads and writes.
            rebuffer_head(inner, drained).await;
            *inner.state.lock().await = prior_turn
                .map(|last_turn_id| HarnessState::TurnCompleted { last_turn_id })
                .unwrap_or(HarnessState::Idle);
            *inner.issuance_retry_after.lock().await = Some(Instant::now() + TRANSIENT_RETRY_DELAY);
            *inner.issuance_block.lock().await = Some(
                "Could not prepare the recovery decision briefing. Your messages remain queued; the system will retry."
                    .into(),
            );
            persist_snapshot(inner).await?;
            return Err(error);
        }
    };

    let mut prepared = prepared;
    let joined_observation_text = prepared
        .segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let Some(thread_id) = inner.thread_id.read().await.clone() else {
        rebuffer_head(inner, drained).await;
        *inner.state.lock().await = HarnessState::PendingThreadStart;
        *inner.issued_turn_id.lock().await = None;
        persist_snapshot(inner).await?;
        return Ok(());
    };
    let drained_count = drained.len();
    let text = prepend_diff_block(diff.block.clone(), joined_observation_text);
    tracing::debug!(
        target: "calm_server::planner_harness_issue",
        runtime_id = %inner.worker_session_id,
        card_id = %inner.card_id,
        track_id = %inner.track_id,
        thread_id = %thread_id,
        drained_count,
        "calling daemon.turn_start"
    );

    // ── The model is resolved HERE, and the lateness is the whole point ────
    //
    // #1505 S4 review. This used to run off the `card_get` near the top of
    // this function, which is up to twenty-five seconds older than this line:
    // a transcript-refresh write transaction (bounded by
    // `TRANSCRIPT_REFRESH_TIMEOUT`, contending the single sqlite writer) and a
    // since-last-turn diff both sit in between. A person who changed the model
    // inside that window got a 200, saw the pill update, and then watched the
    // turn run under the model they had just replaced — with nothing on screen
    // saying so. Reading the row again here costs one primary-key lookup per
    // issued turn and closes it.
    //
    // The window that remains, stated exactly: from this read to the frame
    // leaving the process. For a card carrying an explicit slug that is the
    // few microseconds it takes to build the frame. For a card that follows
    // the default *and has chosen one before*, it also covers one
    // `config/read` (and, rarer still, one `model/list`) — a change landing
    // inside those RPCs is sent on the following turn rather than this one,
    // which is the same promise a change made mid-turn already carries.
    let selection = match resolve_model_selection_for_issue(inner).await {
        Ok(selection) => selection,
        Err(failure) => {
            // Not a wedge, whichever kind this is. That is a correctness
            // claim rather than a preference: `HarnessState::Wedged` has no
            // exit in this tree (`can_issue_turn` admits only
            // `Idle | TurnCompleted`, every assignment back to `Idle` is
            // guarded on a different phase, and a snapshot restore rehydrates
            // `Wedged` as `Wedged`). Wedging on a codex restart ended the
            // conversation permanently for a condition that resolves itself in
            // seconds, which was #1505 S4's first BLOCKER.
            //
            // But "not a wedge" is not the same as "retry and say nothing",
            // and treating it as such was the SECOND one. Only some of the
            // failures reachable here clear themselves; the rest need a person,
            // and retrying those in silence leaves a queued sentence rendering
            // as healthy forever. So the two are separated below rather than
            // both being answered with a timer.
            tracing::warn!(
                target: "calm_server::planner_harness_issue",
                worker_session_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                reason = %failure.log,
                kind = ?failure.kind,
                "not issuing this turn: the model to run it under is undetermined; will retry"
            );
            // The two arms differ in what waiting is worth, so they differ in
            // how long we wait and in whether the reader is told. A codex
            // restart is nobody's problem to act on; a config that names no
            // model is nothing BUT the reader's, and staying quiet about it is
            // how a queued sentence sits there looking healthy forever.
            apply_refusal(inner, &failure).await;
            #[cfg(feature = "fixtures")]
            inner.refused_issuances.fetch_add(1, Ordering::SeqCst);
            rebuffer_head(inner, drained).await;
            *inner.state.lock().await = prior_turn
                .map(|last_turn_id| HarnessState::TurnCompleted { last_turn_id })
                .unwrap_or(HarnessState::Idle);
            *inner.issued_turn_id.lock().await = None;
            persist_snapshot(inner).await?;
            return Ok(());
        }
    };
    *inner.issuance_retry_after.lock().await = None;

    // Text first, then one `localImage` per attachment, in queue order.
    //
    // Every path here is a string recorded at bind time and verified then;
    // this builds no path and touches no disk, which is the property that
    // makes a re-buffered batch safe — `rebuffer_head` below puts these same
    // entries back, and their attachments are exactly where they were.
    //
    // What is lost, and is worth naming: when several queued messages are
    // drained together their texts are joined into one string, so the payload
    // codex receives no longer says which image belonged to which sentence.
    // #1505 GAP-A3. The transcript is unaffected — `input_segments` keeps one
    // segment per entry, each with its own attachments.
    let mut items = vec![InputItem::text(text)];
    items.extend(
        drained
            .iter()
            .flat_map(QueueEntry::attachments)
            .map(|attachment| InputItem::local_image(attachment.path.clone())),
    );
    // The two failures this block can end in are told apart because they are
    // opposite facts about the batch: a projection row that could not be
    // written is a LOCAL failure before codex was asked anything, and a
    // refused `turn/start` is codex's answer. Both re-buffer (below), but the
    // log must not call the first "turn/start failed" — the operator reading
    // it would look at codex for a fault in sqlite.
    enum IssueFailure {
        ProjectionWrite(CalmError),
        TurnStart(CalmError),
    }
    let issued = async {
        if !prepared.actions.is_empty()
            && let Some(problem) = crate::semantic_recovery::binding_problem(
                &serde_json::to_string(&items)
                    .map_err(|e| IssueFailure::TurnStart(CalmError::from(e)))?,
                &prepared.actions,
            )
        {
            // Known cosmetic gap: a fragment/value mismatch here (our bug,
            // `CalmError::Internal`) lands in the `Err` arm below and is
            // worded as a codex turn/start refusal, not as the briefing
            // preparation failure it is; the `prepared` arm is unreachable
            // from here without hoisting this step above `items`. (With
            // #1625 P2's typed split it is `IssueFailure::TurnStart`, which
            // is that same wording — the gap is unchanged, not widened.)
            prepared
                .use_exact_interface(problem)
                .map_err(IssueFailure::TurnStart)?;
            items[0] = InputItem::text(prepend_diff_block(
                diff.block.clone(),
                prepared
                    .segments
                    .iter()
                    .map(|segment| segment.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ));
        }
        let issuance = if prepared.actions.is_empty() {
            None
        } else {
            Some(
                crate::semantic_recovery::prepare(
                    inner.repo.as_ref(),
                    &inner.worker_session_id,
                    inner.track_id.as_str(),
                    &thread_id,
                    &items,
                    std::mem::take(&mut prepared.actions),
                )
                .await
                .map_err(IssueFailure::TurnStart)?,
            )
        };
        // Written before `turn/start` goes out, and after the last edit to
        // `prepared.segments` above, so the row says what codex is told.
        write_projection_row(inner, &thread_id, client_id.as_str(), &prepared.segments)
            .await
            .map_err(IssueFailure::ProjectionWrite)?;
        let turn = IssueTurnHandle::from_reconciliation(inner)
            .issue(&thread_id, items, &selection, Some(client_id.as_str()))
            .await
            .map_err(IssueFailure::TurnStart)?;
        if let Some(issuance) = issuance {
            crate::semantic_recovery::bind_turn(inner.repo.as_ref(), &issuance, &turn)
                .await
                .map_err(IssueFailure::TurnStart)?;
        }
        Ok::<_, IssueFailure>(turn)
    }
    .await;
    match issued {
        Ok(turn_id) => {
            tracing::debug!(
                target: "calm_server::planner_harness_issue",
                runtime_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                track_id = %inner.track_id,
                thread_id = %thread_id,
                turn_id = %turn_id,
                "daemon.turn_start ok"
            );
            // A turn that went out ends the run of refusals, so the notice and
            // the clock behind it both go with it.
            *inner.issuance_block.lock().await = None;
            *inner.refusing_since.lock().await = None;
            *inner.last_turn_id.lock().await = Some(turn_id.clone());
            *inner.issued_turn_id.lock().await = Some(turn_id.clone());
            *inner.issued_turn_head.lock().await = diff.current_head.clone();
            // The batch is out and its row is keyed; the next batch decides
            // its own key. Cleared in the same snapshot that empties the
            // queue, so no restart can pair this key with a later batch.
            *inner.projection_client_id.lock().await = None;
            persist_issuance_outcome(inner).await?;
        }
        Err(failure) => {
            // #1505 S4 review round 2 — paced, like the refusal above.
            //
            // This arm is older than #1505 and was unpaced: `rebuffer_head`
            // arms `hard_fire`, so the next 50 ms tick re-issued, which is
            // roughly twenty `turn/start` RPCs and forty persist writes a
            // second for as long as codex kept refusing. That was tolerable
            // only while reaching it needed an operator. It does not any more:
            // `PUT /planner/model` stores a slug codex does not know BY DESIGN
            // ("a hint, not a refusal"), so picking one from the picker is now
            // a supported way for a person to make every `turn/start` fail.
            // Pacing it is part of shipping that picker, not a drive-by.
            //
            // Codex refusing this input and codex being unreachable are
            // opposite facts, and the reader is owed opposite sentences, so the
            // split is made on the TYPED error rather than on its text:
            // `CalmError::CodexRefused` exists only at the one place the
            // distinction is still known. The comment that stood here said the
            // two were indistinguishable from this arm — true of the error type
            // as it stood, and the fix was to change the type rather than to
            // match on a formatted string.
            //
            // It matters because `PUT /planner/model` stores a slug codex has
            // never heard of BY DESIGN, so an unaccepted model is a menu click
            // away, and calling that transient told the person their message
            // "will be sent when it answers" about a turn that will never go
            // out. Naming the real cause of a failed turn is still #1507's;
            // this only stops promising delivery that cannot happen.
            // Through the same classifier as `config/read` and `model/list`,
            // so "every codex call on this path goes through it" is a fact
            // rather than a wish — it was written as one while this site still
            // re-implemented the check inline. `Rejected` rather than
            // `NeedsAChoice`: no choice the reader can make is KNOWN to remove
            // the need for `turn/start`, so its sentence names no certain
            // remedy the way the other two do.
            let (e, stage) = match failure {
                IssueFailure::ProjectionWrite(e) => {
                    (e, "projection row write failed before turn/start was sent")
                }
                IssueFailure::TurnStart(e) => (e, "turn/start failed"),
            };
            let refusal =
                classify_codex_failure(&e, format!("{stage}: {e}"), IssuanceRefusal::rejected);
            apply_refusal(inner, &refusal).await;
            #[cfg(feature = "fixtures")]
            inner.refused_issuances.fetch_add(1, Ordering::SeqCst);
            // #1625 P2 — the batch goes back on the queue, so the row that
            // said it was sent goes too; the queue region lists it again.
            // A failed delete is logged, not propagated: the next drain of
            // this batch replaces the row (`write_projection_row`), under
            // the same key because `projection_client_id` is kept across the
            // re-buffer. (On a projection-write failure there may be no row
            // to delete, or a row whose event never went out; the delete is
            // right either way.)
            //
            // How a reader learns the row is gone: the delete emits no event
            // of its own. The phase change that follows the re-buffer —
            // `IssuingTurn → TurnCompleted`, written by
            // `persist_issuance_outcome` below — is the signal, and
            // `fe/core/events/invalidation-plan.ts` maps
            // `harness.phase.changed` to `['harness-items', card_id]` for
            // it. Legacy `web/` (`usePlannerChatHistory`, being deleted under
            // #1334) fetches only rows above its last id and never drops one,
            // so it keeps showing the deleted row until a reload; declared,
            // not fixed.
            if let Err(delete_error) = inner
                .repo
                .transcript_projection_delete(inner.card_id.as_str(), client_id.as_str())
                .await
            {
                tracing::warn!(
                    worker_session_id = %inner.worker_session_id,
                    card_id = %inner.card_id,
                    client_id = %client_id,
                    error = %delete_error,
                    "planner harness could not delete the projection of a re-buffered batch"
                );
            }
            rebuffer_head(inner, drained).await;
            *inner.state.lock().await = prior_turn
                .map(|last_turn_id| HarnessState::TurnCompleted { last_turn_id })
                .unwrap_or(HarnessState::TurnCompleted {
                    last_turn_id: "unknown-turn".into(),
                });
            *inner.issued_turn_id.lock().await = None;
            *inner.issued_turn_head.lock().await = None;
            persist_issuance_outcome(inner).await?;
            tracing::warn!(
                worker_session_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                stage,
                error = %e,
                "planner harness could not issue the batch; re-buffered it"
            );
        }
    }
    Ok(())
}

async fn transcript_refresh_with_timeout<F>(
    fut: F,
    timeout: Duration,
    runtime_id: &String,
    card_id: &str,
    track_id: &str,
) -> Option<track_vcs::CommitHash>
where
    F: std::future::Future<Output = Result<track_vcs::CommitHash>>,
{
    match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(head)) => Some(head),
        Ok(Err(e)) => {
            tracing::warn!(
                target: "calm_server::planner_harness_issue",
                runtime_id = %runtime_id,
                card_id,
                track_id,
                error = %e,
                "pre-diff transcript refresh failed; issuing turn without refreshed transcripts"
            );
            None
        }
        Err(_) => {
            tracing::warn!(
                target: "calm_server::planner_harness_issue",
                runtime_id = %runtime_id,
                card_id,
                track_id,
                timeout_secs = timeout.as_secs(),
                "pre-diff transcript refresh timed out; issuing turn without refreshed transcripts"
            );
            None
        }
    }
}

/// Wrap `since_last_turn_diff_block` in a 5s timeout. On timeout, log a warn
/// and fall through without a diff block so the turn still issues — the diff
/// block is contextual augmentation, never a correctness requirement (#639).
async fn diff_with_timeout(
    inner: &Arc<Inner>,
    current_override: Option<&track_vcs::CommitHash>,
) -> track_vcs::SinceLastTurnBlock {
    diff_or_fallback_on_timeout(
        since_last_turn_diff_block(inner, current_override),
        SINCE_LAST_TURN_DIFF_TIMEOUT,
        &inner.worker_session_id,
        inner.card_id.as_str(),
        inner.track_id.as_str(),
        || async {
            track_vcs::SinceLastTurnBlock {
                current_head: current_head_after_diff_timeout(inner, current_override).await,
                block: None,
            }
        },
    )
    .await
}

async fn diff_or_fallback_on_timeout<F, G, H>(
    fut: F,
    timeout: Duration,
    runtime_id: &String,
    card_id: &str,
    track_id: &str,
    fallback: H,
) -> track_vcs::SinceLastTurnBlock
where
    F: std::future::Future<Output = track_vcs::SinceLastTurnBlock>,
    G: std::future::Future<Output = track_vcs::SinceLastTurnBlock>,
    H: FnOnce() -> G,
{
    match tokio::time::timeout(timeout, fut).await {
        Ok(diff) => diff,
        Err(_) => {
            tracing::warn!(
                target: "calm_server::planner_harness_issue",
                runtime_id = %runtime_id,
                card_id,
                track_id,
                timeout_secs = timeout.as_secs(),
                "since-last-turn diff timed out; issuing turn without diff block"
            );
            fallback().await
        }
    }
}

async fn current_head_after_diff_timeout(
    inner: &Arc<Inner>,
    current_override: Option<&track_vcs::CommitHash>,
) -> Option<track_vcs::CommitHash> {
    if let Some(current) = current_override {
        return Some(current.clone());
    }

    let pool = inner.repo.sqlite_pool()?;

    match tokio::time::timeout(
        SINCE_LAST_TURN_HEAD_FALLBACK_TIMEOUT,
        track_vcs::head(&pool, &inner.track_id),
    )
    .await
    {
        Ok(Ok(head)) => head,
        Ok(Err(head_err)) => {
            tracing::warn!(
                target: "calm_server::planner_harness_issue",
                track_id = %inner.track_id,
                error = %head_err,
                "planner harness could not read track-vcs head after diff timeout"
            );
            None
        }
        Err(_) => {
            tracing::warn!(
                target: "calm_server::planner_harness_issue",
                track_id = %inner.track_id,
                timeout_secs = SINCE_LAST_TURN_HEAD_FALLBACK_TIMEOUT.as_secs(),
                "planner harness track-vcs head read timed out after diff timeout"
            );
            None
        }
    }
}

async fn since_last_turn_diff_block(
    inner: &Arc<Inner>,
    current_override: Option<&track_vcs::CommitHash>,
) -> track_vcs::SinceLastTurnBlock {
    let Some(pool) = inner.repo.sqlite_pool() else {
        return track_vcs::SinceLastTurnBlock::empty();
    };
    let last_seen_head = inner.last_seen_head.lock().await.clone();
    match track_vcs::since_last_turn_block(
        &pool,
        &inner.track_id,
        last_seen_head.as_deref(),
        current_override,
        Some(&inner.card_id),
    )
    .await
    {
        Ok(diff) => diff,
        Err(e) => {
            let current_head = match current_override {
                Some(current) => Some(current.clone()),
                None => match track_vcs::head(&pool, &inner.track_id).await {
                    Ok(head) => head,
                    Err(head_err) => {
                        tracing::warn!(
                            track_id = %inner.track_id,
                            error = %head_err,
                            "planner harness could not read track-vcs head after diff failure"
                        );
                        None
                    }
                },
            };
            tracing::warn!(
                track_id = %inner.track_id,
                card_id = %inner.card_id,
                last_seen_head = ?last_seen_head,
                current_head = ?current_head,
                error = %e,
                "planner harness track-vcs diff failed; issuing turn without diff block"
            );
            track_vcs::SinceLastTurnBlock {
                current_head,
                block: None,
            }
        }
    }
}

fn prepend_diff_block(diff_block: Option<String>, observation_text: String) -> String {
    match diff_block {
        Some(diff) => format!("{diff}\n\n---\n\n{observation_text}"),
        None => observation_text,
    }
}

async fn rebuffer_head(inner: &Arc<Inner>, drained: Vec<QueueEntry>) {
    let mut queue = inner.pending_queue.lock().await;
    // #1449 — a re-buffered batch keeps the ids it was drained with: it is the
    // same instances going back, not new ones. #1505 PR1 makes that free —
    // the ids ride inside the entry, so there is no second array a re-buffer
    // could put back in a different order.
    for entry in drained.into_iter().rev() {
        queue.push_front(entry);
    }
    let now = Instant::now();
    *inner.debounce.lock().await = DebounceState {
        first_pending_at: Some(now),
        last_pending_at: Some(now),
        hard_fire: true,
    };
}

async fn watchdog_tick(inner: &Arc<Inner>) -> Result<()> {
    let resume_elapsed = {
        let state = inner.state.lock().await;
        match &*state {
            HarnessState::Resumed { resumed_at } => {
                Instant::now().duration_since(*resumed_at) >= inner.config.resumed_reconcile_budget
            }
            _ => false,
        }
    };
    if resume_elapsed {
        let mut state = inner.state.lock().await;
        if let HarnessState::Resumed { resumed_at } = &*state
            && Instant::now().duration_since(*resumed_at) >= inner.config.resumed_reconcile_budget
        {
            *state = HarnessState::Idle;
            drop(state);
            persist_snapshot(inner).await?;
            return Ok(());
        }
    }

    if let Some((_, deadline)) = *inner.interrupt_deadline.lock().await
        && Instant::now() >= deadline
    {
        *inner.state.lock().await = HarnessState::Wedged {
            since: Instant::now(),
            reason: "interrupt_timeout".into(),
        };
        *inner.issued_turn_id.lock().await = None;
        *inner.interrupt_deadline.lock().await = None;
        persist_snapshot(inner).await?;
        return Ok(());
    }

    let should_interrupt = {
        let state = inner.state.lock().await;
        match &*state {
            HarnessState::TurnRunning {
                turn_id,
                started_at,
            } if Instant::now().duration_since(*started_at) >= inner.config.max_turn_duration => {
                Some(turn_id.clone())
            }
            _ => None,
        }
    };
    if let Some(turn_id) = should_interrupt {
        issue_interrupt_for_turn(inner, turn_id, "max_turn_duration".into()).await?;
    }
    Ok(())
}

async fn issue_interrupt(inner: &Arc<Inner>, reason: String) -> Result<()> {
    enum InterruptTarget {
        Known(String),
        ActiveThread,
    }

    let target = {
        let state = inner.state.lock().await;
        match &*state {
            HarnessState::TurnRunning { .. } => state.active_turn_id().map(InterruptTarget::Known),
            HarnessState::Issuing {
                kind: IssuingKind::TurnStart,
                ..
            } => Some(InterruptTarget::ActiveThread),
            _ => {
                tracing::debug!(
                    phase = ?*state,
                    "planner harness interrupt ignored because no turn is active"
                );
                None
            }
        }
    };
    let turn_id = match target {
        Some(InterruptTarget::Known(turn_id)) => Some(turn_id),
        Some(InterruptTarget::ActiveThread) => {
            let Some(thread_id) = inner.thread_id.read().await.clone() else {
                return Ok(());
            };
            inner.daemon.active_turn_id_for_thread(&thread_id)
        }
        None => None,
    };
    let Some(turn_id) = turn_id else {
        tracing::debug!("planner harness interrupt ignored because no active turn id is known");
        return Ok(());
    };
    issue_interrupt_for_turn(inner, turn_id, reason).await
}

async fn issue_interrupt_for_turn(
    inner: &Arc<Inner>,
    target_turn_id: String,
    reason: String,
) -> Result<()> {
    let Some(thread_id) = inner.thread_id.read().await.clone() else {
        return Ok(());
    };
    {
        let mut state = inner.state.lock().await;
        if matches!(*state, HarnessState::Wedged { .. }) {
            return Ok(());
        }
        *inner.issued_turn_id.lock().await = None;
        *state = HarnessState::Issuing {
            since: Instant::now(),
            kind: IssuingKind::Interrupt {
                target_turn_id: target_turn_id.clone(),
                reason: reason.clone(),
            },
        };
    }
    *inner.interrupt_deadline.lock().await = Some((
        target_turn_id.clone(),
        Instant::now() + inner.config.interrupt_completion_budget,
    ));
    persist_snapshot(inner).await?;
    if let Err(e) = inner
        .daemon
        .turn_interrupt(&thread_id, &target_turn_id)
        .await
    {
        tracing::warn!(
            thread_id,
            turn_id = %target_turn_id,
            reason,
            error = %e,
            "planner harness turn/interrupt failed; interrupt timeout watchdog remains armed"
        );
    }
    Ok(())
}

async fn snapshot_for(inner: &Arc<Inner>) -> HarnessSnapshot {
    let state = inner.state.lock().await.clone();
    let entries = inner.pending_queue.lock().await.iter().cloned().collect();
    let push_watermark = *inner.push_watermark.lock().await;
    let last_thread_id = inner.thread_id.read().await.clone();
    let last_turn_id = inner.last_turn_id.lock().await.clone();
    let issued_turn_head = inner.issued_turn_head.lock().await.clone();
    let projection_client_id = inner.projection_client_id.lock().await.clone();
    let last_report_body_sha256 = inner.last_report_body_sha256.lock().await.clone();
    let last_seen_head = inner.last_seen_head.lock().await.clone();
    let token_usage = inner.token_usage.lock().await.clone();
    let mut snapshot = HarnessSnapshot::from_state(
        &state,
        push_watermark,
        entries,
        last_thread_id,
        last_turn_id,
        last_report_body_sha256,
    );
    snapshot.last_seen_head = last_seen_head;
    snapshot.issued_turn_head = issued_turn_head;
    snapshot.projection_client_id = projection_client_id;
    snapshot.token_usage = token_usage;
    snapshot
}

/// #1449 — is this runtime still the row the card is being driven from?
///
/// A pool read of one row by id. NOT `write_in_tx_typed`, which opens with
/// `BEGIN IMMEDIATE` and takes SQLite's single writer lock; this runs on every
/// issuance attempt, and behind the writer lock it starved other writers
/// (`token_usage_round_trips_through_the_persisted_runtime_snapshot` went red
/// in a full-suite run while it was one). NOT `session_projection_by_id`
/// either: that SELECT is card-backed, so a row the card has moved off answers
/// `None`, and a `None` here refuses — which would refuse every runtime whose
/// card has moved on, live or not.
///
/// A missing row is refused. The rows are deleted by card, track and area
/// deletion, by a start's compensation, and by the dev replay reset; that list
/// comes from scanning every `DELETE FROM worker_sessions` in the tree and is
/// not ratcheted — `worker_sessions_row_disappearance.rs` freezes the FK
/// cascades and triggers, which is a different set.
async fn runtime_is_still_the_live_carrier(inner: &Arc<Inner>) -> Result<bool> {
    let state = inner
        .repo
        .session_projection_state_by_id(inner.worker_session_id.as_str())
        .await?;
    Ok(match state {
        Some(state) => state.is_active_authority(),
        None => false,
    })
}

/// #1449 — persist what the runtime owes after an issuance resolved, on a row
/// the ordinary writer may already refuse.
///
/// Two independent gates make the ordinary [`persist_snapshot`] a no-op exactly
/// when this write matters most, and BOTH are set by the re-point fence before
/// the run loop reaches this point:
///
/// * `persist_snapshot_inner` returns early once `shutting_down` is set, and
///   `shutdown_inner` sets that flag BEFORE it queues behind `inner.issuance`;
/// * `session_set_handle_state_tx` carries
///   `AND state IN ('starting','running','idle','turn_pending')`, and the
///   fence's transaction commits `superseded` before it touches the process.
///
/// So the last thing ever written about a fenced runtime is the pre-drain
/// snapshot — "the batch is still queued" — no matter what happened to the
/// batch. That was invisible while nothing read an abandoned snapshot; it is
/// the whole basis of the harvest now. `session_set_handle_state_of_retired_runtime_tx`
/// is the narrow exception: `handle_state_json` and `updated_at_ms`, retired
/// rows only.
///
/// It runs after the ordinary write, not instead of it — and only when that
/// write cannot have landed, so a live runtime does not open a second
/// transaction per turn to discover it matched nothing.
async fn persist_issuance_outcome(inner: &Arc<Inner>) -> Result<()> {
    persist_snapshot(inner).await?;
    // Only the runtimes that can actually need it open the second transaction.
    // For a live runtime the ordinary write above is the one that lands and
    // this one matches zero rows, so opening a write transaction to discover
    // that on every turn is pure contention on the single writer lock.
    if !inner.shutting_down.load(Ordering::SeqCst)
        && runtime_is_still_the_live_carrier(inner).await?
    {
        return Ok(());
    }
    let snapshot = snapshot_for(inner).await;
    let worker_session_id = inner.worker_session_id.clone();
    let snapshot_value = serde_json::to_value(snapshot)?;
    let now = crate::model::now_ms();
    let written = write_in_tx_typed(inner.repo.as_ref(), move |tx| {
        Box::pin(async move {
            crate::db::sqlite::session_set_handle_state_of_retired_runtime_tx(
                tx,
                &worker_session_id,
                Some(snapshot_value),
                now,
            )
            .await
            .map_err(CalmError::from)
        })
    })
    .await?;
    if !written {
        // Neither writer matched: the row flipped back into the active set
        // between the ordinary write and this one (`restore_old_runtime`), so
        // it still carries its PRE-drain queue. Once a restore clears its
        // marker that queue is harvestable again — the same sentence twice.
        // Logged rather than returned: this runs after the daemon already has
        // the batch, so failing here would undo nothing.
        tracing::warn!(
            target: "calm_server::planner_harness_issue",
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            "planner harness: neither handle-state writer matched after an issuance; the row \
             may still carry the pre-drain queue"
        );
    }
    Ok(())
}

/// #1625 P1 — make a turn's terminal status durable and readable.
///
/// One transcript row per finished turn, `method = "turn/completed"`,
/// `params` = codex's final `turn` object minus `items` / `itemsView` (the
/// items are already rows of their own; what is new here is `status`,
/// `error { message, codexErrorInfo }` and the timings). Written from the
/// `TurnCompleted` arm only, AFTER its non-target and stale-completion gates —
/// a completion the FSM ignores leaves no row — and BEFORE
/// `persist_snapshot_stamping_issued_head`, so by the time the resulting
/// `HarnessPhaseChanged` reaches a client the row is already there to fetch.
/// That ordering is what lets the phase event double as the delivery signal:
/// no item-added event is emitted for this row (one fewer track-vcs commit
/// per turn), and `fe/core/events/invalidation-plan.ts` invalidates
/// `['harness-items', card_id]` on `harness.phase.changed` instead.
///
/// `turn_id` is the id the arm accepted the completion under — the frame's
/// own `id`, or `last_turn_id` when the frame carries none — and not a
/// re-read of the frame: the arm's fallback is what lets an id-less
/// completion advance the FSM, and a row written under the same resolution
/// is the only row that names the turn the FSM actually finished. The id is
/// the row's whole identity (a later per-turn grouping keys on it), so it is
/// the row's `turn_id` column even when `params` has none.
///
/// Best-effort on purpose: a failed insert is logged, never propagated. The
/// FSM has already moved to `TurnCompleted` and the snapshot commit that
/// follows is what unblocks the next turn; a missing outcome line must not
/// stall the harness.
async fn persist_turn_outcome(inner: &Arc<Inner>, turn_id: &str, turn: &Value) {
    // Same guard as the `turn/plan/updated` arm: the transcript table's
    // `thread_id` column is NOT NULL, and `Notification::TurnCompleted.thread_id` is
    // `unwrap_or_default()` upstream, so the harness's own thread is the only
    // value that is never `""`.
    let Some(thread_id) = inner.thread_id.read().await.clone() else {
        tracing::warn!(
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            turn_id,
            "planner harness skipping turn/completed row: no thread is known yet"
        );
        return;
    };
    let mut outcome = turn.clone();
    if let Some(object) = outcome.as_object_mut() {
        object.remove("items");
        object.remove("itemsView");
    }
    let params_json = match serde_json::to_string(&outcome) {
        Ok(json) => json,
        Err(error) => {
            tracing::warn!(error = %error, turn_id, "planner harness could not serialize turn outcome");
            return;
        }
    };
    if let Err(error) = inner
        .repo
        .harness_item_insert(
            &inner.worker_session_id,
            inner.card_id.as_str(),
            inner.track_id.as_str(),
            &thread_id,
            Some(turn_id),
            // A turn is not an item: no `item_uuid`, no `item_type`.
            None,
            None,
            "turn/completed",
            &params_json,
            None,
        )
        .await
    {
        tracing::warn!(
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            turn_id,
            error = %error,
            "planner harness could not persist turn outcome row"
        );
    }
}

async fn persist_snapshot(inner: &Arc<Inner>) -> Result<()> {
    persist_snapshot_inner(inner, None).await.map(|_| ())
}

/// #1449 — persist a durable user send, and REFUSE it if the row was not
/// written.
///
/// `session_set_handle_state_tx` carries
/// `AND state IN ('starting','running','idle','turn_pending')`, so the write
/// matches nothing once the row leaves that set — and it used to report success
/// anyway. That is how a sentence got a 201, an `harness.user_message.enqueued`
/// row, and no durable home.
///
/// The write can miss for four reasons, and only one of them has a successor:
/// the row is `superseded` (a mint took over), `failed`/`exited`/`completed`,
/// the row was deleted, or `shutting_down` short-circuited the write. The
/// message therefore says "retry" without promising where it lands.
///
/// The `shutting_down` case is not reachable from HERE, and the argument is
/// specific: its only setter, `shutdown_inner`, takes `inner.durable_observation`
/// first, and `observe_durable_entries` holds that same lock across both
/// the send and its confirmation.
///
/// Writing through the retired-row writer instead would not help for the
/// `superseded` case: that row is stamped, so what landed on it would not be
/// read again.
async fn persist_snapshot_for_durable_send(inner: &Arc<Inner>) -> Result<()> {
    if persist_snapshot_inner(inner, None).await? {
        return Ok(());
    }
    Err(CalmError::PlannerHarnessRuntimeSuperseded(
        "this runtime is no longer the card's; your message was not stored — send it again".into(),
    ))
}

async fn persist_snapshot_stamping_issued_head(inner: &Arc<Inner>) -> Result<()> {
    let issued_head = inner.issued_turn_head.lock().await.clone();
    let _written = persist_snapshot_inner(inner, issued_head.clone()).await?;
    if issued_head.is_some() {
        *inner.last_seen_head.lock().await = issued_head;
    }
    *inner.issued_turn_head.lock().await = None;
    Ok(())
}

/// Returns whether the runtime's own row was written. `false` means the write
/// matched no row — the runtime is shutting down, or its row has left the
/// active set — and a caller that promised durability must not report success.
async fn persist_snapshot_inner(
    inner: &Arc<Inner>,
    last_seen_head_override: Option<track_vcs::CommitHash>,
) -> Result<bool> {
    if inner.shutting_down.load(Ordering::SeqCst) {
        return Ok(false);
    }
    // #1505 PR2b — the truncation's record goes in BEFORE the truncation does.
    // `?` and not a warn: a write that proceeded here would make a discarded
    // user message permanently gone with nothing saying so, and the caller
    // would report success for it. Refusing leaves the untruncated row as it
    // was — see `flush_dropped_announcements` for what that does and does not
    // guarantee about those entries being read again.
    flush_dropped_announcements(inner).await?;
    let mut snapshot = snapshot_for(inner).await;
    if let Some(head) = last_seen_head_override {
        snapshot.last_seen_head = Some(head);
        snapshot.issued_turn_head = None;
    }
    let runtime_id = inner.worker_session_id.clone();
    let thread_id = snapshot.last_thread_id.clone();
    let active_turn_id = match snapshot.phase {
        HarnessPhaseTag::TurnRunning | HarnessPhaseTag::IssuingInterrupt => {
            snapshot.last_turn_id.as_deref()
        }
        _ => None,
    }
    .map(ToOwned::to_owned);
    let state_for_status = inner.state.lock().await.clone();
    let status = run_status_for(&state_for_status);
    let new_phase = snapshot.phase;
    let event_runtime_id = runtime_id.clone();
    let event_card_id = inner.card_id.clone();
    let event_track_id = inner.track_id.clone();
    let snapshot_value = serde_json::to_value(snapshot)?;
    let repo = Arc::clone(&inner.repo);

    let written = write_in_tx_typed(repo.as_ref(), move |tx| {
        Box::pin(async move {
            let written = crate::db::sqlite::session_set_handle_state_tx(
                tx,
                &runtime_id,
                Some(snapshot_value),
            )
            .await?;
            crate::db::sqlite::session_set_harness_observation_runtime_tx(
                tx,
                &runtime_id,
                status,
                thread_id.as_deref(),
                active_turn_id.as_deref(),
            )
            .await?;
            Ok(written)
        })
    })
    .await?;

    let mut last_phase = inner.last_phase.lock().await;
    if *last_phase != new_phase {
        let old_phase = *last_phase;
        let scope = harness_event_scope(inner, "harness.phase.changed");
        if let Err(e) = inner
            .repo
            .log_pure_event(
                ActorId::Kernel,
                scope,
                None,
                &inner.events,
                &inner.card_role_cache,
                &inner.track_area_cache,
                Event::HarnessPhaseChanged {
                    worker_session_id: event_runtime_id,
                    card_id: event_card_id,
                    track_id: event_track_id,
                    old_phase,
                    new_phase,
                },
            )
            .await
        {
            tracing::warn!(
                runtime_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                track_id = %inner.track_id,
                ?old_phase,
                ?new_phase,
                error = %e,
                "planner harness phase event persist failed after snapshot commit; retaining previous phase for retry"
            );
            // The snapshot transaction above is already committed. Phase audit
            // is intentionally retryable/best-effort here: reporting failure
            // would make durable ingress roll back memory after its message was
            // durably accepted, allowing a later snapshot to erase it.
            return Ok(written);
        }
        *last_phase = new_phase;
    }
    Ok(written)
}

fn state_from_snapshot(snapshot: &HarnessSnapshot) -> HarnessState {
    let now = Instant::now();
    match snapshot.phase {
        HarnessPhaseTag::PendingThreadStart => HarnessState::PendingThreadStart,
        HarnessPhaseTag::Idle => HarnessState::Idle,
        HarnessPhaseTag::IssuingTurn => {
            if snapshot.last_turn_id.is_some() {
                HarnessState::Resumed { resumed_at: now }
            } else {
                HarnessState::TurnCompleted {
                    last_turn_id: String::new(),
                }
            }
        }
        HarnessPhaseTag::IssuingInterrupt | HarnessPhaseTag::TurnRunning => {
            HarnessState::Resumed { resumed_at: now }
        }
        HarnessPhaseTag::TurnCompleted => HarnessState::TurnCompleted {
            last_turn_id: snapshot
                .last_turn_id
                .clone()
                .unwrap_or_else(|| "unknown-turn".into()),
        },
        HarnessPhaseTag::Resumed => HarnessState::Resumed { resumed_at: now },
        HarnessPhaseTag::Wedged => HarnessState::Wedged {
            since: now,
            reason: snapshot
                .wedged_reason
                .clone()
                .unwrap_or_else(|| "wedged".into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HarnessObservationDelivery, harness_tick, map_observation_send_error,
        should_persist_item_method,
    };
    use crate::error::CalmError;
    use crate::harness::observation::Observation;
    use crate::harness::queue::QueueEntry;
    use axum::http::StatusCode;
    use tokio::sync::mpsc;

    fn delivery(text: &str) -> HarnessObservationDelivery {
        HarnessObservationDelivery {
            entry: QueueEntry::system(Observation::TrackGoal { text: text.into() }, None)
                .expect("a track goal is a system entry"),
        }
    }

    #[tokio::test]
    async fn harness_tick_skips_missed_ticks_instead_of_bursting() {
        assert_eq!(
            harness_tick().missed_tick_behavior(),
            tokio::time::MissedTickBehavior::Skip,
            "run loop tick must not burst-replay a backlog against the other select! branches"
        );
    }

    #[test]
    fn item_persistence_filter_keeps_terminal_items_and_drops_deltas() {
        assert!(should_persist_item_method("item/started"));
        assert!(should_persist_item_method("item/completed"));

        assert!(!should_persist_item_method("item/agentMessage/delta"));
        assert!(!should_persist_item_method("item/reasoning/delta"));
        assert!(!should_persist_item_method("turn/completed"));
        assert!(!should_persist_item_method("item/other"));
    }

    #[tokio::test]
    async fn observe_delivery_full_maps_to_service_unavailable() {
        let (tx, _rx) = mpsc::channel::<HarnessObservationDelivery>(1);
        tx.try_send(delivery("goal")).unwrap();

        let err = tx
            .try_send(delivery("next"))
            .map_err(map_observation_send_error)
            .unwrap_err();

        assert!(matches!(
            err,
            CalmError::ServiceUnavailable(ref msg) if msg.contains("queue full")
        ));
        assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn observe_delivery_closed_maps_to_conflict() {
        let (tx, rx) = mpsc::channel::<HarnessObservationDelivery>(4);
        drop(rx);

        let err = tx
            .try_send(delivery("x"))
            .map_err(map_observation_send_error)
            .unwrap_err();

        assert!(matches!(
            err,
            CalmError::Conflict(ref msg) if msg.contains("shutting down")
        ));
        assert_eq!(err.status(), StatusCode::CONFLICT);
    }

    #[tokio::test(start_paused = true)]
    async fn diff_or_fallback_on_timeout_returns_fallback_when_underlying_future_hangs() {
        use super::diff_or_fallback_on_timeout;
        use crate::track_vcs::SinceLastTurnBlock;
        use std::future::pending;
        use std::time::Duration;

        let runtime_id: String = "c501ea4e-test".into();

        let result = diff_or_fallback_on_timeout(
            pending::<SinceLastTurnBlock>(),
            Duration::from_secs(5),
            &runtime_id,
            "47e6ce46-test",
            "w-test",
            || async {
                SinceLastTurnBlock {
                    current_head: Some("head-after-timeout".into()),
                    block: None,
                }
            },
        )
        .await;

        assert!(
            result.block.is_none(),
            "timeout fallback must return an empty diff block"
        );
        assert_eq!(
            result.current_head.as_deref(),
            Some("head-after-timeout"),
            "timeout fallback must preserve the fallback current head"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn transcript_refresh_with_timeout_collapses_success_error_and_timeout() {
        use super::transcript_refresh_with_timeout;
        use crate::track_vcs::CommitHash;
        use std::future::pending;
        use std::time::Duration;

        let runtime_id: String = "c501ea4e-test".into();
        let card_id = "47e6ce46-test";
        let track_id = "w-test";
        let timeout = Duration::from_secs(5);

        let success = transcript_refresh_with_timeout(
            async { Ok("head-before-diff".into()) },
            timeout,
            &runtime_id,
            card_id,
            track_id,
        )
        .await;
        assert_eq!(success.as_deref(), Some("head-before-diff"));

        let failure = transcript_refresh_with_timeout(
            async { Err(CalmError::Conflict("refresh failed".into())) },
            timeout,
            &runtime_id,
            card_id,
            track_id,
        )
        .await;
        assert!(
            failure.is_none(),
            "refresh errors must degrade to live-HEAD diff"
        );

        let timed_out = transcript_refresh_with_timeout(
            pending::<crate::error::Result<CommitHash>>(),
            timeout,
            &runtime_id,
            card_id,
            track_id,
        )
        .await;
        assert!(
            timed_out.is_none(),
            "refresh timeouts must degrade to live-HEAD diff"
        );
    }

    #[test]
    fn user_message_fold_refuses_beyond_the_production_cap() {
        // The cap arithmetic itself lives in `harness::queue`; this keeps the
        // constant the run loop actually passes in under test.
        use crate::harness::queue::{FoldOutcome, try_fold_tail};
        use std::collections::VecDeque;

        let seed = "a".repeat(super::MAX_FOLDED_USER_MESSAGE_CHARS - 1);
        let mut queue = VecDeque::from(vec![QueueEntry::user_message(
            seed.clone(),
            Some(1),
            Vec::new(),
        )]);

        let outcome = try_fold_tail(
            &mut queue,
            &QueueEntry::user_message("x".repeat(10), Some(2), Vec::new()),
            super::MAX_FOLDED_USER_MESSAGE_CHARS,
        );

        assert_eq!(
            outcome,
            FoldOutcome::NotFolded,
            "fold must refuse when the result would exceed the cap"
        );
        let view = queue[0].user_view().expect("tail is still addressable");
        assert_eq!(view.text.chars().count(), seed.chars().count());
        assert_eq!(view.rev, 0, "a refused fold must not bump rev");
        assert_eq!(queue[0].envelope_id(), Some(1));
    }
}

#[cfg(test)]
mod completed_commit_tests;

#[cfg(test)]
mod recovery_briefing_tests;

#[cfg(test)]
mod result_receipt_tests;
