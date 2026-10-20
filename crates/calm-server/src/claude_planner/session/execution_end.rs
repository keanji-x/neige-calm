//! The existing session retains one actual driver and its completed result.
use super::*;

enum Completion {
    Running(tokio::task::JoinHandle<bool>),
    Finished(std::result::Result<bool, ()>),
}

pub(super) struct DriverSlot {
    completion: Completion,
    thread: Uuid,
    turn: Uuid,
    execution: Option<crate::harness::backend::ExecutionInput>,
}

impl DriverSlot {
    pub(super) fn new(
        driver: tokio::task::JoinHandle<bool>,
        thread: Uuid,
        turn: Uuid,
        execution: Option<crate::harness::backend::ExecutionInput>,
    ) -> Self {
        Self {
            completion: Completion::Running(driver),
            thread,
            turn,
            execution,
        }
    }

    fn matches(&self, target: &crate::harness::backend::ExecutionEndTarget) -> bool {
        use crate::harness::backend::ExecutionEndScope;
        match &target.scope {
            ExecutionEndScope::LocalProcess {
                thread_id,
                local_turn_id,
            } => {
                self.thread == *thread_id
                    && self.turn == *local_turn_id
                    && self
                        .execution
                        .as_ref()
                        .is_some_and(|input| input.matches(target))
            }
            ExecutionEndScope::NativeTurn { .. } => false,
        }
    }

    async fn qualified(&mut self) -> Result<bool> {
        match &self.completion {
            Completion::Running(driver) if !driver.is_finished() => return Ok(false),
            Completion::Running(_) | Completion::Finished(_) => {}
        }
        self.finish().await
    }

    async fn finish(&mut self) -> Result<bool> {
        let result = match &mut self.completion {
            Completion::Running(driver) => {
                // Borrowed await retains task ownership if the caller is cancelled.
                let result = driver.await.map_err(|_| ());
                self.completion = Completion::Finished(result);
                result
            }
            Completion::Finished(result) => *result,
        };
        result.map_err(|()| CalmError::Conflict("Claude driver did not finish normally".into()))
    }
}

pub(super) async fn join_driver(shared: &Shared) -> Result<()> {
    if let Some(driver) = shared.driver.lock().await.as_mut() {
        driver.finish().await?;
    }
    Ok(())
}

impl ClaudePlannerSession {
    pub(crate) async fn owns_local_execution(
        &self,
        target: &crate::harness::backend::ExecutionEndTarget,
    ) -> bool {
        let params = &self.shared.params;
        target.matches_owner(&params.card_id, &params.track_id, &params.worker_session_id)
            && self
                .shared
                .driver
                .lock()
                .await
                .as_ref()
                .is_some_and(|driver| driver.matches(target))
    }

    pub(crate) async fn qualify_execution_end(
        &self,
        target: &crate::harness::backend::ExecutionEndTarget,
    ) -> Result<bool> {
        let params = &self.shared.params;
        if !target.matches_owner(&params.card_id, &params.track_id, &params.worker_session_id) {
            return Ok(false);
        }
        let mut driver = self.shared.driver.lock().await;
        match driver.as_mut() {
            Some(driver) if driver.matches(target) => driver.qualified().await,
            Some(_) | None => Ok(false),
        }
    }

    pub(crate) async fn interrupt_execution(
        &self,
        target: &crate::harness::backend::ExecutionEndTarget,
        admission: &crate::harness::native_admission::NativePromptAdmission<'_>,
    ) -> Result<()> {
        if !self.owns_local_execution(target).await {
            return Err(CalmError::Conflict(
                "control names another Claude execution".into(),
            ));
        }
        let (thread, turn) = target.scope.thread_and_turn();
        let slot = self
            .shared
            .state()
            .active
            .as_ref()
            .filter(|active| active.thread_id == thread && active.turn_id == turn)
            .map(|active| Arc::clone(&active.slot))
            .ok_or_else(|| CalmError::Conflict("Claude execution already ended".into()))?;
        super::interrupt(&slot, admission.gate())
            .await
            .map_err(|(error, _evidence)| admission.take_refusal().unwrap_or(error))
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
}
