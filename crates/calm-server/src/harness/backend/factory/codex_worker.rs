//! Codex worker native liveness and best-effort settled cleanup integration.
use super::PlannerFactory;
use crate::error::Result;
use crate::model::{Task, TaskKind, now_ms};
use provider::worker::{CodexDaemonProbe, CodexLivenessFacts, ThreadStatusLite, TurnStatusLite};
use std::sync::Arc;
use std::time::Duration;

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

/// How the worker's last turn itself ended, from the turn's own `status`: the thread status
/// only says the thread is resting, and it keeps `systemError` after a turn that completed and
/// comes back `idle` on reload after a failed one (#1813).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TurnEnd {
    Ended,
    Failed,
}

/// How the thread's most recent turn ended, when it ended at least `grace_ms` ago, the thread
/// rests (`Idle` or `SystemError`) and nothing runs now. `completed_at` is Unix SECONDS.
fn turn_ended_past_grace(
    facts: &CodexLivenessFacts,
    active_turn_id: Option<&str>,
    now_ms: i64,
    grace_ms: i64,
) -> Option<TurnEnd> {
    let turn = facts.last_turn?;
    let completed_at_s = turn.completed_at?;
    let resting = matches!(
        facts.status,
        ThreadStatusLite::Idle | ThreadStatusLite::SystemError
    );
    let end = match turn.status {
        TurnStatusLite::Failed => TurnEnd::Failed,
        TurnStatusLite::Completed
        | TurnStatusLite::Interrupted
        | TurnStatusLite::InProgress
        | TurnStatusLite::Unknown => TurnEnd::Ended,
    };
    (resting
        && active_turn_id.is_none()
        && now_ms.saturating_sub(completed_at_s.saturating_mul(1000)) >= grace_ms)
        .then_some(end)
}

/// The candidate's thread: the card's latest codex session, only while its persisted status is
/// `idle` or `systemError` (the liveness feeder's spelling for a thread whose turn failed). Each
/// execution gets a fresh card and thread, so this thread is this execution's.
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
        Some((Some(thread_id), Some(status)))
            if matches!(status.as_str(), "idle" | "systemError") =>
        {
            Some(thread_id)
        }
        Some((None, _)) | Some((Some(_), None)) | Some((Some(_), Some(_))) | None => None,
    })
}

impl PlannerFactory<'_> {
    /// Select the native idle integration using the original task kind, before any probe.
    pub(crate) async fn worker_idle_observation(
        idle: &WorkerIdleWake,
        pool: &sqlx::SqlitePool,
        task: &Task,
        card_id: &str,
    ) -> Option<TurnEnd> {
        match task.kind {
            TaskKind::Codex => idle.observe(pool, task, card_id).await,
            TaskKind::Claude | TaskKind::Terminal | TaskKind::Compact => None,
        }
    }
}

impl WorkerIdleWake {
    async fn observe(
        &self,
        pool: &sqlx::SqlitePool,
        task: &Task,
        card_id: &str,
    ) -> Option<TurnEnd> {
        let thread_id = match idle_candidate_thread(pool, card_id).await {
            Ok(Some(thread_id)) => thread_id,
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "scheduler sweep: idle candidate read failed");
                return None;
            }
        };
        let facts = match tokio::time::timeout(
            self.probe_timeout,
            self.probe.read_liveness_facts(&thread_id),
        )
        .await
        {
            Ok(Some(facts)) => facts,
            Ok(None) => {
                tracing::debug!(task_id = %task.id, %thread_id, "scheduler sweep: idle recheck unreachable; no action");
                return None;
            }
            Err(_) => {
                tracing::warn!(task_id = %task.id, %thread_id, "scheduler sweep: idle recheck timed out; no action");
                return None;
            }
        };
        let active_turn_id = self.probe.active_turn_id_for_thread(&thread_id);
        turn_ended_past_grace(
            &facts,
            active_turn_id.as_deref(),
            (self.clock)(),
            i64::try_from(self.grace.as_millis()).unwrap_or(i64::MAX),
        )
    }
}

impl PlannerFactory<'_> {
    /// The guarded settlement write has committed. Native cleanup is warning-only;
    /// terminal reaping must proceed even when interruption is unconfirmed.
    pub(crate) async fn end_settled_worker_resources(
        &self,
        session: &crate::terminal_sweeper::SettledWorkerSession,
    ) {
        use calm_truth::worker::{WorkerProviderDecodeError, WorkerProviderKind};
        use provider::control::NativeInterruptOutcome;
        let provider = match WorkerProviderKind::try_from(session.provider.clone()) {
            Ok(provider) => provider,
            Err(error @ WorkerProviderDecodeError::UnknownProvider(_)) => {
                tracing::warn!(worker_session_id = %session.id, %error, "settled-worker provider does not decode");
                return;
            }
        };
        match provider {
            WorkerProviderKind::Codex => {}
            WorkerProviderKind::Claude
            | WorkerProviderKind::OpenCode
            | WorkerProviderKind::Grok
            | WorkerProviderKind::Terminal => return,
        }
        let Some(thread_id) = session.thread_id.as_deref() else {
            return;
        };
        let Some(daemon) = self.daemon.upgrade() else {
            return;
        };
        match daemon.interrupt_active_outcome(thread_id).await {
            NativeInterruptOutcome::NativeAck | NativeInterruptOutcome::NoTarget => {}
            NativeInterruptOutcome::Unconfirmed { error: e } => {
                tracing::warn!(
                    target: "shared_codex_daemon::orphan_turn",
                    worker_session_id = %session.id,
                    card_id = %session.card_id,
                    thread_id = %thread_id,
                    error = %e,
                    "failed to interrupt active shared codex turn while ending a settled-worker session"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provider::worker::LastTurnFacts;

    fn facts(status: ThreadStatusLite, last_turn: Option<LastTurnFacts>) -> CodexLivenessFacts {
        CodexLivenessFacts {
            loaded: true,
            status,
            last_turn,
        }
    }

    fn turn(completed_at: Option<i64>, status: TurnStatusLite) -> Option<LastTurnFacts> {
        Some(LastTurnFacts {
            completed_at,
            status,
        })
    }

    const GRACE_MS: i64 = 300_000;
    const COMPLETED_S: i64 = 1_790_160_084;

    #[test]
    fn turn_ended_needs_a_resting_thread_ended_turn_no_active_turn_and_elapsed_grace() {
        let at = |secs: i64| COMPLETED_S * 1000 + secs * 1000;
        let resting = [ThreadStatusLite::Idle, ThreadStatusLite::SystemError];
        let ended = [
            (TurnStatusLite::Completed, TurnEnd::Ended),
            (TurnStatusLite::Interrupted, TurnEnd::Ended),
            (TurnStatusLite::Failed, TurnEnd::Failed),
            (TurnStatusLite::InProgress, TurnEnd::Ended),
            (TurnStatusLite::Unknown, TurnEnd::Ended),
        ];
        for status in resting {
            for (turn_status, end) in ended {
                let ended = facts(status, turn(Some(COMPLETED_S), turn_status));
                assert_eq!(turn_ended_past_grace(&ended, None, at(299), GRACE_MS), None);
                assert_eq!(
                    turn_ended_past_grace(&ended, None, at(300), GRACE_MS),
                    Some(end),
                    "{ended:?}"
                );
                assert_eq!(
                    turn_ended_past_grace(&ended, Some("turn-live"), at(301), GRACE_MS),
                    None
                );
            }
        }
        let active = ThreadStatusLite::Active {
            waiting_on_user_input: false,
            waiting_on_approval: false,
        };
        let mut others = vec![
            facts(active, turn(Some(COMPLETED_S), TurnStatusLite::Completed)),
            facts(
                ThreadStatusLite::NotLoaded,
                turn(Some(COMPLETED_S), TurnStatusLite::Completed),
            ),
        ];
        for status in resting {
            others.push(facts(status, None));
            others.push(facts(status, turn(None, TurnStatusLite::InProgress)));
        }
        for other in others {
            assert_eq!(
                turn_ended_past_grace(&other, None, at(301), GRACE_MS),
                None,
                "{other:?}"
            );
        }
    }
}
