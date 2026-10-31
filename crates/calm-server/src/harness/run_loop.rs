use provider::control::{
    CorrelationKey, InputRequest, InterruptRequest, SteerOutcome, SteerRequest,
};
use std::collections::{HashSet, VecDeque};
#[cfg(feature = "fixtures")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use calm_types::harness::HARNESS_SYSTEM_ERROR_REASON;
use serde_json::Value;
#[cfg(feature = "fixtures")]
use std::collections::HashMap;
#[cfg(feature = "fixtures")]
use std::sync::OnceLock;
#[cfg(feature = "fixtures")]
use tokio::sync::Notify;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::card_role_cache::CardRoleCache;
use crate::db::{Repo, write_in_tx_typed};
use crate::error::{CalmError, Result};
use crate::event::{Event, EventBus, EventScope, HarnessQueueChange};
use crate::harness::backend::{BackendRewind, PlannerBackend, PlannerEvents, TurnStartFailure};
use crate::harness::config::HarnessConfig;
use crate::harness::held_requests::{self, HeldRequestReceiver, HeldRequestSender, HeldRequests};
use crate::harness::issuance::{IssuanceRefusal, SelectionSource};
use crate::harness::live_replies::{LiveReplyClaim, LiveReplyWriter};
use crate::harness::observation::Observation;
use crate::harness::queue::{
    FoldOutcome, MutationApplied, MutationRefused, MutationResult, QueueEntry, QueueEntryId,
    QueueMutation, apply_mutation, input_segments_for_entries, locate_entry,
    try_fold_report_edit_tail, try_fold_tail,
};
use crate::harness::snapshot::{HarnessPhaseTag, HarnessSnapshot, IssuedInputSegments, TurnBase};
use crate::harness::state::{HarnessState, IssuingKind, RunningTurn, run_status_for};
use crate::harness::token_usage::{TokenUsage, TokenUsageExt};
use crate::ids::{ActorId, CardId, TrackId};
use crate::model::{HarnessInputPresentation, HarnessInputSegment};
use crate::planner_attachments::bind::BoundAttachment;
use crate::planner_model::FailureKind;
#[cfg(test)]
use crate::planner_permission_mode::PlannerPermissionMode;
use crate::thread_seals::{DeletionThreadSeals, ThreadSeals};
use crate::track_area_cache::TrackAreaCache;
use crate::track_vcs;
use provider::InputItem;
#[cfg(test)]
use provider::TurnModelSelection;
use provider::events::{ItemPhase, PlannerEvent, PlannerEventKind};

mod canonical;
#[cfg(test)]
pub(in crate::harness) mod compact_task_tests;
mod content_observer;
mod input_contract;
mod self_compact;
#[cfg(test)]
mod self_compact_tests;
pub use input_contract::HarnessRuntimeSnapshot;
use input_contract::{LoadedRuntimeSnapshot, RuntimeInputs, RuntimeSnapshotKind};

/// Fixtures-only: park a runtime immediately before it turns its pending queue into a turn.
/// Parked one statement before `inner.issuance` is taken, because `shutdown_inner` takes that
/// same lock and a hook parked under it would deadlock the `PATCH` a test orders against.
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

/// Arm the hook for whichever runtime reaches the drain next; the runtime under test cannot be
/// named before `POST /api/tracks` mints it.
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

async fn wait_at_planner_harness_drain_race_hook(inner: &Arc<Inner>) {
    #[cfg(feature = "fixtures")]
    {
        let hook = {
            let mut hooks = planner_harness_drain_race_hooks()
                .lock()
                .expect("planner harness drain hook mutex");
            hooks
                .remove(&inner.worker_session_id)
                .or_else(|| hooks.remove(ANY_RUNTIME))
        };
        if let Some(hook) = hook {
            let mut shutdown = inner.shutdown.subscribe();
            hook.entered.notify_one();
            if !inner.shutting_down.load(Ordering::SeqCst) {
                tokio::select! {
                    _ = hook.release.notified() => {},
                    _ = shutdown.recv() => {},
                }
            }
        }
    }
    #[cfg(not(feature = "fixtures"))]
    let _ = inner;
}

/// Pause one ordinary delivery after its replay watermark advances, before its
/// queue entry exists. Recovery must let the run loop finish both changes.
#[cfg(feature = "fixtures")]
#[derive(Clone)]
pub struct PlannerHarnessObservationRaceHook {
    pub entered: Arc<Notify>,
    pub release: Arc<Notify>,
}

#[cfg(feature = "fixtures")]
fn planner_harness_observation_race_hooks()
-> &'static StdMutex<HashMap<String, PlannerHarnessObservationRaceHook>> {
    static HOOKS: OnceLock<StdMutex<HashMap<String, PlannerHarnessObservationRaceHook>>> =
        OnceLock::new();
    HOOKS.get_or_init(|| StdMutex::new(HashMap::new()))
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn install_planner_harness_observation_race_hook_for_test(
    worker_session_id: &str,
    hook: PlannerHarnessObservationRaceHook,
) {
    planner_harness_observation_race_hooks()
        .lock()
        .expect("planner harness observation hook mutex")
        .insert(worker_session_id.to_owned(), hook);
}

#[cfg(feature = "fixtures")]
async fn wait_at_planner_harness_observation_race_hook(worker_session_id: &str) {
    let hook = planner_harness_observation_race_hooks()
        .lock()
        .expect("planner harness observation hook mutex")
        .remove(worker_session_id);
    if let Some(hook) = hook {
        hook.entered.notify_one();
        hook.release.notified().await;
    }
}

const OBSERVATION_BUFFER: usize = 256;
/// Hard cap on queued observations. Wire-visible: at this length a user message folds into
/// the tail instead of taking a slot (`entry_id: null`).
pub const MAX_PENDING_QUEUE_LEN: usize = 256;
const RECENT_HOOK_KEY_CACHE_LEN: usize = 256;
/// Upper bound on a folded `UserMessage` tail entry; beyond it the fold is refused and the
/// eviction fallback in `enqueue_pending_observation` runs.
const MAX_FOLDED_USER_MESSAGE_CHARS: usize = 4 * 32_768;

#[derive(Clone)]
pub struct PlannerHarness {
    inner: Arc<Inner>,
}

pub struct PlannerHarnessParams {
    /// The same track fence used by operation launch and worktree clean.
    pub track_delete_locks: crate::per_card_lock::KeyedLocks,
    pub worker_session_id: String,
    pub track_id: TrackId,
    pub card_id: CardId,
    pub thread_id: Option<String>,
    pub repo: Arc<dyn Repo>,
    pub events: EventBus,
    pub card_role_cache: CardRoleCache,
    pub track_area_cache: TrackAreaCache,
    pub backend: PlannerBackend,
    /// The actual installation registry owns both replies and worker input sources.
    pub registry: crate::harness::HarnessRegistry,
    pub config: HarnessConfig,
    pub snapshot: HarnessRuntimeSnapshot,
}

mod objective_usage;

pub(super) struct Inner {
    objective_usage: Mutex<objective_usage::TurnMeter>,
    track_delete_locks: crate::per_card_lock::KeyedLocks,
    pub(super) worker_session_id: String,
    track_id: TrackId,
    card_id: CardId,
    pub(super) thread_id: RwLock<Option<String>>,
    repo: Arc<dyn Repo>,
    events: EventBus,
    card_role_cache: CardRoleCache,
    track_area_cache: TrackAreaCache,
    pub(super) backend: PlannerBackend,
    /// Installed with the harness; the run loop holds its writer.
    live_claim: LiveReplyClaim,
    observations: ObservationIngress,
    state: Mutex<HarnessState>,
    last_phase: Mutex<HarnessPhaseTag>,
    last_context_feedback: Mutex<(
        Option<crate::harness::compact_outcome::CompactOutcome>,
        bool,
        Option<canonical::PublicFeedback>,
    )>,
    inputs: RuntimeInputs,
    recent_hook_keys: Mutex<VecDeque<String>>,
    recent_hook_key_set: Mutex<HashSet<String>>,
    push_watermark: Mutex<i64>,
    last_turn_id: Mutex<Option<String>>,
    issued_turn_id: Mutex<Option<String>>,
    issued_turn_head: Mutex<Option<track_vcs::CommitHash>>,
    /// See `HarnessSnapshot::projection_client_id`. Live copy of the slot;
    /// `maybe_issue_turn` is its only writer.
    projection_client_id: Mutex<Option<QueueEntryId>>,
    /// Segments of the turn an older binary left in flight, read from its snapshot at boot and
    /// never written back.
    legacy_issued_input_segments: Mutex<Option<IssuedInputSegments>>,
    last_report_body_sha256: Mutex<Option<String>>,
    last_seen_head: Mutex<Option<track_vcs::CommitHash>>,
    /// Latest context-window reading from `thread/tokenUsage/updated`; latest-wins.
    token_usage: Mutex<Option<TokenUsage>>,
    compact_outcome: Mutex<Option<crate::harness::compact_outcome::CompactOutcome>>,
    planner_compaction: Mutex<Option<crate::harness::compact_outcome::PlannerCompaction>>,
    compaction_retry_after: Mutex<Option<Instant>>,
    maintenance_turns: Mutex<HashSet<String>>,
    context_usage_invalidation: Mutex<Option<compact_command::UsageInvalidation>>,
    /// See `HarnessSnapshot::pending_rewind`; `maybe_issue_turn` hands it to the next `turn/start`
    /// and clears it once that returns `Ok`.
    pending_rewind: Mutex<Option<BackendRewind>>,
    /// See `HarnessSnapshot::last_turn_base`; written when a turn goes out, read by a rewind.
    last_turn_base: Mutex<Option<TurnBase>>,
    /// Turns a rewind removed: a late `item/*` or plan frame for one writes no row. Live-only. An
    /// outcome needs none: every completion arm requires the running turn, the interrupt target or
    /// `last_turn_id`, and a rewind leaves none of them naming the removed turn.
    rewound_turns: Mutex<HashSet<String>>,
    debounce: Mutex<DebounceState>,
    interrupt_deadline: Mutex<Option<(String, Instant)>>,
    /// Durable causal evidence outlives an interrupt RPC and the recovery phase.
    /// Clear it only after its outcome is stored or a new batch supersedes the turn.
    interruption_intent: Mutex<Option<calm_types::harness::HarnessInterruptionIntent>>,
    /// Do not re-attempt turn issuance before this instant. Without it a re-buffered batch re-arms
    /// `hard_fire` and the loop spins at twenty write transactions a second while codex is unreachable.
    issuance_retry_after: Mutex<Option<Instant>>,
    /// What to tell the reader about why their message has not been sent, or `None`. Live-only,
    /// like `phase`: re-derived by the next attempt, deliberately not on `HarnessSnapshot`.
    issuance_block: Mutex<Option<String>>,
    /// When the current run of consecutive refusals began; feeds `transient_silence_budget`.
    refusing_since: Mutex<Option<Instant>>,
    /// How many issuance attempts have been refused, so a test can assert the retry is PACED
    /// without waiting on a wall clock. Bumped only once the batch is back on the queue, so a
    /// test that sees the count also sees the re-buffered entry.
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
    /// then interrupts the now-known turn after the loop has stopped.
    issuance: Mutex<()>,
    parked: crate::harness::carrier_park::ParkState,
    /// Issuance kill-switch for dev-forced harnesses; only `pause_issuance_for_dev` sets it.
    issuance_paused: AtomicBool,
    provider_held: AtomicBool,
    provider_hold_owns_block: AtomicBool,
    /// Queue entries the load-time truncation discarded whose `dropped` row does not exist yet.
    /// A `tokio::Mutex` held across the event inserts so two racing flushers cannot take the same id;
    /// `persist_snapshot_inner` drains this first and refuses to write if it cannot.
    unannounced_drops: Mutex<Vec<QueueEntryId>>,
    /// Entries codex accepted through `turn/steer` into the running turn, held until that turn
    /// completes. Live-only: a restart loses the turn with it.
    steered_into_running_turn: Mutex<Vec<SteeredEntry>>,
    /// Requeue transfer still owed; cleared only with an atomic snapshot/projection commit.
    restored_projections: Mutex<Vec<QueueEntryId>>,
    input_commit: Mutex<Option<input_commit::Continuation>>,
    /// The provider requests the running turn is paused on (#2348); see `run_loop::held`.
    held_requests: Arc<HeldRequests>,
    /// The sending half of the one channel the run loop reads the table's input from.
    held_request_sender: HeldRequestSender,
    /// The last watchdog tick that saw the table hold a request; `None` while it holds none.
    watchdog_held_since: Mutex<Option<Instant>>,
    watchdog_held_time: std::sync::Mutex<Duration>,
    // Retain the task across cancelled shutdown waits. A closed ingress is not proof
    // that an in-flight notification finished registering its input transfer.
    run_loop_task: Mutex<Option<JoinHandle<()>>>,
    turn_budget: Mutex<budget::FrozenBudget>,
    config: HarnessConfig,
}

/// One entry codex accepted into the running turn. Codex records a steered input only at its
/// next model request, and an interrupt before that clears it; the completion sweep tells the
/// two apart by whether the transcript row is still a projection.
struct SteeredEntry {
    /// The turn `turn/steer` named as `expectedTurnId` and codex confirmed.
    turn_id: String,
    entry: QueueEntry,
    /// The transcript projection written once codex accepted, or `None` if that write failed;
    /// without a row the sweep cannot tell a delivery from a drop and leaves the entry alone.
    row_id: Option<i64>,
}

impl Inner {
    // Early refusal only; callers retain authoritative owner/current-carrier checks.
    fn require_unparked(&self) -> Result<()> {
        if self.parked.is_parked() {
            return Err(CalmError::Conflict("carrier is parked".into()));
        }
        Ok(())
    }

    /// Sample only when the bound Codex stop needs its fallback, under the caller's issuance lock.
    pub(super) async fn shutdown_last_turn(&self) -> Option<String> {
        self.last_turn_id.lock().await.clone()
    }

    fn historical_queue(&self) -> Result<&Mutex<VecDeque<QueueEntry>>> {
        self.inputs.historical_queue()
    }
}

use super::backend::IssueTurnHandle;

#[derive(Clone, Debug)]
pub struct HarnessObservationDelivery {
    pub entry: QueueEntry,
}

#[derive(Clone, Copy)]
enum OwnerRelease {
    Shutdown,
    SystemError,
}

enum HarnessObservationCommand {
    CanonicalInterrupt {
        target: crate::harness::backend::ExecutionEndTarget,
        source: crate::auth::HumanInputSource,
        answer: oneshot::Sender<Result<()>>,
    },
    CanonicalSource {
        input: calm_types::conversation::CanonicalPendingIdentity,
        source: crate::conversation_inputs::InputSource,
        observed: oneshot::Sender<Result<()>>,
    },
    InspectInput {
        entry_id: QueueEntryId,
        answer: oneshot::Sender<Result<input_inspection::InputInspection>>,
    },
    Hibernate {
        done: oneshot::Sender<Result<bool>>,
    },
    /// Recovery must snapshot on the loop that owns both queue and watermark,
    /// after earlier deliveries/notifications settle and before stopping it.
    Release {
        kind: OwnerRelease,
        done: oneshot::Sender<Result<()>>,
    },
    RequestCompaction {
        request_id: String,
        answer: oneshot::Sender<Result<crate::harness::compact_outcome::CompactionAnswer>>,
    },
    Compact {
        source: compact_command::Source,
        answer: oneshot::Sender<Result<()>>,
    },
    Delivery(HarnessObservationDelivery),
    Durable {
        deliveries: Vec<HarnessObservationDelivery>,
        key: SendKey,
        source: Option<crate::conversation_inputs::HistoricalHumanSource>,
        persisted: oneshot::Sender<Result<DurableAck>>,
    },
    /// A human edit or delete against one queue entry. Rides the same mpsc as every other command,
    /// so it never runs part-way through a tick; the delete-vs-drain atomicity itself comes from
    /// `queue::apply_mutation` doing its whole compare-and-swap under one queue-lock hold.
    Mutate {
        mutation: QueueMutation,
        actor: ActorId,
        applied: oneshot::Sender<Result<MutationResult>>,
    },
    /// A human asking for one queued entry to go into the running turn. Take-before-ask under the
    /// queue lock rules out double delivery; awaiting `turn/steer` inside this arm orders the steer
    /// with the completion, so the `TurnCompleted` sweep always sees an accepted entry registered.
    Steer {
        entry_id: QueueEntryId,
        if_entry_rev: u32,
        actor: ActorId,
        applied: oneshot::Sender<Result<SteerResult>>,
    },
    /// A human replacing the conversation's latest turn with one message (#1923, #2043): the turn's
    /// removal, the message and the send's key commit in one transaction. Served between ticks and
    /// notifications, under the issuance lock, so no turn starts and no row lands while it runs.
    Replace {
        turn_id: String,
        delivery: HarnessObservationDelivery,
        key: SendKey,
        source: Option<crate::conversation_inputs::HistoricalHumanSource>,
        answer: oneshot::Sender<Result<DurableAck>>,
    },
}

/// A steer that took effect: codex has the entry inside `turn_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteerApplied {
    pub entry_id: QueueEntryId,
    /// The rev the entry carried when it left the queue.
    pub rev: u32,
    /// The turn codex put the message into — the one that was running.
    pub turn_id: String,
}

/// Why a steer delivered nothing. In every arm the entry is still in the queue with the id
/// and rev the client read.
#[derive(Debug, Clone, PartialEq)]
pub enum SteerRefused {
    /// The queue's own answer — not there, not the rev you read, or not uniquely named.
    Queue(MutationRefused),
    /// No turn is running at this moment: nothing was taken out of the queue
    /// and codex was not asked.
    NoRunningTurn { phase: HarnessPhaseTag },
    /// Codex was asked and said no. The entry is back at the head of the queue; no transcript row
    /// was written. `phase` is still `TurnRunning` until the completion reaches this loop.
    NotTaken {
        message: String,
        phase: HarnessPhaseTag,
    },
    /// Codex was asked and did not answer, so whether it took the message is NOT known. The entry
    /// goes with the next turn — if codex did take it the sentence reaches the model twice.
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

async fn checkpoint_durable_user_message(inner: &Inner) -> Result<DurableUserMessageCheckpoint> {
    Ok(DurableUserMessageCheckpoint {
        pending_queue: inner.historical_queue()?.lock().await.clone(),
        debounce: *inner.debounce.lock().await,
    })
}

async fn restore_durable_user_message(
    inner: &Inner,
    checkpoint: DurableUserMessageCheckpoint,
) -> Result<()> {
    *inner.historical_queue()?.lock().await = checkpoint.pending_queue;
    *inner.debounce.lock().await = checkpoint.debounce;
    Ok(())
}

/// Offer a durable send's entries to the in-memory queue, behind a checkpoint the caller restores
/// if storing them then fails. A rejected entry restores it here and is a 503. The ack names the
/// last user-authored entry, as [`DurableAck`] says.
async fn stage_durable_entries(
    inner: &Arc<Inner>,
    deliveries: Vec<HarnessObservationDelivery>,
) -> Result<(DurableAck, DurableUserMessageCheckpoint)> {
    let checkpoint = checkpoint_durable_user_message(inner).await?;
    let mut ack = DurableAck { entry_id: None };
    for delivery in deliveries {
        let user_authored = delivery.entry.is_user_authored();
        match on_observation(inner, delivery.entry).await {
            EnqueueOutcome::Accepted { entry_id } => {
                if user_authored {
                    ack.entry_id = entry_id;
                }
            }
            EnqueueOutcome::Rejected => {
                restore_durable_user_message(inner, checkpoint).await?;
                return Err(CalmError::ServiceUnavailable(
                    "planner harness pending queue full, retry shortly".into(),
                ));
            }
            EnqueueOutcome::Unsupported => {
                return Err(CalmError::Conflict(
                    "canonical input must be accepted by its conversation owner".into(),
                ));
            }
        }
    }
    Ok((ack, checkpoint))
}

/// What a durable enqueue tells the caller. `entry_id` is `None` in exactly one accepted case —
/// the message folded into a `LegacyUser` tail. The LAST user-authored delivery wins, `None` included.
#[derive(Debug, Clone, PartialEq)]
pub struct DurableAck {
    pub entry_id: Option<QueueEntryId>,
}

/// The `Idempotency-Key` a durable send is accepted under (#2043). Its binding commits in the
/// snapshot transaction that stores the message, and only when that transaction stored it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendKey {
    pub idempotency_key: String,
    pub payload_hash: String,
}

#[cfg(feature = "fixtures")]
impl SendKey {
    /// A key no other send uses, for tests that drive the harness below the route.
    pub fn unique_for_test() -> Self {
        Self {
            idempotency_key: crate::model::new_id(),
            payload_hash: "test".into(),
        }
    }
}

/// Result of offering one entry to the pending queue.
enum EnqueueOutcome {
    Unsupported,
    /// The queue was full of hard-fire entries and nothing could be evicted.
    Rejected,
    Accepted {
        entry_id: Option<QueueEntryId>,
    },
}

impl PlannerHarness {
    pub(crate) fn card_id(&self) -> &CardId {
        &self.inner.card_id
    }

    pub(crate) async fn steer_canonical(
        &self,
        source: crate::auth::HumanInputSource,
        message: calm_types::conversation::MessageRevisionRef,
        turn: String,
    ) -> Result<calm_types::conversation_controls::QueuedMessageSteerResult> {
        // Once the request reaches this owner, an HTTP disconnect must not drop
        // the native ticket or its durable report. The exact source still gates every write.
        let owner = self.clone();
        tokio::spawn(async move { owner.steer_canonical_owned(source, message, turn).await })
            .await
            .map_err(|_| CalmError::Internal("supplemental input owner failed".into()))?
    }

    async fn steer_canonical_owned(
        &self,
        source: crate::auth::HumanInputSource,
        message: calm_types::conversation::MessageRevisionRef,
        turn: String,
    ) -> Result<calm_types::conversation_controls::QueuedMessageSteerResult> {
        // Share the existing issuance order with controls/END; never hold SQL across transport.
        let _issuance = self.inner.issuance.lock().await;
        self.inner.require_unparked()?;
        if self.inner.shutting_down.load(Ordering::SeqCst) {
            return Err(CalmError::Conflict("runtime is shutting down".into()));
        }
        let outcome = crate::harness::input_dispatch::supplemental(
            self.inner.repo.as_ref(),
            source,
            self.inner.card_id.as_str(),
            &self.inner.worker_session_id,
            message,
            turn,
            &self.inner.backend,
        )
        .await;
        // END may have arrived before this report. Its durable commit (including
        // uncertain/error outcomes) must wake the owner even after HTTP disconnect.
        self.request_progress();
        let (result, consumed) = outcome?;
        self.inner
            .inputs
            .canonical()
            .ok_or_else(|| CalmError::Conflict("not a canonical runtime".into()))?
            .consumed(&consumed);
        Ok(result)
    }

    /// Refresh from committed Truth after a same-phase Human resolution.
    pub(crate) async fn refresh_canonical_input(&self) -> Result<()> {
        if self.inner.inputs.canonical().is_some() {
            // The caller already committed a same-phase Human mutation. Wake
            // before this advisory refresh, which may fail independently.
            self.request_progress();
            let frame = canonical::read_current(&self.inner).await?;
            canonical::publish(&self.inner, frame).await;
        }
        Ok(())
    }

    /// Supply only the live source of an already committed exact revision.
    /// Acceptance is owned by the conversation transaction, independently of wake.
    pub(crate) async fn observe_canonical_source(
        &self,
        input: impl Into<calm_types::conversation::CanonicalPendingIdentity>,
        source: impl Into<crate::conversation_inputs::InputSource>,
    ) -> Result<()> {
        let input = input.into();
        let source = source.into();
        if self.inner.inputs.canonical().is_none() {
            return Err(CalmError::Conflict(
                "historical runtime cannot consume canonical input".into(),
            ));
        }
        let (observed, answer) = oneshot::channel();
        let sender = match &self.inner.observations {
            ObservationIngress::Running(sender) => sender,
            #[cfg(feature = "fixtures")]
            ObservationIngress::Unstarted(_) => {
                return Err(CalmError::Conflict(
                    "canonical runtime is not running".into(),
                ));
            }
        };
        sender
            .send(HarnessObservationCommand::CanonicalSource {
                input,
                source,
                observed,
            })
            .await
            .map_err(|_| CalmError::Conflict("canonical runtime is closed".into()))?;
        answer
            .await
            .map_err(|_| CalmError::Conflict("canonical runtime is closed".into()))?
    }
    pub fn track_id(&self) -> &TrackId {
        &self.inner.track_id
    }

    pub(crate) fn worker_session_id(&self) -> &str {
        &self.inner.worker_session_id
    }

    /// The provider requests this harness's running turn is paused on (#2348).
    pub fn held_requests(&self) -> &HeldRequests {
        &self.inner.held_requests
    }

    /// Where a provider adapter reports the requests it pauses on; the run loop reads them in
    /// order (#2348).
    pub fn held_request_sender(&self) -> HeldRequestSender {
        self.inner.held_request_sender.clone()
    }

    pub async fn run(params: PlannerHarnessParams) -> Result<Self> {
        let loaded = match &params.snapshot.kind {
            RuntimeSnapshotKind::Historical(snapshot) => {
                LoadedRuntimeSnapshot::Historical((**snapshot).clone())
            }
            RuntimeSnapshotKind::Canonical => {
                LoadedRuntimeSnapshot::Canonical(canonical::load_current(&params).await?)
            }
        };
        let (obs_tx, obs_rx) = mpsc::channel(OBSERVATION_BUFFER);
        let (shutdown_tx, shutdown_rx) = broadcast::channel(4);
        let events = params.backend.subscribe_events();
        let (held_tx, held_rx) = held_requests::channel();
        let (inner, announce_dropped_first) = inner_from_params(
            params,
            loaded,
            ObservationIngress::Running(obs_tx),
            shutdown_tx,
            held_tx,
        );
        let live = inner.live_claim.writer();
        let handle = Self {
            inner: Arc::clone(&inner),
        };
        let task = tokio::spawn(run_loop(
            inner,
            obs_rx,
            held_rx,
            shutdown_rx,
            events,
            live,
            announce_dropped_first,
        ));
        *handle
            .inner
            .run_loop_task
            .try_lock()
            .expect("new run loop task") = Some(task);
        Ok(handle)
    }

    #[cfg(feature = "fixtures")]
    pub fn run_unstarted_for_test(
        params: PlannerHarnessParams,
        observation_buffer: usize,
    ) -> (Self, mpsc::Receiver<HarnessObservationDelivery>) {
        let RuntimeSnapshotKind::Historical(snapshot) = &params.snapshot.kind else {
            panic!("the unstarted historical fixture cannot stand in for a canonical runtime");
        };
        let loaded = LoadedRuntimeSnapshot::Historical((**snapshot).clone());
        let (obs_tx, obs_rx) = mpsc::channel(observation_buffer);
        let (shutdown_tx, _shutdown_rx) = broadcast::channel(4);
        // No run loop on this path to flush the drop announcements early; the `persist_snapshot_inner`
        // drain still covers them. Nothing reads the held-request channel either.
        let (held_tx, _held_rx) = held_requests::channel();
        let (inner, _announce_dropped_first) = inner_from_params(
            params,
            loaded,
            ObservationIngress::Unstarted(obs_tx),
            shutdown_tx,
            held_tx,
        );
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
        self.inner.historical_queue()?;
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

    /// Fold and persist non-replayable user intent before acknowledging it. The ack names the
    /// entry the text ended up in — under backpressure, the fold survivor.
    pub async fn observe_user_message_durable(
        &self,
        text: String,
        attachments: Vec<BoundAttachment>,
        key: SendKey,
    ) -> Result<DurableAck> {
        self.observe_user_message_admitted(text, attachments, key, None)
            .await
    }

    pub(crate) async fn observe_user_message_admitted(
        &self,
        text: String,
        attachments: Vec<BoundAttachment>,
        key: SendKey,
        source: Option<crate::conversation_inputs::HistoricalHumanSource>,
    ) -> Result<DurableAck> {
        self.observe_durable_entries(
            vec![QueueEntry::user_message(text, None, attachments)],
            key,
            source,
        )
        .await
    }

    async fn observe_durable_entries(
        &self,
        entries: Vec<QueueEntry>,
        key: SendKey,
        source: Option<crate::conversation_inputs::HistoricalHumanSource>,
    ) -> Result<DurableAck> {
        self.inner.historical_queue()?;
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
                        key,
                        source,
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
                durable_command::handle_durable(&self.inner, deliveries, &key, source).await
            }
        }
    }

    /// Edit or delete one queue entry from the REST write port. Outer `Result` is transport; inner
    /// [`MutationResult`] is the queue's own answer.
    pub async fn mutate_pending_entry(
        &self,
        mutation: QueueMutation,
        actor: ActorId,
    ) -> Result<MutationResult> {
        self.inner.historical_queue()?;
        // Deliberately NOT taking `durable_observation`.
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

    /// Send one queued entry into the running turn from the REST write port. Every inner refusal
    /// leaves the entry queued under the id and rev the client read.
    pub async fn steer_pending_entry(
        &self,
        entry_id: QueueEntryId,
        if_entry_rev: u32,
        actor: ActorId,
    ) -> Result<SteerResult> {
        self.inner.historical_queue()?;
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

    pub(crate) async fn compact_from_request(
        &self,
        source: Option<crate::auth::HumanInputSource>,
    ) -> Result<()> {
        match &self.inner.inputs {
            RuntimeInputs::Canonical(_) => {
                self.compact_canonical(source.ok_or(CalmError::Unauthorized)?)
                    .await
            }
            RuntimeInputs::Historical(_) => self.compact().await,
        }
    }

    pub(crate) async fn compact_canonical(
        &self,
        source: crate::auth::HumanInputSource,
    ) -> Result<()> {
        self.submit_compact(compact_command::Source::Canonical(source))
            .await
    }

    pub async fn compact(&self) -> Result<()> {
        self.inner.historical_queue()?;
        self.submit_compact(compact_command::Source::Historical)
            .await
    }

    async fn submit_compact(&self, source: compact_command::Source) -> Result<()> {
        if self.inner.shutting_down.load(Ordering::SeqCst) {
            return Err(CalmError::Conflict(
                "the conversation is shutting down".into(),
            ));
        }
        match &self.inner.observations {
            ObservationIngress::Running(sender) => {
                let (answer, result) = oneshot::channel();
                sender
                    .try_send(HarnessObservationCommand::Compact { source, answer })
                    .map_err(map_observation_send_error)?;
                result.await.map_err(|_| {
                    CalmError::Conflict(
                        "compaction submission was not confirmed; refresh the conversation".into(),
                    )
                })?
            }
            #[cfg(feature = "fixtures")]
            ObservationIngress::Unstarted(_) => {
                compact_command::handle_compact(&self.inner, source).await
            }
        }
    }

    pub async fn interrupt(&self, reason: String) -> Result<()> {
        if self.inner.inputs.canonical().is_some() {
            return Err(CalmError::Conflict(
                "canonical Stop requires its current Human source".into(),
            ));
        }
        self.inner.historical_queue()?;
        issue_interrupt(&self.inner, reason).await
    }

    pub(crate) async fn stop_canonical(&self, source: crate::auth::HumanInputSource) -> Result<()> {
        if self.inner.inputs.canonical().is_none() {
            return Err(CalmError::Conflict("Stop contract changed".into()));
        }
        let target = canonical::control::select_human_stop(&self.inner, &source).await?;
        let sender = match &self.inner.observations {
            ObservationIngress::Running(sender) => sender,
            #[cfg(feature = "fixtures")]
            ObservationIngress::Unstarted(_) => {
                return Err(CalmError::Conflict("canonical runtime is closed".into()));
            }
        };
        let (answer, result) = oneshot::channel();
        sender
            .try_send(HarnessObservationCommand::CanonicalInterrupt {
                target,
                source,
                answer,
            })
            .map_err(map_observation_send_error)?;
        result
            .await
            .map_err(|_| CalmError::Conflict("Stop owner closed before confirmation".into()))?
    }

    /// Replace the latest turn `turn_id` with one message under the send's key (#2043): the turn
    /// leaves the conversation and the message enters the queue in one commit that also binds the
    /// key, or nothing changes. Every refusal is a `PlannerTurnNotReplaceable` for the reader.
    pub async fn replace_turn_durable(
        &self,
        turn_id: String,
        text: String,
        attachments: Vec<BoundAttachment>,
        key: SendKey,
    ) -> Result<DurableAck> {
        self.replace_turn_admitted(turn_id, text, attachments, key, None)
            .await
    }

    pub(crate) async fn replace_turn_admitted(
        &self,
        turn_id: String,
        text: String,
        attachments: Vec<BoundAttachment>,
        key: SendKey,
        source: Option<crate::conversation_inputs::HistoricalHumanSource>,
    ) -> Result<DurableAck> {
        self.inner.historical_queue()?;
        let _durable_guard = self.inner.durable_observation.lock().await;
        if self.inner.shutting_down.load(Ordering::SeqCst) {
            return Err(CalmError::PlannerTurnNotReplaceable(
                "the conversation is shutting down; nothing was changed".into(),
            ));
        }
        let delivery = HarnessObservationDelivery {
            entry: QueueEntry::user_message(text, None, attachments),
        };
        match &self.inner.observations {
            ObservationIngress::Running(sender) => {
                let (answer, replaced) = oneshot::channel();
                sender
                    .try_send(HarnessObservationCommand::Replace {
                        turn_id,
                        delivery,
                        key,
                        source,
                        answer,
                    })
                    .map_err(map_observation_send_error)?;
                // Not a refusal: the loop may have committed before it stopped.
                replaced.await.map_err(|_| {
                    CalmError::Conflict(
                        "the conversation stopped before the edit was answered".into(),
                    )
                })?
            }
            #[cfg(feature = "fixtures")]
            ObservationIngress::Unstarted(_) => {
                replace_command::handle_replace_admitted(
                    &self.inner,
                    &turn_id,
                    delivery.entry,
                    &key,
                    source,
                )
                .await
            }
        }
    }

    /// Stop only this failed loop, without interrupting or sealing its provider
    /// thread. Recovery probes that exact thread before granting authority again.
    pub(crate) async fn quiesce_system_error_for_recovery(&self) -> Result<()> {
        self.inner.historical_queue()?;
        let _durable = self.inner.durable_observation.lock().await;
        self.request_owner_release(OwnerRelease::SystemError).await
    }

    /// A closing owner may still be processing a failed turn. Let that owner
    /// settle on its loop; only a stopped receiver permits direct settlement.
    async fn request_owner_release(&self, kind: OwnerRelease) -> Result<()> {
        match &self.inner.observations {
            ObservationIngress::Running(sender) => {
                let (done, answer) = oneshot::channel();
                match sender.try_send(HarnessObservationCommand::Release { kind, done }) {
                    Ok(()) => {
                        if let Ok(result) = answer.await {
                            return result;
                        }
                        // An earlier cancelled release can stop the loop with this
                        // retry still queued. Join, then run the real guards below.
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        return Err(CalmError::ServiceUnavailable(
                            "conversation input is busy; retry settlement shortly".into(),
                        ));
                    }
                    // A stopped loop cannot receive a retry. Join it before touching
                    // its memory; do not infer settlement from closing or channel state.
                    Err(mpsc::error::TrySendError::Closed(_)) => {}
                }
            }
            #[cfg(feature = "fixtures")]
            ObservationIngress::Unstarted(_) => {}
        }
        self.wait_for_run_loop().await?;
        let _issuance = self.inner.issuance.lock().await;
        release_owner_input(&self.inner, kind).await
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.shutdown_inner(false).await
    }

    /// Quiesce an owner that is preparing for deletion and return its retained
    /// thread seal. An error/panic releases the seal through the local guard;
    /// the caller owns it only after strict interruption succeeds.
    pub async fn shutdown_for_deletion(&self, seals: Arc<ThreadSeals>) -> Result<Option<String>> {
        let thread_id = self.inner.thread_id.read().await.clone();
        let mut seals = DeletionThreadSeals::new(seals);
        if let Some(thread_id) = thread_id.clone() {
            seals.seal(thread_id);
        }
        self.shutdown_inner(true).await?;
        Ok(seals.retain().pop())
    }

    async fn shutdown_inner(&self, strict_interrupt: bool) -> Result<()> {
        {
            let mut closed = self
                .inner
                .observations_closed
                .lock()
                .expect("planner harness observation gate mutex poisoned");
            *closed = true;
            self.inner.shutting_down.store(true, Ordering::SeqCst);
        }
        // Wake pre-issuance work without aborting an in-flight restoration.
        // The ordered release command stops the loop only after settlement.
        let _ = self.inner.shutdown.send(());
        // A durable caller may be awaiting the loop behind a track waiter.
        // Wake that waiter before waiting for the caller to finish persistence.
        let _durable_guard = self.inner.durable_observation.lock().await;
        self.request_owner_release(OwnerRelease::Shutdown).await?;
        self.wait_for_run_loop().await?;
        let _issuance_guard = self.inner.issuance.lock().await;
        let invocation = IssueTurnHandle::from_reconciliation(&self.inner).await;
        let interrupt_error = invocation.shutdown().await.err();
        held::release_all(&self.inner);
        if strict_interrupt && let Some(error) = interrupt_error {
            return Err(error);
        }
        Ok(())
    }

    /// Await by mutable reference while retaining the handle: cancellation must not
    /// detach the only proof that the previous queue/notification owner has stopped.
    async fn wait_for_run_loop(&self) -> Result<()> {
        let mut task = self.inner.run_loop_task.lock().await;
        if let Some(running) = task.as_mut() {
            let result = running.await;
            *task = None;
            result.map_err(|error| {
                CalmError::Internal(format!("conversation loop stopped: {error}"))
            })?;
        }
        Ok(())
    }

    pub async fn snapshot(&self) -> Result<HarnessSnapshot> {
        snapshot_for(&self.inner).await
    }

    /// Runtime metadata has one representation and carries no human body queue.
    /// Canonical metadata follows the actual committed carrier, including a
    /// fact whose loop projection has not caught up yet. Never synthesize V1.
    pub async fn runtime_header(
        &self,
    ) -> Result<calm_types::harness::snapshot::HarnessRuntimeHeader> {
        match &self.inner.inputs {
            RuntimeInputs::Canonical(_) => {
                Ok(canonical::read_current(&self.inner).await?.snapshot.header)
            }
            RuntimeInputs::Historical(_) => Ok(snapshot_for(&self.inner).await?.runtime_header()),
        }
    }

    /// The snapshot and the running turn, both derived from ONE read of the state, so a running
    /// turn is never paired with a different phase.
    pub async fn snapshot_with_running_turn(
        &self,
    ) -> Result<(HarnessSnapshot, Option<RunningTurn>)> {
        let state = self.inner.state.lock().await.clone();
        let running_turn = state.running_turn(Instant::now());
        Ok((
            snapshot_from_state(&self.inner, &state).await?,
            running_turn,
        ))
    }

    /// Called by the registry when this handle becomes its `Live` slot (#1791 §5.1 item 2).
    pub(crate) fn mark_installed(&self) {
        self.inner
            .backend
            .mark_installed(&self.inner.held_request_sender);
        self.inner.live_claim.install();
    }

    /// Operation shutdown's backend-owned, best-effort extra cleanup.
    pub(crate) async fn post_registry_shutdown_cleanup(&self) {
        self.inner
            .backend
            .post_registry_shutdown_cleanup(&self.inner.worker_session_id)
            .await;
    }
    /// The bound execution backend's ability, independent of the caller's authority.
    pub(crate) fn history_replacement_supported(&self) -> bool {
        PlannerBackend::metadata(&self.inner.backend.provider())
            .capabilities
            .history_replacement
    }

    /// Which provider runs this harness's turns.
    pub fn provider(&self) -> crate::session_projection_repo::AgentProvider {
        self.inner.backend.provider()
    }
    pub(crate) fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    #[cfg(feature = "fixtures")]
    pub fn acp_session_for_test(
        &self,
    ) -> Option<Arc<crate::acp_planner::session::AcpPlannerSession>> {
        self.inner.backend.acp_session_for_test()
    }

    /// Fixtures only: the Claude session behind this harness, for its interleaving hooks.
    #[cfg(feature = "fixtures")]
    pub fn claude_session_for_test(
        &self,
    ) -> Option<Arc<crate::claude_planner::session::ClaudePlannerSession>> {
        self.inner.backend.claude_session_for_test()
    }

    /// Why this conversation's queue is not draining, or `None`. `None` does NOT mean waiting is
    /// the right answer.
    pub async fn issuance_block(&self) -> Option<String> {
        if let Some(reader) = self.inner.backend.issuance_hold() {
            return Some(reader);
        }
        if self.inner.provider_hold_owns_block.load(Ordering::SeqCst) {
            return None;
        }
        self.inner.issuance_block.lock().await.clone()
    }

    /// See [`Inner::refused_issuances`].
    #[cfg(feature = "fixtures")]
    pub fn refused_issuances_for_test(&self) -> u64 {
        self.inner.refused_issuances.load(Ordering::SeqCst)
    }

    /// Forget a refusal so the next tick re-attempts immediately (called when a person changes the selection).
    pub(crate) fn request_progress(&self) {
        if let RuntimeInputs::Canonical(inputs) = &self.inner.inputs {
            inputs.request_progress();
        }
    }

    pub async fn retry_issuance_now(&self) {
        *self.inner.issuance_retry_after.lock().await = None;
        *self.inner.issuance_block.lock().await = None;
    }

    pub async fn persist_snapshot(&self) -> Result<()> {
        let _issuance_guard = self.inner.issuance.lock().await;
        persist_snapshot(&self.inner).await
    }

    pub async fn state_for_test(&self) -> HarnessState {
        self.inner.state.lock().await.clone()
    }

    #[cfg(feature = "fixtures")]
    pub fn is_closing_for_test(&self) -> bool {
        self.inner.shutting_down.load(Ordering::SeqCst)
    }

    pub async fn thread_id_for_test(&self) -> Option<String> {
        self.inner.thread_id.read().await.clone()
    }

    pub async fn pending_len_for_test(&self) -> usize {
        match &self.inner.inputs {
            RuntimeInputs::Historical(queue) => queue.lock().await.len(),
            RuntimeInputs::Canonical(inputs) => inputs
                .committed()
                .await
                .snapshot
                .canonical_inputs
                .pending
                .len(),
        }
    }

    #[cfg(feature = "fixtures")]
    pub async fn pending_queue_for_test(&self) -> Vec<Observation> {
        self.inner
            .historical_queue()
            .expect("historical queue fixture")
            .lock()
            .await
            .iter()
            .map(QueueEntry::observation)
            .collect()
    }

    #[cfg(feature = "fixtures")]
    pub async fn pending_entries_for_test(&self) -> Vec<QueueEntry> {
        self.inner
            .historical_queue()
            .expect("historical queue fixture")
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

    /// Dev-only seam for `POST /dev/force-planner-phase`: forces the FSM into `tag` and runs the
    /// regular `persist_snapshot` path. `wedged` is rejected — it would persist as `Failed` and the
    /// active-runtime read would report dormant. Returns `(old_phase, new_phase)`.
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
            HarnessPhaseTag::Compacting => HarnessState::Compacting { since: now },
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
            HarnessPhaseTag::TurnRunning => {
                HarnessState::turn_running(DEV_FORCED_TURN_ID.into(), now)
            }
            HarnessPhaseTag::TurnCompleted => HarnessState::TurnCompleted {
                last_turn_id: DEV_FORCED_TURN_ID.into(),
            },
            HarnessPhaseTag::Resumed => HarnessState::Resumed { resumed_at: now },
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
        // Phases that imply a known turn need `last_turn_id` populated so `persist_snapshot` can
        // derive `active_turn_id`. Keep a real id if one exists.
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
        // Disarm async followers of the previous state before persisting the forced one, or the
        // interrupt watchdog could flip a freshly forced phase to `Wedged` mid-test.
        *self.inner.issued_turn_id.lock().await = None;
        *self.inner.legacy_issued_input_segments.lock().await = None;
        *self.inner.interrupt_deadline.lock().await = None;
        persist_snapshot(&self.inner).await?;
        Ok((old_phase, tag))
    }

    /// Permanently stop this harness from issuing turns; in replay mode the app-server is a stub
    /// and `turn_start` always fails.
    #[cfg(feature = "fixtures")]
    pub fn pause_issuance_for_dev(&self) {
        self.inner.issuance_paused.store(true, Ordering::SeqCst);
        self.request_progress();
    }

    /// Lift a pause set by [`Self::pause_issuance_for_dev`].
    #[cfg(feature = "fixtures")]
    pub fn resume_issuance_for_test(&self) {
        self.inner.issuance_paused.store(false, Ordering::SeqCst);
        self.request_progress();
    }

    /// The debounce arming, read directly rather than inferred from whether a turn fired.
    #[cfg(feature = "fixtures")]
    pub async fn debounce_hard_fire_for_test(&self) -> bool {
        self.inner.debounce.lock().await.hard_fire
    }

    /// Whether `(first_pending_at, last_pending_at)` are set.
    #[cfg(feature = "fixtures")]
    pub async fn debounce_timestamps_set_for_test(&self) -> (bool, bool) {
        let debounce = self.inner.debounce.lock().await;
        (
            debounce.first_pending_at.is_some(),
            debounce.last_pending_at.is_some(),
        )
    }

    /// Age the pending window by `by` without sleeping; the loop reads `std::time::Instant`,
    /// which `tokio::time::pause` cannot move.
    #[cfg(feature = "fixtures")]
    pub async fn rewind_debounce_for_test(&self, by: Duration) {
        let mut debounce = self.inner.debounce.lock().await;
        let rewind = |at: Instant| at.checked_sub(by).unwrap_or(at);
        debounce.first_pending_at = debounce.first_pending_at.map(rewind);
        debounce.last_pending_at = debounce.last_pending_at.map(rewind);
    }

    /// Let `by` pass for the running turn without sleeping: the turn started, its watchdog
    /// started counting, and the table has held its requests since, `by` earlier. Same clock as
    /// [`Self::rewind_debounce_for_test`].
    #[cfg(feature = "fixtures")]
    pub async fn rewind_turn_clock_for_test(&self, by: Duration) {
        let rewind = |at: Instant| at.checked_sub(by).unwrap_or(at);
        if let HarnessState::TurnRunning {
            started_at,
            watchdog_from,
            ..
        } = &mut *self.inner.state.lock().await
        {
            *started_at = rewind(*started_at);
            *watchdog_from = rewind(*watchdog_from);
        }
        self.inner.held_requests.rewind_active_for_test(by);
        if let Some(inputs) = self.inner.inputs.canonical() {
            inputs.request_progress();
        }
    }

    /// Advance only the deadline armed by a real interrupt; never invent an interrupt state.
    #[cfg(feature = "fixtures")]
    pub async fn expire_interrupt_deadline_for_test(&self) {
        let mut deadline = self.inner.interrupt_deadline.lock().await;
        let (_, at) = deadline
            .as_mut()
            .expect("a real interrupt armed its deadline");
        *at = Instant::now();
        drop(deadline);
        if let Some(inputs) = self.inner.inputs.canonical() {
            inputs.request_progress();
        }
    }

    /// Whether a watchdog tick has seen the table hold a request since it last saw it empty.
    #[cfg(feature = "fixtures")]
    pub async fn watchdog_saw_held_request_for_test(&self) -> bool {
        self.inner.watchdog_held_since.lock().await.is_some()
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

    /// How long since the newest pending entry landed, in milliseconds; zero when there is no window.
    #[cfg(feature = "fixtures")]
    pub async fn debounce_last_pending_elapsed_ms_for_test(&self) -> u128 {
        self.inner
            .debounce
            .lock()
            .await
            .last_pending_at
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
    loaded: LoadedRuntimeSnapshot,
    observations: ObservationIngress,
    shutdown: broadcast::Sender<()>,
    held_request_sender: HeldRequestSender,
) -> (Arc<Inner>, bool) {
    params
        .backend
        .bind_provider_registry(params.registry.clone());
    let mut planner_compaction = None;
    let (
        header,
        inputs,
        dropped_on_load,
        debounce,
        recent_hook_keys,
        recent_hook_key_set,
        projection_client_id,
        legacy_issued_input_segments,
    ) = match loaded {
        LoadedRuntimeSnapshot::Historical(mut snapshot) => {
            let dropped = truncate_snapshot_pending_queue(&mut snapshot);
            let queue = snapshot
                .pending_entries()
                .into_iter()
                .collect::<VecDeque<_>>();
            let debounce = debounce_from_initial_queue(&queue);
            let (keys, key_set) = recent_hook_keys_from_pending_queue(&queue);
            snapshot.restore_context_feedback();
            planner_compaction = snapshot.planner_compaction.clone();
            (
                snapshot.runtime_header(),
                RuntimeInputs::Historical(Mutex::new(queue)),
                dropped,
                debounce,
                keys,
                key_set,
                snapshot.projection_client_id,
                snapshot.issued_input_segments,
            )
        }
        LoadedRuntimeSnapshot::Canonical(frame) => (
            frame.snapshot.header.clone(),
            RuntimeInputs::Canonical(Box::new(canonical::CanonicalInputs::new(
                frame,
                params
                    .registry
                    .canonical_sources(&params.worker_session_id, &params.track_id),
                params.registry.maintenance_execution_cell(),
            ))),
            Vec::new(),
            DebounceState::default(),
            VecDeque::new(),
            HashSet::new(),
            None,
            None,
        ),
    };
    let announce_first = !dropped_on_load.is_empty();
    let state = state_from_runtime_header(&header, &inputs);
    let last_phase = header.phase;
    let thread_id = if inputs.canonical().is_some() {
        header.last_thread_id.clone()
    } else {
        params.thread_id.or(header.last_thread_id.clone())
    };
    let live_claim = params
        .registry
        .live_replies()
        .claim(&params.card_id, &params.worker_session_id);
    let progress = match &inputs {
        RuntimeInputs::Canonical(inputs) => inputs.progress_signal(),
        RuntimeInputs::Historical(_) => Arc::new(tokio::sync::Notify::new()),
    };
    let inner = Arc::new(Inner {
        track_delete_locks: params.track_delete_locks,
        worker_session_id: params.worker_session_id,
        track_id: params.track_id,
        card_id: params.card_id,
        thread_id: RwLock::new(thread_id),
        repo: params.repo,
        events: params.events,
        card_role_cache: params.card_role_cache,
        track_area_cache: params.track_area_cache,
        backend: params.backend,
        live_claim,
        observations,
        state: Mutex::new(state),
        last_phase: Mutex::new(last_phase),
        last_context_feedback: Mutex::new((
            header.compact_outcome.clone(),
            header.context_usage_invalidated,
            None,
        )),
        inputs,
        recent_hook_keys: Mutex::new(recent_hook_keys),
        recent_hook_key_set: Mutex::new(recent_hook_key_set),
        push_watermark: Mutex::new(header.push_watermark),
        last_turn_id: Mutex::new(header.last_turn_id),
        issued_turn_id: Mutex::new(None),
        interruption_intent: Mutex::new(header.interruption_intent),
        issued_turn_head: Mutex::new(header.issued_turn_head),
        projection_client_id: Mutex::new(projection_client_id),
        legacy_issued_input_segments: Mutex::new(legacy_issued_input_segments),
        last_report_body_sha256: Mutex::new(header.last_report_body_sha256),
        last_seen_head: Mutex::new(header.last_seen_head),
        objective_usage: Mutex::new(objective_usage::TurnMeter::default()),
        // Round-trips through the snapshot: codex only re-pushes it on the next model response, so a
        // resumed-but-idle thread would otherwise read as having no context usage.
        token_usage: Mutex::new(header.token_usage),
        maintenance_turns: Mutex::new(
            header
                .compact_outcome
                .as_ref()
                .and_then(|outcome| outcome.turn_id.clone())
                .into_iter()
                .collect(),
        ),
        compact_outcome: Mutex::new(header.compact_outcome),
        planner_compaction: Mutex::new(planner_compaction),
        compaction_retry_after: Mutex::new(None),
        context_usage_invalidation: Mutex::new(header.context_usage_invalidated.then_some(
            compact_command::UsageInvalidation {
                user_turn_id: header.context_usage_invalidated_turn,
            },
        )),
        pending_rewind: Mutex::new(header.pending_rewind.map(BackendRewind::from_persisted)),
        last_turn_base: Mutex::new(header.last_turn_base),
        rewound_turns: Mutex::new(HashSet::new()),
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
        parked: crate::harness::carrier_park::ParkState::with_signal(progress.clone()),
        provider_held: AtomicBool::new(false),
        provider_hold_owns_block: AtomicBool::new(false),
        issuance_paused: AtomicBool::new(false),
        unannounced_drops: Mutex::new(dropped_on_load),
        steered_into_running_turn: Mutex::new(Vec::new()),
        restored_projections: Mutex::new(Vec::new()),
        input_commit: Mutex::new(None),
        held_requests: Arc::new(HeldRequests::with_signal(progress)),
        held_request_sender,
        watchdog_held_since: Mutex::new(None),
        watchdog_held_time: std::sync::Mutex::new(Duration::ZERO),
        run_loop_task: Mutex::new(None),
        turn_budget: Mutex::new(header.turn_budget),
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

/// Seed hook-stop dedupe from the restored pending queue so fallback replay or bridge retry
/// cannot deliver the same hook again. Empty keys are skipped (old snapshot rows default them).
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

/// Cadence timer. The loop can be parked ~41s inside `maybe_issue_turn`; `MissedTickBehavior::Skip`
/// collapses the resulting tick backlog into one.
fn harness_tick() -> tokio::time::Interval {
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tick
}

async fn run_loop(
    inner: Arc<Inner>,
    mut observations: mpsc::Receiver<HarnessObservationCommand>,
    mut held_messages: HeldRequestReceiver,
    mut shutdown: broadcast::Receiver<()>,
    mut events: PlannerEvents,
    live: LiveReplyWriter,
    announce_dropped_first: bool,
) {
    // Early flush before the first command is served. Correctness does not rest on it:
    // `persist_snapshot_inner` drains the same list and refuses to write without it.
    if announce_dropped_first && let Err(error) = flush_dropped_announcements(&inner).await {
        tracing::error!(
            card_id = %inner.card_id,
            error = %error,
            "planner queue drop announcements failed on load; the truncated queue will not be \
             persisted until they land"
        );
    }
    // A harness built over a session that already has open `hold` asks (a restart, a respawn, a
    // system-error recovery) holds none of their requests.
    held::sweep(&inner).await;
    let mut tick = harness_tick();
    let mut canonical_probe = tokio::time::Instant::now();
    loop {
        tokio::select! {
            // The sender lives on `inner`, so the channel never closes while the loop runs.
            Some(message) = held_messages.recv() => {
                held::on_message(&inner, message).await;
                if let Some(inputs) = inner.inputs.canonical() {
                    inputs.request_progress();
                }
            }
            command = observations.recv() => {
                let Some(command) = command else { break };
                #[cfg(feature = "fixtures")]
                crate::test_seams::pause_point("conversation-command-dequeued", inner.card_id.as_str()).await;
                let Some(command) = input_commit::admit(&inner, command).await else { continue };
                let progress = !matches!(&command,
                    HarnessObservationCommand::CanonicalSource { .. }
                    | HarnessObservationCommand::InspectInput { .. }
                    | HarnessObservationCommand::Hibernate { .. }
                    | HarnessObservationCommand::Release { .. });
                match command {
                    HarnessObservationCommand::CanonicalInterrupt { target, source, answer } => {
                        canonical::control::manual(&inner, target, source, answer).await;
                    }
                    HarnessObservationCommand::CanonicalSource { input, source, observed } => {
                        let result = match inner.inputs.canonical() {
                            Some(inputs) if !inner.shutting_down.load(Ordering::SeqCst) => {
                                match source.check_current() {
                                    Ok(_) => { inputs.observe_source(input, source).await; Ok(()) }
                                    Err(error) => Err(error),
                                }
                            }
                            _ => Err(CalmError::Conflict("canonical runtime is closed".into())),
                        };
                        let _ = observed.send(result);
                    }
                    HarnessObservationCommand::Hibernate { done } => {
                        let result = hibernation::quiesce(&inner, observations.is_empty(), Some(&done)).await;
                        #[cfg(feature = "fixtures")]
                        crate::test_seams::pause_point(
                            crate::test_seams::PLANNER_HIBERNATION_DECIDED, inner.card_id.as_str(),
                        ).await;
                        let stop = matches!(result, Ok(true));
                        let _ = done.send(result);
                        if stop { break; }
                    }
                    HarnessObservationCommand::Release {kind, done} => {
                        let result = {
                            let _issuance = inner.issuance.lock().await;
                            release_owner_input(&inner, kind).await
                        };
                        let stop=result.is_ok();
                        let _=done.send(result);
                        if stop { break; }
                    }
                    HarnessObservationCommand::RequestCompaction { request_id, answer } => {
                        let _ = answer.send(self_compact::request(&inner, request_id).await);
                    }
                    HarnessObservationCommand::Compact { source, answer } => {
                        let outcome = compact_command::handle_compact(&inner, source).await;
                        let _ = answer.send(outcome);
                    }
                    HarnessObservationCommand::Delivery(delivery) => {
                        let _accepted = on_observation(&inner, delivery.entry).await;
                        // This command owns the observation even if shutdown closed
                        // ingress while it was being folded. Commit here, before the
                        // ordered Release, rather than making every release checkpoint.
                        if let Err(e) = persist_owned_input_snapshot(&inner).await {
                            tracing::warn!(error = %e, "planner harness snapshot persist failed after observation");
                        }
                    }
                    HarnessObservationCommand::Durable { deliveries, key, source, persisted } => {
                        let result = durable_command::handle_durable(&inner, deliveries, &key, source).await;
                        let _ = persisted.send(result);
                    }
                    HarnessObservationCommand::InspectInput { entry_id, answer } => {
                        let _ = answer.send(input_inspection::inspect(&inner, &entry_id).await);
                    }
                    HarnessObservationCommand::Mutate { mutation, actor, applied } => {
                        let outcome = handle_queue_mutation(&inner, &mutation, &actor).await;
                        let _ = applied.send(outcome);
                    }
                    HarnessObservationCommand::Steer { entry_id, if_entry_rev, actor, applied } => {
                        let outcome = handle_steer(&inner, &entry_id, if_entry_rev, &actor).await;
                        let _ = applied.send(outcome);
                    }
                    HarnessObservationCommand::Replace { turn_id, delivery, key, source, answer } => {
                        let outcome =
                            replace_command::handle_replace_admitted(&inner, &turn_id, delivery.entry, &key, source).await;
                        // A reply of the removed turn that failed to store stays live until the
                        // next turn starts; the turn is gone, so its text goes now.
                        if outcome.is_ok() {
                            live.discard(&turn_id);
                        }
                        let _ = answer.send(outcome);
                    }
                }
                if progress && let Some(inputs) = inner.inputs.canonical() {
                    inputs.request_progress();
                }
                #[cfg(feature = "fixtures")]
                crate::test_seams::pause_point("conversation-command-completed", inner.card_id.as_str()).await;
            }
            event = events.recv() => {
                match event {
                    Ok(event) => {
                        if let Err(e) = on_notification(&inner, &live, event).await {
                            tracing::warn!(error = %e, "planner harness notification handling failed");
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "planner harness notification receiver lagged");
                        live.input_lost();
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = async {
                match inner.inputs.canonical() {
                    Some(inputs) => inputs.progress_requested().await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                #[cfg(feature = "fixtures")]
                crate::test_seams::pause_point("canonical-before-progress-advance", inner.card_id.as_str()).await;
                let progress = canonical::advance(&inner, observations.is_empty()).await;
                canonical_probe = canonical::scheduling::next_probe(&inner, progress).await;
            },
            _ = async {
                if inner.inputs.canonical().is_some() {
                    tokio::time::sleep_until(canonical_probe).await;
                } else {
                    tick.tick().await;
                }
            } => {
                if inner.inputs.canonical().is_some() {
                    let progress = canonical::advance(&inner, observations.is_empty()).await;
                    canonical_probe = canonical::scheduling::next_probe(&inner, progress).await;
                    continue;
                }
                if let Err(e) = watchdog_tick(&inner).await {
                    tracing::warn!(error = %e, "planner harness watchdog tick failed");
                }
                #[cfg(feature = "fixtures")]
                crate::test_seams::pause_point("planner-compact-tick", inner.card_id.as_str()).await;
                if let Err(e) = self_compact::tick(&inner, observations.is_empty()).await {
                    tracing::warn!(error = %e, "Planner compaction tick failed");
                }
                if let Err(e) = maybe_issue_turn(&inner).await {
                    tracing::warn!(error = %e, "planner harness turn issuance failed");
                }
            }
            _ = shutdown.recv() => {
                // Wake only. The ordered Release command owns settlement and stop.
                // A failed release must keep processing terminal notifications.
            }
        }
    }
    held::release_all(&inner);
}

/// A failed provider's accepted steers still belong to its terminal notification.
/// Do not stop that notification owner before it can register dropped input.
async fn require_failed_turn_settled(inner: &Arc<Inner>) -> Result<()> {
    if matches!(&*inner.state.lock().await,
        HarnessState::Wedged { reason, .. } if reason == HARNESS_SYSTEM_ERROR_REASON)
        && !inner.steered_into_running_turn.lock().await.is_empty()
    {
        return Err(CalmError::ServiceUnavailable(
            "Waiting for the failed turn to settle its messages before recovery; retry shortly."
                .into(),
        ));
    }
    Ok(())
}

/// Settle only the input transfer this owner still owes. Ordinary ingress and
/// issuance already persist through their own owners; stopping physical resources
/// does not authorize another checkpoint (especially after a committed MOVE).
/// Call on the run loop, or after joining it under durable_observation and issuance.
async fn settle_owner_input_before_release(inner: &Arc<Inner>) -> Result<()> {
    input_commit::resume(inner).await?;
    require_failed_turn_settled(inner).await?;
    // This is the existing atomic rebuffer/retirement obligation, not a queue-
    // emptiness or runtime-state inference. No obligation means no storage access.
    if inner.restored_projections.lock().await.is_empty() {
        return Ok(());
    }
    persist_owned_input_snapshot(inner).await?;
    if !inner.restored_projections.lock().await.is_empty() {
        return Err(CalmError::Conflict(
            "conversation changed before its requeued input could be settled".into(),
        ));
    }
    Ok(())
}

/// Commit at the input owner's processing boundary, including work accepted
/// before ingress closed. Only the original current-carrier guards grant writes.
async fn persist_owned_input_snapshot(inner: &Arc<Inner>) -> Result<()> {
    // A terminal transition may have reached memory while the active checkpoint failed.
    // Let that original guard commit the transition before trying the failed-current guard.
    if persist_guarded_snapshot(inner, None, None, false).await? {
        return Ok(());
    }
    if matches!(&*inner.state.lock().await,
        HarnessState::Wedged { reason, .. } if reason == HARNESS_SYSTEM_ERROR_REASON)
    {
        persist_guarded_failed_system_error_snapshot(inner).await
    } else {
        Err(CalmError::Conflict(
            "conversation changed before its input could be settled".into(),
        ))
    }
}

/// Called on the run loop, or after joining it under the ingress/issuance
/// fences. Ordinary delivery, queue mutation and notifications cannot interleave.
async fn release_owner_input(inner: &Arc<Inner>, kind: OwnerRelease) -> Result<()> {
    if matches!(kind, OwnerRelease::SystemError)
        && !matches!(&*inner.state.lock().await, HarnessState::Wedged {reason,..} if reason==HARNESS_SYSTEM_ERROR_REASON)
    {
        return Err(CalmError::Conflict(
            "conversation changed before recovery".into(),
        ));
    }
    settle_owner_input_before_release(inner).await?;
    *inner.observations_closed.lock().expect("observation gate") = true;
    inner.shutting_down.store(true, Ordering::SeqCst);
    Ok(())
}

/// Take the queue lock, apply the mutation, re-arm the debounce, persist, emit. A
/// `QueueMutation::Steer` is refused here: applying it would take the entry out and deliver it nowhere.
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
    let checkpoint = checkpoint_durable_user_message(inner).await?;
    let outcome = apply_mutation(&mut *inner.historical_queue()?.lock().await, mutation);
    let applied = match outcome {
        Ok(applied) => applied,
        // A refusal changed nothing; a 404 does NOT mean the entry was delivered (`rebuffer_head`
        // can put a drained batch back), so no event is invented.
        Err(refused) => return Ok(Err(refused)),
    };

    if applied.change == HarnessQueueChange::Deleted {
        rearm_debounce_after_departure(inner, &applied).await;
    }

    if let Err(error) = persist_owned_input_snapshot(inner).await {
        // Restore both queue and debounce, including a refused zero-row write.
        restore_durable_user_message(inner, checkpoint).await?;
        return Err(error);
    }

    if let Err(error) = emit_queue_changed(inner, actor, &applied.entry_id, applied.change).await {
        // The snapshot is already committed, so the change has happened; reporting failure would
        // invite a retry of a delete that already succeeded.
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

/// One rule for every departure from the queue: `hard_fire` is recomputed over what is left;
/// the timestamps are NOT touched unless the queue emptied.
async fn rearm_debounce_after_departure(inner: &Inner, applied: &MutationApplied) {
    let mut debounce = inner.debounce.lock().await;
    debounce.hard_fire = applied.remaining_hard_fire;
    if applied.queue_now_empty {
        debounce.first_pending_at = None;
        debounce.last_pending_at = None;
    }
}

/// The whole of `HarnessObservationCommand::Steer`: take the entry OUT of the queue before
/// `turn/steer` is asked; on yes write its transcript row (AFTER codex said yes, unlike the
/// drain) and remember it for the completion; otherwise put it back at the head and refuse.
async fn handle_steer(
    inner: &Arc<Inner>,
    entry_id: &QueueEntryId,
    if_entry_rev: u32,
    actor: &ActorId,
) -> Result<SteerResult> {
    let _issuance = inner.issuance.lock().await;
    inner.require_unparked()?;
    // Read on this task: the phase leaves `TurnRunning` through the notification arm of the same
    // `select!`; a Stop landing after codex accepted is what `SteeredEntry` exists for.
    let running = inner.state.lock().await.clone();
    let phase = HarnessPhaseTag::from(&running);
    if let Some(message) = inner.backend.issuance_hold() {
        return Ok(Err(SteerRefused::NotTaken { message, phase }));
    }
    // #1791 §5.9: decided before the entry is taken, so its id and rev stay as they are and it
    // runs as the next turn.
    if !PlannerBackend::metadata(&inner.backend.provider()).steer {
        return Ok(Err(SteerRefused::NotTaken {
            message: "this Planner's provider cannot take messages into a running \
                      turn; it stays queued"
                .into(),
            phase,
        }));
    }
    let turn_id = match running {
        HarnessState::TurnRunning { turn_id, .. } => Some(turn_id),
        _ => None,
    };
    let (applied, turn_id) = {
        let mut queue = inner.historical_queue()?.lock().await;
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
        // Unreachable in practice (`TurnRunning` is only entered from a `turn/started` on this thread).
        rebuffer_head(inner, vec![entry]).await?;
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
        "calling backend.turn_steer"
    );
    let steered = IssueTurnHandle::from_reconciliation(inner)
        .await
        .turn_steer(SteerRequest {
            turn: inner.backend.turn_identity(turn_id.clone()),
            input: InputRequest {
                items,
                correlation: CorrelationKey(entry_id.as_str().to_owned()),
            },
        })
        .await;
    let (error, mut refusal) = match steered? {
        SteerOutcome::Taken { turn } => {
            let taken_by = turn.into_string();
            let incorporating = input_commit::retain_result(
                inner,
                input_commit::Continuation::SteerAccepted(SteerApplied {
                    entry_id: entry_id.clone(),
                    rev: applied.rev,
                    turn_id: taken_by.clone(),
                }),
            )
            .await;
            // Codex has the sentence. A failed row write is logged rather than reported: the delivery has
            // happened whatever this says, and a 500 would invite a retry of it.
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
            // The queue without the entry is the truth to persist; a failed write is logged and the steer
            // still answers 200.
            drop(incorporating);
            if let Err(error) = input_commit::resume(inner).await {
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
        SteerOutcome::NotTaken { error } => {
            let message = error.reason();
            (error, SteerRefused::NotTaken { message, phase })
        }
        SteerOutcome::Unknown { error, .. } => {
            let message = error.to_string();
            (error, SteerRefused::Unanswered { message, phase })
        }
    };

    // Refused, unreachable, or timed out: the sentence goes back. No row was written and the
    // on-disk snapshot never stopped listing the entry, so a persist failure is a warning.
    rebuffer_head(inner, vec![entry]).await?;
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
        // The entry IS back; reporting failure would turn a typed "still queued" into an untyped 500.
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
    match &mut refusal {
        SteerRefused::Queue(_) => {}
        SteerRefused::NoRunningTurn { phase: current }
        | SteerRefused::NotTaken { phase: current, .. }
        | SteerRefused::Unanswered { phase: current, .. } => *current = phase,
    }
    Ok(Err(refusal))
}

/// `TurnCompleted` sweep: every steered entry whose transcript row is still a projection was
/// dropped by codex before it was recorded. Rebuffer one rev up and persist pending BEFORE
/// deleting projections, so the transfer never removes the only durable input source.
/// Runs after the arm's gates and before its final phase/head-stamping persist.
async fn restore_steered_entries_codex_dropped(
    inner: &Arc<Inner>,
    turn_id: &str,
) -> Result<Vec<QueueEntryId>> {
    budget::settle_projection(inner).await;
    let steered = std::mem::take(&mut *inner.steered_into_running_turn.lock().await);
    if steered.is_empty() {
        return Ok(Vec::new());
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
        return Ok(Vec::new());
    }
    let ids = restored
        .iter()
        .filter_map(|entry| entry.id().cloned())
        .collect::<Vec<_>>();
    // One rev up before it goes back, so the client hiding the entry can tell the restored page
    // from the one it read before the steer.
    for entry in &mut restored {
        // Accepted steers reserve this increment in apply_mutation. A retained
        // historical edge must still preserve its input if that proof is absent.
        if !entry.bump_rev_for_restore() && entry.user_view().is_some() {
            tracing::error!(card_id = %inner.card_id, entry_id = ?entry.id(),
                "restored input has no revision capacity; preserving its unchanged body");
        }
    }
    rebuffer_head_retiring(inner, restored, &ids).await?;
    // A restored steer can be the first visible user input (after a system-only turn).
    // Keep its projection until pending is durably written, including a no-row result.
    let failed_owner = matches!(&*inner.state.lock().await,
        HarnessState::Wedged { reason, .. } if reason == HARNESS_SYSTEM_ERROR_REASON);
    let persisted = if failed_owner {
        persist_failed_system_error_snapshot(inner).await
    } else {
        persist_snapshot_inner(inner, None, None, false)
            .await
            .map(|_| ())
    };
    if let Err(error) = persisted {
        tracing::warn!(
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            error = %error,
            "planner harness could not transfer restored entries; retaining their projections"
        );
    }
    Ok(ids)
}

async fn announce_restored_entries(inner: &Arc<Inner>, ids: Vec<QueueEntryId>) {
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

/// One `harness.queue.changed` row for `entry_id`. The change it announces has already
/// happened by the time this is called.
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
    #[cfg(feature = "fixtures")]
    wait_at_planner_harness_observation_race_hook(&inner.worker_session_id).await;
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

/// The cap is applied here, from the OLD end, so harvested human sentences are the first
/// dropped. Returns the ids of the addressable user entries discarded, so the caller can
/// announce each as `dropped`; a `LegacyUser` has no id and a `System` entry is not a person's.
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

/// Drain the `dropped` rows the load-time truncation still owes. Fail-closed: `persist_snapshot_inner`
/// refuses its write if this returns `Err`. The lock is held across the inserts so two flushers
/// in this process cannot announce the same id; across boots one row per entry is NOT guaranteed.
async fn flush_dropped_announcements(inner: &Arc<Inner>) -> Result<()> {
    // One flusher at a time; a `tokio::Mutex` because the guard is held across the await.
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

async fn enqueue_pending_observation(inner: &Arc<Inner>, entry: QueueEntry) -> EnqueueOutcome {
    let Ok(queue) = inner.historical_queue() else {
        return EnqueueOutcome::Unsupported;
    };
    let mut queue = queue.lock().await;
    // Adjacent, contiguous report edits of the same track fold on every enqueue, so the planner
    // reads ONE diff; user text keeps its own slot until the cap forces a fold.
    if let FoldOutcome::Folded { entry_id } = try_fold_report_edit_tail(&mut queue, &entry) {
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
        // Eviction only ever takes a non-hard-fire entry, which is always a `System` entry, so
        // neither fold nor eviction can destroy an id a client has been shown.
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

async fn on_notification(
    inner: &Arc<Inner>,
    live: &LiveReplyWriter,
    event: PlannerEvent,
) -> Result<()> {
    let PlannerEvent { thread_id, kind } = event;
    if thread_id != *inner.thread_id.read().await {
        return Ok(());
    }
    if inner.inputs.canonical().is_some() {
        return canonical::on_notification(inner, live, thread_id.as_deref(), kind).await;
    }

    let maintenance_item = match &kind {
        PlannerEventKind::Item { params, .. } | PlannerEventKind::PlanUpdated { params } => {
            let turns = inner.maintenance_turns.lock().await;
            item_turn_id(params).is_some_and(|turn| turns.contains(turn))
        }
        _ => false,
    };
    if maintenance_item {
        // Maintenance items are not conversational history. Recording them would put another
        // turn after the latest user reply and prevent that reply's normal edit/rewind.
        return persist_snapshot(inner).await;
    }
    match kind {
        PlannerEventKind::Approval { method } => {
            tracing::warn!(
                method,
                "planner harness ignoring an approval-shaped notification; approvals arrive as server requests"
            );
            return Ok(());
        }
        PlannerEventKind::ThreadStarted => {
            if let Some(thread_id) = thread_id {
                *inner.thread_id.write().await = Some(thread_id);
            }
            let mut state = inner.state.lock().await;
            if matches!(
                *state,
                HarnessState::PendingThreadStart | HarnessState::Resumed { .. }
            ) {
                *state = HarnessState::Idle;
            }
        }
        PlannerEventKind::ThreadSystemError => {
            compact_command::settle(
                inner,
                crate::harness::compact_outcome::CompactStatus::Unconfirmed,
            )
            .await;
            *inner.state.lock().await = HarnessState::Wedged {
                since: Instant::now(),
                reason: HARNESS_SYSTEM_ERROR_REASON.into(),
            };
            *inner.issued_turn_id.lock().await = None;
            *inner.interrupt_deadline.lock().await = None;
        }
        PlannerEventKind::ThreadIdle => {
            let mut state = inner.state.lock().await;
            if matches!(*state, HarnessState::Resumed { .. }) {
                *state = HarnessState::Idle;
            }
        }
        PlannerEventKind::TurnStarted { turn_id } => {
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
                HarnessState::Compacting { .. } => {
                    // Fresh provider streams deliver one ordered start per operation, before
                    // completion. Only an unbound current intent may accept that start.
                    last_seen.as_deref() != Some(turn_id.as_str())
                        && !inner.maintenance_turns.lock().await.contains(&turn_id)
                        && inner
                            .compact_outcome
                            .lock()
                            .await
                            .as_ref()
                            .is_some_and(|outcome| {
                                outcome.status
                                    == crate::harness::compact_outcome::CompactStatus::Started
                                    && outcome.turn_id.is_none()
                            })
                }
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
                live_reply::on_unaccepted_start(live, &state_snap, issued.as_deref(), &turn_id);
                return persist_snapshot(inner).await;
            }
            if matches!(state_snap, HarnessState::Compacting { .. }) {
                // Compaction has no user message: retain the user's latest turn for editing.
                inner.maintenance_turns.lock().await.insert(turn_id.clone());
                if let Some(outcome) = inner.compact_outcome.lock().await.as_mut() {
                    outcome.turn_id = Some(turn_id.clone());
                }
                *inner.state.lock().await = HarnessState::CompactionRunning {
                    turn_id,
                    started_at: Instant::now(),
                };
                return persist_snapshot(inner).await;
            }
            let already_running_same = matches!(
                &state_snap,
                HarnessState::TurnRunning { turn_id: active, .. } if active == &turn_id
            );
            let new_user_start = matches!(
                state_snap,
                HarnessState::Issuing {
                    kind: IssuingKind::TurnStart,
                    ..
                }
            ) || last_seen.as_deref() != Some(turn_id.as_str());
            if !already_running_same && new_user_start {
                *inner.compact_outcome.lock().await = None;
                self_compact::clear_settled(inner).await;
            }
            *inner.last_turn_id.lock().await = Some(turn_id.clone());
            live.turn_started(&turn_id);
            if !already_running_same {
                let mut state = inner.state.lock().await;
                held::reset_held_time(inner);
                *state = HarnessState::turn_running(turn_id, Instant::now());
                drop(state);
                held::sweep(inner).await;
            }
            *inner.issued_turn_id.lock().await = None;
            *inner.interrupt_deadline.lock().await = None;
        }
        PlannerEventKind::TurnCompleted { turn } => {
            let compact_state = inner.state.lock().await.clone();
            if let HarnessState::CompactionRunning { turn_id, .. } = compact_state {
                if turn.get("id").and_then(Value::as_str) != Some(turn_id.as_str()) {
                    return Ok(());
                }
                use crate::harness::compact_outcome::CompactStatus;
                let status = match turn.get("status").and_then(Value::as_str) {
                    Some("completed") => CompactStatus::Completed,
                    Some("interrupted") => CompactStatus::Interrupted,
                    Some("failed") => CompactStatus::Failed,
                    _ => CompactStatus::Unconfirmed,
                };
                if status == CompactStatus::Completed {
                    compact_command::invalidate_usage(
                        inner,
                        inner.last_turn_id.lock().await.clone(),
                    )
                    .await;
                }
                compact_command::settle(inner, status).await;
                *inner.state.lock().await = match turn.get("status").and_then(Value::as_str) {
                    Some("completed" | "interrupted") => HarnessState::TurnCompleted {
                        last_turn_id: inner
                            .last_turn_id
                            .lock()
                            .await
                            .clone()
                            .expect("compaction requires history"),
                    },
                    _ => HarnessState::Wedged {
                        since: Instant::now(),
                        reason: "Context compaction failed; reset the conversation to continue."
                            .into(),
                    },
                };
                // A provider maintenance turn must not become the result of the user's previous prompt.
                return persist_snapshot(inner).await;
            }
            if let Some(turn) = turn.get("id").and_then(Value::as_str)
                && inner.maintenance_turns.lock().await.contains(turn)
            {
                return Ok(());
            }
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
                held::end_turn(inner).await;
                live_reply::settle(inner, live, &turn_id, &turn).await;
                let _ = persist_turn_outcome(inner, &turn_id, &turn).await;
                // The Stop path is the one that drops steered input; the sweep runs before the phase persist.
                let restored = restore_steered_entries_codex_dropped(inner, &turn_id).await?;
                persist_snapshot_stamping_issued_head(inner).await?;
                announce_restored_entries(inner, restored).await;
                return Ok(());
            }
            let state = inner.state.lock().await.clone();
            // Codex sends systemError BEFORE the failed turn/completed. Only the explicit ID of our last
            // turn qualifies; stale completions and missing IDs cannot settle another turn.
            if matches!(&state, HarnessState::Wedged { reason, .. } if reason == HARNESS_SYSTEM_ERROR_REASON)
                && turn.get("id").and_then(Value::as_str) == fallback_turn_id.as_deref()
                && fallback_turn_id.is_some()
            {
                held::end_turn(inner).await;
                live_reply::settle(inner, live, &turn_id, &turn).await;
                let item = persist_turn_outcome(inner, &turn_id, &turn).await;
                let restored = restore_steered_entries_codex_dropped(inner, &turn_id).await?;
                persist_failed_system_error_snapshot(inner).await?;
                objective_usage::observe(
                    inner,
                    HarnessPhaseTag::TurnCompleted,
                    Some(&turn_id),
                    None,
                    true,
                )
                .await;
                announce_restored_entries(inner, restored).await;
                if let Some(item_id) = item {
                    emit_item_added(
                        inner,
                        item_id,
                        None,
                        None,
                        Some(turn_id),
                        "turn/completed".into(),
                    )
                    .await?;
                }
                return Ok(());
            }
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
            // This terminal-origin cut holds no state, queue, issuance, or SQL guard.
            #[cfg(feature = "fixtures")]
            crate::test_seams::pause_point(
                "historical-turn-completed-published",
                inner.card_id.as_str(),
            )
            .await;
            held::end_turn(inner).await;
            live_reply::settle(inner, live, &turn_id, &turn).await;
            let _ = persist_turn_outcome(inner, &turn_id, &turn).await;
            // A turn can end without a model request after the steer on this branch too; same sweep.
            let restored = restore_steered_entries_codex_dropped(inner, &turn_id).await?;
            persist_snapshot_stamping_issued_head(inner).await?;
            announce_restored_entries(inner, restored).await;
            return Ok(());
        }
        // Codex has no `turn/aborted` notification; an interrupt arrives as `turn/completed` with
        // `status: "interrupted"`.
        PlannerEventKind::TurnAborted {
            turn_id: aborted_turn_id,
        } => {
            if matches!(&*inner.state.lock().await, HarnessState::CompactionRunning { turn_id, .. } if turn_id == &aborted_turn_id)
            {
                compact_command::settle(
                    inner,
                    crate::harness::compact_outcome::CompactStatus::Interrupted,
                )
                .await;
                *inner.state.lock().await = HarnessState::TurnCompleted {
                    last_turn_id: inner
                        .last_turn_id
                        .lock()
                        .await
                        .clone()
                        .expect("compaction requires history"),
                };
                return persist_snapshot(inner).await;
            }
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
            held::end_turn(inner).await;
            clear_interruption_intent(inner, &aborted_turn_id).await;
            let restored = restore_steered_entries_codex_dropped(inner, &aborted_turn_id).await?;
            persist_snapshot_stamping_issued_head(inner).await?;
            announce_restored_entries(inner, restored).await;
            return Ok(());
        }
        PlannerEventKind::Item {
            phase,
            params,
            questions,
        } => {
            content_observer::on_item(inner, live, phase, params, questions).await?;
        }
        // `turn/plan/updated` — codex's whole TODO checklist for the running turn, superseding the
        // previous one. Persisted only; no UI reads it yet.
        PlannerEventKind::PlanUpdated { params } => {
            let method = "turn/plan/updated";
            // The transcript thread ID is NOT NULL, so there is no row to write without one.
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
            // `turnId` is top-level on a plan; `item_turn_id` falls back to it and accepts `turn_id` too.
            let turn_id = item_turn_id(&params).map(ToOwned::to_owned);
            if is_rewound(inner, turn_id.as_deref()).await {
                return persist_snapshot(inner).await;
            }
            let params_json = serde_json::to_string(&params)?;
            insert_item_row(
                inner,
                &thread_id,
                turn_id.as_deref(),
                // No `item_uuid` and no `item_type`: a plan is not an item.
                None,
                None,
                method,
                &params_json,
                None,
            )
            .await?;
            // Deliberately no transcript notification for a plan row: nothing reads plan rows, and the
            // event would append a track-vcs commit per plan frame. Transcript rows use out-of-domain
            // storage, so a row without an event is legal here.
        }
        PlannerEventKind::ContextReplaced { turn_id } => {
            let last_user = inner.last_turn_id.lock().await.clone();
            let maintenance = inner
                .compact_outcome
                .lock()
                .await
                .as_ref()
                .and_then(|outcome| outcome.turn_id.clone());
            if inner.state.lock().await.accepts_context_boundary(
                &turn_id,
                last_user.as_deref(),
                maintenance.as_deref(),
            ) {
                compact_command::invalidate_usage(inner, last_user).await;
            }
        }
        // Lifetime billing is retained even when same-turn occupancy is invalid.
        PlannerEventKind::TokenUsage { params } => {
            let reading_turn = item_turn_id(&params);
            if matches!(
                &*inner.state.lock().await,
                HarnessState::Compacting { .. } | HarnessState::CompactionRunning { .. }
            ) {
                return Ok(());
            }
            let mut preserve_invalidation = false;
            if let Some(invalidation) = inner.context_usage_invalidation.lock().await.as_ref() {
                let last = inner.last_turn_id.lock().await.clone();
                if reading_turn.is_none() || reading_turn != last.as_deref() {
                    return Ok(());
                }
                // A normal turn's result after an automatic boundary still carries its billed
                // lifetime total. Keep that for restart, without treating its occupancy proxy as
                // retained context. Manual maintenance usage never reaches this branch.
                preserve_invalidation = reading_turn == invalidation.user_turn_id.as_deref();
            } else if reading_turn.is_none()
                || reading_turn != inner.last_turn_id.lock().await.as_deref()
            {
                return Ok(());
            }
            match TokenUsage::from_params(&params, crate::model::now_ms()) {
                Some(incoming) => {
                    let mut slot = inner.token_usage.lock().await;
                    let merged = incoming.sticky_merge(slot.as_ref());
                    // Logged at ingest rather than in `TokenUsage::percent`, which runs once per client poll and
                    // would emit the same line forever for one bad frame.
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
                    if !preserve_invalidation {
                        *inner.context_usage_invalidation.lock().await = None;
                    }
                }
                // A frame without a usable `last.totalTokens` yields no reading; storing a zero would claim
                // an empty context, so the previous reading is left in place.
                None => tracing::warn!(
                    target: "planner.harness.token_usage",
                    runtime_id = %inner.worker_session_id,
                    card_id = %inner.card_id,
                    method = "thread/tokenUsage/updated",
                    "planner harness dropping thread/tokenUsage/updated: no usable \
                     (non-negative integer) tokenUsage.last.totalTokens in the frame"
                ),
            }
        }
        PlannerEventKind::ReplyDelta {
            turn_id,
            item_id,
            delta,
        } => {
            // Memory only: a delta writes neither a row nor the snapshot.
            live.delta(&turn_id, &item_id, &delta);
            return Ok(());
        }
        // The provider declares these frames have no harness state to change. In particular,
        // high-frequency reasoning deltas must not contend for the database's writer lock.
        PlannerEventKind::Ignored => return Ok(()),
    }
    persist_snapshot(inner).await
}

/// Whether a rewind removed `turn_id`: a late frame for it must not put a row back.
async fn is_rewound(inner: &Inner, turn_id: Option<&str>) -> bool {
    let Some(turn_id) = turn_id else {
        return false;
    };
    let rewound = inner.rewound_turns.lock().await.contains(turn_id);
    if rewound {
        tracing::debug!(
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            turn_id,
            "planner harness dropping a late frame of a rewound turn"
        );
    }
    rewound
}

fn item_turn_id(params: &Value) -> Option<&str> {
    params
        .get("turn")
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .or_else(|| params.get("turn_id").and_then(Value::as_str))
        .or_else(|| params.get("turnId").and_then(Value::as_str))
}

/// Live codex sends `userMessage`; the kernel stores `item.type` verbatim and tests have used snake case.
pub(super) fn is_user_message_type(item_type: Option<&str>) -> bool {
    calm_types::harness::is_native_user_message_type(item_type)
}

/// One transcript row for a codex `item/*` notification. `input_segments` is NULL for every
/// turn this binary issued (they live on the projection row); `legacy_segments_json` is the
/// exception for an older binary's in-flight turn.
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
    let owner = inner.clone();
    let thread = thread_id.to_owned();
    let turn = turn_id.map(str::to_owned);
    let id = item_uuid.map(str::to_owned);
    let kind = item_type.map(str::to_owned);
    let method = method.to_owned();
    let params = params_json.to_owned();
    let segments = legacy_segments_json.map(str::to_owned);
    write_in_tx_typed(inner.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let mut carrier = crate::db::sqlite::conversation::current_native_fact_tx(
                tx,
                owner.card_id.as_str(),
                &owner.worker_session_id,
                true,
            )
            .await?;
            Ok(crate::db::sqlite::record_native_item_tx(
                &mut carrier,
                &thread,
                turn.as_deref(),
                id.as_deref(),
                kind.as_deref(),
                &method,
                &params,
                segments.as_deref(),
            )
            .await?)
        })
    })
    .await
}

async fn emit_presentation_changed(
    inner: &Arc<Inner>,
    cursor: calm_types::conversation_display::ConversationCursor,
) -> Result<()> {
    let card = inner.card_id.clone();
    let worker = inner.worker_session_id.clone();
    let scope = harness_event_scope(inner, "harness.presentation.changed");
    let event = Event::HarnessPresentationChanged {
        card_id: card.clone(),
        track_id: inner.track_id.clone(),
        cursor,
    };
    crate::db::write_with_actor_events_typed(
        inner.repo.as_ref(),
        None,
        &inner.events,
        &crate::state::WriteContext::new(
            inner.card_role_cache.clone(),
            inner.track_area_cache.clone(),
        ),
        move |tx| {
            Box::pin(async move {
                crate::db::sqlite::conversation::current_input_fact_carrier_tx(
                    tx,
                    card.as_str(),
                    &worker,
                )
                .await?;
                Ok(((), vec![(ActorId::Kernel, scope, event)]))
            })
        },
    )
    .await?;
    Ok(())
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

/// The drained batch, written to the transcript BEFORE `turn/start` goes out, in the shape
/// codex will echo and keyed by `client_id` (= `clientUserMessageId`), `turn_id` NULL. One row
/// per drained turn. A stale projection under the same key is replaced, not joined.
async fn write_projection_row(
    inner: &Arc<Inner>,
    thread_id: &str,
    client_id: &str,
    segments: &[HarnessInputSegment],
) -> Result<i64> {
    let item_db_id = insert_projection_row(inner, thread_id, client_id, segments).await?;
    // The existing per-row event, so every client refetches the transcript now rather than at the echo.
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

/// The row half of [`write_projection_row`], without the announcement; a steer writes the same
/// row once codex has taken the input and announces it with the departure.
async fn insert_projection_row(
    inner: &Arc<Inner>,
    thread_id: &str,
    client_id: &str,
    segments: &[HarnessInputSegment],
) -> Result<i64> {
    // Without the diff block: it is context the kernel prepends for codex, and no transcript
    // reader wants it.
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
    let owner = inner.clone();
    let thread = thread_id.to_owned();
    let client = client_id.to_owned();
    let (item_db_id, stale) = write_in_tx_typed(inner.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let mut carrier = crate::db::sqlite::conversation::current_native_fact_tx(
                tx,
                owner.card_id.as_str(),
                &owner.worker_session_id,
                true,
            )
            .await?;
            Ok(crate::db::sqlite::record_native_input_projection_tx(
                &mut carrier,
                &thread,
                &client,
                &params_json,
                &input_segments,
            )
            .await?)
        })
    })
    .await?;
    if stale > 0 {
        tracing::debug!(worker_session_id = %inner.worker_session_id, card_id = %inner.card_id,
            client_id, stale, "planner harness replaced a stale user-message projection");
    }
    Ok(item_db_id)
}

/// Defensive cap on the track-vcs diff-block fetch; the diff block is never a correctness
/// requirement, so a stalled SELECT becomes a warn and a degraded turn.
const SINCE_LAST_TURN_DIFF_TIMEOUT: Duration = Duration::from_secs(5);
const TRANSCRIPT_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);
const SINCE_LAST_TURN_HEAD_FALLBACK_TIMEOUT: Duration = Duration::from_secs(1);

/// How long to leave a card alone after an attempt that could still succeed on its own; without
/// it the re-buffered batch re-arms `hard_fire` and the next tick tries again 50 ms later.
const TRANSIENT_RETRY_DELAY: Duration = Duration::from_secs(2);

/// The same, for an attempt that CANNOT succeed until a person changes something. Still a poll
/// rather than a full stop because the fix may arrive from outside this process.
const NEEDS_A_CHOICE_RETRY_DELAY: Duration = Duration::from_secs(30);

impl SelectionSource for Inner {
    fn check_current(&self) -> Result<()> {
        self.historical_queue().map(drop)
    }

    fn admit_native_read(&self) -> futures_util::future::BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // Preserve the historical caller's existing authorization, not a
            // grant derived from a schema number. Canonical requires its bound
            // live-source admission and can never use this compatibility path.
            self.historical_queue()?;
            Ok(())
        })
    }
    /// Read the card as it stands NOW — the last moment before the frame is built; reading any
    /// earlier is the staleness bug this exists to prevent.
    async fn card_payload(&self) -> std::result::Result<Value, IssuanceRefusal> {
        match self.repo.card_get(self.card_id.as_str()).await {
            // A read that failed is a read that can succeed next time.
            Err(e) => Err(IssuanceRefusal::retryable(format!(
                "could not re-read the card to resolve its model: {e}"
            ))),
            // A card that is gone has nobody left to tell; transient keeps the loop cheap without
            // putting a message on a surface no one is looking at.
            Ok(None) => Err(IssuanceRefusal::retryable(
                "the card this conversation belongs to no longer exists".into(),
            )),
            Ok(Some(card)) => Ok(card.payload),
        }
    }

    /// It must be the path `thread/start` was given, or `config/read` answers a question we did
    /// not ask; an unreadable track is an `Err`, not a `None`.
    async fn installation_cwd(&self) -> std::result::Result<String, IssuanceRefusal> {
        // Deterministic card-read-then-track-read window. No-op in production.
        wait_at_planner_harness_cwd_race_hook(self.worker_session_id.as_str()).await;
        match self.repo.track_get(self.track_id.as_str()).await {
            Ok(Some(track)) => Ok(track.workspace.agent_cwd().to_string()),
            // "There is no workspace" and "we could not read the workspace" are different facts, and
            // neither means "read the global layers instead". `Ok(None)` IS reachable: a
            // `track_delete_tx` can commit between the card read and this one.
            Ok(None) => Err(IssuanceRefusal::retryable(format!(
                "track {} is not readable, so this conversation's config scope is unknown",
                self.track_id
            ))),
            Err(e) => Err(IssuanceRefusal::retryable(format!(
                "could not read the track workspace for a config/read cwd: {e}"
            ))),
        }
    }
}

/// Record a refusal: how long before the next attempt, and what (if anything) the reader is told.
async fn apply_refusal(inner: &Arc<Inner>, failure: &IssuanceRefusal) {
    let (delay, notice) = match failure.kind {
        // Nobody can act, and repeating may work. Silent while that is
        // plausibly still true; see `transient_notice`.
        FailureKind::Retryable => (TRANSIENT_RETRY_DELAY, transient_notice(inner).await),
        // The provider refused. Retried slowly rather than not at all, but the reader is told now.
        FailureKind::Rejected => (NEEDS_A_CHOICE_RETRY_DELAY, Some(failure.reader.clone())),
        FailureKind::NeedsAChoice => (NEEDS_A_CHOICE_RETRY_DELAY, Some(failure.reader.clone())),
    };
    *inner.issuance_retry_after.lock().await = Some(Instant::now() + delay);
    *inner.issuance_block.lock().await = notice;
}

/// What to tell the reader about a run of RETRYABLE refusals — nothing at first (a codex
/// restart lasts seconds), then past `transient_silence_budget` that the conversation is waiting.
/// Pacing bounded the retry's RATE; this bounds its SILENCE.
async fn transient_notice(inner: &Arc<Inner>) -> Option<String> {
    let now = Instant::now();
    let began = {
        let mut since = inner.refusing_since.lock().await;
        *since.get_or_insert(now)
    };
    let provider = inner.backend.provider().wire_name();
    (now.duration_since(began) >= inner.config.transient_silence_budget).then(|| {
        format!(
            "Waiting for {provider} — it has not accepted this conversation's last few turns. \
             Your message is still queued and will be sent when it answers."
        )
    })
}

/// Fixtures-only: park between the card read and the track read, so a test can order "the
/// track is deleted" between them. The window is real: a foreign key is an invariant over one
/// transaction, never over a read-then-read across an await.
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

/// Consume only successful bookkeeping for a Done Track; the event log and accepted push
/// watermark stay intact, other observations keep their queue order.
async fn consume_completed_worktree_commits(inner: &Arc<Inner>) -> Result<()> {
    let is_commit = |entry: &QueueEntry| {
        matches!(entry, QueueEntry::System {
            observation: Observation::WorktreeCommitted { track_id, .. }, ..
        } if track_id == &inner.track_id)
    };
    if !inner.historical_queue()?.lock().await.iter().any(is_commit) {
        return Ok(());
    }
    if !inner
        .repo
        .track_get(inner.track_id.as_str())
        .await?
        .is_some_and(|track| !track.is_open())
    {
        return Ok(());
    }
    let checkpoint = checkpoint_durable_user_message(inner).await?;
    let consumed = {
        let mut queue = inner.historical_queue()?.lock().await;
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
        restore_durable_user_message(inner, checkpoint).await?;
        return Err(error);
    }
    tracing::debug!(
        track_id = %inner.track_id,
        consumed,
        "consumed successful worktree commit observations for a Done Track"
    );
    Ok(())
}

/// True iff the queue is non-empty and EVERY entry is a `ReportEdited` observation.
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

/// True iff the queue is non-empty and EVERY entry is a `ReportEdited` carrying its
/// `body_before`, so the since-last-turn unified patch may be omitted as the same change told twice.
fn queue_report_edits_all_carry_diffs(queue: &VecDeque<QueueEntry>) -> bool {
    !queue.is_empty()
        && queue.iter().all(|entry| {
            matches!(
                entry,
                QueueEntry::System {
                    observation: Observation::ReportEdited {
                        body_before: Some(_),
                        ..
                    },
                    ..
                }
            )
        })
}

/// The turn's channel statement, appended once at the end of the batch (a user message drained
/// with an edit opens an ordinary turn, so per-observation text would tell the planner to
/// swallow the request) and only when `queue_report_edits_all_carry_diffs` holds.
const REPORT_EDIT_BATCH_CHANNEL_LINE: &str = "This is a background sync turn: \
    an ordinary reply here is folded away by the front end. \
    Call neige_user_ask only for a conflict with work still in flight, \
    data you cannot parse, or a decision only the user can make; \
    otherwise end the turn silently.\n";

/// Close a batch that is nothing but report edits (each with its diff) with the channel line on
/// the last segment; "the patch is omitted" and "the channel line is present" are one fact.
fn append_report_edit_batch_channel_line(
    segments: &mut [HarnessInputSegment],
    all_report_edits_with_diffs: bool,
) {
    if !all_report_edits_with_diffs {
        return;
    }
    let Some(last) = segments.last_mut() else {
        return;
    };
    if !last.text.ends_with('\n') {
        last.text.push('\n');
    }
    last.text.push_str(REPORT_EDIT_BATCH_CHANNEL_LINE);
}

/// A hold never drains or revises the queue. Recovery removes only the auth pacing it installed.
async fn provider_holds_issuance(inner: &Inner) -> bool {
    if inner.backend.issuance_hold().is_some() {
        inner.provider_held.store(true, Ordering::SeqCst);
        *inner.issuance_retry_after.lock().await = None;
        // The accessor exposes this provider-owned reader. Do not overwrite a
        // model-choice refusal: it may still be true when this hold clears.
        return true;
    }
    if inner.provider_held.swap(false, Ordering::SeqCst) {
        *inner.issuance_retry_after.lock().await = None;
        if inner.provider_hold_owns_block.swap(false, Ordering::SeqCst) {
            *inner.issuance_block.lock().await = None;
            *inner.refusing_since.lock().await = None;
        }
    }
    false
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

/// Wrap `since_last_turn_diff_block` in a 5s timeout; on timeout the turn still issues without a diff block.
async fn diff_with_timeout(
    inner: &Arc<Inner>,
    current_override: Option<&track_vcs::CommitHash>,
    report_patch: track_vcs::ReportPatch,
) -> track_vcs::SinceLastTurnBlock {
    diff_or_fallback_on_timeout(
        since_last_turn_diff_block(inner, current_override, report_patch),
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

    let pool = inner.repo.sqlite_db()?;

    match tokio::time::timeout(
        SINCE_LAST_TURN_HEAD_FALLBACK_TIMEOUT,
        track_vcs::head(pool.read_pool(), &inner.track_id),
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
    report_patch: track_vcs::ReportPatch,
) -> track_vcs::SinceLastTurnBlock {
    let Some(pool) = inner.repo.sqlite_db() else {
        return track_vcs::SinceLastTurnBlock::empty();
    };
    let last_seen_head = inner.last_seen_head.lock().await.clone();
    let mut diff = match track_vcs::since_last_turn_block(
        pool.read_pool(),
        &inner.track_id,
        last_seen_head.as_deref(),
        current_override,
        Some(&inner.card_id),
        &calm_types::worker::WorkerSessionId::from(inner.worker_session_id.clone()),
        report_patch,
    )
    .await
    {
        Ok(diff) => diff,
        Err(e) => {
            let current_head = match current_override {
                Some(current) => Some(current.clone()),
                None => match track_vcs::head(pool.read_pool(), &inner.track_id).await {
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
    };
    if report_patch == track_vcs::ReportPatch::Include {
        match crate::report_read_ledger::delta::for_track(
            inner.repo.as_ref(),
            &inner.track_id,
            &inner.worker_session_id,
        )
        .await
        {
            Ok(report) if !report.is_empty() => {
                let block = diff.block.get_or_insert_with(String::new);
                if !block.is_empty() {
                    block.push('\n');
                }
                block.push_str(&report);
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "Report knowledge suggestion unavailable"),
        }
    }
    diff
}

fn prepend_diff_block(diff_block: Option<String>, observation_text: String) -> String {
    match diff_block {
        Some(diff) => format!("{diff}\n\n---\n\n{observation_text}"),
        None => observation_text,
    }
}

async fn rebuffer_head(inner: &Arc<Inner>, drained: Vec<QueueEntry>) -> Result<()> {
    rebuffer_head_retiring(inner, drained, &[]).await
}

async fn rebuffer_head_retiring(
    inner: &Arc<Inner>,
    drained: Vec<QueueEntry>,
    restored: &[QueueEntryId],
) -> Result<()> {
    let mut queue = inner.historical_queue()?.lock().await;
    let mut debounce = inner.debounce.lock().await;
    let mut projections = inner.restored_projections.lock().await;
    // Acquire every lock before changing memory: cancellation cannot register retirement
    // without also re-buffering the input whose durable snapshot authorizes that retirement.
    projections.extend_from_slice(restored);
    // A re-buffered batch keeps the ids it was drained with: the same instances going back.
    for entry in drained.into_iter().rev() {
        queue.push_front(entry);
    }
    let now = Instant::now();
    *debounce = DebounceState {
        first_pending_at: Some(now),
        last_pending_at: Some(now),
        hard_fire: true,
    };
    Ok(())
}

async fn watchdog_tick(inner: &Arc<Inner>) -> Result<()> {
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("conversation-watchdog-tick", inner.card_id.as_str()).await;
    held::pause_watchdog_while_held(inner).await;
    let compact_timed_out =
        compact_command::observation_expired(&*inner.state.lock().await, &inner.config);
    if compact_timed_out && inner.inputs.canonical().is_none() {
        compact_command::settle(
            inner,
            crate::harness::compact_outcome::CompactStatus::Unconfirmed,
        )
        .await;
        *inner.state.lock().await = HarnessState::Wedged {
            since: Instant::now(),
            reason: "Compaction did not report a start; reset the conversation to continue.".into(),
        };
        persist_snapshot(inner).await?;
        return Ok(());
    }
    let resume_elapsed = {
        let state = inner.state.lock().await;
        match &*state {
            HarnessState::Resumed { resumed_at } => {
                Instant::now().duration_since(*resumed_at) >= inner.config.resumed_reconcile_budget
            }
            _ => false,
        }
    };
    if resume_elapsed && inner.inputs.canonical().is_none() {
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

    let interrupt_expired = inner
        .interrupt_deadline
        .lock()
        .await
        .as_ref()
        .is_some_and(|(_, deadline)| Instant::now() >= *deadline);
    if interrupt_expired {
        if inner.inputs.canonical().is_some() {
            return canonical::control::failed(
                inner,
                calm_types::harness::HARNESS_INTERRUPT_TIMEOUT_REASON,
            )
            .await;
        }
        *inner.state.lock().await = HarnessState::Wedged {
            since: Instant::now(),
            reason: calm_types::harness::HARNESS_INTERRUPT_TIMEOUT_REASON.into(),
        };
        *inner.issued_turn_id.lock().await = None;
        *inner.interrupt_deadline.lock().await = None;
        persist_snapshot(inner).await?;
        return Ok(());
    }

    let turn_limit = budget::limit(inner).await;
    let should_interrupt = {
        let state = inner.state.lock().await;
        match &*state {
            HarnessState::TurnRunning {
                turn_id,
                watchdog_from,
                ..
            } if Instant::now().duration_since(*watchdog_from) >= turn_limit => {
                Some(turn_id.clone())
            }
            _ => None,
        }
    };
    if let Some(turn_id) = should_interrupt {
        issue_interrupt_for_turn(inner, turn_id, "max_turn_duration".into()).await?;
    } else {
        budget::warn(inner).await?;
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
            inner.backend.active_turn_id_for_thread(&thread_id)
        }
        None => None,
    };
    let Some(turn_id) = turn_id else {
        tracing::debug!("planner harness interrupt ignored because no active turn id is known");
        return Ok(());
    };
    issue_interrupt_for_turn(inner, turn_id, reason).await
}

async fn snapshot_for(inner: &Arc<Inner>) -> Result<HarnessSnapshot> {
    let state = inner.state.lock().await.clone();
    snapshot_from_state(inner, &state).await
}

async fn snapshot_from_state(inner: &Arc<Inner>, state: &HarnessState) -> Result<HarnessSnapshot> {
    let entries = inner
        .historical_queue()?
        .lock()
        .await
        .iter()
        .cloned()
        .collect();
    let push_watermark = *inner.push_watermark.lock().await;
    let last_thread_id = inner.thread_id.read().await.clone();
    let last_turn_id = inner.last_turn_id.lock().await.clone();
    let issued_turn_head = inner.issued_turn_head.lock().await.clone();
    let projection_client_id = inner.projection_client_id.lock().await.clone();
    let last_report_body_sha256 = inner.last_report_body_sha256.lock().await.clone();
    let last_seen_head = inner.last_seen_head.lock().await.clone();
    let token_usage = inner.token_usage.lock().await.clone();
    let mut snapshot = HarnessSnapshot::from_state(
        state,
        inner.turn_budget.lock().await.clone(),
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
    snapshot.compact_outcome = inner.compact_outcome.lock().await.clone();
    snapshot.planner_compaction = inner.planner_compaction.lock().await.clone();
    let invalidation = inner.context_usage_invalidation.lock().await;
    snapshot.context_usage_invalidated = invalidation.is_some();
    snapshot.context_usage_invalidated_turn = invalidation
        .as_ref()
        .and_then(|fact| fact.user_turn_id.clone());
    snapshot.interruption_intent = inner.interruption_intent.lock().await.clone();
    snapshot.pending_rewind = inner.pending_rewind.lock().await.clone();
    snapshot.last_turn_base = inner.last_turn_base.lock().await.clone();
    Ok(snapshot)
}

/// Is this runtime still the row the card is being driven from? A pool read of one row by id —
/// NOT `write_in_tx_typed` (takes the single writer lock on every tick) and NOT
/// `session_projection_by_id` (card-backed, so a row the card moved off answers `None`).
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

/// The original issuance writer keeps its closing/retired boundary. Its caller must check
/// whether either guarded transaction actually wrote; a skipped checkpoint is not settlement.
async fn persist_issuance_outcome(inner: &Arc<Inner>) -> Result<bool> {
    let active_written = persist_snapshot_inner(inner, None, None, false).await?;
    if !inner.shutting_down.load(Ordering::SeqCst)
        && runtime_is_still_the_live_carrier(inner).await?
    {
        return Ok(active_written);
    }
    let retired_written = persist_retired_issuance_snapshot(inner).await?;
    Ok(active_written || retired_written)
}

async fn persist_retired_issuance_snapshot(inner: &Arc<Inner>) -> Result<bool> {
    let mut completion = inner.input_commit.lock().await;
    let restored = inner.restored_projections.lock().await.clone();
    let retired = restored.clone();
    let card = inner.card_id.to_string();
    let snapshot = snapshot_for(inner).await?;
    let worker_session_id = inner.worker_session_id.clone();
    let snapshot_value = serde_json::to_value(snapshot)?;
    let now = crate::model::now_ms();
    let written = write_in_tx_typed(inner.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let written = crate::db::sqlite::session_set_handle_state_of_retired_runtime_tx(
                tx,
                &worker_session_id,
                Some(snapshot_value),
                now,
            )
            .await?;
            if written {
                retire_restored_projections(tx, &card, &restored).await?;
            }
            Ok(written)
        })
    })
    .await?;
    if written {
        *completion = None;
        inner
            .restored_projections
            .lock()
            .await
            .retain(|id| !retired.contains(id));
    }
    Ok(written)
}

async fn clear_interruption_intent(inner: &Arc<Inner>, turn_id: &str) {
    let mut intent = inner.interruption_intent.lock().await;
    if intent
        .as_ref()
        .is_some_and(|intent| intent.turn_id() == turn_id)
    {
        *intent = None;
    }
}

/// Make a turn's terminal status durable: one `turn/completed` row per finished turn, written
/// AFTER the arm's gates and BEFORE `persist_snapshot_stamping_issued_head`, so the phase event
/// doubles as the delivery signal (no item-added event). Best-effort: a failed insert is logged.
async fn persist_turn_outcome(inner: &Arc<Inner>, turn_id: &str, turn: &Value) -> Option<i64> {
    // `thread_id` is NOT NULL and Codex's `TurnCompleted.thread_id` is `unwrap_or_default()`
    // upstream, so the harness's own thread is the only value that is never `""`.
    let Some(thread_id) = inner.thread_id.read().await.clone() else {
        tracing::warn!(
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            turn_id,
            "planner harness skipping turn/completed row: no thread is known yet"
        );
        return None;
    };
    match crate::harness::turn_outcome::record(
        inner.repo.as_ref(),
        &inner.worker_session_id,
        inner.card_id.as_str(),
        inner.track_id.as_str(),
        &thread_id,
        turn_id,
        turn,
        if inner.backend.terminal_event_confirms_stop() {
            crate::harness::turn_outcome::Confirmation::ProviderTerminal
        } else {
            crate::harness::turn_outcome::Confirmation::Unconfirmed
        },
    )
    .await
    {
        Ok(id) => {
            clear_interruption_intent(inner, turn_id).await;
            Some(id)
        }
        Err(error) => {
            tracing::warn!(
                worker_session_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                turn_id,
                error = %error,
                "planner harness could not persist turn outcome row"
            );
            None
        }
    }
}

async fn persist_failed_system_error_snapshot(inner: &Arc<Inner>) -> Result<()> {
    if inner.shutting_down.load(Ordering::SeqCst) {
        return Err(CalmError::Conflict("conversation is shutting down".into()));
    }
    persist_guarded_failed_system_error_snapshot(inner).await
}

async fn persist_guarded_failed_system_error_snapshot(inner: &Arc<Inner>) -> Result<()> {
    let mut completion = inner.input_commit.lock().await;
    let restored = inner.restored_projections.lock().await.clone();
    let retired = restored.clone();
    let snapshot = serde_json::to_value(snapshot_for(inner).await?)?;
    let card = inner.card_id.to_string();
    let id = inner.worker_session_id.clone();
    let written = write_in_tx_typed(inner.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let written = crate::db::sqlite::session_set_failed_harness_snapshot_tx(
                tx, &card, &id, &snapshot,
            )
            .await?;
            if written {
                retire_restored_projections(tx, &card, &restored).await?;
            }
            Ok(written)
        })
    })
    .await?;
    if written {
        *completion = None;
        inner
            .restored_projections
            .lock()
            .await
            .retain(|id| !retired.contains(id));
        Ok(())
    } else {
        Err(CalmError::Conflict(
            "failed conversation changed while settling its input".into(),
        ))
    }
}

async fn persist_snapshot(inner: &Arc<Inner>) -> Result<()> {
    persist_snapshot_inner(inner, None, None, false)
        .await
        .map(|_| ())
}

/// Persist a durable user send, and REFUSE it if the row was not written: the writer matches
/// nothing once the row leaves the active set.
/// Already accepted commands finish persistence even when shutdown has woken a track waiter.
async fn persist_snapshot_for_durable_send(
    inner: &Arc<Inner>,
    key: &SendKey,
    entry_id: Option<&QueueEntryId>,
    source: Option<crate::conversation_inputs::HistoricalHumanSource>,
) -> Result<()> {
    let mut bind = DurableSendBinding::new(inner, key, entry_id);
    bind.source = source;
    if persist_guarded_snapshot(inner, None, Some(bind), false).await? {
        return Ok(());
    }
    Err(CalmError::PlannerHarnessRuntimeSuperseded(
        "this runtime is no longer the card's; your message was not stored — send it again".into(),
    ))
}

async fn persist_snapshot_stamping_issued_head(inner: &Arc<Inner>) -> Result<()> {
    let issued_head = inner.issued_turn_head.lock().await.clone();
    let _written = persist_snapshot_inner(inner, issued_head.clone(), None, false).await?;
    if issued_head.is_some() {
        *inner.last_seen_head.lock().await = issued_head;
    }
    *inner.issued_turn_head.lock().await = None;
    Ok(())
}

/// A durable send's key binding, written by the same transaction as the snapshot that holds it.
struct DurableSendBinding {
    source: Option<crate::conversation_inputs::HistoricalHumanSource>,
    idempotency_key: String,
    binding: crate::db::sqlite::PlannerInputBinding,
}

impl DurableSendBinding {
    /// `key` bound to the answer this runtime gives: its session and the entry the message is in.
    fn new(inner: &Inner, key: &SendKey, entry_id: Option<&QueueEntryId>) -> Self {
        Self {
            source: None,
            idempotency_key: key.idempotency_key.clone(),
            binding: crate::db::sqlite::PlannerInputBinding {
                payload_hash: key.payload_hash.clone(),
                worker_session_id: inner.worker_session_id.clone(),
                entry_id: entry_id.map(|id| id.as_str().to_string()),
            },
        }
    }
}

/// Shared by snapshot owners, inside their guarded write transaction.
async fn retire_restored_projections(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card: &str,
    restored: &[QueueEntryId],
) -> Result<()> {
    for id in restored {
        crate::db::sqlite::transcript_projection_delete_tx(tx, card, id.as_str()).await?;
    }
    Ok(())
}

/// Returns whether the runtime's own row was written. `false` means the write
/// matched no row — the runtime is shutting down, or its row has left the
/// active set — and a caller that promised durability must not report success.
/// `send` is bound only when the row was written, in the same transaction.
async fn persist_snapshot_inner(
    inner: &Arc<Inner>,
    last_seen_head_override: Option<track_vcs::CommitHash>,
    send: Option<DurableSendBinding>,
    compact_start: bool,
) -> Result<bool> {
    if inner.shutting_down.load(Ordering::SeqCst) {
        return Ok(false);
    }
    persist_guarded_snapshot(inner, last_seen_head_override, send, compact_start).await
}

/// The existing guarded transaction, also used by the shared owner release entry.
/// Closing an owner suppresses ordinary checkpoints, but does not invalidate its row guard.
async fn persist_guarded_snapshot(
    inner: &Arc<Inner>,
    last_seen_head_override: Option<track_vcs::CommitHash>,
    send: Option<DurableSendBinding>,
    compact_start: bool,
) -> Result<bool> {
    if inner.inputs.canonical().is_some() {
        if send.is_some() || last_seen_head_override.is_some() {
            return Err(CalmError::Conflict(
                "historical snapshot mutation cannot change canonical input".into(),
            ));
        }
        return Err(CalmError::Conflict(
            "canonical runtime mutations require their committed frame owner".into(),
        ));
    }
    // The truncation's record goes in BEFORE the truncation does. `?` and not a warn: a write that
    // proceeded would make a discarded user message permanently gone with nothing saying so.
    if let Some(source) = send.as_ref().and_then(|send| send.source.as_ref()) {
        source
            .require_card(inner.repo.as_ref(), inner.card_id.as_str())
            .await?;
    }
    flush_dropped_announcements(inner).await?;
    let mut completion = inner.input_commit.lock().await;
    let restored = inner.restored_projections.lock().await.clone();
    let retired = restored.clone();
    let mut snapshot = snapshot_for(inner).await?;
    if let Some(head) = last_seen_head_override {
        snapshot.last_seen_head = Some(head);
        snapshot.issued_turn_head = None;
    }
    let runtime_id = inner.worker_session_id.clone();
    let thread_id = snapshot.last_thread_id.clone();
    let active_turn_id = match snapshot.phase {
        HarnessPhaseTag::TurnRunning | HarnessPhaseTag::IssuingInterrupt => {
            snapshot.last_turn_id.clone()
        }
        HarnessPhaseTag::Compacting => inner.state.lock().await.active_turn_id(),
        _ => None,
    };
    let state_for_status = inner.state.lock().await.clone();
    let status = run_status_for(&state_for_status);
    let new_phase = snapshot.phase;
    let wake = self_compact::prepare_wake(inner, &mut snapshot);
    let compact_request = snapshot.planner_compaction.clone();
    let compact_track = inner.track_id.clone();
    let compact_scope = harness_event_scope(inner, "task.completed");
    let snapshot_value = serde_json::to_value(&snapshot)?;
    let repo = Arc::clone(&inner.repo);
    let bound_card_id = inner.card_id.to_string();

    let written = write_in_tx_typed(repo.as_ref(), move |tx| {
        Box::pin(async move {
            if let Some(source) = send.as_ref().and_then(|send| send.source.as_ref()) {
                source.require_card_tx(tx, &bound_card_id).await?;
            }
            let written = crate::db::sqlite::session_set_handle_state_tx(
                tx,
                &runtime_id,
                Some(snapshot_value),
            )
            .await?;
            if written {
                retire_restored_projections(tx, &bound_card_id, &restored).await?;
            }
            if written && let Some(send) = &send {
                crate::db::sqlite::planner_input_bind_tx(
                    tx,
                    &bound_card_id,
                    &send.idempotency_key,
                    &send.binding,
                )
                .await?;
            }
            // A refused snapshot must not retire its own active guard by mirroring a
            // terminal phase that never committed. Retry still owns that transition.
            if written {
                crate::db::sqlite::session_set_harness_observation_runtime_tx(
                    tx,
                    &runtime_id,
                    status,
                    thread_id.as_deref(),
                    active_turn_id.as_deref(),
                )
                .await?;
            }
            let task_events = if written {
                self_compact::append_task_lifecycle(
                    tx,
                    &compact_track,
                    compact_request.as_ref(),
                    &compact_scope,
                    compact_start,
                )
                .await?
            } else {
                vec![]
            };
            let wake = if written {
                self_compact::append_wake(tx, wake).await?
            } else {
                None
            };
            Ok((written, (wake, task_events)))
        })
    })
    .await;
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point(
        "conversation-input-checkpoint-attempted",
        inner.card_id.as_str(),
    )
    .await;
    let (written, (wake, task_events)) = written?;
    if !written {
        return Ok(false);
    }
    *completion = None;
    // Publish lifecycle before wake, preserving committed event order.
    for event in task_events {
        inner.events.emit_envelope(event);
    }
    self_compact::publish_wake(inner, snapshot.planner_compaction.clone(), wake).await;
    drop(completion);
    inner
        .restored_projections
        .lock()
        .await
        .retain(|id| !retired.contains(id));

    objective_usage::observe(
        inner,
        new_phase,
        snapshot.last_turn_id.as_deref(),
        crate::harness::token_usage::planner_context_usage(&snapshot).map(|r| r.used_tokens),
        false,
    )
    .await;
    emit_phase_refresh(
        inner,
        new_phase,
        (
            inner.compact_outcome.lock().await.clone(),
            inner.context_usage_invalidation.lock().await.is_some(),
            None,
        ),
    )
    .await;
    Ok(written)
}

async fn emit_phase_refresh(
    inner: &Inner,
    new_phase: HarnessPhaseTag,
    feedback: (
        Option<crate::harness::compact_outcome::CompactOutcome>,
        bool,
        Option<canonical::PublicFeedback>,
    ),
) {
    let mut last_feedback = inner.last_context_feedback.lock().await;
    let mut last_phase = inner.last_phase.lock().await;
    if *last_phase == new_phase && *last_feedback == feedback {
        return;
    }
    let scope = harness_event_scope(inner, "harness.phase.changed");
    if let Err(error) = inner
        .repo
        .log_pure_event(
            ActorId::Kernel,
            scope,
            None,
            &inner.events,
            &inner.card_role_cache,
            &inner.track_area_cache,
            Event::HarnessPhaseChanged {
                worker_session_id: inner.worker_session_id.clone(),
                card_id: inner.card_id.clone(),
                track_id: inner.track_id.clone(),
                old_phase: *last_phase,
                new_phase,
            },
        )
        .await
    {
        tracing::warn!(%error,"phase refresh failed after commit; retaining previous emitted state for retry");
        return;
    }
    *last_phase = new_phase;
    *last_feedback = feedback;
}

fn state_from_runtime_header(
    snapshot: &calm_types::harness::snapshot::HarnessRuntimeHeader,
    inputs: &RuntimeInputs,
) -> HarnessState {
    let now = Instant::now();
    match snapshot.phase {
        HarnessPhaseTag::PendingThreadStart => HarnessState::PendingThreadStart,
        HarnessPhaseTag::Idle => HarnessState::Idle,
        HarnessPhaseTag::Compacting => match inputs {
            RuntimeInputs::Historical(_) => HarnessState::Wedged { since: now, reason: "Context compaction was interrupted by a server restart; reset the conversation to continue.".into() },
            // Canonical recovery settles the persisted guard before construction.
            // A live publication therefore projects the committed maintenance phase.
            RuntimeInputs::Canonical(_) => match snapshot.compact_outcome.as_ref().and_then(|outcome| outcome.turn_id.clone()) {
                Some(turn_id) => HarnessState::CompactionRunning { turn_id, started_at: now },
                None => HarnessState::Compacting { since: now },
            },
        },
        HarnessPhaseTag::IssuingTurn => {
            if inputs.canonical().is_some() {
                // An unresolved canonical invocation is still issuing. Neither
                // a missing native ID nor restart makes it a completed turn.
                HarnessState::Issuing {
                    since: now,
                    kind: IssuingKind::TurnStart,
                }
            } else if snapshot.last_turn_id.is_some() {
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
        HarnessPhaseTag::TurnCompleted => {
            let last_turn_id = if inputs.canonical().is_some() {
                snapshot.last_turn_id.clone()
                    .expect("canonical load checks the completed turn identity")
            } else {
                snapshot.last_turn_id.clone().unwrap_or_else(|| "unknown-turn".into())
            };
            HarnessState::TurnCompleted { last_turn_id }
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
    use super::{HarnessObservationDelivery, harness_tick, map_observation_send_error};
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

    #[test]
    fn report_edit_batch_channel_line_follows_the_omit_predicate() {
        use super::{
            REPORT_EDIT_BATCH_CHANNEL_LINE, append_report_edit_batch_channel_line,
            queue_report_edits_all_carry_diffs,
        };
        use crate::harness::queue::input_segments_for_entries;
        use crate::ids::{CardId, TrackId};
        use calm_types::event::EditAuthor;
        use std::collections::VecDeque;

        let edit = |before: Option<&str>| {
            QueueEntry::system(
                Observation::ReportEdited {
                    presentation: Default::default(),
                    track_id: TrackId::from("track-1"),
                    body_sha256: "sha".into(),
                    body: "# T\n\nnew\n".into(),
                    author: Some(EditAuthor::User),
                    body_before: before.map(str::to_string),
                    doc_rev_after: None,
                    blocks_after: None,
                },
                None,
            )
            .expect("a report edit is a system entry")
        };
        let card_id = CardId::from("card-1");
        let render = |queue: VecDeque<QueueEntry>| {
            let entries = queue.iter().cloned().collect::<Vec<_>>();
            let mut segments = input_segments_for_entries(&card_id, &entries);
            append_report_edit_batch_channel_line(
                &mut segments,
                queue_report_edits_all_carry_diffs(&queue),
            );
            segments
        };

        let pure = render(VecDeque::from(vec![
            edit(Some("# T\n\nold\n")),
            edit(Some("# T\n\nolder\n")),
        ]));
        assert!(
            !pure[0].text.contains(REPORT_EDIT_BATCH_CHANNEL_LINE),
            "not on an inner segment: {}",
            pure[0].text
        );
        assert_eq!(
            pure[1].text.matches(REPORT_EDIT_BATCH_CHANNEL_LINE).count(),
            1
        );
        assert!(pure[1].text.ends_with(REPORT_EDIT_BATCH_CHANNEL_LINE));

        let mixed = render(VecDeque::from(vec![
            QueueEntry::user_message("what changed?".into(), None, Vec::new()),
            edit(Some("# T\n\nold\n")),
        ]));
        assert!(
            mixed
                .iter()
                .all(|s| !s.text.contains(REPORT_EDIT_BATCH_CHANNEL_LINE))
        );

        let legacy = render(VecDeque::from(vec![edit(None)]));
        assert!(!legacy[0].text.contains(REPORT_EDIT_BATCH_CHANNEL_LINE));
    }
}

pub(crate) mod budget;
mod compact_command;
#[cfg(test)]
mod compact_tests;
mod durable_command;
mod held;
pub(crate) mod hibernation;
mod input_commit;
pub(crate) mod input_inspection;
#[cfg(test)]
mod input_inspection_recovery_tests;
#[cfg(test)]
mod input_inspection_tests;
#[cfg(test)]
mod input_version_tests;
mod live_reply;
mod native_ask;
pub(super) mod provider_execution;
mod replace_command;
mod timeout_notice;
mod turn_permission;

#[cfg(test)]
mod checkpoint_tests;
#[cfg(test)]
pub(super) async fn registry_handle_for_test(worker_session_id: &str) -> PlannerHarness {
    completed_commit_tests::Fixture::for_worker_session(worker_session_id.to_owned())
        .await
        .harness
}

#[cfg(test)]
mod completed_commit_tests;
#[cfg(test)]
mod notification_write_tests;

#[cfg(test)]
mod report_edit_replay_tests;

#[cfg(test)]
mod result_receipt_tests;

#[cfg(test)]
mod turn_origin_tests;

#[cfg(test)]
mod workspace_report_tests;

#[cfg(test)]
mod control_mapping_tests;
#[cfg(test)]
mod conversation_title_tests;
#[cfg(test)]
mod start_mapping_tests;

mod interrupt;
pub(crate) mod issuance;
use interrupt::issue_interrupt_for_turn;

async fn maybe_issue_turn(inner: &Arc<Inner>) -> Result<()> {
    if inner.inputs.canonical().is_some() {
        canonical::tick(inner, false).await
    } else {
        issuance::maybe_issue_turn(inner).await
    }
}

#[cfg(test)]
mod context_usage_tests;
