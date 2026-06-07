use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tokio::task::AbortHandle;

use crate::codex_appserver::{InputItem, Notification};
use crate::db::{Repo, write_in_tx_typed};
use crate::error::Result;
use crate::harness::config::HarnessConfig;
use crate::harness::observation::Observation;
use crate::harness::snapshot::{HarnessPhaseTag, HarnessSnapshot};
use crate::harness::state::{HarnessState, IssuingKind, run_status_for};
use crate::ids::{CardId, WaveId};
use crate::model::now_ms;
use crate::runtime_repo::{RunStatus, RuntimeId};
use crate::shared_codex_appserver::SharedCodexAppServer;

const OBSERVATION_BUFFER: usize = 256;

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
    pub daemon: Arc<SharedCodexAppServer>,
    pub config: HarnessConfig,
    pub snapshot: HarnessSnapshot,
}

struct Inner {
    runtime_id: RuntimeId,
    _wave_id: WaveId,
    card_id: CardId,
    thread_id: RwLock<Option<String>>,
    repo: Arc<dyn Repo>,
    daemon: Arc<SharedCodexAppServer>,
    observations: mpsc::Sender<Observation>,
    state: Mutex<HarnessState>,
    pending_queue: Mutex<VecDeque<Observation>>,
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
        let state = state_from_snapshot(&params.snapshot);
        let inner = Arc::new(Inner {
            runtime_id: params.runtime_id,
            _wave_id: params.wave_id,
            card_id: params.card_id,
            thread_id: RwLock::new(params.thread_id.or(params.snapshot.last_thread_id.clone())),
            repo: params.repo,
            daemon: params.daemon,
            observations: obs_tx,
            state: Mutex::new(state),
            pending_queue: Mutex::new(params.snapshot.pending_queue.into_iter().collect()),
            push_watermark: Mutex::new(params.snapshot.push_watermark),
            last_turn_id: Mutex::new(params.snapshot.last_turn_id),
            last_report_body_sha256: Mutex::new(params.snapshot.last_report_body_sha256),
            debounce: Mutex::new(DebounceState::default()),
            interrupt_deadline: Mutex::new(None),
            shutdown: shutdown_tx,
            shutting_down: Arc::new(AtomicBool::new(false)),
            abort_handle: StdMutex::new(None),
            config: params.config,
        });
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

    pub fn observe(&self, obs: Observation) {
        if let Err(e) = self.inner.observations.try_send(obs) {
            tracing::warn!(error = %e, "spec harness observation queue full or closed");
        }
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

    pub async fn set_state_for_test(&self, state: HarnessState) {
        *self.inner.state.lock().await = state;
    }
}

async fn run_loop(
    inner: Arc<Inner>,
    mut observations: mpsc::Receiver<Observation>,
    mut shutdown: broadcast::Receiver<()>,
    mut notifications: broadcast::Receiver<Notification>,
) {
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    loop {
        tokio::select! {
            obs = observations.recv() => {
                let Some(obs) = obs else { break };
                on_observation(&inner, obs).await;
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

async fn on_observation(inner: &Arc<Inner>, obs: Observation) {
    if let Some(hash) = obs.report_sha256() {
        *inner.last_report_body_sha256.lock().await = Some(hash.to_string());
    }
    let hard_fire = obs.is_hard_fire();
    inner.pending_queue.lock().await.push_back(obs);
    let now = Instant::now();
    let mut debounce = inner.debounce.lock().await;
    if debounce.first_pending_at.is_none() {
        debounce.first_pending_at = Some(now);
    }
    debounce.last_pending_at = Some(now);
    debounce.hard_fire |= hard_fire;
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
            if inner.interrupt_deadline.lock().await.is_some()
                && turn.get("status").and_then(Value::as_str) != Some("interrupted")
            {
                tracing::warn!(
                    status = ?turn.get("status"),
                    "spec harness waiting for interrupted completion before clearing interrupt watchdog"
                );
                return persist_snapshot(inner).await;
            }
            let fallback_turn_id = inner.last_turn_id.lock().await.clone();
            let turn_id = turn
                .get("id")
                .and_then(Value::as_str)
                .or(fallback_turn_id.as_deref())
                .unwrap_or("unknown-turn")
                .to_string();
            *inner.last_turn_id.lock().await = Some(turn_id.clone());
            *inner.state.lock().await = HarnessState::TurnCompleted {
                last_turn_id: turn_id,
            };
            *inner.interrupt_deadline.lock().await = None;
        }
        Notification::Item { .. } | Notification::Other { .. } => {}
    }
    persist_snapshot(inner).await
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

    let drained = {
        let mut queue = inner.pending_queue.lock().await;
        queue.drain(..).collect::<Vec<_>>()
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
        rebuffer_head(inner, drained).await;
        *inner.state.lock().await = HarnessState::PendingThreadStart;
        persist_snapshot(inner).await?;
        return Ok(());
    };

    match inner
        .daemon
        .turn_start(&thread_id, vec![InputItem::text(text)])
        .await
    {
        Ok(turn_id) => {
            *inner.last_turn_id.lock().await = Some(turn_id);
            persist_snapshot(inner).await?;
        }
        Err(e) => {
            rebuffer_head(inner, drained).await;
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

async fn rebuffer_head(inner: &Arc<Inner>, drained: Vec<Observation>) {
    let mut queue = inner.pending_queue.lock().await;
    for obs in drained.into_iter().rev() {
        queue.push_front(obs);
    }
    let now = Instant::now();
    *inner.debounce.lock().await = DebounceState {
        first_pending_at: Some(now),
        last_pending_at: Some(now),
        hard_fire: true,
    };
}

async fn watchdog_tick(inner: &Arc<Inner>) -> Result<()> {
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
    let push_watermark = *inner.push_watermark.lock().await;
    let last_thread_id = inner.thread_id.read().await.clone();
    let last_turn_id = inner.last_turn_id.lock().await.clone();
    let last_report_body_sha256 = inner.last_report_body_sha256.lock().await.clone();
    HarnessSnapshot::from_state(
        &state,
        push_watermark,
        queue,
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
    let card_id = inner.card_id.to_string();
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
    let watermark = snapshot.push_watermark;
    let snapshot_value = serde_json::to_value(snapshot)?;
    let repo = Arc::clone(&inner.repo);

    write_in_tx_typed(repo.as_ref(), move |tx| {
        Box::pin(async move {
            crate::db::sqlite::runtime_set_handle_state_tx(
                tx,
                &runtime_id,
                Some(snapshot_value),
            )
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
            sqlx::query(
                r#"UPDATE cards
                      SET payload = json_set(
                                      COALESCE(payload, '{}'),
                                      '$.push_watermark',
                                      CASE
                                          WHEN COALESCE(json_extract(payload, '$.push_watermark'), 0) < ?1
                                          THEN ?1
                                          ELSE COALESCE(json_extract(payload, '$.push_watermark'), 0)
                                      END
                                   ),
                          updated_at = ?2
                    WHERE id = ?3"#,
            )
            .bind(watermark)
            .bind(now_ms())
            .bind(card_id)
            .execute(&mut **tx)
            .await?;
            Ok(())
        })
    })
    .await
}

fn state_from_snapshot(snapshot: &HarnessSnapshot) -> HarnessState {
    let now = Instant::now();
    match snapshot.phase {
        HarnessPhaseTag::PendingThreadStart => HarnessState::PendingThreadStart,
        HarnessPhaseTag::Idle => HarnessState::Idle,
        // IssuingTurn is persisted before turn/start fires, so retrying after
        // recovery is correct and cannot duplicate a turn codex has seen.
        HarnessPhaseTag::IssuingTurn => HarnessState::TurnCompleted {
            last_turn_id: snapshot.last_turn_id.clone().unwrap_or_default(),
        },
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
