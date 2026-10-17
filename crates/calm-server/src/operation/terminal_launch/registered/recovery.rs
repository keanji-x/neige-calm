//! Exact original-ticket reads and lifecycle control. No current cwd/source
//! admission and no operation reissue are involved in these paths.
use super::super::super::terminal_adapter::admission::{
    TerminalExecutionContract, TerminalExecutionGrant,
};
use super::*;

pub(crate) enum OriginalLaunch {
    Legacy,
    AwaitingOriginalRequest,
    Registered(Box<OriginalRegisteredLaunch>),
}

pub(crate) struct OriginalRegisteredLaunch {
    pub(crate) operation: Operation,
    pub(crate) original: OriginalExecutionRequest,
}

pub(crate) async fn lookup(
    repo: &dyn crate::db::RouteRepo,
    terminal_id: &str,
) -> Result<OriginalLaunch> {
    let terminal_id = terminal_id.to_owned();
    crate::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move { lookup_tx(tx, &terminal_id).await })
    })
    .await
}

pub(crate) async fn lookup_tx(tx: &mut Tx<'_>, terminal_id: &str) -> Result<OriginalLaunch> {
    let link: Option<Option<String>> =
        sqlx::query_scalar("SELECT launch_operation_id FROM terminals WHERE id=?1")
            .bind(terminal_id)
            .fetch_optional(&mut **tx)
            .await?;
    let Some(link) = link else {
        return Err(CalmError::NotFound(format!("terminal {terminal_id}")));
    };
    let Some(operation_id) = link else {
        return Ok(OriginalLaunch::Legacy);
    };
    let row = sqlx::query(
        "SELECT o.* FROM operations o JOIN terminals term ON term.launch_operation_id=o.id \
        JOIN cards c ON c.id=term.card_id WHERE term.id=?1 AND o.id=?2",
    )
    .bind(terminal_id)
    .bind(operation_id)
    .fetch_one(&mut **tx)
    .await?;
    let op = super::super::super::repo_sqlite::operation_from_row(&row)?;
    if op.kind != "terminal-create" {
        return Err(CalmError::Conflict(
            "Terminal original operation subject is inconsistent".into(),
        ));
    }
    if let Some(original) = execution_request::load_tx(tx, &op).await? {
        if original.request.identity.operation_id != op.id
            || original.request.identity.process_alias != terminal_id
        {
            return Err(CalmError::Conflict(
                "Terminal original executor subject is inconsistent".into(),
            ));
        }
        // Original stop/query rights are not new-source authorization. A
        // removed current grant cannot erase already-issued lifecycle control.
        return Ok(OriginalLaunch::Registered(Box::new(
            OriginalRegisteredLaunch {
                operation: op,
                original,
            },
        )));
    }
    let grant = TerminalExecutionGrant::load_tx(tx, &op).await?;
    Ok(match grant {
        Some(grant) if grant.contract == TerminalExecutionContract::Registered => {
            OriginalLaunch::AwaitingOriginalRequest
        }
        _ => OriginalLaunch::Legacy,
    })
}

async fn query(
    config: &RegisteredExecutorConfig,
    original: &OriginalRegisteredLaunch,
) -> Result<(ExecutionReceipt, ExecutionState)> {
    let mut client = config.connect().await?;
    match client
        .request(ExecutionRequest::Query(consume(&original.original.request)))
        .await?
    {
        ExecutionReply::Receipt {
            receipt: Some(receipt),
            state,
        } if receipt.identity == original.original.request.identity => Ok((receipt, state)),
        _ => Err(CalmError::Conflict(
            "original Terminal has no process receipt; reconcile its original ticket".into(),
        )),
    }
}

pub(crate) async fn restore(
    config: &RegisteredExecutorConfig,
    repo: std::sync::Arc<dyn crate::db::RouteRepo>,
    original: &OriginalRegisteredLaunch,
) -> Result<std::sync::Arc<RegisteredExecution>> {
    let (receipt, state) = query(config, original).await?;
    if matches!(state, ExecutionState::Unknown { .. }) {
        return Err(CalmError::Conflict(
            "original Terminal execution is Unknown; retained for recovery".into(),
        ));
    }
    Ok(std::sync::Arc::new(RegisteredExecution {
        _retention: ExecutionInputRetention::OriginalControlOnly,
        control: crate::terminal_executor_client::ExecutionControlHandle::new(
            config.clone(),
            original.original.request.clone(),
            receipt,
        ),
        repo,
        operation_id: original.operation.id.clone(),
        original_payload_hash: original.operation.payload_hash.clone(),
        render_history: RenderHistory::UnprovenReattachment,
    }))
}

pub(crate) async fn stop_original(
    config: &RegisteredExecutorConfig,
    original: &OriginalRegisteredLaunch,
) -> Result<()> {
    let mut client = config.connect().await?;
    let reply = client
        .request(ExecutionRequest::Revoke(consume(
            &original.original.request,
        )))
        .await?;
    let receipt = match reply {
        ExecutionReply::RevocationAcknowledged { state, .. } if state.permits_handoff() => {
            return Ok(());
        }
        ExecutionReply::RevocationAcknowledged {
            receipt: Some(receipt),
            ..
        } if receipt.identity == original.original.request.identity => receipt,
        _ => {
            return Err(CalmError::Conflict(
                "original Terminal entry could not be durably revoked".into(),
            ));
        }
    };
    let control = crate::terminal_executor_client::ExecutionControlHandle::new(
        config.clone(),
        original.original.request.clone(),
        receipt,
    );
    let _ = control
        .control(ProcessAction::Signal(
            calm_session::control::ProcSignal::Term,
        ))
        .await?;
    let started = tokio::time::Instant::now();
    let deadline = started + std::time::Duration::from_secs(2);
    let mut killed = false;
    while tokio::time::Instant::now() < deadline {
        let mut client = config.connect().await?;
        match client
            .request(ExecutionRequest::Query(consume(&original.original.request)))
            .await?
        {
            ExecutionReply::Receipt {
                state: ExecutionState::Drained { .. },
                ..
            } => return Ok(()),
            ExecutionReply::Receipt {
                state: ExecutionState::Unknown { .. },
                ..
            } => {
                return Err(CalmError::Conflict(
                    "original Terminal scope drain is Unknown".into(),
                ));
            }
            _ => {}
        }
        if !killed && started.elapsed() >= std::time::Duration::from_millis(200) {
            let _ = control
                .control(ProcessAction::Signal(
                    calm_session::control::ProcSignal::Kill,
                ))
                .await?;
            killed = true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    Err(CalmError::Conflict(
        "original Terminal descendants have no durable drain receipt".into(),
    ))
}
