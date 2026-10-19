//! The existing session retains one actual driver and its completed result.
use super::*;

enum Completion {
    Running(tokio::task::JoinHandle<bool>),
    Finished(std::result::Result<bool, ()>),
}

pub(super) struct DriverSlot {
    completion: Completion,
}

impl DriverSlot {
    pub(super) fn new(driver: tokio::task::JoinHandle<bool>) -> Self {
        Self {
            completion: Completion::Running(driver),
        }
    }

    async fn finish(&mut self) -> Result<bool> {
        if let Completion::Running(driver) = &mut self.completion {
            // Borrowed await retains task ownership if the caller is cancelled.
            let qualified = driver.await.map_err(|_| ());
            self.completion = Completion::Finished(qualified);
        }
        match self.completion {
            Completion::Finished(Ok(qualified)) => Ok(qualified),
            _ => Err(CalmError::Conflict(
                "Claude driver did not finish normally".into(),
            )),
        }
    }
}

pub(super) async fn join_driver(shared: &Shared) -> Result<()> {
    if let Some(driver) = shared.driver.lock().await.as_mut() {
        driver.finish().await?;
    }
    Ok(())
}

impl ClaudePlannerSession {
    /// Harness shutdown: refuse new turns, end the live one as `Interrupted`, and stop the marker
    /// whether or not a turn runs. The `stop` result is returned for strict callers. A session that
    /// was never installed (an install-race loser) minted and spawned nothing, so it signals nothing:
    /// its id may belong to the winner's live turn.
    pub async fn shutdown(&self) -> Result<()> {
        let shared = &self.shared;
        shared.state().shutting_down = true;
        let _issue = shared.issue.lock().await;
        if shared.installed.get().is_none() {
            return Ok(());
        }
        let slot = shared
            .state()
            .active
            .as_ref()
            .map(|active| Arc::clone(&active.slot));
        if let Some(slot) = &slot {
            slot.record(TerminalCause::Interrupted);
            slot.arm_stop(tokio::time::Instant::now());
        }
        let params = &shared.params;
        let stopped = stop(&params.host.instance, &params.worker_session_id).await;
        if let Some(slot) = slot {
            let mut settled = slot.settled.subscribe();
            if tokio::time::timeout(SHUTDOWN_SETTLE_WAIT, settled.wait_for(|done| *done))
                .await
                .is_err()
            {
                tracing::warn!(
                    worker_session_id = %params.worker_session_id,
                    "claude planner: the running turn did not settle during shutdown"
                );
            }
        }
        let joined = tokio::time::timeout(SHUTDOWN_SETTLE_WAIT, join_driver(shared)).await;
        stopped?;
        joined
            .map_err(|_| CalmError::Conflict("Claude driver cleanup did not finish".into()))??;
        Ok(())
    }
}
