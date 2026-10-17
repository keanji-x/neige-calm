//! Retain the existing receipt writer's join result, without a second journal.
use super::*;
use crate::db::sqlite::AcpSubmissionState;
use crate::harness::backend::{ExecutionEndScope, ExecutionEndTarget};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct DriverIdentity {
    client: String,
    thread: String,
    session: String,
    turn: String,
}

impl DriverIdentity {
    pub(super) fn from_finished_receipt(receipt: &AcpSubmission) -> Self {
        Self {
            client: receipt.client_id.clone(),
            thread: receipt.thread_id.clone(),
            session: receipt.native_session_id.clone(),
            turn: receipt.turn_id.clone(),
        }
    }
}

enum Completion {
    Running(tokio::task::JoinHandle<DriverIdentity>),
    Finished(std::result::Result<DriverIdentity, ()>),
}

pub(super) struct DriverSlot {
    input: ExecutionInput,
    completion: Completion,
}

impl DriverSlot {
    pub(super) fn new(
        driver: tokio::task::JoinHandle<DriverIdentity>,
        input: ExecutionInput,
    ) -> Self {
        Self {
            input,
            completion: Completion::Running(driver),
        }
    }

    async fn finish(&mut self) -> Result<&DriverIdentity> {
        if let Completion::Running(driver) = &mut self.completion {
            // Never take a live handle out of its owning slot across an await.
            let result = driver.await.map_err(|_| ());
            self.completion = Completion::Finished(result);
        }
        match &self.completion {
            Completion::Finished(Ok(identity)) => Ok(identity),
            _ => Err(CalmError::Conflict(
                "ACP receipt writer did not finish normally".into(),
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

impl AcpPlannerSession {
    #[cfg(feature = "fixtures")]
    pub(crate) async fn hold_execution_driver_for_test(&self) -> impl Drop + '_ {
        self.shared.driver.lock().await
    }

    #[cfg(feature = "fixtures")]
    pub(crate) async fn hold_execution_issue_for_test(&self) -> impl Drop + '_ {
        self.shared.issue.lock().await
    }

    async fn finished_execution(&self, target: &ExecutionEndTarget) -> Option<DriverIdentity> {
        let ExecutionEndScope::LocalExecution {
            thread_id,
            native_session_id,
            local_turn_id,
        } = &target.scope
        else {
            return None;
        };
        let _issue = self.shared.issue.try_lock().ok()?;
        let mut slot = self.shared.driver.try_lock().ok()?;
        let driver = slot.as_mut()?;
        if !driver.input.matches(target)
            || matches!(&driver.completion, Completion::Running(handle) if !handle.is_finished())
        {
            return None;
        }
        let actual = driver.finish().await.ok()?;
        (actual.client == target.proof.attempt().submission_id.as_str()
            && actual.thread == *thread_id
            && actual.session == *native_session_id
            && actual.turn == *local_turn_id)
            .then(|| actual.clone())
    }

    pub(crate) async fn qualify_execution_end(&self, target: &ExecutionEndTarget) -> Result<bool> {
        let params = &self.shared.params;
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
            || native_session_id.is_empty()
            || local_turn_id.is_empty()
        {
            return Ok(false);
        }
        // Refuse live/issuing drivers before any pool wait: the same run loop
        // may need to answer the permission that lets this driver finish.
        let Some(actual) = self.finished_execution(target).await else {
            return Ok(false);
        };
        let pool = params
            .repo
            .sqlite_pool()
            .ok_or_else(|| CalmError::Internal("ACP journal pool missing".into()))?;
        let client = target.proof.attempt().submission_id.as_str();
        let Some(receipt) = acp_submission_get(&pool, &params.worker_session_id, client).await?
        else {
            return Ok(false);
        };
        // Neither native gate is held over the journal read. Recheck the same
        // actual slot after that await before creating the monotone observation.
        if self.finished_execution(target).await.as_ref() != Some(&actual) {
            return Ok(false);
        }
        Ok(matches!(receipt.state, AcpSubmissionState::Completed)
            && receipt.thread_id == *thread_id
            && receipt.turn_id == *local_turn_id
            && receipt.native_session_id == *native_session_id
            && serde_json::from_str::<SubmissionDispatchProof>(&receipt.input_json)
                .is_ok_and(|proof| proof == target.proof)
            && receipt
                .outcome_json
                .as_deref()
                .and_then(|value| serde_json::from_str::<Value>(value).ok())
                .is_some_and(|outcome| outcome["id"].as_str() == Some(local_turn_id)))
    }
}
