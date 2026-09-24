//! Ending a `running` Track worker's execution (#1785 S1): the liveness deadline, a codex worker
//! whose turn ended without a task report, and the Planner's cancel all fail or cancel the row
//! and write the same cleanup marker, which the reconcile sweep turns into a worker reap.

use std::sync::Arc;
use std::time::Duration;

use calm_provider::provider::{CodexDaemonProbe, CodexLivenessFacts, ThreadStatusLite};
use dashmap::DashMap;

use super::{InflightGuard, Scheduler, duration_ms_i64, is_race_lost, race_lost_err};
use crate::db::sqlite::{TaskReporter, task_fail_from_worker_tx};
use crate::db::write_with_actor_events_typed;
use crate::error::Result;
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use crate::model::{Task, TaskKind, Track, TrackLifecycle, now_ms};
use crate::track_lifecycle::auto_transition_if_current_in_tx;

/// How long a codex worker's last turn must have been over, with the thread idle and no turn
/// active, before the sweep fails its task as `worker-turn-ended`.
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

/// Wall clock in Unix ms; injected so a fixture can pin `now` against recorded thread facts.
pub type WorkerIdleClock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// The live recheck behind the idle arm. The persisted `last_thread_status` only selects a
/// candidate; this probe's `thread/read` is the evidence.
pub struct WorkerIdleWake {
    probe: Arc<dyn CodexDaemonProbe>,
    grace: Duration,
    probe_timeout: Duration,
    clock: WorkerIdleClock,
}

impl WorkerIdleWake {
    pub fn new(probe: Arc<dyn CodexDaemonProbe>, grace: Duration, probe_timeout: Duration) -> Self {
        Self {
            probe,
            grace,
            probe_timeout,
            clock: Arc::new(now_ms),
        }
    }

    #[cfg(any(test, feature = "fixtures"))]
    #[doc(hidden)]
    pub fn with_clock_for_test(mut self, clock: WorkerIdleClock) -> Self {
        self.clock = clock;
        self
    }
}

/// True when the thread's most recent turn ended at least `grace_ms` ago and nothing runs now.
/// `completed_at` is Unix SECONDS on the wire.
fn turn_ended_past_grace(
    facts: &CodexLivenessFacts,
    active_turn_id: Option<&str>,
    now_ms: i64,
    grace_ms: i64,
) -> bool {
    let Some(Some(completed_at_s)) = facts.last_turn_completed_at else {
        return false;
    };
    facts.status == ThreadStatusLite::Idle
        && active_turn_id.is_none()
        && now_ms.saturating_sub(completed_at_s.saturating_mul(1000)) >= grace_ms
}

/// How a running worker's execution ends; each variant fixes the row detail and the event text.
pub(super) enum RunningWorkerFailure {
    LivenessTimeout,
    /// The live read ran against this card's thread, so the fail CAS is pinned to it.
    TurnEnded {
        card_id: String,
    },
}

impl RunningWorkerFailure {
    const fn detail(&self) -> &'static str {
        match self {
            Self::LivenessTimeout => "worker-timeout",
            Self::TurnEnded { .. } => WORKER_TURN_ENDED,
        }
    }

    const fn reason(&self) -> &'static str {
        match self {
            Self::LivenessTimeout => "worker exceeded the running liveness deadline",
            Self::TurnEnded { .. } => "worker turn ended without a task report",
        }
    }

    const fn auto_message(&self) -> &'static str {
        match self {
            Self::LivenessTimeout => "[auto] worker liveness timeout",
            Self::TurnEnded { .. } => "[auto] worker turn ended",
        }
    }

    const fn cleanup_reason(&self) -> WorkerCleanupReason {
        match self {
            Self::LivenessTimeout => WorkerCleanupReason::LivenessTimeout,
            Self::TurnEnded { .. } => WorkerCleanupReason::TurnEnded,
        }
    }

    fn guard_card_id(&self) -> Option<&str> {
        match self {
            Self::LivenessTimeout => None,
            Self::TurnEnded { card_id } => Some(card_id),
        }
    }
}

/// The candidate's thread: the card's latest codex session, only while its persisted status is
/// `idle`. Each execution gets a fresh card and thread, so this thread is this execution's.
async fn idle_candidate_thread(pool: &sqlx::SqlitePool, card_id: &str) -> Result<Option<String>> {
    let latest: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT thread_id, last_thread_status FROM worker_sessions \
         WHERE card_id = ?1 AND provider = 'codex' \
         ORDER BY created_at_ms DESC, id DESC LIMIT 1",
    )
    .bind(card_id)
    .fetch_optional(pool)
    .await?;
    Ok(match latest {
        Some((Some(thread_id), Some(status))) if status == "idle" => Some(thread_id),
        _ => None,
    })
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
        if task.kind != TaskKind::Codex {
            return;
        }
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
        let Some(pool) = self.repo.sqlite_pool() else {
            return;
        };
        let thread_id = match idle_candidate_thread(&pool, &card_id).await {
            Ok(Some(thread_id)) => thread_id,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "scheduler sweep: idle candidate read failed");
                return;
            }
        };
        let idle = &self.worker_idle;
        let facts = match tokio::time::timeout(
            idle.probe_timeout,
            idle.probe.read_liveness_facts(&thread_id),
        )
        .await
        {
            Ok(Some(facts)) => facts,
            Ok(None) => {
                tracing::debug!(task_id = %task.id, %thread_id, "scheduler sweep: idle recheck unreachable; no action");
                return;
            }
            Err(_) => {
                tracing::warn!(task_id = %task.id, %thread_id, "scheduler sweep: idle recheck timed out; no action");
                return;
            }
        };
        let active_turn_id = idle.probe.active_turn_id_for_thread(&thread_id);
        if !turn_ended_past_grace(
            &facts,
            active_turn_id.as_deref(),
            (idle.clock)(),
            duration_ms_i64(idle.grace),
        ) {
            return;
        }
        self.fail_running_worker(task, RunningWorkerFailure::TurnEnded { card_id })
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
    /// marker in one tx. `Ok(false)` = another writer moved the row first.
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
        let reason = failure.reason().to_string();
        let auto_message = failure.auto_message().to_string();
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
                    if let Some(card_id) = timeout_cleanup_card_id.as_deref() {
                        let marked = super::mark_running_timeout_cleanup_tx(
                            tx,
                            card_id,
                            &task_id,
                            now,
                            cleanup_reason,
                        )
                        .await?;
                        if marked == 0 {
                            tracing::warn!(
                                task_id = %task_id,
                                card_id,
                                "scheduler sweep: no live worker session to mark; the failed worker is not reaped"
                            );
                        }
                    }
                    let mut events = vec![(
                        ActorId::KernelDispatcher,
                        scope.clone(),
                        Event::TaskFailed {
                            idempotency_key: task_id.clone(),
                            reason,
                            details: None,
                            agent_message: None,
                        },
                    )];
                    if let Some(auto_events) = auto_transition_if_current_in_tx(
                        tx,
                        &track_id,
                        TrackLifecycle::Working,
                        TrackLifecycle::Reviewing,
                        &ActorId::KernelDispatcher,
                        Some(auto_message),
                    )
                    .await?
                    {
                        events.extend(
                            auto_events
                                .into_iter()
                                .map(|event| (ActorId::KernelDispatcher, scope.clone(), event)),
                        );
                    }
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

    /// Reap now the workers whose cleanup marker a committed cancel just wrote; the reconcile
    /// sweep retries any cleanup that fails here.
    pub fn poke_worker_cleanups(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            this.sweep_timeout_worker_cleanups().await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(status: ThreadStatusLite, last: Option<Option<i64>>) -> CodexLivenessFacts {
        CodexLivenessFacts {
            loaded: true,
            status,
            last_turn_completed_at: last,
        }
    }

    const GRACE_MS: i64 = 300_000;
    const COMPLETED_S: i64 = 1_790_160_084;

    #[test]
    fn turn_ended_needs_idle_ended_turn_no_active_turn_and_elapsed_grace() {
        let at = |secs: i64| COMPLETED_S * 1000 + secs * 1000;
        let ended = facts(ThreadStatusLite::Idle, Some(Some(COMPLETED_S)));
        assert!(!turn_ended_past_grace(&ended, None, at(299), GRACE_MS));
        assert!(turn_ended_past_grace(&ended, None, at(300), GRACE_MS));
        assert!(!turn_ended_past_grace(
            &ended,
            Some("turn-live"),
            at(301),
            GRACE_MS
        ));
        for other in [
            facts(ThreadStatusLite::Idle, None),
            facts(ThreadStatusLite::Idle, Some(None)),
            facts(
                ThreadStatusLite::Active {
                    waiting_on_user_input: false,
                    waiting_on_approval: false,
                },
                Some(Some(COMPLETED_S)),
            ),
            facts(ThreadStatusLite::NotLoaded, Some(Some(COMPLETED_S))),
            facts(ThreadStatusLite::SystemError, Some(Some(COMPLETED_S))),
        ] {
            assert!(
                !turn_ended_past_grace(&other, None, at(301), GRACE_MS),
                "{other:?}"
            );
        }
    }
}
