use std::collections::{HashSet, VecDeque};
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
use crate::event::{Event, EventBus, EventScope};
use crate::harness::config::HarnessConfig;
use crate::harness::observation::Observation;
use crate::harness::snapshot::{HarnessPhaseTag, HarnessSnapshot, IssuedInputSegments};
use crate::harness::state::{HarnessState, IssuingKind, run_status_for};
use crate::harness::token_usage::TokenUsage;
use crate::ids::{ActorId, CardId, TrackId};
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
    runtime_id: &str,
    hook: PlannerHarnessDrainRaceHook,
) {
    planner_harness_drain_race_hooks()
        .lock()
        .expect("planner harness drain hook mutex")
        .insert(runtime_id.to_string(), hook);
}

async fn wait_at_planner_harness_drain_race_hook(runtime_id: &str) {
    #[cfg(feature = "fixtures")]
    {
        let hook = {
            let mut hooks = planner_harness_drain_race_hooks()
                .lock()
                .expect("planner harness drain hook mutex");
            hooks
                .remove(runtime_id)
                .or_else(|| hooks.remove(ANY_RUNTIME))
        };
        if let Some(hook) = hook {
            hook.entered.notify_one();
            hook.release.notified().await;
        }
    }
    #[cfg(not(feature = "fixtures"))]
    let _ = runtime_id;
}

const OBSERVATION_BUFFER: usize = 256;
const MAX_PENDING_QUEUE_LEN: usize = 256;
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
    pending_queue: Mutex<VecDeque<Observation>>,
    pending_envelope_ids: Mutex<VecDeque<Option<i64>>>,
    /// #1449 — message ids, one set per `pending_queue` entry. Identity only;
    /// see `HarnessSnapshot::pending_message_ids`.
    pending_message_ids: Mutex<VecDeque<Vec<String>>>,
    recent_hook_keys: Mutex<VecDeque<String>>,
    recent_hook_key_set: Mutex<HashSet<String>>,
    push_watermark: Mutex<i64>,
    last_turn_id: Mutex<Option<String>>,
    issued_turn_id: Mutex<Option<String>>,
    issued_turn_head: Mutex<Option<track_vcs::CommitHash>>,
    issued_input_segments: Mutex<Option<IssuedInputSegments>>,
    last_report_body_sha256: Mutex<Option<String>>,
    last_seen_head: Mutex<Option<track_vcs::CommitHash>>,
    /// #1255 S3 — latest context-window reading from `thread/tokenUsage/updated`.
    /// Latest-wins: every frame replaces this whole value (modulo the sticky
    /// window in [`TokenUsage::sticky_merge`]), and it rides the runtime
    /// snapshot out to `worker_sessions.handle_state` on the next persist.
    token_usage: Mutex<Option<TokenUsage>>,
    debounce: Mutex<DebounceState>,
    interrupt_deadline: Mutex<Option<(String, Instant)>>,
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
    abort_handle: StdMutex<Option<AbortHandle>>,
    config: HarnessConfig,
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

    pub(super) async fn issue(&self, thread_id: &str, input: Vec<InputItem>) -> Result<String> {
        self.daemon.turn_start(thread_id, input).await
    }
}

#[derive(Clone, Debug)]
pub struct HarnessObservationDelivery {
    pub observation: Observation,
    pub envelope_id: Option<i64>,
}

enum HarnessObservationCommand {
    Delivery(HarnessObservationDelivery),
    Durable {
        deliveries: Vec<HarnessObservationDelivery>,
        persisted: oneshot::Sender<Result<()>>,
    },
}

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
    pending_queue: VecDeque<Observation>,
    pending_envelope_ids: VecDeque<Option<i64>>,
    pending_message_ids: VecDeque<Vec<String>>,
    debounce: DebounceState,
}

async fn checkpoint_durable_user_message(inner: &Inner) -> DurableUserMessageCheckpoint {
    DurableUserMessageCheckpoint {
        pending_queue: inner.pending_queue.lock().await.clone(),
        pending_envelope_ids: inner.pending_envelope_ids.lock().await.clone(),
        pending_message_ids: inner.pending_message_ids.lock().await.clone(),
        debounce: *inner.debounce.lock().await,
    }
}

async fn restore_durable_user_message(inner: &Inner, checkpoint: DurableUserMessageCheckpoint) {
    *inner.pending_queue.lock().await = checkpoint.pending_queue;
    *inner.pending_envelope_ids.lock().await = checkpoint.pending_envelope_ids;
    *inner.pending_message_ids.lock().await = checkpoint.pending_message_ids;
    *inner.debounce.lock().await = checkpoint.debounce;
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
        let inner = inner_from_params(params, ObservationIngress::Running(obs_tx), shutdown_tx);
        let handle = Self {
            inner: Arc::clone(&inner),
        };
        let task = tokio::spawn(run_loop(inner, obs_rx, shutdown_rx, notifications));
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
        let inner = inner_from_params(params, ObservationIngress::Unstarted(obs_tx), shutdown_tx);
        (Self { inner }, obs_rx)
    }

    pub fn observe(&self, obs: Observation) -> Result<()> {
        self.observe_delivery(HarnessObservationDelivery {
            observation: obs,
            envelope_id: None,
        })
    }

    pub fn observe_envelope(&self, obs: Observation, envelope_id: i64) -> Result<()> {
        self.observe_delivery(HarnessObservationDelivery {
            observation: obs,
            envelope_id: Some(envelope_id),
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
    pub async fn observe_user_message_durable(&self, text: String) -> Result<()> {
        self.observe_durable_observations(vec![Observation::UserMessage { text }])
            .await
    }

    async fn observe_durable_observations(&self, observations: Vec<Observation>) -> Result<()> {
        let _durable_guard = self.inner.durable_observation.lock().await;
        if self.inner.shutting_down.load(Ordering::SeqCst) {
            return Err(CalmError::Conflict(
                "planner harness is shutting down; refusing new observation".into(),
            ));
        }
        let deliveries = observations
            .into_iter()
            .map(|observation| HarnessObservationDelivery {
                observation,
                envelope_id: None,
            })
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
                for delivery in deliveries {
                    if !on_observation(&self.inner, delivery.observation, delivery.envelope_id)
                        .await
                    {
                        restore_durable_user_message(&self.inner, checkpoint).await;
                        return Err(CalmError::ServiceUnavailable(
                            "planner harness pending queue full, retry shortly".into(),
                        ));
                    }
                }
                if let Err(error) = persist_snapshot_for_durable_send(&self.inner).await {
                    restore_durable_user_message(&self.inner, checkpoint).await;
                    return Err(error);
                }
                Ok(())
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
            .cloned()
            .collect()
    }

    #[cfg(feature = "fixtures")]
    pub async fn observe_for_test(&self, obs: Observation, envelope_id: Option<i64>) {
        let _ = on_observation(&self.inner, obs, envelope_id).await;
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
        *self.inner.issued_input_segments.lock().await = None;
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
) -> Arc<Inner> {
    let mut snapshot = params.snapshot;
    snapshot.align_pending_side_arrays();
    truncate_snapshot_pending_queue(&mut snapshot);
    let debounce = debounce_from_initial_queue(&snapshot.pending_queue);
    let state = state_from_snapshot(&snapshot);
    let last_phase = snapshot.phase;
    let pending_queue: VecDeque<_> = snapshot.pending_queue.into_iter().collect();
    let (recent_hook_keys, recent_hook_key_set) =
        recent_hook_keys_from_pending_queue(&pending_queue);
    Arc::new(Inner {
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
        pending_envelope_ids: Mutex::new(snapshot.pending_envelope_ids.into_iter().collect()),
        pending_message_ids: Mutex::new(snapshot.pending_message_ids.into_iter().collect()),
        pending_queue: Mutex::new(pending_queue),
        recent_hook_keys: Mutex::new(recent_hook_keys),
        recent_hook_key_set: Mutex::new(recent_hook_key_set),
        push_watermark: Mutex::new(snapshot.push_watermark),
        last_turn_id: Mutex::new(snapshot.last_turn_id),
        issued_turn_id: Mutex::new(None),
        issued_turn_head: Mutex::new(snapshot.issued_turn_head),
        issued_input_segments: Mutex::new(snapshot.issued_input_segments),
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
        shutdown,
        shutting_down: Arc::new(AtomicBool::new(false)),
        observations_closed: StdMutex::new(false),
        durable_observation: Mutex::new(()),
        issuance: Mutex::new(()),
        issuance_paused: AtomicBool::new(false),
        abort_handle: StdMutex::new(None),
        config: params.config,
    })
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

fn debounce_from_initial_queue(queue: &[Observation]) -> DebounceState {
    if queue.is_empty() {
        return DebounceState::default();
    }
    let now = Instant::now();
    DebounceState {
        first_pending_at: Some(now),
        last_pending_at: Some(now),
        hard_fire: queue.iter().any(Observation::is_hard_fire),
    }
}

/// Seed hook-stop dedupe from the restored pending queue.
///
/// Snapshot recovery has already accepted these `WorkerHookStop` observations, so
/// their non-empty `idempotency_key` values must populate the recent-key LRU
/// before fallback replay or bridge retry can deliver the same hook again. Empty
/// keys are skipped because old snapshot rows deserialize them from the default.
fn recent_hook_keys_from_pending_queue(
    pending_queue: &VecDeque<Observation>,
) -> (VecDeque<String>, HashSet<String>) {
    let mut keys = VecDeque::with_capacity(RECENT_HOOK_KEY_CACHE_LEN);
    let mut set = HashSet::with_capacity(RECENT_HOOK_KEY_CACHE_LEN);
    for obs in pending_queue {
        let Observation::WorkerHookStop {
            idempotency_key, ..
        } = obs
        else {
            continue;
        };
        if idempotency_key.is_empty() || !set.insert(idempotency_key.clone()) {
            continue;
        }
        keys.push_back(idempotency_key.clone());
        while keys.len() > RECENT_HOOK_KEY_CACHE_LEN {
            if let Some(evicted) = keys.pop_front() {
                set.remove(&evicted);
            }
        }
    }
    (keys, set)
}

async fn run_loop(
    inner: Arc<Inner>,
    mut observations: mpsc::Receiver<HarnessObservationCommand>,
    mut shutdown: broadcast::Receiver<()>,
    mut notifications: broadcast::Receiver<Notification>,
) {
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    loop {
        tokio::select! {
            command = observations.recv() => {
                let Some(command) = command else { break };
                match command {
                    HarnessObservationCommand::Delivery(delivery) => {
                        let _accepted =
                            on_observation(&inner, delivery.observation, delivery.envelope_id).await;
                        if let Err(e) = persist_snapshot(&inner).await {
                            tracing::warn!(error = %e, "planner harness snapshot persist failed after observation");
                        }
                    }
                    HarnessObservationCommand::Durable { deliveries, persisted } => {
                        let checkpoint = checkpoint_durable_user_message(&inner).await;
                        let mut accepted = true;
                        for delivery in deliveries {
                            if !on_observation(
                                &inner,
                                delivery.observation,
                                delivery.envelope_id,
                            )
                            .await
                            {
                                accepted = false;
                                break;
                            }
                        }
                        let result = if accepted {
                            match persist_snapshot_for_durable_send(&inner).await {
                                Ok(()) => Ok(()),
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

async fn on_observation(inner: &Arc<Inner>, obs: Observation, envelope_id: Option<i64>) -> bool {
    if let Some(envelope_id) = envelope_id {
        let mut watermark = inner.push_watermark.lock().await;
        *watermark = (*watermark).max(envelope_id);
    }
    if suppress_duplicate_hook_stop(inner, &obs).await {
        return false;
    }
    let hard_fire = obs.is_hard_fire();
    if !enqueue_pending_observation(inner, obs.clone(), envelope_id).await {
        return false;
    }
    if let Some(hash) = obs.report_sha256() {
        *inner.last_report_body_sha256.lock().await = Some(hash.to_string());
    }
    let now = Instant::now();
    let mut debounce = inner.debounce.lock().await;
    if debounce.first_pending_at.is_none() {
        debounce.first_pending_at = Some(now);
    }
    debounce.last_pending_at = Some(now);
    debounce.hard_fire |= hard_fire;
    true
}

fn truncate_snapshot_pending_queue(snapshot: &mut HarnessSnapshot) {
    let len = snapshot.pending_queue.len();
    if len <= MAX_PENDING_QUEUE_LEN {
        return;
    }
    let drop_count = len - MAX_PENDING_QUEUE_LEN;
    // #1449 — align first, so this function does not depend on its caller
    // having done it. The two side arrays are drained by the same range as the
    // queue; against a shorter array that range is out of bounds and `drain`
    // panics, and a hand-built or pre-#1449 snapshot has exactly that shape.
    snapshot.align_pending_side_arrays();
    snapshot.pending_queue.drain(..drop_count);
    snapshot.pending_envelope_ids.drain(..drop_count);
    snapshot.pending_message_ids.drain(..drop_count);
    tracing::warn!(
        target: "planner.harness.backpressure",
        original_len = len,
        retained_len = snapshot.pending_queue.len(),
        "snapshot pending_queue truncated to newest observations"
    );
}

async fn enqueue_pending_observation(
    inner: &Arc<Inner>,
    obs: Observation,
    envelope_id: Option<i64>,
) -> bool {
    let mut queue = inner.pending_queue.lock().await;
    let mut envelope_ids = inner.pending_envelope_ids.lock().await;
    let mut message_ids = inner.pending_message_ids.lock().await;
    // #1449 — ONE of the two minting sites for a message id (the other is the
    // `first_message` seed in `PlannerHarnessStartAdapter::prepare_tx`). Every
    // observation that reaches a queue in this process reaches it here, so an
    // id minted here is minted once. Every other path that moves a
    // `UserMessage` between queues — the harvest, the inherit — CARRIES the id
    // it finds; none of them may mint.
    let minted: Vec<String> = match &obs {
        Observation::UserMessage { .. } => vec![crate::model::new_id()],
        _ => Vec::new(),
    };
    if queue.len() >= MAX_PENDING_QUEUE_LEN {
        if try_fold_pending_tail(
            &mut queue,
            &mut envelope_ids,
            &mut message_ids,
            &obs,
            envelope_id,
            &minted,
        ) {
            return true;
        }
        let hard = obs.is_hard_fire();
        if let Some(drop_idx) = queue.iter().position(|queued| !queued.is_hard_fire()) {
            queue.remove(drop_idx);
            envelope_ids.remove(drop_idx);
            message_ids.remove(drop_idx);
        } else {
            tracing::warn!(
                target: "planner.harness.backpressure",
                queue_len = queue.len(),
                hard,
                variant = ?obs,
                "pending_queue full, incoming observation dropped"
            );
            return false;
        }
    }
    queue.push_back(obs);
    envelope_ids.push_back(envelope_id);
    message_ids.push_back(minted);
    true
}

fn try_fold_pending_tail(
    queue: &mut VecDeque<Observation>,
    envelope_ids: &mut VecDeque<Option<i64>>,
    message_ids: &mut VecDeque<Vec<String>>,
    obs: &Observation,
    envelope_id: Option<i64>,
    minted_message_ids: &[String],
) -> bool {
    let Some(last) = queue.back_mut() else {
        return false;
    };
    let folded = match (last, obs) {
        (Observation::TrackGoal { text }, Observation::TrackGoal { text: new_text }) => {
            *text = new_text.clone();
            true
        }
        (
            Observation::ReportEdited {
                track_id,
                body_sha256,
                body,
                author,
            },
            Observation::ReportEdited {
                track_id: new_track_id,
                body_sha256: new_body_sha256,
                body: new_body,
                author: new_author,
            },
        ) if track_id == new_track_id => {
            *body_sha256 = new_body_sha256.clone();
            *body = new_body.clone();
            // The fold keeps the NEWEST edit's state, attribution included:
            // the planner is told to treat the surviving body as ground truth,
            // so it must be told who actually wrote that body (#1252 F2).
            *author = *new_author;
            true
        }
        // #615 F3: preserve both adjacent user intents under backpressure
        // rather than evicting the older send. Capped at
        // `MAX_FOLDED_USER_MESSAGE_CHARS` so the per-tail size cannot grow
        // unboundedly under sustained backpressure; once the cap is reached the
        // eviction fallback in `enqueue_pending_observation` drops a
        // non-hard-fire entry and lets the new UserMessage take a fresh slot.
        // Replacing would lose earlier intent, separate entries surface as
        // separate `User says:` blocks at turn-issuance.
        (Observation::UserMessage { text }, Observation::UserMessage { text: new_text }) => {
            let current_chars = text.chars().count();
            let new_chars = new_text.chars().count();
            if current_chars.saturating_add(new_chars).saturating_add(2)
                > MAX_FOLDED_USER_MESSAGE_CHARS
            {
                false
            } else {
                text.push_str("\n\n");
                text.push_str(new_text);
                true
            }
        }
        _ => false,
    };
    if folded && let Some(last_envelope_id) = envelope_ids.back_mut() {
        *last_envelope_id = envelope_id;
    }
    // #1449 — a fold turns two entries into one, so the surviving entry carries
    // BOTH sets of ids. Overwriting instead of unioning would discard an
    // instance the caller may later have to move back, which is exactly the
    // loss of identity the ids exist to prevent; the ids are a set per entry
    // rather than one id per entry for this reason alone.
    if folded && let Some(last_message_ids) = message_ids.back_mut() {
        last_message_ids.extend_from_slice(minted_message_ids);
    }
    folded
}

async fn suppress_duplicate_hook_stop(inner: &Arc<Inner>, obs: &Observation) -> bool {
    let Observation::WorkerHookStop {
        idempotency_key, ..
    } = obs
    else {
        return false;
    };
    if idempotency_key.is_empty() {
        return false;
    }
    let mut set = inner.recent_hook_key_set.lock().await;
    if set.contains(idempotency_key) {
        tracing::warn!(
            target: "planner.harness.dedupe",
            key = %idempotency_key,
            "duplicate WorkerHookStop suppressed"
        );
        return true;
    }
    set.insert(idempotency_key.clone());
    let mut keys = inner.recent_hook_keys.lock().await;
    keys.push_back(idempotency_key.clone());
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
                last_turn_id: turn_id,
            };
            *inner.interrupt_deadline.lock().await = None;
            return persist_snapshot_stamping_issued_head(inner).await;
        }
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
            let input_segments_json =
                if matches!(item_type.as_deref(), Some("userMessage" | "user_message")) {
                    let issued = inner.issued_input_segments.lock().await;
                    issued
                        .as_ref()
                        .filter(|issued| turn_id.as_deref() == Some(issued.turn_id.as_str()))
                        .map(|issued| serde_json::to_string(&issued.segments))
                        .transpose()?
                } else {
                    None
                };
            let params_json = serde_json::to_string(&params)?;
            let consumes_input_segments =
                method == "item/completed" && input_segments_json.is_some();
            let item_db_id = inner
                .repo
                .harness_item_insert(
                    &inner.worker_session_id,
                    inner.card_id.as_str(),
                    inner.track_id.as_str(),
                    &thread_id,
                    turn_id.as_deref(),
                    item_uuid.as_deref(),
                    item_type.as_deref(),
                    &method,
                    &params_json,
                    input_segments_json.as_deref(),
                )
                .await?;
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
            if consumes_input_segments {
                *inner.issued_input_segments.lock().await = None;
            }
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
        // Storage is the runtime snapshot, not `harness_items`. The reading is
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

/// 5s defensive cap on the track-vcs diff-block fetch inside `maybe_issue_turn`.
/// The diff block is a context augmentation prepended to planner turn
/// observations (#595 PR2); it is never a correctness requirement. If the
/// underlying sqlite SELECT chain stalls (issue #639 — silent stuck-turn
/// hypothesis), this ceiling converts an unobservable hang into a logged
/// warn + a degraded-but-functional turn issuance.
const SINCE_LAST_TURN_DIFF_TIMEOUT: Duration = Duration::from_secs(5);
const TRANSCRIPT_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);
const SINCE_LAST_TURN_HEAD_FALLBACK_TIMEOUT: Duration = Duration::from_secs(1);

async fn maybe_issue_turn(inner: &Arc<Inner>) -> Result<()> {
    // Issue #682 review — dev-forced harnesses run against the replay
    // binary's stub app-server; see `PlannerHarness::pause_issuance_for_dev`.
    if inner.issuance_paused.load(Ordering::SeqCst) {
        return Ok(());
    }
    // Most ticks find the queue empty; bail before any logging so the 50ms
    // tick cadence does not flood the log with one entry line per tick.
    let queue_len = inner.pending_queue.lock().await.len();
    if queue_len == 0 {
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
        now.duration_since(last) >= inner.config.debounce_min_idle
            || now.duration_since(first) >= inner.config.debounce_max_wait
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
            runtime_id = %inner.worker_session_id,
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
    let (skip_transcript_refresh, skip_track_diff) =
        match inner.repo.card_get(inner.card_id.as_str()).await? {
            Some(card) => {
                let role = inner.repo.card_role_get(card.id.as_str()).await?;
                if crate::plain_chat::card_is_plain_chat(&card, role, true) {
                    (true, true)
                } else if crate::plain_chat::card_is_track_assistant(&card, role, true) {
                    (true, false)
                } else {
                    (false, false)
                }
            }
            None => (false, false),
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
            runtime_id = %inner.worker_session_id,
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
    *inner.issued_input_segments.lock().await = None;
    persist_snapshot(inner).await?;

    let (drained, drained_envelope_ids, drained_message_ids) = {
        let mut queue = inner.pending_queue.lock().await;
        let mut envelope_ids = inner.pending_envelope_ids.lock().await;
        let mut message_ids = inner.pending_message_ids.lock().await;
        (
            queue.drain(..).collect::<Vec<_>>(),
            envelope_ids.drain(..).collect::<Vec<_>>(),
            message_ids.drain(..).collect::<Vec<_>>(),
        )
    };
    if drained.is_empty() {
        *inner.state.lock().await = prior_turn
            .map(|last_turn_id| HarnessState::TurnCompleted { last_turn_id })
            .unwrap_or(HarnessState::Idle);
        *inner.issued_turn_id.lock().await = None;
        return Ok(());
    }
    *inner.debounce.lock().await = DebounceState::default();
    let input_segments = Observation::input_segments_for(&drained);

    let joined_observation_text = input_segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let Some(thread_id) = inner.thread_id.read().await.clone() else {
        rebuffer_head(inner, drained, drained_envelope_ids, drained_message_ids).await;
        *inner.state.lock().await = HarnessState::PendingThreadStart;
        *inner.issued_turn_id.lock().await = None;
        persist_snapshot(inner).await?;
        return Ok(());
    };
    let drained_count = drained.len();
    let text = prepend_diff_block(diff.block, joined_observation_text);
    tracing::debug!(
        target: "calm_server::planner_harness_issue",
        runtime_id = %inner.worker_session_id,
        card_id = %inner.card_id,
        track_id = %inner.track_id,
        thread_id = %thread_id,
        drained_count,
        "calling daemon.turn_start"
    );

    match IssueTurnHandle::from_reconciliation(inner)
        .issue(&thread_id, vec![InputItem::text(text)])
        .await
    {
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
            *inner.last_turn_id.lock().await = Some(turn_id.clone());
            *inner.issued_turn_id.lock().await = Some(turn_id.clone());
            *inner.issued_turn_head.lock().await = diff.current_head.clone();
            *inner.issued_input_segments.lock().await = Some(IssuedInputSegments {
                turn_id,
                segments: input_segments,
            });
            persist_issuance_outcome(inner).await?;
        }
        Err(e) => {
            rebuffer_head(inner, drained, drained_envelope_ids, drained_message_ids).await;
            *inner.state.lock().await = prior_turn
                .map(|last_turn_id| HarnessState::TurnCompleted { last_turn_id })
                .unwrap_or(HarnessState::TurnCompleted {
                    last_turn_id: "unknown-turn".into(),
                });
            *inner.issued_turn_id.lock().await = None;
            *inner.issued_turn_head.lock().await = None;
            persist_issuance_outcome(inner).await?;
            tracing::warn!(error = %e, "planner harness turn/start failed; re-buffered batch");
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

async fn rebuffer_head(
    inner: &Arc<Inner>,
    drained: Vec<Observation>,
    drained_envelope_ids: Vec<Option<i64>>,
    drained_message_ids: Vec<Vec<String>>,
) {
    let mut queue = inner.pending_queue.lock().await;
    let mut envelope_ids = inner.pending_envelope_ids.lock().await;
    let mut message_ids = inner.pending_message_ids.lock().await;
    for obs in drained.into_iter().rev() {
        queue.push_front(obs);
    }
    for envelope_id in drained_envelope_ids.into_iter().rev() {
        envelope_ids.push_front(envelope_id);
    }
    // #1449 — a re-buffered batch keeps the ids it was drained with: it is the
    // same instances going back, not new ones.
    for ids in drained_message_ids.into_iter().rev() {
        message_ids.push_front(ids);
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
    let queue = inner.pending_queue.lock().await.iter().cloned().collect();
    let pending_envelope_ids = inner
        .pending_envelope_ids
        .lock()
        .await
        .iter()
        .copied()
        .collect();
    let pending_message_ids = inner
        .pending_message_ids
        .lock()
        .await
        .iter()
        .cloned()
        .collect();
    let push_watermark = *inner.push_watermark.lock().await;
    let last_thread_id = inner.thread_id.read().await.clone();
    let last_turn_id = inner.last_turn_id.lock().await.clone();
    let issued_turn_head = inner.issued_turn_head.lock().await.clone();
    let issued_input_segments = inner.issued_input_segments.lock().await.clone();
    let last_report_body_sha256 = inner.last_report_body_sha256.lock().await.clone();
    let last_seen_head = inner.last_seen_head.lock().await.clone();
    let token_usage = inner.token_usage.lock().await.clone();
    let mut snapshot = HarnessSnapshot::from_state(
        &state,
        push_watermark,
        crate::harness::snapshot::PendingQueueState {
            queue,
            envelope_ids: pending_envelope_ids,
            message_ids: pending_message_ids,
        },
        last_thread_id,
        last_turn_id,
        last_report_body_sha256,
    );
    snapshot.last_seen_head = last_seen_head;
    snapshot.issued_turn_head = issued_turn_head;
    snapshot.issued_input_segments = issued_input_segments;
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
    let runtime_id = inner.worker_session_id.clone();
    let snapshot_value = serde_json::to_value(snapshot)?;
    let now = crate::model::now_ms();
    let written = write_in_tx_typed(inner.repo.as_ref(), move |tx| {
        Box::pin(async move {
            crate::db::sqlite::session_set_handle_state_of_retired_runtime_tx(
                tx,
                &runtime_id,
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
/// first, and `observe_durable_observations` holds that same lock across both
/// the send and its confirmation.
///
/// Writing through the retired-row writer instead would not help for the
/// `superseded` case: that row is stamped, so what landed on it would not be
/// read again.
async fn persist_snapshot_for_durable_send(inner: &Arc<Inner>) -> Result<()> {
    if persist_snapshot_inner(inner, None).await? {
        return Ok(());
    }
    Err(CalmError::Conflict(
        "planner harness runtime is no longer this card's; retry to reach its successor".into(),
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
        HarnessObservationDelivery, map_observation_send_error, should_persist_item_method,
    };
    use crate::error::CalmError;
    use crate::harness::observation::Observation;
    use axum::http::StatusCode;
    use tokio::sync::mpsc;

    fn delivery(text: &str) -> HarnessObservationDelivery {
        HarnessObservationDelivery {
            observation: Observation::TrackGoal { text: text.into() },
            envelope_id: None,
        }
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

    #[test]
    fn user_message_folds_with_paragraph_breaks() {
        use super::try_fold_pending_tail;
        use crate::harness::observation::Observation;
        use std::collections::VecDeque;

        let mut queue: VecDeque<Observation> = VecDeque::new();
        let mut env_ids: VecDeque<Option<i64>> = VecDeque::new();
        let mut msg_ids: VecDeque<Vec<String>> = VecDeque::new();
        queue.push_back(Observation::UserMessage {
            text: "first message".into(),
        });
        env_ids.push_back(Some(1));
        msg_ids.push_back(vec!["first-instance".to_string()]);

        let folded = try_fold_pending_tail(
            &mut queue,
            &mut env_ids,
            &mut msg_ids,
            &Observation::UserMessage {
                text: "second message".into(),
            },
            Some(2),
            &["second-instance".to_string()],
        );

        assert!(folded);
        assert_eq!(queue.len(), 1);
        let Some(Observation::UserMessage { text }) = queue.back() else {
            panic!("expected single folded UserMessage, got {:?}", queue);
        };
        assert_eq!(text, "first message\n\nsecond message");
        assert_eq!(
            env_ids.back().copied().flatten(),
            Some(2),
            "folded envelope id should advance to the newest send"
        );
        // #1449 — the envelope id ADVANCES to the newest send, but the message
        // ids UNION. They answer different questions: one is "which push am I
        // acknowledging", the other is "which instances am I still holding",
        // and a fold is still holding both.
        assert_eq!(
            msg_ids.back().cloned().unwrap_or_default(),
            vec!["first-instance".to_string(), "second-instance".to_string()],
            "a fold must keep BOTH instances identifiable; dropping one is the loss of \
             identity the ids exist to prevent"
        );
    }

    #[test]
    fn user_message_does_not_fold_with_other_kinds() {
        use super::try_fold_pending_tail;
        use crate::harness::observation::Observation;
        use std::collections::VecDeque;

        let mut queue: VecDeque<Observation> = VecDeque::new();
        let mut env_ids: VecDeque<Option<i64>> = VecDeque::new();
        let mut msg_ids: VecDeque<Vec<String>> = VecDeque::new();
        queue.push_back(Observation::TrackGoal {
            text: "goal".into(),
        });
        env_ids.push_back(None);

        let folded = try_fold_pending_tail(
            &mut queue,
            &mut env_ids,
            &mut msg_ids,
            &Observation::UserMessage {
                text: "user".into(),
            },
            None,
            &[],
        );

        assert!(!folded, "UserMessage must not fold into TrackGoal");
        assert_eq!(
            queue.len(),
            1,
            "non-folding path should not mutate the queue"
        );
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
    fn user_message_fold_refuses_beyond_cap() {
        use super::{MAX_FOLDED_USER_MESSAGE_CHARS, try_fold_pending_tail};
        use crate::harness::observation::Observation;
        use std::collections::VecDeque;

        let mut queue: VecDeque<Observation> = VecDeque::new();
        let mut env_ids: VecDeque<Option<i64>> = VecDeque::new();
        let mut msg_ids: VecDeque<Vec<String>> = VecDeque::new();
        let seed = "a".repeat(MAX_FOLDED_USER_MESSAGE_CHARS - 1);
        queue.push_back(Observation::UserMessage { text: seed });
        env_ids.push_back(Some(1));

        let folded = try_fold_pending_tail(
            &mut queue,
            &mut env_ids,
            &mut msg_ids,
            &Observation::UserMessage {
                text: "x".repeat(10),
            },
            Some(2),
            &["second-instance".to_string()],
        );

        assert!(!folded, "fold must refuse when result would exceed cap");
        let Some(Observation::UserMessage { text }) = queue.back() else {
            panic!("expected UserMessage tail");
        };
        assert_eq!(text.chars().count(), MAX_FOLDED_USER_MESSAGE_CHARS - 1);
        assert_eq!(env_ids.back().copied().flatten(), Some(1));
    }
}
