//! Ending a `running` Track worker's execution (#1785 S1): the liveness windows, a codex worker
//! whose turn ended (normally or in an error) without a task report, and the Planner's cancel
//! all fail or cancel the row and write the same cleanup marker, which the reconcile sweep turns
//! into a worker reap.

use std::sync::Arc;
use std::time::Duration;

use crate::harness::backend::factory::{PlannerFactory, TurnEnd};
pub use crate::harness::backend::factory::{WorkerIdleClock, WorkerIdleWake};
use dashmap::DashMap;

use super::{InflightGuard, LivenessExpiry, Scheduler, is_race_lost, race_lost_err};
use crate::db::sqlite::{TaskReporter, task_fail_from_worker_tx, task_running_liveness_tx};
use crate::db::write_with_actor_events_typed;
use crate::error::Result;
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use crate::model::{Task, Track, now_ms};

/// How long a codex worker's last turn must have been over, with the thread at rest (`idle` or
/// `systemError`) and no turn active, before the sweep fails its task as `worker-turn-ended`.
pub const WORKER_IDLE_TURN_GRACE: Duration = Duration::from_secs(300);

/// Bound on one live `thread/read` recheck (connect plus two RPCs of at most 10 s each).
pub const WORKER_IDLE_PROBE_TIMEOUT: Duration = Duration::from_secs(25);

/// `status_detail` of a codex task whose worker turn ended without a report.
pub const WORKER_TURN_ENDED: &str = "worker-turn-ended";

/// `status_detail` of a running task the Planner canceled.
pub const PLANNER_CANCELED: &str = "planner-canceled";

/// Why a worker's cleanup marker was written; persisted as the marker's `reason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkerCleanupReason {
    LivenessTimeout,
    TurnEnded,
    PlannerCanceled,
}

impl WorkerCleanupReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::LivenessTimeout => "running_liveness_timeout",
            Self::TurnEnded => "worker_turn_ended",
            Self::PlannerCanceled => "planner_canceled",
        }
    }
}

/// Fixtures only: parks a liveness fail after the sweep judged the worker expired and before the
/// fail tx re-reads its facts, so a test can land progress in between.
#[cfg(feature = "fixtures")]
#[doc(hidden)]
#[derive(Clone)]
pub struct LivenessFailTestHook {
    pub judged: Arc<tokio::sync::Notify>,
    pub resume: Arc<tokio::sync::Notify>,
}

/// How a running worker's execution ends; each variant fixes the row detail and the event text.
pub(super) enum RunningWorkerFailure {
    /// The sweep saw this window outlived; the fail tx re-judges it on fresh facts.
    LivenessTimeout(LivenessExpiry),
    /// The live read ran against this card's thread, so the fail CAS is pinned to it.
    TurnEnded { card_id: String, end: TurnEnd },
}

impl RunningWorkerFailure {
    const fn detail(&self) -> &'static str {
        match self {
            Self::LivenessTimeout(_) => "worker-timeout",
            Self::TurnEnded { .. } => WORKER_TURN_ENDED,
        }
    }

    fn reason(&self) -> String {
        match self {
            Self::LivenessTimeout(expiry) => expiry.reason(),
            Self::TurnEnded {
                end: TurnEnd::Ended,
                ..
            } => "worker turn ended without a task report".to_string(),
            Self::TurnEnded {
                end: TurnEnd::Failed { cause },
                ..
            } => {
                let base = "worker turn ended in an error without a task report";
                match cause {
                    Some(cause) => {
                        use provider::worker::WorkerFailureKind;
                        let next_step = match cause.kind {
                            WorkerFailureKind::ConcurrencyLimit => {
                                "Retry later with a new task key"
                            }
                            WorkerFailureKind::ContentRefusal => {
                                "Rewrite the task before retrying with a new task key"
                            }
                            WorkerFailureKind::ProviderError => {
                                "Review the provider error before retrying"
                            }
                        };
                        format!(
                            "{base}; category={}; next step: {next_step}; provider error (untrusted, truncated={}): {}",
                            cause.kind.as_str(),
                            cause.truncated,
                            cause.message
                        )
                    }
                    None => base.to_string(),
                }
            }
        }
    }

    fn details(&self) -> Option<serde_json::Value> {
        match self {
            Self::TurnEnded {
                end: TurnEnd::Failed { cause: Some(cause) },
                ..
            } => Some(serde_json::json!({"cause": cause})),
            _ => None,
        }
    }

    const fn cleanup_reason(&self) -> WorkerCleanupReason {
        match self {
            Self::LivenessTimeout(_) => WorkerCleanupReason::LivenessTimeout,
            Self::TurnEnded { .. } => WorkerCleanupReason::TurnEnded,
        }
    }

    fn guard_card_id(&self) -> Option<&str> {
        match self {
            Self::LivenessTimeout(_) => None,
            Self::TurnEnded { card_id, .. } => Some(card_id),
        }
    }
}

impl Scheduler {
    pub(super) fn new_idle_checks() -> Arc<DashMap<String, ()>> {
        Arc::new(DashMap::new())
    }

    /// Idle checks spawned by a sweep and not finished yet. Exposed for test synchronization.
    #[doc(hidden)]
    pub fn worker_idle_checks_in_flight(&self) -> usize {
        self.idle_checks.len()
    }

    /// Sweep idle arm for one `running` codex row inside its deadline. The live recheck is
    /// spawned so a slow daemon never stalls the serial sweep; one check per task at a time.
    pub(super) fn spawn_worker_idle_check(self: &Arc<Self>, task: &Task) {
        let Some(card_id) = task.worker_card_id.clone() else {
            return;
        };
        let Some(guard) = InflightGuard::acquire(&self.idle_checks, &task.id) else {
            return;
        };
        let this = Arc::clone(self);
        let task = task.clone();
        tokio::spawn(async move {
            let _guard = guard;
            this.check_worker_idle(task, card_id).await;
        });
    }

    async fn check_worker_idle(self: &Arc<Self>, task: Task, card_id: String) {
        let Some(pool) = self.repo.sqlite_db() else {
            return;
        };
        let Some(end) = PlannerFactory::worker_idle_observation(
            &self.worker_idle,
            pool.read_pool(),
            &task,
            &card_id,
        )
        .await
        else {
            return;
        };
        self.fail_running_worker(task, RunningWorkerFailure::TurnEnded { card_id, end })
            .await;
    }

    /// Fail a `running` worker row and reap its worker once the fail commits.
    pub(super) async fn fail_running_worker(
        self: &Arc<Self>,
        task: Task,
        failure: RunningWorkerFailure,
    ) {
        let track = match self.repo.track_get(&task.track_id).await {
            Ok(Some(track)) => track,
            Ok(None) => {
                tracing::warn!(
                    task_id = %task.id,
                    "scheduler sweep: running worker task's track row is gone; leaving row"
                );
                return;
            }
            Err(e) => {
                tracing::warn!(
                    task_id = %task.id,
                    error = %e,
                    "scheduler sweep: running worker track_get failed"
                );
                return;
            }
        };

        let cleanup_card_id = match failure.guard_card_id() {
            Some(card_id) => Some(card_id.to_string()),
            None => self.worker_card_id_for_task(&task).await,
        };
        #[cfg(feature = "fixtures")]
        if matches!(failure, RunningWorkerFailure::LivenessTimeout(_)) {
            let hook = self
                .liveness_fail_test_hook
                .lock()
                .expect("liveness fail hook lock")
                .take();
            if let Some(hook) = hook {
                hook.judged.notify_one();
                hook.resume.notified().await;
            }
        }

        match self
            .fail_task_liveness_timeout(&task, &track, &failure, cleanup_card_id.as_deref())
            .await
        {
            Ok(true) => {
                self.sweep_timeout_worker_cleanups().await;
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(
                    task_id = %task.id,
                    error = %e,
                    "scheduler sweep: running worker fail tx failed"
                );
            }
        }
    }

    /// Kernel `dispatched/running → failed(<failure detail>)` plus `task.failed` and the cleanup
    /// marker in one tx. `Ok(false)` = another writer moved the row first, or, for a liveness
    /// timeout, the worker's facts in this tx no longer show it expired (progress landed after
    /// the sweep read them).
    pub(super) async fn fail_task_liveness_timeout(
        &self,
        task: &Task,
        track: &Track,
        failure: &RunningWorkerFailure,
        timeout_cleanup_card_id: Option<&str>,
    ) -> Result<bool> {
        let scope = EventScope::Track {
            track: track.id.clone(),
            area: track.area_id.clone(),
        };
        let task_id = task.id.clone();
        let track_id = track.id.clone();
        let detail = failure.detail();
        let reason = failure.reason();
        let details = failure.details();
        let liveness_recheck = matches!(failure, RunningWorkerFailure::LivenessTimeout(_))
            .then(|| (self.worker_liveness, self.liveness_floor()));
        let cleanup_reason = failure.cleanup_reason();
        let guard_card_id = failure.guard_card_id().map(str::to_string);
        let timeout_cleanup_card_id = timeout_cleanup_card_id.map(str::to_string);
        let result = write_with_actor_events_typed::<(), _>(
            self.repo.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    let now = now_ms();
                    let reason = if let Some((liveness, floor_ms)) = liveness_recheck {
                        let facts = task_running_liveness_tx(
                            tx,
                            &task_id,
                            timeout_cleanup_card_id.as_deref(),
                        )
                        .await?;
                        match facts.and_then(|facts| liveness.expiry(&facts, now, floor_ms)) {
                            Some(expiry) => expiry.reason(),
                            None => return Err(race_lost_err()),
                        }
                    } else {
                        reason
                    };
                    let reporter = match guard_card_id.as_deref() {
                        Some(card_id) => TaskReporter::Card {
                            card_id,
                            owns_key: false,
                        },
                        None => TaskReporter::Kernel,
                    };
                    let rows = task_fail_from_worker_tx(
                        tx,
                        &task_id,
                        track_id.as_str(),
                        reporter,
                        detail,
                        now,
                    )
                    .await?;
                    if rows == 0 {
                        return Err(race_lost_err());
                    }
                    let mut released = Vec::new();
                    if let Some(card_id) = timeout_cleanup_card_id.as_deref() {
                        let mark = super::mark_running_timeout_cleanup_tx(
                            tx,
                            card_id,
                            &task_id,
                            now,
                            cleanup_reason,
                        )
                        .await?;
                        if mark.marked == 0 {
                            tracing::warn!(
                                task_id = %task_id,
                                card_id,
                                "scheduler sweep: no live worker session to mark; the failed worker is not reaped"
                            );
                        }
                        released = mark.released;
                    }
                    let mut events = vec![(
                        ActorId::KernelDispatcher,
                        scope.clone(),
                        Event::TaskFailed {
 notification: calm_types::event::PlannerNotification::Notify,
                            idempotency_key: task_id.clone(),
                            reason,
                            details,
                            agent_message: None,
                        },
                    )];
                    events.extend(released);
                    Ok(((), events))
                })
            },
        )
        .await;

        match result {
            Ok(_) => Ok(true),
            Err(e) if is_race_lost(&e) => Ok(false),
            Err(e) => Err(e),
        }
    }

    #[cfg(feature = "fixtures")]
    #[doc(hidden)]
    pub fn set_liveness_fail_test_hook(&self, hook: LivenessFailTestHook) {
        *self
            .liveness_fail_test_hook
            .lock()
            .expect("liveness fail hook lock") = Some(hook);
    }

    /// Reap now the workers whose cleanup marker a committed cancel just wrote; the reconcile
    /// sweep retries any cleanup that fails here.
    pub fn poke_worker_cleanups(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            this.sweep_timeout_worker_cleanups().await;
        });
    }
}
