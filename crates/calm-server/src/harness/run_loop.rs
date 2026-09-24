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
use tokio::task::AbortHandle;

use crate::card_role_cache::CardRoleCache;
use crate::codex_appserver::{InputItem, Notification};
use crate::db::{Repo, write_in_tx_typed};
use crate::error::{CalmError, Result};
use crate::event::{Event, EventBus, EventScope, HarnessQueueChange};
use crate::harness::backend::PlannerBackend;
use crate::harness::config::HarnessConfig;
use crate::harness::observation::Observation;
use crate::harness::queue::{
    FoldOutcome, MutationApplied, MutationRefused, MutationResult, QueueEntry, QueueEntryId,
    QueueMutation, apply_mutation, input_segments_for_entries, locate_entry,
    try_fold_report_edit_tail, try_fold_tail,
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
use crate::track_area_cache::TrackAreaCache;
use crate::track_vcs;

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
    pub worker_session_id: String,
    pub track_id: TrackId,
    pub card_id: CardId,
    pub thread_id: Option<String>,
    pub repo: Arc<dyn Repo>,
    pub events: EventBus,
    pub card_role_cache: CardRoleCache,
    pub track_area_cache: TrackAreaCache,
    pub backend: PlannerBackend,
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
    backend: PlannerBackend,
    observations: ObservationIngress,
    state: Mutex<HarnessState>,
    last_phase: Mutex<HarnessPhaseTag>,
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
    /// Segments of the turn an older binary left in flight, read from its snapshot at boot and
    /// never written back.
    legacy_issued_input_segments: Mutex<Option<IssuedInputSegments>>,
    last_report_body_sha256: Mutex<Option<String>>,
    last_seen_head: Mutex<Option<track_vcs::CommitHash>>,
    /// Latest context-window reading from `thread/tokenUsage/updated`; latest-wins.
    token_usage: Mutex<Option<TokenUsage>>,
    debounce: Mutex<DebounceState>,
    interrupt_deadline: Mutex<Option<(String, Instant)>>,
    /// Do not re-attempt turn issuance before this instant. Without it a re-buffered batch re-arms
    /// `hard_fire` and the loop spins at twenty write transactions a second while codex is unreachable.
    issuance_retry_after: Mutex<Option<Instant>>,
    /// What to tell the reader about why their message has not been sent, or `None`. Live-only,
    /// like `phase`: re-derived by the next attempt, deliberately not on `HarnessSnapshot`.
    issuance_block: Mutex<Option<String>>,
    /// When the current run of consecutive refusals began; feeds `transient_silence_budget`.
    refusing_since: Mutex<Option<Instant>>,
    /// How many issuance attempts have been refused, so a test can assert the retry is PACED
    /// without waiting on a wall clock.
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
    /// Issuance kill-switch for dev-forced harnesses; only `pause_issuance_for_dev` sets it.
    issuance_paused: AtomicBool,
    /// Queue entries the load-time truncation discarded whose `dropped` row does not exist yet.
    /// A `tokio::Mutex` held across the event inserts so two racing flushers cannot take the same id;
    /// `persist_snapshot_inner` drains this first and refuses to write if it cannot.
    unannounced_drops: Mutex<Vec<QueueEntryId>>,
    /// Entries codex accepted through `turn/steer` into the running turn, held until that turn
    /// completes. Live-only: a restart loses the turn with it.
    steered_into_running_turn: Mutex<Vec<SteeredEntry>>,
    abort_handle: StdMutex<Option<AbortHandle>>,
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

pub(super) struct IssueTurnHandle<'a> {
    backend: &'a PlannerBackend,
}

impl<'a> IssueTurnHandle<'a> {
    pub(super) fn from_reconciliation(inner: &'a Inner) -> Self {
        Self {
            backend: &inner.backend,
        }
    }

    /// `selection` is required: a default would silently answer which model runs this turn.
    /// `client_user_message_id` is the projection row's key; codex hands it back as `item.clientId`.
    pub(super) async fn issue(
        &self,
        thread_id: &str,
        input: Vec<InputItem>,
        selection: &TurnModelSelection,
        client_user_message_id: &str,
    ) -> Result<String> {
        self.backend
            .turn_start(thread_id, input, selection, client_user_message_id)
            .await
    }
}

#[derive(Clone, Debug)]
pub struct HarnessObservationDelivery {
    pub entry: QueueEntry,
}

enum HarnessObservationCommand {
    /// Recovery must snapshot on the loop that owns both queue and watermark,
    /// after earlier deliveries/notifications settle and before stopping it.
    QuiesceSystemError {
        done: oneshot::Sender<Result<()>>,
    },
    Delivery(HarnessObservationDelivery),
    Durable {
        deliveries: Vec<HarnessObservationDelivery>,
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

/// What a durable enqueue tells the caller. `entry_id` is `None` in exactly one accepted case —
/// the message folded into a `LegacyUser` tail. The LAST user-authored delivery wins, `None` included.
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
        let notifications = params.backend.subscribe_notifications();
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
        // No run loop on this path to flush the drop announcements early; the `persist_snapshot_inner`
        // drain still covers them.
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

    /// Fold and persist non-replayable user intent before acknowledging it. The ack names the
    /// entry the text ended up in — under backpressure, the fold survivor.
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

    /// Edit or delete one queue entry from the REST write port. Outer `Result` is transport; inner
    /// [`MutationResult`] is the queue's own answer.
    pub async fn mutate_pending_entry(
        &self,
        mutation: QueueMutation,
        actor: ActorId,
    ) -> Result<MutationResult> {
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

    /// Stop only this failed loop, without interrupting or sealing its provider
    /// thread. Recovery probes that exact thread before granting authority again.
    pub(crate) async fn quiesce_system_error_for_recovery(&self) -> Result<()> {
        let _durable = self.inner.durable_observation.lock().await;
        if self.inner.shutting_down.load(Ordering::SeqCst) {
            return Ok(());
        }
        match &self.inner.observations {
            ObservationIngress::Running(sender) => {
                let (done, answer) = oneshot::channel();
                sender
                    .try_send(HarnessObservationCommand::QuiesceSystemError { done })
                    .map_err(map_observation_send_error)?;
                answer.await.map_err(|_| {
                    CalmError::Conflict(
                        "conversation stopped before recovery quiescence completed".into(),
                    )
                })?
            }
            #[cfg(feature = "fixtures")]
            ObservationIngress::Unstarted(_) => quiesce_system_error(&self.inner).await,
        }
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.shutdown_inner(false, false).await
    }

    /// Quiesce an owner that is preparing for deletion and return its retained
    /// thread seal. An error/panic releases the seal through the local guard;
    /// the caller owns it only after strict interruption succeeds.
    pub async fn shutdown_for_deletion(&self) -> Result<Option<String>> {
        let thread_id = self.inner.thread_id.read().await.clone();
        let mut seals = crate::shared_codex_appserver::DeletionThreadSeals::new(
            self.inner.backend.codex().clone(),
        );
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
            self.inner
                .backend
                .codex()
                .seal_turn_thread_for_deletion(thread_id);
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
            let active_turn_id = self.inner.backend.active_turn_id_for_thread(&thread_id);
            if let Err(e) = self.inner.backend.interrupt_active_turn(&thread_id).await {
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
                    .backend
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

    /// Why this conversation's queue is not draining, or `None`. `None` does NOT mean waiting is
    /// the right answer.
    pub async fn issuance_block(&self) -> Option<String> {
        self.inner.issuance_block.lock().await.clone()
    }

    /// See [`Inner::refused_issuances`].
    #[cfg(feature = "fixtures")]
    pub fn refused_issuances_for_test(&self) -> u64 {
        self.inner.refused_issuances.load(Ordering::SeqCst)
    }

    /// Forget a refusal so the next tick re-attempts immediately (called when a person changes the selection).
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
        backend: params.backend,
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
        // Round-trips through the snapshot: codex only re-pushes it on the next model response, so a
        // resumed-but-idle thread would otherwise read as having no context usage.
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
    mut shutdown: broadcast::Receiver<()>,
    mut notifications: broadcast::Receiver<Notification>,
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
    let mut tick = harness_tick();
    loop {
        tokio::select! {
            command = observations.recv() => {
                let Some(command) = command else { break };
                match command {
                    HarnessObservationCommand::QuiesceSystemError {done} => {
                        let result=quiesce_system_error(&inner).await;
                        let stop=result.is_ok();
                        let _=done.send(result);
                        if stop { break; }
                    }
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

/// Called only by the run loop (or the unstarted fixture). Ordinary delivery,
/// queue mutation, notification processing and ticks cannot interleave here.
async fn quiesce_system_error(inner: &Arc<Inner>) -> Result<()> {
    if !matches!(&*inner.state.lock().await, HarnessState::Wedged {reason,..} if reason==HARNESS_SYSTEM_ERROR_REASON)
    {
        return Err(CalmError::Conflict(
            "conversation changed before recovery".into(),
        ));
    }
    if !inner.steered_into_running_turn.lock().await.is_empty() {
        return Err(CalmError::ServiceUnavailable(
            "Waiting for the failed turn to settle its messages before recovery; retry shortly."
                .into(),
        ));
    }
    persist_failed_system_error_snapshot(inner).await?;
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
    let (outcome, checkpoint) = {
        let mut queue = inner.pending_queue.lock().await;
        let before = queue.clone();
        (apply_mutation(&mut queue, mutation), before)
    };
    let applied = match outcome {
        Ok(applied) => applied,
        // A refusal changed nothing; a 404 does NOT mean the entry was delivered (`rebuffer_head`
        // can put a drained batch back), so no event is invented.
        Err(refused) => return Ok(Err(refused)),
    };

    if applied.change == HarnessQueueChange::Deleted {
        rearm_debounce_after_departure(inner, &applied).await;
    }

    if let Err(error) = persist_snapshot(inner).await {
        // Memory is rolled back to the exact queue the mutation started from.
        *inner.pending_queue.lock().await = checkpoint;
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
    // Read on this task: the phase leaves `TurnRunning` through the notification arm of the same
    // `select!`; a Stop landing after codex accepted is what `SteeredEntry` exists for.
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
        // Unreachable in practice (`TurnRunning` is only entered from a `turn/started` on this thread).
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
        "calling backend.turn_steer"
    );
    let steered = inner
        .backend
        .turn_steer(&thread_id, &turn_id, items, entry_id.as_str())
        .await;
    let error = match steered {
        Ok(taken_by) => {
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

    // Refused, unreachable, or timed out: the sentence goes back. No row was written and the
    // on-disk snapshot never stopped listing the entry, so a persist failure is a warning.
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
    // `CodexRefused` is the one error shape in which codex is known to have seen the request;
    // every other error leaves the outcome unknown.
    Ok(Err(match error {
        CalmError::CodexRefused(message) => SteerRefused::NotTaken { message, phase },
        other => SteerRefused::Unanswered {
            message: other.to_string(),
            phase,
        },
    }))
}

/// `TurnCompleted` sweep: every steered entry whose transcript row is still a projection was
/// dropped by codex before it was recorded, so the row is deleted and the entry goes back to
/// the head one rev up. Runs after the arm's gates and BEFORE the phase persist.
async fn restore_steered_entries_codex_dropped(
    inner: &Arc<Inner>,
    turn_id: &str,
) -> Vec<QueueEntryId> {
    let steered = std::mem::take(&mut *inner.steered_into_running_turn.lock().await);
    if steered.is_empty() {
        return Vec::new();
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
        return Vec::new();
    }
    let ids = restored
        .iter()
        .filter_map(|entry| entry.id().cloned())
        .collect::<Vec<_>>();
    // One rev up before it goes back, so the client hiding the entry can tell the restored page
    // from the one it read before the steer.
    for entry in &mut restored {
        entry.bump_rev_for_restore();
    }
    rebuffer_head(inner, restored).await;
    ids
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
    let mut queue = inner.pending_queue.lock().await;
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
                    reason: HARNESS_SYSTEM_ERROR_REASON.into(),
                };
                *inner.issued_turn_id.lock().await = None;
                *inner.interrupt_deadline.lock().await = None;
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
                let _ = persist_turn_outcome(inner, &turn_id, &turn).await;
                // The Stop path is the one that drops steered input; the sweep runs before the phase persist.
                let restored = restore_steered_entries_codex_dropped(inner, &turn_id).await;
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
                let item = persist_turn_outcome(inner, &turn_id, &turn).await;
                let restored = restore_steered_entries_codex_dropped(inner, &turn_id).await;
                persist_failed_system_error_snapshot(inner).await?;
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
            let _ = persist_turn_outcome(inner, &turn_id, &turn).await;
            // A turn can end without a model request after the steer on this branch too; same sweep.
            let restored = restore_steered_entries_codex_dropped(inner, &turn_id).await;
            persist_snapshot_stamping_issued_head(inner).await?;
            announce_restored_entries(inner, restored).await;
            return Ok(());
        }
        // Codex has no `turn/aborted` notification; an interrupt arrives as `turn/completed` with
        // `status: "interrupted"`.
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
            let restored = restore_steered_entries_codex_dropped(inner, &aborted_turn_id).await;
            persist_snapshot_stamping_issued_head(inner).await?;
            announce_restored_entries(inner, restored).await;
            return Ok(());
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
            // The one turn an older binary can have left in flight: its drain wrote no projection row,
            // so its echo takes the segments from the legacy slot.
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
            // A `userMessage` echo carrying `item.clientId` names the projection row the drain wrote;
            // the echo upgrades it in place and must never become a second copy. Only a completed
            // `userMessage` renders, so a started echo naming a projection is not stored at all.
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
        // `turn/plan/updated` — codex's whole TODO checklist for the running turn, superseding the
        // previous one. Persisted only; no UI reads it yet.
        Notification::Other { method, params } if method == "turn/plan/updated" => {
            // `harness_items.thread_id` is NOT NULL, so there is no row to write without one.
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
            let params_json = serde_json::to_string(&params)?;
            inner
                .repo
                .harness_item_insert(
                    &inner.worker_session_id,
                    inner.card_id.as_str(),
                    inner.track_id.as_str(),
                    &thread_id,
                    turn_id.as_deref(),
                    // No `item_uuid` and no `item_type`: a plan is not an item.
                    None,
                    None,
                    &method,
                    &params_json,
                    None,
                )
                .await?;
            // Deliberately NO `Event::HarnessItemAdded` for a plan row: nothing reads plan rows, and the
            // event would append a track-vcs commit per plan frame. `harness_items` is out-of-domain
            // storage, so a row without an event is legal here.
        }
        // `thread/tokenUsage/updated`: `tokenUsage.total` is a LIFETIME sum and routinely exceeds the
        // window; `tokenUsage.last` is the occupancy proxy. Storage is the runtime snapshot (latest-wins).
        // The prologue's thread-id check is the only thing keeping card A's meter from showing card B's.
        Notification::Other { method, params } if method == "thread/tokenUsage/updated" => {
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
                }
                // A frame without a usable `last.totalTokens` yields no reading; storing a zero would claim
                // an empty context, so the previous reading is left in place.
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

/// Live codex sends `userMessage`; the kernel stores `item.type` verbatim and tests have used snake case.
fn is_user_message_type(item_type: Option<&str>) -> bool {
    matches!(item_type, Some("userMessage" | "user_message"))
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

/// Defensive cap on the track-vcs diff-block fetch; the diff block is never a correctness
/// requirement, so a stalled SELECT becomes a warn and a degraded turn.
const SINCE_LAST_TURN_DIFF_TIMEOUT: Duration = Duration::from_secs(5);
const TRANSCRIPT_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);
const SINCE_LAST_TURN_HEAD_FALLBACK_TIMEOUT: Duration = Duration::from_secs(1);

/// Budget for the two codex reads a model resolution may need, together. Generous because
/// elapsing costs a refused turn the person then has to retry.
const MODEL_RESOLUTION_BUDGET: Duration = Duration::from_secs(15);

/// How long to leave a card alone after an attempt that could still succeed on its own; without
/// it the re-buffered batch re-arms `hard_fire` and the next tick tries again 50 ms later.
const TRANSIENT_RETRY_DELAY: Duration = Duration::from_secs(2);

/// The same, for an attempt that CANNOT succeed until a person changes something. Still a poll
/// rather than a full stop because the fix may arrive from outside this process.
const NEEDS_A_CHOICE_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Read the card as it stands NOW — the last moment before the frame is built; reading any
/// earlier is the staleness bug this exists to prevent. A vanished card is an `Err`.
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
        // A card that is gone has nobody left to tell; transient keeps the loop cheap without
        // putting a message on a surface no one is looking at.
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
    /// For the reader. Used by every kind except [`FailureKind::Retryable`], which supplies its
    /// own text via `transient_notice`.
    reader: String,
}

/// Classify a codex call that failed: codex ANSWERING with a refusal becomes `refused(log)`,
/// every other failure is retryable. Every codex call on the issuance path (`config/read`,
/// `model/list`, `turn/start`) goes through this.
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

/// Work out what this turn must tell codex about the model, from the card's payload plus —
/// only where the payload cannot answer alone — codex's own config and catalog. `Err` when the
/// answer cannot be established; the turn is not sent under an unknown model.
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
    // Codex not answering and codex answering "no model" are different facts; only the first
    // is worth waiting out, so the read's failure returns here rather than degrading to `None`.
    let config = inner
        .backend
        .codex()
        .config_read(Some(cwd.as_str()), deadline)
        .await
        .map_err(|e| {
            // The sentence is DERIVED: this branch is entered by a disjunction (model, effort, or both
            // follow the default) and a fixed string is right for at most one of them.
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

/// Only reached once codex has answered, so the answer did not name a model — a person must act.
fn unresolved(e: crate::planner_model::UnresolvedSelection) -> IssuanceRefusal {
    IssuanceRefusal::needs_a_choice(e.log_reason().to_string(), e.reason().to_string())
}

/// Record a refusal: how long before the next attempt, and what (if anything) the reader is told.
async fn apply_refusal(inner: &Arc<Inner>, failure: &IssuanceRefusal) {
    let (delay, notice) = match failure.kind {
        // Nobody can act, and repeating may work. Silent while that is
        // plausibly still true; see `transient_notice`.
        FailureKind::Retryable => (TRANSIENT_RETRY_DELAY, transient_notice(inner).await),
        // Codex saw the input and said no. Retried slowly rather than not at all (the person may
        // change the model from another tab), but the reader is told now.
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
    (now.duration_since(began) >= inner.config.transient_silence_budget).then(|| {
        "Waiting for codex — it has not accepted this conversation's last few turns. Your \
         message is still queued and will be sent when it answers."
            .to_string()
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

/// The workspace whose config layers apply to this thread. It must be the path `thread/start`
/// was given, or `config/read` answers a question we did not ask; an unreadable track is an
/// `Err`, not a `None`.
async fn installation_cwd(inner: &Arc<Inner>) -> std::result::Result<String, IssuanceRefusal> {
    // Deterministic card-read-then-track-read window. No-op in production.
    wait_at_planner_harness_cwd_race_hook(inner.worker_session_id.as_str()).await;
    match inner.repo.track_get(inner.track_id.as_str()).await {
        Ok(Some(track)) => Ok(track.workspace.path),
        // "There is no workspace" and "we could not read the workspace" are different facts, and
        // neither means "read the global layers instead". `Ok(None)` IS reachable: a
        // `track_delete_tx` can commit between the card read and this one.
        Ok(None) => Err(IssuanceRefusal::retryable(format!(
            "track {} is not readable, so this conversation's config scope is unknown",
            inner.track_id
        ))),
        Err(e) => Err(IssuanceRefusal::retryable(format!(
            "could not read the track workspace for a config/read cwd: {e}"
        ))),
    }
}

/// Codex's own preset effort for the model that will actually run. `Ok(None)` means the
/// catalog genuinely has no answer; a read that failed is an `Err`.
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
    match inner.backend.codex().model_list(deadline).await {
        Ok(models) => Ok(models
            .into_iter()
            .find(|m| m.model == slug)
            .map(|m| m.default_reasoning_effort)),
        // Only the effort can want the catalog, so a fixed sentence is honest here.
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

/// Consume only successful bookkeeping for a Done Track; the event log and accepted push
/// watermark stay intact, other observations keep their queue order.
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
    Call calm.user.notify only for a conflict with work still in flight, \
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

async fn maybe_issue_turn(inner: &Arc<Inner>) -> Result<()> {
    // Dev-forced harnesses run against the replay binary's stub app-server.
    if inner.issuance_paused.load(Ordering::SeqCst) {
        return Ok(());
    }
    // Most ticks find the queue empty; bail before any logging so the 50ms
    // tick cadence does not flood the log with one entry line per tick.
    let (queue_len, only_report_edits, report_edits_carry_diffs) = {
        let queue = inner.pending_queue.lock().await;
        (
            queue.len(),
            queue_is_only_report_edits(&queue),
            queue_report_edits_all_carry_diffs(&queue),
        )
    };
    if queue_len == 0 {
        return Ok(());
    }
    // Checked before any of the work below, because the point is to skip that work.
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
    // A queue that is nothing but report edits waits for the editor to go quiet; `hard_fire` is
    // checked first and wins outright, so this only ever lengthens a wait.
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
    // The DURABLE half of "may I still speak for this card": a runtime can be retired in the
    // DATABASE with its run loop healthy and unaware. Placed above the work so a refused runtime
    // pays one indexed read per tick. The refusal does NOT wind the handle down.
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
    // Two independent per-turn decisions: `skip_transcript_refresh` skips the track-level WRITE
    // transaction; `skip_track_diff` issues with no since-last-turn block. An area chat skips both;
    // a track assistant skips only the refresh (it cannot read transcripts; the diff IS its context).
    // Do NOT extend the refresh skip to Planner or Worker cards — they would read a stale HEAD.
    let Some(card) = inner.repo.card_get(inner.card_id.as_str()).await? else {
        // A card that is gone has no conversation left to answer. The queue is not drained and the
        // state is not touched, so a card that reappears issues on the next tick.
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
    // A batch of report edits each carrying its diff issues a turn whose input IS the diff, so the
    // since-last-turn block omits the unified patch; a pre-diff entry keeps it.
    let report_patch = if report_edits_carry_diffs {
        track_vcs::ReportPatch::Omit
    } else {
        track_vcs::ReportPatch::Include
    };
    let diff = if skip_track_diff {
        track_vcs::SinceLastTurnBlock::empty()
    } else {
        diff_with_timeout(inner, refresh_head.as_ref(), report_patch).await
    };
    // Deterministic drain-vs-supersede window. No-op in production.
    wait_at_planner_harness_drain_race_hook(&inner.worker_session_id).await;
    let _issuance_guard = inner.issuance.lock().await;
    if inner.shutting_down.load(Ordering::SeqCst) {
        return Ok(());
    }
    // Asked a SECOND time, immediately before the drain: a fence landing during the refresh and
    // diff above is exactly the case this is about.
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
    // A new batch supersedes the legacy slot: by now that turn's echo has either consumed the slot
    // or is not coming.
    *inner.legacy_issued_input_segments.lock().await = None;
    // The key shared by the projection row and `turn/start`'s `clientUserMessageId`, decided HERE
    // so the snapshot written next carries it and a restart re-drains the batch under the SAME key.
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
    // Segments are built from the ENTRIES, not observations: an `Observation` cannot carry an attachment.
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
    // The channel statement is the batch's, appended once here; same flag as `report_patch`.
    append_report_edit_batch_channel_line(&mut prepared.segments, report_edits_carry_diffs);
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
        "calling backend.turn_start"
    );

    // The model is resolved HERE, as late as possible: the transcript refresh and diff above can
    // be tens of seconds, and a person who changed the model inside that window would otherwise
    // watch the turn run under the model they just replaced.
    let selection = match resolve_model_selection_for_issue(inner).await {
        Ok(selection) => selection,
        Err(failure) => {
            // Not a wedge: `HarnessState::Wedged` has no exit in this tree, and wedging on a codex restart
            // ended the conversation permanently. But not every failure here clears itself, so the kinds
            // are separated below rather than all answered with a timer.
            tracing::warn!(
                target: "calm_server::planner_harness_issue",
                worker_session_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                reason = %failure.log,
                kind = ?failure.kind,
                "not issuing this turn: the model to run it under is undetermined; will retry"
            );
            // The two arms differ in what waiting is worth, so they differ in how long we wait and in
            // whether the reader is told.
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

    // Text first, then one `localImage` per attachment, in queue order. Every path was recorded and
    // verified at bind time; this builds no path and touches no disk, so a re-buffered batch is safe.
    let mut items = vec![InputItem::text(text)];
    items.extend(
        drained
            .iter()
            .flat_map(QueueEntry::attachments)
            .map(|attachment| InputItem::local_image(attachment.path.clone())),
    );
    // A projection row that could not be written is a LOCAL failure before codex was asked; a
    // refused `turn/start` is codex's answer. The log must not call the first "turn/start failed".
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
            // Known cosmetic gap: a fragment/value mismatch here (our bug) is worded as a codex
            // turn/start refusal rather than the briefing preparation failure it is.
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
            .issue(&thread_id, items, &selection, client_id.as_str())
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
                "backend.turn_start ok"
            );
            // A turn that went out ends the run of refusals, so the notice and
            // the clock behind it both go with it.
            *inner.issuance_block.lock().await = None;
            *inner.refusing_since.lock().await = None;
            *inner.last_turn_id.lock().await = Some(turn_id.clone());
            *inner.issued_turn_id.lock().await = Some(turn_id.clone());
            *inner.issued_turn_head.lock().await = diff.current_head.clone();
            // Cleared in the same snapshot that empties the queue, so no restart can pair this key with a later batch.
            *inner.projection_client_id.lock().await = None;
            persist_issuance_outcome(inner).await?;
        }
        Err(failure) => {
            // Paced, like the refusal above: `rebuffer_head` arms `hard_fire`, and `PUT /planner/model`
            // can store a slug codex does not know, so an unpaced retry was twenty RPCs a second. The split
            // is on the TYPED error: `Rejected` because no choice is KNOWN to remove the need for `turn/start`.
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
            // The batch goes back on the queue, so the row that said it was sent goes too. A failed delete
            // is logged: the next drain replaces the row under the same key. The phase change that follows
            // the re-buffer is what tells a reader the row is gone.
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
    report_patch: track_vcs::ReportPatch,
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
        report_patch,
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
    // A re-buffered batch keeps the ids it was drained with: the same instances going back.
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
        .backend
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

/// Persist what the runtime owes after an issuance resolved, on a row the ordinary writer may
/// already refuse: `persist_snapshot` is a no-op once `shutting_down` is set or the row left the
/// active set, and both are set by the re-point fence. Runs after the ordinary write, not instead.
async fn persist_issuance_outcome(inner: &Arc<Inner>) -> Result<()> {
    persist_snapshot(inner).await?;
    // Only the runtimes that can actually need it open the second transaction; for a live runtime
    // it would match zero rows and only contend on the writer lock.
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
        // Neither writer matched: the row flipped back into the active set between the two writes, so
        // it still carries its PRE-drain queue. Logged: the daemon already has the batch.
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

/// Make a turn's terminal status durable: one `turn/completed` row per finished turn, written
/// AFTER the arm's gates and BEFORE `persist_snapshot_stamping_issued_head`, so the phase event
/// doubles as the delivery signal (no item-added event). Best-effort: a failed insert is logged.
async fn persist_turn_outcome(inner: &Arc<Inner>, turn_id: &str, turn: &Value) -> Option<i64> {
    // `thread_id` is NOT NULL and `Notification::TurnCompleted.thread_id` is `unwrap_or_default()`
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
    )
    .await
    {
        Ok(id) => Some(id),
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
    let snapshot = serde_json::to_value(snapshot_for(inner).await)?;
    let card = inner.card_id.to_string();
    let id = inner.worker_session_id.clone();
    let written =
        write_in_tx_typed(inner.repo.as_ref(), move |tx| {
            Box::pin(async move {
                Ok(crate::db::sqlite::session_set_failed_harness_snapshot_tx(
                    tx, &card, &id, &snapshot,
                )
                .await?)
            })
        })
        .await?;
    if written {
        Ok(())
    } else {
        Err(CalmError::Conflict(
            "failed conversation changed while settling its input".into(),
        ))
    }
}

async fn persist_snapshot(inner: &Arc<Inner>) -> Result<()> {
    persist_snapshot_inner(inner, None).await.map(|_| ())
}

/// Persist a durable user send, and REFUSE it if the row was not written: the writer matches
/// nothing once the row leaves the active set.
/// `shutting_down` is not reachable from here — `shutdown_inner` takes `durable_observation` first.
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
    // The truncation's record goes in BEFORE the truncation does. `?` and not a warn: a write that
    // proceeded would make a discarded user message permanently gone with nothing saying so.
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
            // The snapshot transaction is already committed; reporting failure here would make durable
            // ingress roll back memory after its message was accepted.
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

#[cfg(test)]
mod completed_commit_tests;

#[cfg(test)]
mod recovery_briefing_tests;

#[cfg(test)]
mod report_edit_replay_tests;

#[cfg(test)]
mod result_receipt_tests;
