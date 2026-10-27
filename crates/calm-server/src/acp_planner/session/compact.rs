//! Canonical maintenance owns one native request and one exact cleanup driver, never an input.
use super::super::profile::CompactionWire;
use super::*;
use crate::harness::backend::{MaintenanceObservation, MaintenanceTarget};
use crate::harness::native_admission::NativePromptAdmission;
use provider::acp::compaction::{CompactionUpdate, Status};
use provider::acp::grok_compaction;
use provider::control::{
    CompactNotSubmitted, CompactSubmission, SubmissionBasis, SubmissionEvidence,
};

type CompactInbox = Arc<std::sync::Mutex<Option<tokio::sync::mpsc::Receiver<Incoming>>>>;

impl AcpPlannerSession {
    fn owns_maintenance(&self, target: &MaintenanceTarget) -> bool {
        let params = &self.shared.params;
        target.card_id == params.card_id
            && target.track_id == params.track_id
            && target.worker_session_id == params.worker_session_id
    }
    pub(crate) async fn compact_canonical(
        &self,
        target: MaintenanceTarget,
        selection: &provider::TurnModelSelection,
        admission: &NativePromptAdmission<'_>,
        track_fence: &crate::provider_execution::TrackFenceHandoff,
    ) -> CompactSubmission<CalmError> {
        let _issue = self.shared.issue.lock().await;
        if !self.owns_maintenance(&target) {
            return not_submitted(CalmError::Conflict(
                "maintenance names another ACP owner".into(),
            ));
        }
        // An operation can cross the write boundary once, even if its caller loses the response.
        if execution_end::maintenance_attempted(&self.shared, &target).await {
            return CompactSubmission::Unconfirmed {
                error: CalmError::Conflict(
                    "ACP maintenance was already attempted; it will not be resent".into(),
                ),
                evidence: SubmissionEvidence::Unobserved,
            };
        }
        match self
            .issue_compact(target, selection, admission, track_fence)
            .await
        {
            Ok(()) => CompactSubmission::Submitted {
                basis: SubmissionBasis::LocalWrite,
            },
            Err(CompactFailure::Before(error)) => not_submitted(error),
            Err(CompactFailure::Crossed(error)) => CompactSubmission::Unconfirmed {
                error,
                evidence: SubmissionEvidence::WriteMayHaveReachedPeer,
            },
        }
    }
    async fn issue_compact(
        &self,
        target: MaintenanceTarget,
        selection: &provider::TurnModelSelection,
        admission: &NativePromptAdmission<'_>,
        track_fence: &crate::provider_execution::TrackFenceHandoff,
    ) -> std::result::Result<(), CompactFailure> {
        let shared = &self.shared;
        let params = &shared.params;
        let thread = &target.thread_id;
        let permission = PlannerPermissionMode::Never;
        let held = shared
            .installed
            .get()
            .ok_or_else(|| before("ACP harness is not installed"))?;
        let row = shared
            .current_owner(thread, Some(permission))
            .await
            .map_err(|e| CompactFailure::Before(e.into()))?;
        let registration = params
            .host
            .registration(&params.provider)
            .map_err(CompactFailure::Before)?;
        if !registration.profile.canonical_inputs(row.contract) {
            return Err(before("ACP profile does not consume canonical compaction"));
        }
        let wire = registration
            .profile
            .compaction(row.contract)
            .ok_or_else(|| before("ACP profile does not declare compaction"))?;
        let native = row
            .agent_session_id
            .ok_or_else(|| before("ACP maintenance requires a native binding"))?;
        {
            let state = shared.state.lock().expect("ACP state");
            if state.closed
                || state.active.is_some()
                || shared.parked.is_parked()
                || params.seals.is_sealed(thread)
            {
                return Err(before("ACP conversation is busy, closed or sealed"));
            }
        }
        if !execution_end::driver_finished(shared) {
            return Err(before("ACP driver is still running"));
        }
        let pool = params
            .repo
            .sqlite_db()
            .ok_or_else(|| before("ACP journal is missing"))?;
        if acp_submission_unresolved(pool.read_pool(), &params.worker_session_id)
            .await
            .map_err(|e| CompactFailure::Before(e.into()))?
        {
            return Err(before("ACP input outcome remains unknown"));
        }
        crate::provider_execution::admission_gate(
            &pool,
            params.engine.as_ref(),
            &params.worker_session_id,
            Some(&params.host.provider_execution),
        )
        .await
        .map_err(CompactFailure::Before)?;
        join_driver(shared).await.map_err(CompactFailure::Before)?;
        admission
            .check_local()
            .await
            .map_err(CompactFailure::Before)?;
        let digest = registration
            .config
            .native_session_digest(&params.cwd)
            .map_err(CompactFailure::Before)?;
        crate::db::sqlite::acp_registration_claim(&pool, &params.worker_session_id, &digest, true)
            .await
            .map_err(|e| CompactFailure::Before(start::registration_error(e).into()))?;
        let engine = crate::provider_execution::engine(params.engine.as_ref())
            .map_err(CompactFailure::Before)?;
        super::super::execution::validate_registration(registration)
            .map_err(CompactFailure::Before)?;
        let turn = uuid::Uuid::new_v4().to_string();
        let server = registration.profile.mcp_server_key();
        let mut guard = IssueGuard::new(shared);
        let token = match mint_token(params).await {
            Ok(token) => token,
            Err(error) => {
                guard.finish().await.map_err(CompactFailure::Before)?;
                return Err(CompactFailure::Before(error));
            }
        };
        let mut usage = provider::acp::usage::UsageTracker::new(registration.profile.usage);
        let prepared = async {
            guard
                .launch(
                    engine,
                    registration,
                    &token,
                    permission,
                    crate::provider_execution::Purpose::Compaction {
                        turn_id: turn.clone(),
                    },
                    track_fence,
                )
                .await?;
            let capabilities = super::super::process::negotiate(
                &guard
                    .process
                    .as_ref()
                    .expect("admitted process")
                    .connection
                    .client,
                registration,
                wire == CompactionWire::StandardPrompt,
            )
            .await
            .map_err(|error| StartFailure::Failed(CalmError::Conflict(error)))?;
            guard
                .process
                .as_mut()
                .expect("admitted process")
                .capabilities = capabilities;
            let process = guard.process.as_mut().expect("ACP maintenance process");
            let owner = shared.admission(thread, Some(permission), Some(&native), None);
            let combined = admission.combined(&owner);
            let mcp = [params.mcp_server(server.clone(), &token)];
            let (_, configuration) = setup::attach(
                process,
                params,
                thread,
                Some(native.clone()),
                &mcp,
                &combined,
                &mut usage,
            )
            .await?;
            shared
                .configure(
                    process,
                    &native,
                    configuration,
                    selection,
                    &combined,
                    &mut usage,
                )
                .await?;
            if !process
                .commands
                .iter()
                .any(|(session, command)| session == &native && command == "compact")
            {
                return Err(refused("ACP did not declare the compact command"));
            }
            Ok::<_, StartFailure>(())
        }
        .await;
        if let Err(error) = prepared {
            guard.finish().await.map_err(CompactFailure::Before)?;
            return Err(CompactFailure::Before(error.into()));
        }
        let (mut process, claim, driver_guard) = guard.handoff_process();
        let execution_id = crate::provider_execution::execution_id(claim.record())
            .map_err(CompactFailure::Before)?;
        let approvals = Arc::new(Approvals::for_turn(
            permission,
            held,
            &turn,
            &process.connection.client,
            registration.profile.asks(&server, Vec::<String>::new()),
        ));
        let (cancel, cancelled) = watch::channel(CancelRequest::Running);
        shared.state.lock().expect("ACP state").active = Some(Active {
            execution_id: execution_id.clone(),
            thread: thread.clone(),
            turn: turn.clone(),
            cancel,
            approvals: Arc::downgrade(&approvals),
            native_session_id: native.clone(),
        });
        // Keep the inbox with this one driver, but expose its queued prefix to
        // the final synchronous admission check before the first write poll.
        let inbox = (wire == CompactionWire::XaiExtension).then(|| {
            let (_, empty) = tokio::sync::mpsc::channel(1);
            Arc::new(std::sync::Mutex::new(Some(std::mem::replace(
                &mut process.connection.incoming,
                empty,
            ))))
        });
        #[cfg(all(test, feature = "fixtures"))]
        if let Some(inbox) = &inbox {
            super::super::test_seams::observe_compact_inbox(&params.worker_session_id, {
                let inbox = inbox.clone();
                move || inbox.lock().unwrap().as_ref().map_or(0, |rx| rx.len())
            });
        }
        let driver_inbox = inbox.clone();
        let client = process.connection.client.clone();
        let capabilities = process.capabilities.clone();
        let (send, pending) = tokio::sync::oneshot::channel();
        let task_shared = shared.clone();
        let task_native = native.clone();
        let driver = tokio::spawn(async move {
            drive_compact(
                task_shared,
                process,
                task_native,
                wire,
                pending,
                driver_inbox,
                cancelled,
                approvals,
                claim,
                driver_guard,
            )
            .await
        });
        execution_end::install(
            shared,
            DriverSlot::maintenance(driver, target.clone(), turn.clone(), native.clone())
                .with_execution(execution_id),
        );
        let owner = shared.admission(thread, Some(permission), Some(&native), Some(&turn));
        let combined = admission.combined(&owner);
        let result = match wire {
            CompactionWire::StandardPrompt => {
                protocol::prompt(
                    &client,
                    &capabilities,
                    &native,
                    &[protocol::ContentBlock::text("/compact")],
                    combined.gate(),
                )
                .await
            }
            CompactionWire::XaiExtension => {
                let gate = combined.gate_with_final_check(|| {
                    let mut inbox = inbox
                        .as_ref()
                        .expect("Grok compact inbox")
                        .lock()
                        .expect("compact inbox");
                    let incoming = inbox.as_mut().expect("compact driver awaits handoff");
                    // Exclude the finite prefix queued during async admission.
                    // This precedes the first AsyncWrite poll, not necessarily
                    // actual bytes: EngineStdio may await SQL/RPC work internally.
                    // The matching RPC result and cleanup remain mandatory.
                    // Never drain after submit: preserve fast completions.
                    for _ in 0..incoming.len() {
                        if matches!(incoming.try_recv(), Ok(Incoming::Request { .. })) {
                            return Err(CalmError::Conflict(
                                "ACP compact has a pre-send client request".into(),
                            ));
                        }
                    }
                    Ok(())
                });
                client
                    .submit_gated(
                        grok_compaction::METHOD,
                        grok_compaction::encode(
                            &native,
                            "Preserve the conversation context for subsequent turns.",
                        ),
                        &gate,
                    )
                    .await
            }
        };
        let failure = match &result {
            Ok(_) => None,
            Err(provider::acp::WriteFailure::BeforeWrite(error)) => Some(CompactFailure::Before(
                CalmError::Conflict(error.to_string()),
            )),
            Err(provider::acp::WriteFailure::CrossedWrite(error)) => Some(CompactFailure::Crossed(
                CalmError::Conflict(error.to_string()),
            )),
        };
        let _ = send.send(result);
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    pub(crate) async fn observe_maintenance(
        &self,
        target: &MaintenanceTarget,
    ) -> Result<Option<MaintenanceObservation>> {
        if !self.owns_maintenance(target) {
            return Ok(Some(MaintenanceObservation::Unconfirmed));
        }
        let row = self.shared.current_owner(&target.thread_id, None).await;
        let native = row.ok().and_then(|row| row.agent_session_id);
        Ok(Some(
            execution_end::observe_maintenance(&self.shared, target, native.as_deref()).await,
        ))
    }
}
enum CompactFailure {
    Before(CalmError),
    Crossed(CalmError),
}
fn before(message: &str) -> CompactFailure {
    CompactFailure::Before(CalmError::Conflict(message.into()))
}
fn not_submitted(error: CalmError) -> CompactSubmission<CalmError> {
    CompactSubmission::NotSubmitted {
        error,
        reason: CompactNotSubmitted::PreparationFailed,
    }
}

#[derive(Default)]
struct Correlation {
    native_id: Option<String>,
    settled: Option<bool>,
}
impl Correlation {
    fn observe(
        &mut self,
        wire: CompactionWire,
        method: &str,
        params: &Value,
        native: &str,
    ) -> Result<()> {
        match wire {
            CompactionWire::StandardPrompt if method == "session/update" => {
                self.update(params, native)
            }
            CompactionWire::XaiExtension => {
                if let Some(completion) =
                    grok_compaction::decode(method, params).map_err(wire_error)?
                {
                    if completion.is_replay().map_err(wire_error)? {
                        return Ok(());
                    }
                    if completion.session_id != native {
                        return Err(CalmError::Conflict("xAI compaction session changed".into()));
                    }
                    self.settled = Some(true);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn update(&mut self, frame: &Value, native: &str) -> Result<()> {
        if frame["sessionId"].as_str() != Some(native) {
            return Err(CalmError::Conflict("ACP compaction session changed".into()));
        }
        let update = CompactionUpdate::decode(&frame["update"]).map_err(wire_error)?;
        let (compaction_id, status) = match update {
            Some(CompactionUpdate::Lifecycle {
                compaction_id,
                status,
                ..
            }) => (compaction_id, Some(status)),
            Some(CompactionUpdate::SummaryChunk { compaction_id, .. }) => (compaction_id, None),
            None => return Ok(()),
        };
        // Only a previously unseen start can be the sole candidate. A start-less
        // update poisons ownership too, so no set of historical IDs is needed.
        // Ambiguity is sticky, even if it arrives after the candidate completed.
        if self.settled == Some(false) {
            return Ok(());
        }
        match (self.native_id.as_deref(), status) {
            (None, Some(Status::InProgress)) => self.native_id = Some(compaction_id),
            (Some(id), status) if id == compaction_id => match status {
                Some(Status::InProgress) if self.settled.is_some() => self.settled = Some(false),
                Some(Status::Completed) => self.settled = Some(true),
                Some(Status::Cancelled | Status::Failed) => self.settled = Some(false),
                _ => {}
            },
            _ => self.settled = Some(false),
        }
        Ok(())
    }
}
#[allow(clippy::too_many_arguments)]
async fn drive_compact(
    shared: Arc<Shared>,
    mut process: Process,
    native: String,
    wire: CompactionWire,
    pending: tokio::sync::oneshot::Receiver<
        std::result::Result<provider::acp::PendingResponse, provider::acp::WriteFailure>,
    >,
    inbox: Option<CompactInbox>,
    mut cancelled: watch::Receiver<CancelRequest>,
    approvals: Arc<Approvals>,
    claim: crate::provider_execution::Claim,
    _guard: crate::provider_execution::DriverGuard,
) -> execution_end::Settlement {
    let outcome = async {
        let handed = pending.await;
        if let Some(inbox) = inbox {
            process.connection.incoming = inbox.lock().expect("compact inbox").take().expect("one compact driver");
        }
        let pending = handed.map_err(|_| before("ACP compact handoff lost"))?.map_err(|e| CompactFailure::Crossed(wire_error(e.into_error())))?;
        let response = pending.wait(Duration::from_secs(3600)); tokio::pin!(response);
        let mut correlation = Correlation::default();
        loop { tokio::select! {
            result = &mut response => {
                while let Ok(frame) = process.connection.incoming.try_recv() {
                    match frame {
                        Incoming::Notification { method, params } => correlation.observe(wire, &method, &params, &native).map_err(CompactFailure::Before)?,
                        Incoming::Request { .. } => return Err(before("ACP compact ended with a pending client request")),
                    }
                }
                let result = result.map_err(|e| CompactFailure::Crossed(wire_error(e)))?;
                let confirmed = match wire {
                    CompactionWire::StandardPrompt => {
                        let result: protocol::PromptResponse = protocol::decode(result).map_err(|e| CompactFailure::Crossed(wire_error(e)))?;
                        matches!(result.stop_reason, protocol::StopReason::EndTurn)
                    },
                    CompactionWire::XaiExtension => grok_compaction::empty_response(&result),
                };
                return Ok(confirmed && correlation.settled == Some(true));
            },
            _ = cancelled.changed() => return Err(before("ACP compact interrupted")),
            frame = process.connection.incoming.recv() => match frame {
                Some(Incoming::Notification { method, params }) => correlation.observe(wire, &method, &params, &native).map_err(CompactFailure::Before)?,
                Some(Incoming::Request { id, method, .. }) => provider::acp::approvals::refuse(&process.connection.client, id, &method).await.map_err(|e| CompactFailure::Crossed(wire_error(e)))?,
                None => return Err(before("ACP compact connection closed")),
            }
        }}
    }.await;
    approvals.close().await;
    #[cfg(feature = "fixtures")]
    super::super::test_seams::wait_after_fence(&shared.params.worker_session_id).await;
    drop(approvals);
    let revoked = revoke(&shared.params).await;
    process.connection.client.close();
    let stopped = async {
        use crate::provider_execution::{self as execution, KnownEnd, StopReason};
        let params = &shared.params;
        let pool = params
            .repo
            .sqlite_db()
            .ok_or(CalmError::ExecutionUnavailable)?;
        let engine = execution::engine(params.engine.as_ref())?;
        let id = execution::execution_id(claim.record())?;
        let owner = &params.host.provider_execution;
        owner
            .stop(
                &pool,
                Some(engine),
                &params.worker_session_id,
                &claim,
                if outcome.is_ok() {
                    StopReason::Finished
                } else {
                    process.io.stop_reason().unwrap_or(StopReason::Interrupted)
                },
            )
            .await?;
        execution::wait_settled(engine, &id, Duration::from_secs(12)).await?;
        // The maintenance owner revokes and retires only this exact execution claim.
        owner
            .finish(
                &pool,
                Some(engine),
                &params.worker_session_id,
                &claim,
                Some(KnownEnd::AcpCompaction),
            )
            .await
    }
    .await;
    shared.state.lock().expect("ACP state").active = None;
    if matches!(outcome, Ok(true)) && revoked.is_ok() && stopped.is_ok() {
        execution_end::Settlement::Completed
    } else {
        execution_end::Settlement::Unconfirmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn acp_compact_requires_one_unseen_start_and_completion() {
        for updates in [
            vec![("manual", "in_progress"), ("manual", "completed")],
            vec![
                ("manual", "in_progress"),
                ("manual", "in_progress"),
                ("manual", "completed"),
            ],
        ] {
            assert_eq!(correlate(&updates).settled, Some(true));
        }
    }
    #[test]
    fn acp_compact_ambiguous_or_reused_ids_never_certify() {
        for updates in [
            vec![
                ("auto", "in_progress"),
                ("auto", "completed"),
                ("manual", "in_progress"),
                ("manual", "failed"),
            ],
            vec![
                ("old", "completed"),
                ("old", "in_progress"),
                ("old", "completed"),
            ],
            vec![
                ("old", "completed"),
                ("manual", "in_progress"),
                ("manual", "completed"),
            ],
            vec![
                ("manual", "in_progress"),
                ("other", "in_progress"),
                ("manual", "completed"),
            ],
            vec![
                ("manual", "in_progress"),
                ("manual", "completed"),
                ("manual", "in_progress"),
                ("manual", "completed"),
            ],
            vec![
                ("manual", "in_progress"),
                ("manual", "failed"),
                ("manual", "completed"),
            ],
            vec![("manual", "completed")],
        ] {
            assert_eq!(correlate(&updates).settled, Some(false), "{updates:?}");
        }
        let mut correlation = Correlation::default();
        correlation.update(&json!({"sessionId":"native", "update":{"sessionUpdate":"compaction_summary_chunk","compactionId":"manual","content":{"type":"text","text":"early"}}}), "native").unwrap();
        for status in ["in_progress", "completed"] {
            lifecycle(&mut correlation, "manual", status);
        }
        assert_eq!(correlation.settled, Some(false));
    }
    fn correlate(updates: &[(&str, &str)]) -> Correlation {
        let mut correlation = Correlation::default();
        for (id, status) in updates {
            lifecycle(&mut correlation, id, status);
        }
        correlation
    }
    fn lifecycle(correlation: &mut Correlation, id: &str, status: &str) {
        correlation.update(&json!({"sessionId":"native", "update":{"sessionUpdate":"compaction_update","compactionId":id,"status":status}}), "native").unwrap();
    }
    #[cfg(feature = "fixtures")]
    #[tokio::test]
    async fn acp_compact_observer_retains_exact_driver_and_target() {
        let (session, _repo, thread) = super::super::admission_tests::fixture().await;
        let params = &session.shared.params;
        let target = MaintenanceTarget {
            card_id: params.card_id.clone(),
            track_id: params.track_id.clone(),
            worker_session_id: params.worker_session_id.clone(),
            thread_id: thread,
            operation_id: crate::model::new_id(),
        };
        bind_native(params, &target.thread_id, "native")
            .await
            .unwrap();
        let admission = NativePromptAdmission::new(|| Ok(()), || Box::pin(async { Ok(()) }));
        assert!(
            matches!(
                session.observe_maintenance(&target).await.unwrap(),
                Some(MaintenanceObservation::Unconfirmed)
            ),
            "restart without driver is unknown"
        );
        let (finish, pending) = tokio::sync::oneshot::channel();
        let driver = tokio::spawn(async { pending.await.unwrap() });
        execution_end::install(
            &session.shared,
            DriverSlot::maintenance(driver, target.clone(), "turn".into(), "native".into()),
        );
        assert!(
            matches!(session.observe_maintenance(&target).await.unwrap(),
            Some(MaintenanceObservation::Turn { turn_id, settled: None }) if turn_id == "turn")
        );
        finish
            .send(execution_end::Settlement::Unconfirmed)
            .ok()
            .unwrap();
        join_driver(&session.shared).await.unwrap();
        assert!(
            matches!(session.observe_maintenance(&target).await.unwrap(),
            Some(MaintenanceObservation::Turn { turn_id, settled: Some(false) }) if turn_id == "turn")
        );
        let mut wrong = target;
        wrong.operation_id = crate::model::new_id();
        assert!(matches!(
            session.observe_maintenance(&wrong).await.unwrap(),
            Some(MaintenanceObservation::Unconfirmed)
        ));
        wrong.card_id = crate::model::new_id();
        assert!(matches!(
            session
                .compact_canonical(
                    wrong,
                    &provider::TurnModelSelection::inherit(),
                    &admission,
                    &session.handoff_for_test().await
                )
                .await,
            CompactSubmission::NotSubmitted { .. }
        ));
        join_driver(&session.shared).await.unwrap();
    }
}
