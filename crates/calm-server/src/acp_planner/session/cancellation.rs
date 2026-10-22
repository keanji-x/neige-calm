//! A cancelled issuer hands resources to the same retained driver slot.
use super::execution_end::{Settlement, install};
use super::*;

pub(super) struct IssueGuard {
    shared: Arc<Shared>,
    pub(super) process: Option<Process>,
    pub(super) receipt: Option<AcpSubmission>,
    armed: bool,
}
impl IssueGuard {
    pub(super) fn new(shared: &Arc<Shared>) -> Self {
        Self {
            shared: shared.clone(),
            process: None,
            receipt: None,
            armed: true,
        }
    }
    pub(super) fn handoff(&mut self) -> (Process, AcpSubmission) {
        let process = self.process.take().expect("prepared ACP process");
        let receipt = self.receipt.take().expect("prepared ACP receipt");
        self.armed = false;
        (process, receipt)
    }
    /// An ordinary issuer receives the original cleanup error only after the retained task
    /// joins. Cancelling this await drops the receiver, never the cleanup owner.
    pub(super) async fn finish(mut self) -> Result<()> {
        let (send, receive) = tokio::sync::oneshot::channel();
        self.retain_cleanup(Some(send));
        join_driver(&self.shared).await?;
        receive.await.map_err(|_| {
            CalmError::Conflict("ACP issuer cleanup did not return its result".into())
        })?
    }
    fn retain_cleanup(&mut self, result: Option<tokio::sync::oneshot::Sender<Result<()>>>) {
        self.armed = false;
        let shared = self.shared.clone();
        let process = self.process.take();
        let receipt = self.receipt.take();
        let associated = receipt.clone();
        let driver = tokio::spawn(async move {
            let params = &shared.params;
            let revoked = revoke(params).await;
            let prepared = process.is_some();
            let stopped = match process {
                Some(process) => process.stop(&params.host, &params.worker_session_id).await,
                None => {
                    crate::planner_process::stop(&params.host.instance, &params.worker_session_id)
                        .await
                }
            };
            let confirmed = prepared && revoked.is_ok() && stopped.is_ok();
            if let Some(receipt) = receipt
                && let Some(pool) = params.repo.sqlite_db()
            {
                let _ = acp_submission_finish(
                    &pool,
                    &receipt.worker_session_id,
                    &receipt.client_id,
                    None,
                )
                .await;
            }
            if let Err(error) = &stopped {
                tracing::error!(%error, "ACP issuer process cleanup unconfirmed");
            }
            // Base spawn failure propagates revoke only. Prepared failure attempts both,
            // then propagates revoke before stop. The fallback sweep is cancellation cleanup.
            let cleanup = if prepared {
                revoked.and(stopped)
            } else {
                revoked
            };
            if let Err(error) = &cleanup {
                tracing::error!(%error, "ACP issuer cleanup unconfirmed");
            }
            shared.state.lock().expect("ACP state").active = None;
            if let Some(result) = result {
                let _ = result.send(cleanup);
            }
            if confirmed {
                Settlement::CleanUnknown
            } else {
                Settlement::Unconfirmed
            }
        });
        let slot = match associated {
            Some(receipt) => {
                match crate::db::sqlite::AcpSubmissionInput::decode(&receipt.input_json) {
                    Ok(crate::db::sqlite::AcpSubmissionInput::Canonical(association)) => {
                        DriverSlot::new(
                            driver,
                            receipt,
                            super::execution_end::Association::Canonical(association),
                        )
                    }
                    Ok(crate::db::sqlite::AcpSubmissionInput::Historical(_)) => DriverSlot::new(
                        driver,
                        receipt,
                        super::execution_end::Association::Historical,
                    ),
                    Err(error) => {
                        tracing::error!(%error, "ACP cancelled receipt association invalid; cleanup stays held");
                        DriverSlot::cleanup(driver)
                    }
                }
            }
            None => DriverSlot::cleanup(driver),
        };
        install(&self.shared, slot);
    }
}
impl Drop for IssueGuard {
    fn drop(&mut self) {
        if self.armed {
            self.retain_cleanup(None);
        }
    }
}
