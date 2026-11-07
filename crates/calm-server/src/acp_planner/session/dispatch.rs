//! The ACP session owns the receipt and prompt crossing for every process lifetime.
use super::*;
use provider::acp::{PendingResponse, WriteFailure, approvals::AgentAsks, usage::UsageTracker};

pub(super) struct TurnInput<'a, 'i, 'g> {
    pub thread: &'a str,
    pub correlation: &'a str,
    pub contract: &'a InputContract<'i, 'g>,
    pub items: &'a [InputItem],
    pub blocks: &'a [protocol::ContentBlock],
    pub permission: PlannerPermissionMode,
    pub held: HeldRequestSender,
}

pub(super) struct PreparedTurn {
    pub native: String,
    pub turn: String,
    pub usage: UsageTracker,
    pub asks: AgentAsks,
}

pub(super) struct Driver {
    pub shared: Arc<Shared>,
    pub receipt: AcpSubmission,
    pub translator: TurnTranslator,
    pub pending: tokio::sync::oneshot::Receiver<std::result::Result<PendingResponse, WriteFailure>>,
    pub cancelled: watch::Receiver<CancelRequest>,
    pub approvals: Arc<Approvals>,
}

pub(super) async fn send(
    shared: &Arc<Shared>,
    input: TurnInput<'_, '_, '_>,
    prepared: PreparedTurn,
    guard: &mut IssueGuard,
    presence: &mut ReceiptPresence,
) -> std::result::Result<ReceiptAttempt, StartFailure> {
    let params = &shared.params;
    let pool = params
        .repo
        .sqlite_db()
        .ok_or(CalmError::ExecutionUnavailable)?;
    let PreparedTurn {
        native,
        turn,
        usage,
        asks,
    } = prepared;
    let TurnInput {
        thread,
        correlation,
        contract,
        items,
        blocks,
        permission,
        held,
    } = input;
    if params.seals.is_sealed(thread) || shared.state.lock().expect("ACP state").closed {
        return Err(refused("ACP conversation was sealed before dispatch"));
    }
    let capabilities = guard
        .process
        .as_ref()
        .expect("prepared ACP process")
        .capabilities
        .clone();
    protocol::validate_prompt(&capabilities, &native, blocks)
        .map_err(|error| refused(&error.to_string()))?;
    let receipt = AcpSubmission {
        worker_session_id: params.worker_session_id.clone(),
        client_id: correlation.into(),
        turn_id: turn.clone(),
        thread_id: thread.into(),
        native_session_id: native.clone(),
        input_json: contract.receipt(items, blocks)?,
        state: crate::db::sqlite::AcpSubmissionState::Sending,
        kept_cleanup: None,
        outcome_json: None,
    };
    let attempt = || ReceiptAttempt {
        turn: turn.clone(),
        native_session_id: native.clone(),
    };
    guard.receipt = Some(receipt.clone());
    #[cfg(all(test, feature = "fixtures"))]
    super::super::test_seams::wait_issue(
        &params.worker_session_id,
        super::super::test_seams::IssueStage::BeforeReceipt,
    )
    .await;
    match acp_submission_prepare(&pool, &receipt, crate::model::now_ms()).await {
        Ok(()) => *presence = ReceiptPresence::Present(attempt()),
        Err(crate::db::sqlite::AcpPrepareError::BeforeReceipt(error)) => {
            return Err(CalmError::from(error).into());
        }
        Err(crate::db::sqlite::AcpPrepareError::CommitUncertain(error)) => {
            *presence = ReceiptPresence::Present(attempt());
            return Err(CalmError::from(error).into());
        }
    }
    #[cfg(all(test, feature = "fixtures"))]
    {
        super::super::test_seams::wait_issue(
            &params.worker_session_id,
            super::super::test_seams::IssueStage::AfterReceipt,
        )
        .await;
        if let Some(connection) =
            super::super::test_transport::connection(&params.worker_session_id)
        {
            guard
                .process
                .as_mut()
                .expect("prepared ACP process")
                .connection = connection;
        }
    }
    let client = guard
        .process
        .as_ref()
        .expect("prepared ACP process")
        .connection
        .client
        .clone();
    let execution_id = guard.execution_id()?;
    #[cfg(feature = "fixtures")]
    let held = super::super::test_seams::tap_held(&params.worker_session_id, held);
    let approvals = Arc::new(Approvals::for_turn(permission, &held, &turn, &client, asks));
    let (cancel, cancelled) = watch::channel(CancelRequest::Running);
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("carrier-quiescence-acp-issuance", &params.worker_session_id)
        .await;
    shared.state.lock().expect("ACP state").active = Some(Active {
        thread: thread.into(),
        turn: turn.clone(),
        cancel,
        approvals: Arc::downgrade(&approvals),
        native_session_id: native.clone(),
        execution_id: Some(execution_id.clone()),
    });
    let translator = TurnTranslator::new(TurnContext {
        thread_id: thread.into(),
        turn_id: turn.clone(),
        native_session_id: native.clone(),
        usage,
    });
    let _ = shared.events.send(translator.started());
    let (send_pending, pending) = tokio::sync::oneshot::channel();
    let driver = Driver {
        shared: Arc::clone(shared),
        receipt: receipt.clone(),
        translator,
        pending,
        cancelled,
        approvals,
    };
    let (process, claim, driver_guard) = guard.handoff_process();
    let driver = tokio::spawn(driver::drive(driver, process, claim, driver_guard));
    let slot = DriverSlot::new(shared, driver, receipt, contract.association())
        .with_execution(execution_id);
    execution_end::install(shared, slot);
    #[cfg(all(test, feature = "fixtures"))]
    super::super::test_seams::wait_issue(
        &params.worker_session_id,
        super::super::test_seams::IssueStage::DriverInstalled,
    )
    .await;
    // Admission and receipt are installed before any prompt reaches the writer.
    let owner = shared.admission(thread, Some(permission), Some(&native), Some(&turn));
    let admission = contract.admission(&owner);
    let pending = protocol::prompt(&client, &capabilities, &native, blocks, admission.gate()).await;
    let _ = send_pending.send(pending);
    Ok(attempt())
}
