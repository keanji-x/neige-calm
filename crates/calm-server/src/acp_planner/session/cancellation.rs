//! A cancelled issuer hands resources to the same retained driver slot.
use super::execution_end::{Settlement, install};
use super::*;

pub(super) struct IssueGuard {
    shared: Arc<Shared>,
    pub(super) process: Option<Process>,
    pub(super) receipt: Option<AcpSubmission>,
    claim: Option<crate::provider_execution::Claim>,
    guard: Option<crate::provider_execution::DriverGuard>,
    armed: bool,
    reason: crate::provider_execution::StopReason,
}
impl IssueGuard {
    pub(super) fn new(shared: &Arc<Shared>) -> Self {
        Self {
            shared: shared.clone(),
            process: None,
            receipt: None,
            claim: None,
            guard: None,
            armed: true,
            reason: crate::provider_execution::StopReason::SetupFailed,
        }
    }
    pub(super) async fn launch(
        &mut self,
        engine: &crate::execution_boot::ExecutionEngine,
        registration: super::super::config::Registration<'_>,
        token: &str,
        permission: PlannerPermissionMode,
        purpose: crate::provider_execution::Purpose,
        track_fence: &crate::provider_execution::TrackFenceHandoff,
    ) -> Result<()> {
        use crate::provider_execution::{self as execution, stdio::EngineStdio};
        let params = &self.shared.params;
        let pool = params
            .repo
            .sqlite_db()
            .ok_or(CalmError::ExecutionUnavailable)?;
        let id = workspace_runtime::execution::ExecutionId::fresh().map_err(execution::conflict)?;
        use std::os::unix::fs::DirBuilderExt;
        let dir = params
            .host
            .agent_logs
            .join(super::super::process::worker_log_name(
                &params.worker_session_id,
            ));
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        let (intent, payload) =
            super::super::execution::intent(params, registration, token, permission, &id, &dir)
                .await?;
        let (claim, guard) = params
            .host
            .provider_execution
            .admit(
                &pool,
                engine,
                &params.worker_session_id,
                crate::provider_execution::PreparedIntent(intent),
                purpose,
                crate::provider_execution::OccupancyFence::ReleaseOnCommit(track_fence),
            )
            .await?;
        self.claim = Some(claim);
        self.guard = Some(guard);
        self.reason = execution::StopReason::ReleaseRefused;
        execution::release::send(
            &pool,
            engine,
            &params.worker_session_id,
            self.claim.as_ref().unwrap(),
            &payload,
            &crate::provider_execution::release::NoAuthority,
        )
        .await?;
        self.reason = execution::StopReason::SetupFailed;
        let stdin = EngineStdio::new(engine.clone(), id.clone());
        let io = stdin.write_progress();
        let stdout = EngineStdio::new(engine.clone(), id).with_progress(io.clone());
        let connection = provider::acp::Connection::new(stdout, stdin);
        self.process = Some(Process {
            connection,
            capabilities: Default::default(),
            commands: Vec::new(),
            io,
        });
        Ok(())
    }
    pub(super) fn handoff(
        &mut self,
    ) -> (
        Process,
        AcpSubmission,
        crate::provider_execution::Claim,
        crate::provider_execution::DriverGuard,
    ) {
        let process = self.process.take().expect("prepared ACP process");
        let receipt = self.receipt.take().expect("prepared ACP receipt");
        self.armed = false;
        (
            process,
            receipt,
            self.claim.take().expect("admitted execution"),
            self.guard.take().expect("driver guard"),
        )
    }
    pub(super) fn handoff_process(
        &mut self,
    ) -> (
        Process,
        crate::provider_execution::Claim,
        crate::provider_execution::DriverGuard,
    ) {
        self.armed = false;
        (
            self.process
                .take()
                .expect("prepared ACP maintenance process"),
            self.claim.take().expect("admitted execution"),
            self.guard.take().expect("driver guard"),
        )
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
        let reason = process
            .as_ref()
            .and_then(|process| process.io.stop_reason())
            .unwrap_or(self.reason);
        let receipt = self.receipt.take();
        let claim = self.claim.take();
        let guard = self.guard.take();
        let execution_id = claim
            .as_ref()
            .map(|claim| crate::provider_execution::execution_id(claim.record()))
            .transpose()
            .expect("admitted identity");
        let associated = receipt.clone();
        let driver = tokio::spawn(async move {
            let params = &shared.params;
            let revoked = revoke(params).await;
            let _guard = guard;
            if let Some(process) = process {
                process.connection.client.close();
            }
            let prepared = claim.is_some();
            let stopped = async {
                if let Some(claim) = &claim {
                    use crate::provider_execution as execution;
                    let pool = params
                        .repo
                        .sqlite_db()
                        .ok_or(CalmError::ExecutionUnavailable)?;
                    let engine = execution::engine(params.engine.as_ref())?;
                    let owner = &params.host.provider_execution;
                    let id = execution::execution_id(claim.record())?;
                    owner
                        .stop(
                            &pool,
                            Some(engine),
                            &params.worker_session_id,
                            claim,
                            reason,
                        )
                        .await?;
                    execution::wait_settled(engine, &id, Duration::from_secs(12)).await?;
                }
                Ok(())
            }
            .await;
            // Owner projection is retryable bookkeeping, not stop/revoke/join
            // evidence. A failed write retains the claim but not a false Unconfirmed.
            if stopped.is_ok()
                && let Some(claim) = &claim
            {
                use crate::provider_execution::KnownEnd;
                let completion = async {
                    let pool = params
                        .repo
                        .sqlite_db()
                        .ok_or(CalmError::ExecutionUnavailable)?;
                    params
                        .host
                        .provider_execution
                        .finish(
                            &pool,
                            params.engine.as_ref(),
                            &params.worker_session_id,
                            claim,
                            Some(match claim.record().purpose {
                                crate::provider_execution::Purpose::Compaction { .. } => {
                                    KnownEnd::AcpCompaction
                                }
                                _ => KnownEnd::AcpRefused,
                            }),
                        )
                        .await
                }
                .await;
                if let Err(error) = completion {
                    tracing::error!(%error, "ACP owner completion pending; retaining execution claim");
                }
            }
            let confirmed = prepared && revoked.is_ok() && stopped.is_ok();
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
        let slot = match execution_id {
            Some(id) => slot.with_execution(id),
            None => slot,
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
