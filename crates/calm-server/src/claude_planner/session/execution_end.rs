//! The existing session retains one actual driver and its completed result.
use super::*;
use crate::harness::backend::{ExecutionEndScope, ExecutionEndTarget};

pub(super) use crate::harness::backend::ExecutionInput as DriverInput;

enum Completion {
    Running(tokio::task::JoinHandle<bool>),
    Finished(std::result::Result<bool, ()>),
}

pub(super) struct DriverSlot {
    thread: String,
    turn: String,
    input: DriverInput,
    completion: Completion,
}

impl DriverSlot {
    pub(super) fn new(
        thread: String,
        turn: String,
        input: DriverInput,
        driver: tokio::task::JoinHandle<bool>,
    ) -> Self {
        Self {
            thread,
            turn,
            input,
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
    #[cfg(all(test, feature = "fixtures"))]
    pub(crate) async fn hold_execution_driver_for_test(&self) -> impl Drop + '_ {
        self.shared.driver.lock().await
    }

    #[cfg(all(test, feature = "fixtures"))]
    pub(crate) async fn hold_execution_issue_for_test(&self) -> impl Drop + '_ {
        self.shared.issue.lock().await
    }

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

    pub(crate) async fn dispatch_input(
        &self,
        logical_turn_id: &calm_types::conversation::LogicalTurnId,
        thread: &str,
        items: Vec<InputItem>,
        selection: &TurnModelSelection,
        mode: PlannerPermissionMode,
        proof: &SubmissionDispatchProof,
    ) -> Result<String, SpawnFailure> {
        self.start_spawn(
            thread,
            SpawnInput::CanonicalTurn {
                items,
                proof,
                logical_turn_id,
            },
            selection,
            mode,
        )
        .await
    }

    pub(crate) async fn qualify_execution_end(&self, target: &ExecutionEndTarget) -> Result<bool> {
        let shared = &self.shared;
        let params = &shared.params;
        let ExecutionEndScope::LocalExecution {
            thread_id,
            native_session_id,
            local_turn_id,
        } = &target.scope
        else {
            return Ok(false);
        };
        if !target.matches_owner(&params.card_id, &params.track_id, &params.worker_session_id)
            || thread_id.is_empty()
            || local_turn_id.is_empty()
            || native_session_id != thread_id
        {
            return Ok(false);
        }
        // Even acquiring the SAME slot is nonblocking: shutdown may hold it while
        // waiting for a live permission-held driver that needs this run loop.
        // Observe only between actual native issues, never while a replacement
        // is admitted but has not yet installed its driver. This is a short
        // observation gate, not a certificate held through canonical SQL.
        let Ok(_issue) = shared.issue.try_lock() else {
            return Ok(false);
        };
        let Ok(mut slot) = shared.driver.try_lock() else {
            return Ok(false);
        };
        let Some(driver) = slot.as_mut() else {
            return Ok(false);
        };
        if driver.thread != *thread_id
            || driver.turn != *local_turn_id
            || !driver.input.matches(target)
            || matches!(&driver.completion, Completion::Running(handle) if !handle.is_finished())
        {
            return Ok(false);
        }
        Ok(driver.finish().await.unwrap_or(false))
    }
}
