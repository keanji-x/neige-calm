use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tokio::task::AbortHandle;

use crate::card_role_cache::CardRoleCache;
use crate::codex_appserver::{InputItem, Notification};
use crate::db::{Repo, write_in_tx_typed};
use crate::error::{CalmError, Result};
use crate::event::{Event, EventBus, EventScope};
use crate::harness::config::HarnessConfig;
use crate::harness::observation::Observation;
use crate::harness::snapshot::{HarnessPhaseTag, HarnessSnapshot};
use crate::harness::state::{HarnessState, IssuingKind, run_status_for};
use crate::ids::{ActorId, CardId, WaveId};
use crate::model::now_ms;
use crate::runtime_repo::{RunStatus, RuntimeId};
use crate::shared_codex_appserver::SharedCodexAppServer;
use crate::wave_cove_cache::WaveCoveCache;

const OBSERVATION_BUFFER: usize = 256;
const MAX_PENDING_QUEUE_LEN: usize = 256;
const RECENT_HOOK_KEY_CACHE_LEN: usize = 256;

#[derive(Clone)]
pub struct SpecHarness {
    inner: Arc<Inner>,
}

pub struct SpecHarnessParams {
    pub runtime_id: RuntimeId,
    pub wave_id: WaveId,
    pub card_id: CardId,
    pub thread_id: Option<String>,
    pub repo: Arc<dyn Repo>,
    pub events: EventBus,
    pub card_role_cache: CardRoleCache,
    pub wave_cove_cache: WaveCoveCache,
    pub daemon: Arc<SharedCodexAppServer>,
    pub config: HarnessConfig,
    pub snapshot: HarnessSnapshot,
}

pub(super) struct Inner {
    runtime_id: RuntimeId,
    wave_id: WaveId,
    card_id: CardId,
    thread_id: RwLock<Option<String>>,
    repo: Arc<dyn Repo>,
    events: EventBus,
    card_role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
    daemon: Arc<SharedCodexAppServer>,
    observations: mpsc::Sender<HarnessObservationDelivery>,
    state: Mutex<HarnessState>,
    last_phase: Mutex<HarnessPhaseTag>,
    pending_queue: Mutex<VecDeque<Observation>>,
    pending_envelope_ids: Mutex<VecDeque<Option<i64>>>,
    recent_hook_keys: Mutex<VecDeque<String>>,
    recent_hook_key_set: Mutex<HashSet<String>>,
    push_watermark: Mutex<i64>,
    last_turn_id: Mutex<Option<String>>,
    last_report_body_sha256: Mutex<Option<String>>,
    debounce: Mutex<DebounceState>,
    interrupt_deadline: Mutex<Option<(String, Instant)>>,
    shutdown: broadcast::Sender<()>,
    shutting_down: Arc<AtomicBool>,
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

#[derive(Clone, Copy, Debug, Default)]
struct DebounceState {
    first_pending_at: Option<Instant>,
    last_pending_at: Option<Instant>,
    hard_fire: bool,
}

impl SpecHarness {
    pub fn run(params: SpecHarnessParams) -> Self {
        params.snapshot.assert_known_schema();
        let (obs_tx, obs_rx) = mpsc::channel(OBSERVATION_BUFFER);
        let (shutdown_tx, shutdown_rx) = broadcast::channel(4);
        let notifications = params.daemon.subscribe_notifications();
        let inner = inner_from_params(params, obs_tx, shutdown_tx);
        let handle = Self {
            inner: Arc::clone(&inner),
        };
        let task = tokio::spawn(run_loop(inner, obs_rx, shutdown_rx, notifications));
        let abort = task.abort_handle();
        *handle
            .inner
            .abort_handle
            .lock()
            .expect("spec harness abort handle mutex poisoned") = Some(abort);
        tokio::spawn(async move {
            let _ = task.await;
        });
        handle
    }

    #[cfg(feature = "fixtures")]
    pub fn run_unstarted_for_test(
        params: SpecHarnessParams,
        observation_buffer: usize,
    ) -> (Self, mpsc::Receiver<HarnessObservationDelivery>) {
        params.snapshot.assert_known_schema();
        let (obs_tx, obs_rx) = mpsc::channel(observation_buffer);
        let (shutdown_tx, _shutdown_rx) = broadcast::channel(4);
        let inner = inner_from_params(params, obs_tx, shutdown_tx);
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
        self.inner
            .observations
            .try_send(delivery)
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => {
                    CalmError::Internal("spec harness observation queue full".into())
                }
                mpsc::error::TrySendError::Closed(_) => {
                    CalmError::Internal("spec harness observation queue closed".into())
                }
            })
    }

    pub async fn interrupt(&self, reason: String) -> Result<()> {
        issue_interrupt(&self.inner, reason).await
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.inner.shutting_down.store(true, Ordering::SeqCst);
        let _ = self.inner.shutdown.send(());
        self.persist_snapshot().await?;
        if let Some(thread_id) = self.inner.thread_id.read().await.clone()
            && let Err(e) = self.inner.daemon.interrupt_active_turn(&thread_id).await
        {
            tracing::warn!(
                thread_id,
                error = %e,
                "spec harness shutdown thread interrupt failed"
            );
        }
        let abort = self
            .inner
            .abort_handle
            .lock()
            .expect("spec harness abort handle mutex poisoned")
            .take();
        if let Some(abort) = abort {
            abort.abort();
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
        on_observation(&self.inner, obs, envelope_id).await;
    }

    pub async fn set_state_for_test(&self, state: HarnessState) {
        *self.inner.state.lock().await = state;
    }
}

fn inner_from_params(
    params: SpecHarnessParams,
    observations: mpsc::Sender<HarnessObservationDelivery>,
    shutdown: broadcast::Sender<()>,
) -> Arc<Inner> {
    let mut snapshot = params.snapshot;
    snapshot.align_pending_envelope_ids();
    truncate_snapshot_pending_queue(&mut snapshot);
    let debounce = debounce_from_initial_queue(&snapshot.pending_queue);
    let state = state_from_snapshot(&snapshot);
    let last_phase = snapshot.phase;
    Arc::new(Inner {
        runtime_id: params.runtime_id,
        wave_id: params.wave_id,
        card_id: params.card_id,
        thread_id: RwLock::new(params.thread_id.or(snapshot.last_thread_id.clone())),
        repo: params.repo,
        events: params.events,
        card_role_cache: params.card_role_cache,
        wave_cove_cache: params.wave_cove_cache,
        daemon: params.daemon,
        observations,
        state: Mutex::new(state),
        last_phase: Mutex::new(last_phase),
        pending_envelope_ids: Mutex::new(snapshot.pending_envelope_ids.into_iter().collect()),
        pending_queue: Mutex::new(snapshot.pending_queue.into_iter().collect()),
        recent_hook_keys: Mutex::new(VecDeque::with_capacity(RECENT_HOOK_KEY_CACHE_LEN)),
        recent_hook_key_set: Mutex::new(HashSet::with_capacity(RECENT_HOOK_KEY_CACHE_LEN)),
        push_watermark: Mutex::new(snapshot.push_watermark),
        last_turn_id: Mutex::new(snapshot.last_turn_id),
        last_report_body_sha256: Mutex::new(snapshot.last_report_body_sha256),
        debounce: Mutex::new(debounce),
        interrupt_deadline: Mutex::new(None),
        shutdown,
        shutting_down: Arc::new(AtomicBool::new(false)),
        abort_handle: StdMutex::new(None),
        config: params.config,
    })
}

fn harness_event_scope(inner: &Inner, event_name: &'static str) -> EventScope {
    let card = inner.card_id.clone();
    let wave = inner.wave_id.clone();
    match inner.wave_cove_cache.cove_of(&wave) {
        Some(cove) => EventScope::Card { card, wave, cove },
        None => {
            tracing::warn!(
                runtime_id = %inner.runtime_id,
                card_id = %card,
                wave_id = %wave,
                event_name,
                "spec harness event missing wave cove cache entry; using system scope"
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

async fn run_loop(
    inner: Arc<Inner>,
    mut observations: mpsc::Receiver<HarnessObservationDelivery>,
    mut shutdown: broadcast::Receiver<()>,
    mut notifications: broadcast::Receiver<Notification>,
) {
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    loop {
        tokio::select! {
            delivery = observations.recv() => {
                let Some(delivery) = delivery else { break };
                on_observation(&inner, delivery.observation, delivery.envelope_id).await;
                if let Err(e) = persist_snapshot(&inner).await {
                    tracing::warn!(error = %e, "spec harness snapshot persist failed after observation");
                }
            }
            notif = notifications.recv() => {
                match notif {
                    Ok(notif) => {
                        if let Err(e) = on_notification(&inner, notif).await {
                            tracing::warn!(error = %e, "spec harness notification handling failed");
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "spec harness notification receiver lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = tick.tick() => {
                if let Err(e) = watchdog_tick(&inner).await {
                    tracing::warn!(error = %e, "spec harness watchdog tick failed");
                }
                if let Err(e) = maybe_issue_turn(&inner).await {
                    tracing::warn!(error = %e, "spec harness turn issuance failed");
                }
            }
            _ = shutdown.recv() => {
                break;
            }
        }
    }
}

async fn on_observation(inner: &Arc<Inner>, obs: Observation, envelope_id: Option<i64>) {
    if let Some(envelope_id) = envelope_id {
        let mut watermark = inner.push_watermark.lock().await;
        *watermark = (*watermark).max(envelope_id);
    }
    if suppress_duplicate_hook_stop(inner, &obs).await {
        return;
    }
    let hard_fire = obs.is_hard_fire();
    if !enqueue_pending_observation(inner, obs.clone(), envelope_id).await {
        return;
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
}

fn truncate_snapshot_pending_queue(snapshot: &mut HarnessSnapshot) {
    let len = snapshot.pending_queue.len();
    if len <= MAX_PENDING_QUEUE_LEN {
        return;
    }
    let drop_count = len - MAX_PENDING_QUEUE_LEN;
    snapshot.pending_queue.drain(..drop_count);
    snapshot.pending_envelope_ids.drain(..drop_count);
    tracing::warn!(
        target: "spec.harness.backpressure",
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
    if queue.len() >= MAX_PENDING_QUEUE_LEN {
        if try_fold_pending_tail(&mut queue, &mut envelope_ids, &obs, envelope_id) {
            return true;
        }
        if !obs.is_hard_fire()
            && let Some(drop_idx) = queue.iter().position(|queued| !queued.is_hard_fire())
        {
            queue.remove(drop_idx);
            envelope_ids.remove(drop_idx);
        } else {
            tracing::warn!(
                target: "spec.harness.backpressure",
                queue_len = queue.len(),
                variant = ?obs,
                "pending_queue full, hard obs dropped"
            );
            return false;
        }
    }
    queue.push_back(obs);
    envelope_ids.push_back(envelope_id);
    true
}

fn try_fold_pending_tail(
    queue: &mut VecDeque<Observation>,
    envelope_ids: &mut VecDeque<Option<i64>>,
    obs: &Observation,
    envelope_id: Option<i64>,
) -> bool {
    let Some(last) = queue.back_mut() else {
        return false;
    };
    let folded = match (last, obs) {
        (Observation::WaveGoal { text }, Observation::WaveGoal { text: new_text }) => {
            *text = new_text.clone();
            true
        }
        (
            Observation::ReportEdited {
                wave_id,
                body_sha256,
                body,
            },
            Observation::ReportEdited {
                wave_id: new_wave_id,
                body_sha256: new_body_sha256,
                body: new_body,
            },
        ) if wave_id == new_wave_id => {
            *body_sha256 = new_body_sha256.clone();
            *body = new_body.clone();
            true
        }
        _ => false,
    };
    if folded && let Some(last_envelope_id) = envelope_ids.back_mut() {
        *last_envelope_id = envelope_id;
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
            target: "spec.harness.dedupe",
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
            "spec harness ignoring approval-shaped notification under approval_policy=never"
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
            } else if status.get("type").and_then(Value::as_str) == Some("idle") {
                let mut state = inner.state.lock().await;
                if matches!(*state, HarnessState::Resumed { .. }) {
                    *state = HarnessState::Idle;
                }
            }
        }
        Notification::TurnStarted { turn, .. } => {
            let turn_id = turn
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("unknown-turn")
                .to_string();
            *inner.last_turn_id.lock().await = Some(turn_id.clone());
            *inner.state.lock().await = HarnessState::TurnRunning {
                turn_id,
                started_at: Instant::now(),
            };
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
                        "spec harness ignoring non-target completion while interrupt is pending"
                    );
                    return persist_snapshot(inner).await;
                }
                *inner.last_turn_id.lock().await = Some(target_turn_id.clone());
                *inner.state.lock().await = HarnessState::TurnCompleted {
                    last_turn_id: target_turn_id,
                };
                *inner.interrupt_deadline.lock().await = None;
                return persist_snapshot(inner).await;
            }
            *inner.last_turn_id.lock().await = Some(turn_id.clone());
            *inner.state.lock().await = HarnessState::TurnCompleted {
                last_turn_id: turn_id,
            };
            *inner.interrupt_deadline.lock().await = None;
        }
        Notification::Other { method, params } if method == "turn/aborted" => {
            let Some(aborted_turn_id) = other_turn_id(&params).map(ToOwned::to_owned) else {
                tracing::debug!("spec harness ignoring turn/aborted without a turn id");
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
                    "spec harness ignoring turn/aborted outside interrupt issuance"
                );
                return persist_snapshot(inner).await;
            };
            if aborted_turn_id != target_turn_id {
                tracing::debug!(
                    observed_turn_id = %aborted_turn_id,
                    target_turn_id = %target_turn_id,
                    "spec harness ignoring non-target aborted turn while interrupt is pending"
                );
                return persist_snapshot(inner).await;
            }
            *inner.last_turn_id.lock().await = Some(target_turn_id.clone());
            *inner.state.lock().await = HarnessState::TurnCompleted {
                last_turn_id: target_turn_id,
            };
            *inner.interrupt_deadline.lock().await = None;
        }
        Notification::Item { method, params } if should_persist_item_method(&method) => {
            let Some(item) = params.get("item") else {
                tracing::debug!(
                    method,
                    "spec harness ignoring item notification without item"
                );
                return persist_snapshot(inner).await;
            };
            let Some(thread_id) = inner.thread_id.read().await.clone() else {
                tracing::warn!(
                    runtime_id = %inner.runtime_id,
                    card_id = %inner.card_id,
                    method,
                    "spec harness item notification arrived before thread id was known"
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
            let item_db_id = inner
                .repo
                .harness_item_insert(
                    &inner.runtime_id,
                    inner.card_id.as_str(),
                    inner.wave_id.as_str(),
                    &thread_id,
                    turn_id.as_deref(),
                    item_uuid.as_deref(),
                    item_type.as_deref(),
                    &method,
                    &params_json,
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
                    &inner.wave_cove_cache,
                    Event::HarnessItemAdded {
                        runtime_id: inner.runtime_id.clone(),
                        card_id: inner.card_id.clone(),
                        wave_id: inner.wave_id.clone(),
                        item_db_id,
                        item_uuid,
                        item_type,
                        turn_id,
                        method,
                    },
                )
                .await?;
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

async fn maybe_issue_turn(inner: &Arc<Inner>) -> Result<()> {
    {
        let queue = inner.pending_queue.lock().await;
        if queue.is_empty() {
            return Ok(());
        }
    }
    let now = Instant::now();
    let should_issue = {
        let debounce = inner.debounce.lock().await;
        if debounce.hard_fire {
            true
        } else {
            let Some(first) = debounce.first_pending_at else {
                return Ok(());
            };
            let Some(last) = debounce.last_pending_at else {
                return Ok(());
            };
            now.duration_since(last) >= inner.config.debounce_min_idle
                || now.duration_since(first) >= inner.config.debounce_max_wait
        }
    };
    if !should_issue {
        return Ok(());
    }

    let prior_turn = {
        let mut state = inner.state.lock().await;
        if !state.can_issue_turn() {
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
    persist_snapshot(inner).await?;

    let (drained, drained_envelope_ids) = {
        let mut queue = inner.pending_queue.lock().await;
        let mut envelope_ids = inner.pending_envelope_ids.lock().await;
        (
            queue.drain(..).collect::<Vec<_>>(),
            envelope_ids.drain(..).collect::<Vec<_>>(),
        )
    };
    if drained.is_empty() {
        *inner.state.lock().await = prior_turn
            .map(|last_turn_id| HarnessState::TurnCompleted { last_turn_id })
            .unwrap_or(HarnessState::Idle);
        return Ok(());
    }
    *inner.debounce.lock().await = DebounceState::default();

    let text = drained
        .iter()
        .map(Observation::to_turn_text)
        .collect::<Vec<_>>()
        .join("\n");
    let Some(thread_id) = inner.thread_id.read().await.clone() else {
        rebuffer_head(inner, drained, drained_envelope_ids).await;
        *inner.state.lock().await = HarnessState::PendingThreadStart;
        persist_snapshot(inner).await?;
        return Ok(());
    };

    match IssueTurnHandle::from_reconciliation(inner)
        .issue(&thread_id, vec![InputItem::text(text)])
        .await
    {
        Ok(turn_id) => {
            *inner.last_turn_id.lock().await = Some(turn_id);
            persist_snapshot(inner).await?;
        }
        Err(e) => {
            rebuffer_head(inner, drained, drained_envelope_ids).await;
            *inner.state.lock().await = prior_turn
                .map(|last_turn_id| HarnessState::TurnCompleted { last_turn_id })
                .unwrap_or(HarnessState::TurnCompleted {
                    last_turn_id: "unknown-turn".into(),
                });
            persist_snapshot(inner).await?;
            tracing::warn!(error = %e, "spec harness turn/start failed; re-buffered batch");
        }
    }
    Ok(())
}

async fn rebuffer_head(
    inner: &Arc<Inner>,
    drained: Vec<Observation>,
    drained_envelope_ids: Vec<Option<i64>>,
) {
    let mut queue = inner.pending_queue.lock().await;
    let mut envelope_ids = inner.pending_envelope_ids.lock().await;
    for obs in drained.into_iter().rev() {
        queue.push_front(obs);
    }
    for envelope_id in drained_envelope_ids.into_iter().rev() {
        envelope_ids.push_front(envelope_id);
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
                    "spec harness interrupt ignored because no turn is active"
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
        tracing::debug!("spec harness interrupt ignored because no active turn id is known");
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
            "spec harness turn/interrupt failed; interrupt timeout watchdog remains armed"
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
    let push_watermark = *inner.push_watermark.lock().await;
    let last_thread_id = inner.thread_id.read().await.clone();
    let last_turn_id = inner.last_turn_id.lock().await.clone();
    let last_report_body_sha256 = inner.last_report_body_sha256.lock().await.clone();
    HarnessSnapshot::from_state(
        &state,
        push_watermark,
        queue,
        pending_envelope_ids,
        last_thread_id,
        last_turn_id,
        last_report_body_sha256,
    )
}

async fn persist_snapshot(inner: &Arc<Inner>) -> Result<()> {
    if inner.shutting_down.load(Ordering::SeqCst) {
        return Ok(());
    }
    let snapshot = snapshot_for(inner).await;
    let runtime_id = inner.runtime_id.clone();
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
    let status_db = run_status_to_db(&status);
    let new_phase = snapshot.phase;
    let event_runtime_id = runtime_id.clone();
    let event_card_id = inner.card_id.clone();
    let event_wave_id = inner.wave_id.clone();
    let snapshot_value = serde_json::to_value(snapshot)?;
    let repo = Arc::clone(&inner.repo);

    write_in_tx_typed(repo.as_ref(), move |tx| {
        Box::pin(async move {
            crate::db::sqlite::runtime_set_handle_state_tx(tx, &runtime_id, Some(snapshot_value))
                .await?;
            sqlx::query(
                r#"UPDATE runtimes
                      SET status = ?1,
                          thread_id = COALESCE(?2, thread_id),
                          active_turn_id = ?3,
                          updated_at_ms = ?4
                    WHERE id = ?5"#,
            )
            .bind(status_db)
            .bind(thread_id)
            .bind(active_turn_id)
            .bind(now_ms())
            .bind(&runtime_id)
            .execute(&mut **tx)
            .await?;
            Ok(())
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
                &inner.wave_cove_cache,
                Event::HarnessPhaseChanged {
                    runtime_id: event_runtime_id,
                    card_id: event_card_id,
                    wave_id: event_wave_id,
                    old_phase,
                    new_phase,
                },
            )
            .await
        {
            tracing::warn!(
                runtime_id = %inner.runtime_id,
                card_id = %inner.card_id,
                wave_id = %inner.wave_id,
                ?old_phase,
                ?new_phase,
                error = %e,
                "spec harness phase event persist failed; retaining previous phase for retry"
            );
            return Err(e);
        }
        *last_phase = new_phase;
    }
    Ok(())
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

fn run_status_to_db(status: &RunStatus) -> &'static str {
    match status {
        RunStatus::Starting => "starting",
        RunStatus::Running => "running",
        RunStatus::Idle => "idle",
        RunStatus::TurnPending => "turn_pending",
        RunStatus::Failed => "failed",
        RunStatus::Exited => "exited",
        RunStatus::Superseded => "superseded",
    }
}

#[cfg(test)]
mod tests {
    use super::should_persist_item_method;

    #[test]
    fn item_persistence_filter_keeps_terminal_items_and_drops_deltas() {
        assert!(should_persist_item_method("item/started"));
        assert!(should_persist_item_method("item/completed"));

        assert!(!should_persist_item_method("item/agentMessage/delta"));
        assert!(!should_persist_item_method("item/reasoning/delta"));
        assert!(!should_persist_item_method("turn/completed"));
        assert!(!should_persist_item_method("item/other"));
    }
}
