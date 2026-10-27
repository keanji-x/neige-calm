//! Codex worker native liveness, strict timeout and best-effort settled cleanup integration.
use super::PlannerFactory;
use crate::error::{CalmError, Result};
use crate::model::{Task, TaskKind, now_ms};
use provider::control::NativeInterruptOutcome;
use provider::worker::{
    CodexDaemonProbe, CodexDaemonRoute, CodexLivenessFacts, FixedCodexDaemon, ThreadStatusLite,
    TurnStatusLite,
};
use std::sync::Arc;
use std::time::Duration;

/// Wall clock in Unix ms; injected so a fixture can pin `now` against recorded thread facts.
pub type WorkerIdleClock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// The live recheck behind the idle arm. The persisted `last_thread_status` only selects a
/// candidate; this probe's `thread/read` is the evidence.
pub struct WorkerIdleWake {
    route: Arc<dyn CodexDaemonRoute>,
    grace: Duration,
    probe_timeout: Duration,
    clock: WorkerIdleClock,
}

impl WorkerIdleWake {
    pub fn new(probe: Arc<dyn CodexDaemonProbe>, grace: Duration, probe_timeout: Duration) -> Self {
        Self::with_route(Arc::new(FixedCodexDaemon(probe)), grace, probe_timeout)
    }
    pub fn with_route(
        route: Arc<dyn CodexDaemonRoute>,
        grace: Duration,
        probe_timeout: Duration,
    ) -> Self {
        Self {
            route,
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TurnEnd {
    Ended,
    Failed {
        cause: Option<provider::worker::WorkerFailureCause>,
    },
}

/// How the thread's most recent turn ended, when it ended at least `grace_ms` ago, the thread
/// rests (`Idle` or `SystemError`) and nothing runs now. `completed_at` is Unix SECONDS.
fn turn_ended_past_grace(
    facts: &CodexLivenessFacts,
    active_turn_id: Option<&str>,
    now_ms: i64,
    grace_ms: i64,
) -> Option<TurnEnd> {
    let turn = facts.last_turn.as_ref()?;
    let completed_at_s = turn.completed_at?;
    let resting = matches!(
        facts.status,
        ThreadStatusLite::Idle | ThreadStatusLite::SystemError
    );
    let end = match turn.status {
        TurnStatusLite::Failed => TurnEnd::Failed {
            cause: turn.cause.clone(),
        },
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
async fn idle_candidate_thread(
    pool: &sqlx::SqlitePool,
    card_id: &str,
) -> Result<Option<(String, String)>> {
    let latest: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, thread_id, last_thread_status FROM worker_sessions \
         WHERE card_id = ?1 AND provider = 'codex' \
         ORDER BY created_at_ms DESC, id DESC LIMIT 1",
    )
    .bind(card_id)
    .fetch_optional(pool)
    .await?;
    Ok(match latest {
        Some((id, Some(thread_id), Some(status)))
            if matches!(status.as_str(), "idle" | "systemError") =>
        {
            Some((id, thread_id))
        }
        _ => None,
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
        let (session_id, thread_id) = match idle_candidate_thread(pool, card_id).await {
            Ok(Some(thread_id)) => thread_id,
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "scheduler sweep: idle candidate read failed");
                return None;
            }
        };
        let probe = self.route.for_session(&session_id).await?;
        let facts = match tokio::time::timeout(
            self.probe_timeout,
            probe.read_liveness_facts(&thread_id),
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
        let active_turn_id = probe.active_turn_id_for_thread(&thread_id);
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

impl PlannerFactory<'_> {
    /// Strict timeout cleanup: propagate an unconfirmed stop so the product retains its
    /// cleanup marker and lease for retry. Settled-worker cleanup above is warning-only.
    pub(crate) async fn stop_timed_out_worker(
        runtime: &crate::session_projection_repo::WorkerSessionProjection,
        shared: Option<&crate::shared_codex_appserver::SharedCodexAppServer>,
    ) -> Result<()> {
        use crate::operation::TxOutput;
        use crate::session_projection_repo::AgentProvider;
        match runtime.agent_provider {
            Some(AgentProvider::Codex) => {}
            Some(AgentProvider::Claude | AgentProvider::OpenCode | AgentProvider::Grok) | None => {
                return Ok(());
            }
        }
        let Some(thread_id) = TxOutput::non_empty_string(runtime.thread_id.as_deref()) else {
            return Ok(());
        };
        let persisted_turn = TxOutput::non_empty_string(runtime.active_turn_id.as_deref());
        let Some(shared_codex_appserver) = shared else {
            tracing::warn!(
                runtime_id = %runtime.id,
                card_id = %runtime.card_id,
                thread_id = %thread_id,
                "timed-out codex worker has no shared appserver handle; cannot interrupt active turn"
            );
            return Ok(());
        };
        let cached_turn = shared_codex_appserver.active_turn_id_for_thread(&thread_id);
        // A failed interrupt fails the cleanup (#1830 S2 D7): the marker stays, the lease stays
        // held, and the next tick retries — unless the worker is proven stopped anyway.
        let interrupted = shared_codex_appserver
            .interrupt_active_outcome(&thread_id)
            .await;
        codex_interrupt_settled(shared_codex_appserver, &thread_id, interrupted).await?;
        if cached_turn.is_none()
            && let Some(persisted_turn) = persisted_turn.as_deref()
        {
            let interrupted = shared_codex_appserver
                .interrupt_outcome(&thread_id, persisted_turn)
                .await;
            codex_interrupt_settled(shared_codex_appserver, &thread_id, interrupted).await?;
        }
        Ok(())
    }
}

/// Whether a Codex interrupt of a timed-out or canceled worker leaves it stopped (#1830 S2 D7).
/// A daemon that is not Running counts as stopped ("daemon down — brain gone, no turn can run").
/// A JSON-RPC refusal is told apart from a transport failure, and the daemon's own facts decide
/// it: a thread that is `NotLoaded` (as after a daemon restart) or whose last turn ended counts as
/// stopped. Only a transport failure while the daemon is Running, or a thread still active, is an
/// `Err`, so the cleanup retries until the daemon answers or goes down.
async fn codex_interrupt_settled(
    shared: &crate::shared_codex_appserver::SharedCodexAppServer,
    thread_id: &str,
    interrupted: NativeInterruptOutcome<CalmError>,
) -> Result<()> {
    use provider::worker::{CodexDaemonProbe, ThreadStatusLite, TurnStatusLite};
    let error = match interrupted {
        NativeInterruptOutcome::NativeAck | NativeInterruptOutcome::NoTarget => return Ok(()),
        NativeInterruptOutcome::Unconfirmed { error } => error,
    };
    if !shared.is_running_per_readiness() {
        return Ok(());
    }
    if matches!(error, CalmError::CodexRefused(_))
        && let Some(facts) = CodexDaemonProbe::read_liveness_facts(shared, thread_id).await
        && (facts.status == ThreadStatusLite::NotLoaded
            || facts.last_turn.is_some_and(|turn| {
                matches!(
                    turn.status,
                    TurnStatusLite::Completed
                        | TurnStatusLite::Interrupted
                        | TurnStatusLite::Failed
                )
            }))
    {
        return Ok(());
    }
    tracing::warn!(thread_id, %error, "codex worker interrupt failed; its cleanup retries");
    Err(error)
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
            cause: None,
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
            (TurnStatusLite::Failed, TurnEnd::Failed { cause: None }),
            (TurnStatusLite::InProgress, TurnEnd::Ended),
            (TurnStatusLite::Unknown, TurnEnd::Ended),
        ];
        for status in resting {
            for (turn_status, end) in &ended {
                let ended = facts(status, turn(Some(COMPLETED_S), *turn_status));
                assert_eq!(turn_ended_past_grace(&ended, None, at(299), GRACE_MS), None);
                assert_eq!(
                    turn_ended_past_grace(&ended, None, at(300), GRACE_MS),
                    Some(end.clone()),
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
