//! Prompt writer cleanup and durable settlement.
use super::*;

pub(super) async fn drive(
    driver: super::dispatch::Driver,
    mut process: Process,
    claim: crate::provider_execution::Claim,
    _guard: crate::provider_execution::DriverGuard,
) -> super::execution_end::Settlement {
    let super::dispatch::Driver {
        shared,
        receipt,
        mut translator,
        pending,
        mut cancelled,
        approvals,
    } = driver;
    let params = &shared.params;
    let mut retained = Vec::<Value>::new();
    let outcome = collect_turn(
        &shared,
        &receipt,
        &mut process.connection.incoming,
        &process.commands,
        &mut translator,
        pending,
        &mut cancelled,
        &approvals,
        &mut retained,
    )
    .await;
    // Preserve the cause before any cleanup can close pipes or terminate the process.
    // Execution output observations use this same execution identity.
    if let Err(error) = &outcome {
        tracing::warn!(%error, provider = ?params.provider, worker = %params.worker_session_id,
            turn = %receipt.turn_id, execution_id = %claim.record().execution_id,
            io_stop_reason = ?process.io.stop_reason(), transport_error = ?process.connection.client.terminal_error(),
            "ACP turn ended without a known outcome before cleanup");
    }
    // Every frame of this process that will ever be read has been: its requests end here, before
    // any teardown await, so no answer reaches the agent after its turn.
    approvals.close().await;
    #[cfg(feature = "fixtures")]
    super::super::test_seams::wait_after_fence(&params.worker_session_id).await;
    drop(approvals);
    let revoked = revoke(params).await;
    if let Err(error) = &revoked {
        tracing::error!(%error,"ACP credential revocation failed");
    }
    process.connection.client.close();
    let stopped = async {
        use crate::provider_execution::{self as execution, StopReason};
        let pool = params
            .repo
            .sqlite_db()
            .ok_or(CalmError::ExecutionUnavailable)?;
        let engine = execution::engine(params.engine.as_ref())?;
        let id = execution::execution_id(claim.record())?;
        params
            .host
            .provider_execution
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
        execution::wait_settled(engine, &id, Duration::from_secs(12)).await
    }
    .await;
    if let Err(error) = &stopped {
        tracing::warn!(%error, worker = %params.worker_session_id, turn = %receipt.turn_id,
            "ACP agent process stop failed; the turn's outcome is unknown");
    }
    let cleanup_confirmed = stopped.is_ok() && revoked.is_ok();
    let (mut events, known) = match outcome {
        Ok(events) if stopped.is_ok() && revoked.is_ok() => (events, true),
        _ => (
            vec![PlannerEvent {
                thread_id: Some(receipt.thread_id.clone()),
                kind: PlannerEventKind::TurnCompleted {
                    turn: unknown_outcome(&receipt.turn_id),
                },
            }],
            false,
        ),
    };
    let mut turn = events
        .iter()
        .find_map(|event| match &event.kind {
            PlannerEventKind::TurnCompleted { turn } => Some(turn.clone()),
            _ => None,
        })
        .expect("ACP terminal event");
    for event in &events {
        retain_item(&mut retained, event);
    }
    let mut durable = turn.clone();
    durable["items"] = Value::Array(retained);
    #[cfg(feature = "fixtures")]
    super::super::test_seams::wait_at_settlement(&params.worker_session_id).await;
    let settlement = if let Some(pool) = params.repo.sqlite_db() {
        let outcome_json = known.then(|| durable.to_string());
        settle_receipt(&pool, &receipt, outcome_json.as_deref()).await
    } else {
        ReceiptSettlement::Unconfirmed
    };
    if settlement != ReceiptSettlement::Completed {
        turn = unknown_outcome(&receipt.turn_id);
        events = vec![PlannerEvent {
            thread_id: Some(receipt.thread_id.clone()),
            kind: PlannerEventKind::TurnCompleted { turn: turn.clone() },
        }];
    }
    let completion = async {
        use crate::provider_execution::{self as execution, CompletionEvidence, KnownEnd};
        let end = KnownEnd::AcpTurn(Box::new(super::super::execution::End {
            shared: shared.clone(),
            receipt: receipt.clone(),
            events,
            turn,
        }));
        let pool = params
            .repo
            .sqlite_db()
            .ok_or(CalmError::ExecutionUnavailable)?;
        if stopped.is_ok() {
            params
                .host
                .provider_execution
                .finish(
                    &pool,
                    params.engine.as_ref(),
                    &params.worker_session_id,
                    &claim,
                    Some(end),
                )
                .await
        } else {
            tokio::time::timeout(
                Duration::from_secs(10),
                params.host.provider_execution.owner.complete(
                    &params.worker_session_id,
                    claim.record(),
                    CompletionEvidence::Unconfirmed,
                    Some(end),
                ),
            )
            .await
            .map_err(execution::conflict)?
        }
    }
    .await;
    if let Err(error) = completion {
        tracing::error!(%error, "ACP owner completion pending; retaining execution claim");
        // The retained task still returns the cleanup/receipt facts it observed.
        // A failed owner write retains the claim; it does not erase a normal join.
    }
    if settlement == ReceiptSettlement::Completed {
        super::execution_end::Settlement::Completed
    } else if cleanup_confirmed {
        super::execution_end::Settlement::CleanUnknown
    } else {
        super::execution_end::Settlement::Unconfirmed
    }
}
pub(super) use crate::acp_session::driver::retain_item;
#[allow(clippy::too_many_arguments)]
pub(super) async fn collect_turn(
    shared: &Shared,
    receipt: &AcpSubmission,
    incoming: &mut tokio::sync::mpsc::Receiver<Incoming>,
    commands: &super::super::process::Commands,
    translator: &mut TurnTranslator,
    pending: tokio::sync::oneshot::Receiver<
        std::result::Result<provider::acp::PendingResponse, provider::acp::WriteFailure>,
    >,
    cancelled: &mut watch::Receiver<CancelRequest>,
    approvals: &Approvals,
    retained: &mut Vec<Value>,
) -> Result<Vec<PlannerEvent>> {
    let admission = shared.admission(
        &receipt.thread_id,
        None,
        Some(&receipt.native_session_id),
        Some(&receipt.turn_id),
    );
    crate::acp_session::driver::collect_turn(
        crate::acp_session::driver::NativeTurnIdentity {
            native: &receipt.native_session_id,
        },
        &|event| {
            let _ = shared.events.send(event);
        },
        admission.gate(),
        incoming,
        commands,
        translator,
        pending,
        cancelled,
        approvals,
        retained,
    )
    .await
}

#[derive(PartialEq, Eq)]
pub(super) enum ReceiptSettlement {
    Completed,
    Unknown,
    Unconfirmed,
}

/// Persistence pending is not execution unknown. This function has no provider access.
pub(super) async fn settle_receipt(
    pool: &crate::db::sqlite::SqliteDb,
    receipt: &AcpSubmission,
    outcome_json: Option<&str>,
) -> ReceiptSettlement {
    match settle_with_retry(pool, receipt, outcome_json).await {
        Ok(()) if outcome_json.is_some() => ReceiptSettlement::Completed,
        Ok(()) => ReceiptSettlement::Unknown,
        Err(error) => {
            #[cfg(feature = "fixtures")]
            super::super::test_seams::settlement_note(&receipt.worker_session_id, "failed");
            tracing::error!(%error, "ACP settlement failed; durable submission remains fenced");
            // The receipt stays unresolved even if the writer remains unavailable.
            // Never overwrite a completed CAS winner with an unknown outcome.
            match tokio::time::timeout(
                Duration::from_secs(1),
                acp_submission_finish(pool, &receipt.worker_session_id, &receipt.client_id, None),
            )
            .await
            {
                Ok(Ok(())) => ReceiptSettlement::Unknown,
                _ => ReceiptSettlement::Unconfirmed,
            }
        }
    }
}

async fn settle_with_retry(
    pool: &crate::db::sqlite::SqliteDb,
    receipt: &AcpSubmission,
    outcome_json: Option<&str>,
) -> Result<()> {
    // Bound receipt-only persistence to 45s without holding the active session
    // indefinitely. Writer-pool admission, SQLite busy handling and backoff all
    // consume this same whole-future budget.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    // The active turn remains owned by drive until this returns. A crash loses the
    // in-memory known result; restart recovery fences the sending receipt as unknown.
    loop {
        // The receipt owns settlement; projections can always be rebuilt from it.
        // Never publish success before its final frames are durable.
        let result = tokio::time::timeout_at(
            deadline,
            acp_submission_finish(
                pool,
                &receipt.worker_session_id,
                &receipt.client_id,
                outcome_json,
            ),
        )
        .await;
        match result {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(calm_truth::TruthError::Db(ref error)))
                if crate::db::sqlite::is_sqlite_busy(error)
                    || matches!(error, sqlx::Error::PoolTimedOut) =>
            {
                #[cfg(feature = "fixtures")]
                super::super::test_seams::settlement_note(&receipt.worker_session_id, "retry");
                tracing::debug!(%error, "ACP receipt persistence pending; retrying settlement only");
                tokio::time::sleep_until(
                    deadline.min(tokio::time::Instant::now() + Duration::from_millis(100)),
                )
                .await;
            }
            Ok(Err(error)) => return Err(error.into()),
            Err(_) => {
                return Err(CalmError::Conflict(
                    "ACP receipt settlement retry budget exhausted".into(),
                ));
            }
        }
    }
}
