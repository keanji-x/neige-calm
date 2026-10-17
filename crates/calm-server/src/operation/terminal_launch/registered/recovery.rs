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
    pub(crate) original: RegisteredRequest,
}

pub(crate) async fn lookup(pool: &sqlx::SqlitePool, terminal_id: &str) -> Result<OriginalLaunch> {
    let mut tx = begin_immediate_tx(pool).await?;
    let result = lookup_tx(&mut tx, terminal_id).await?;
    tx.commit().await?;
    Ok(result)
}

pub(crate) async fn lookup_tx(tx: &mut Tx<'_>, terminal_id: &str) -> Result<OriginalLaunch> {
    let rows = sqlx::query("SELECT o.* FROM operations o \
        JOIN terminals term ON json_extract(o.tx_output_json,'$.data.terminal_id')=term.id \
        JOIN cards c ON c.id=term.card_id \
        WHERE term.id=?1 AND o.kind='terminal-create' AND json_extract(o.payload_json,'$.track_id')=c.track_id LIMIT 2")
        .bind(terminal_id).fetch_all(&mut **tx).await?;
    if rows.len() > 1 {
        return Err(CalmError::Conflict(
            "Terminal has ambiguous original operations".into(),
        ));
    }
    let Some(row) = rows.first() else {
        return Ok(OriginalLaunch::Legacy);
    };
    let op = super::super::super::repo_sqlite::operation_from_row(row)?;
    let grant = TerminalExecutionGrant::load_tx(tx, &op).await?;
    let original = RegisteredRequest::read(
        &op.tx_output
            .as_ref()
            .ok_or_else(|| CalmError::Conflict("Terminal original output is missing".into()))?
            .data,
    )?;
    let result = match (grant, original) {
        (Some(grant), Some(original))
            if grant.contract == TerminalExecutionContract::Registered =>
        {
            if original.request.identity.operation_id != op.id
                || original.request.identity.process_alias != terminal_id
                || original.request.identity.authority != grant.authority
            {
                return Err(CalmError::Conflict(
                    "Terminal original executor checkpoint disagrees with acceptance".into(),
                ));
            }
            OriginalLaunch::Registered(Box::new(OriginalRegisteredLaunch {
                operation: op,
                original,
            }))
        }
        (Some(grant), None) if grant.contract == TerminalExecutionContract::Registered => {
            OriginalLaunch::AwaitingOriginalRequest
        }
        (_, Some(_)) => {
            return Err(CalmError::Conflict(
                "registered Terminal has no matching original grant".into(),
            ));
        }
        (_, None) => OriginalLaunch::Legacy,
    };
    Ok(result)
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
    pool: &sqlx::SqlitePool,
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
        pool: pool.clone(),
        operation_id: original.operation.id.clone(),
        original_payload_hash: original.operation.payload_hash.clone(),
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
