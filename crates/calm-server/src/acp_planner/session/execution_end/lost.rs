//! Accept loss once, before projection; retirement cannot subsequently refuse it.
use super::*;
use crate::harness::backend::MaintenanceTarget;

struct AcceptedLoss {
    retained: Option<Arc<tokio::sync::Mutex<DriverSlot>>>,
    driver: Option<tokio::sync::OwnedMutexGuard<DriverSlot>>,
    target: Option<MaintenanceTarget>,
}

impl AcpPlannerSession {
    fn accept_lost_execution(
        &self,
        id: &workspace_runtime::execution::ExecutionId,
        turn: &str,
        maintenance: bool,
    ) -> Result<AcceptedLoss> {
        let retained = slot(&self.shared);
        let driver = retained
            .as_ref()
            .map(|slot| {
                slot.clone()
                    .try_lock_owned()
                    .map_err(|_| CalmError::Conflict("ACP driver join is pending".into()))
            })
            .transpose()?;
        // Identity precedes slot kind. Targetless cleanup belongs to this
        // execution; an unidentified slot is not restart absence.
        if let Some(driver) = driver.as_deref()
            && ((maintenance && driver.execution_id.as_ref() != Some(id))
                || driver
                    .execution_id
                    .as_ref()
                    .is_some_and(|expected| expected != id))
        {
            return Err(CalmError::Conflict(
                "ACP completion identity changed".into(),
            ));
        }
        let target = match driver.as_deref().map(|driver| &driver.input) {
            Some(DriverInput::Maintenance {
                target,
                turn: expected,
                ..
            }) => {
                if maintenance {
                    if expected != turn {
                        return Err(CalmError::Conflict(
                            "ACP completion identity changed".into(),
                        ));
                    }
                    Some(target.clone())
                } else {
                    None
                }
            }
            None
            | Some(DriverInput::Cleanup)
            | Some(DriverInput::Historical)
            | Some(DriverInput::Canonical(_)) => None,
        };
        if maintenance
            && self
                .shared
                .state
                .lock()
                .expect("ACP state")
                .active
                .as_ref()
                .is_some_and(|active| active.execution_id != *id || active.turn != turn)
        {
            return Err(CalmError::Conflict(
                "ACP completion identity changed".into(),
            ));
        }
        if driver
            .as_ref()
            .is_some_and(|driver| !driver.completion.is_finished())
        {
            return Err(CalmError::Conflict("ACP driver remains live".into()));
        }
        Ok(AcceptedLoss {
            retained,
            driver,
            target,
        })
    }

    pub(crate) async fn complete_lost_execution_with_projection<F, Fut>(
        &self,
        id: &workspace_runtime::execution::ExecutionId,
        turn: &str,
        purpose: &crate::provider_execution::Purpose,
        outcome: Value,
        project: F,
    ) -> Result<()>
    where
        F: FnOnce(Option<MaintenanceTarget>) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let maintenance = matches!(
            purpose,
            crate::provider_execution::Purpose::Compaction { .. }
        );
        let mut accepted = self.accept_lost_execution(id, turn, maintenance)?;
        // Keep the accepted slot locked across projection. No identity/liveness
        // decision may occur after this durable write.
        project(accepted.target.take()).await?;
        let shared = &self.shared;
        if let Some(driver) = accepted.driver.as_mut() {
            // A failed join is evidence of uncertainty, never normal cleanup.
            // Retire it for both Turn and Compaction so later joins can proceed.
            if driver.finish().await.is_err() {
                let mut current = shared.driver.lock().expect("ACP driver");
                if current
                    .as_ref()
                    .zip(accepted.retained.as_ref())
                    .is_some_and(|(current, retained)| Arc::ptr_eq(current, retained))
                {
                    *current = None;
                }
            }
        }
        drop(accepted);
        if maintenance {
            let mut state = shared.state.lock().expect("ACP state");
            if state
                .active
                .as_ref()
                .is_some_and(|active| active.execution_id == *id && active.turn == turn)
            {
                state.active = None;
            }
        } else {
            let thread = shared
                .state
                .lock()
                .expect("ACP state")
                .active
                .as_ref()
                .filter(|active| active.execution_id == *id && active.turn == turn)
                .map(|active| active.thread.clone());
            if let Some(thread) = thread {
                shared.finish_execution(id.as_str(), turn, &thread, Vec::new(), outcome);
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) async fn lost_maintenance_target(
        &self,
        id: &workspace_runtime::execution::ExecutionId,
        turn: &str,
    ) -> Result<Option<MaintenanceTarget>> {
        Ok(self.accept_lost_execution(id, turn, true)?.target)
    }

    #[cfg(test)]
    pub(crate) async fn complete_lost_execution(
        &self,
        id: &workspace_runtime::execution::ExecutionId,
        turn: &str,
        purpose: &crate::provider_execution::Purpose,
        outcome: Value,
    ) -> Result<()> {
        self.complete_lost_execution_with_projection(id, turn, purpose, outcome, |_| async {
            Ok(())
        })
        .await
    }
}
