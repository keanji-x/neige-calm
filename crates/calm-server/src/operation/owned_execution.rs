//! Owner-neutral execution transport and lifecycle mechanisms.
pub(crate) mod pipe;
use super::{Operation, SpawnCtx};
use crate::error::{CalmError, Result};
use calm_truth::db::sqlite::gate_execution as sql;
use serde::{Deserialize, Serialize};
use workspace_runtime::execution::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Release {
    Held,
    Releasing { authorized_at_ms: i64 },
    Released { authorized_at_ms: i64 },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum StopReason {
    Timeout,
    KernelRestarted,
    ReleaseRefused,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Decision {
    Observing,
    Stopping { reason: StopReason },
}
fn conflict(message: impl std::fmt::Debug) -> CalmError {
    CalmError::Conflict(format!("{message:?}"))
}
pub(crate) fn engine(ctx: &SpawnCtx) -> Result<&crate::execution_boot::ExecutionEngine> {
    ctx.execution
        .engine()
        .filter(|e| e.available())
        .ok_or(CalmError::ExecutionUnavailable)
}

/// Send owner-provided release bytes only after the spawned receipt, then close stdin.
pub(crate) async fn send_release(
    ctx: &SpawnCtx,
    execution_id: &ExecutionId,
    token: &[u8],
) -> Result<()> {
    let engine = engine(ctx)?;
    let send = async {
        loop {
            if let Ok(ExecutionReply::Observation {
                receipt: Some(_),
                state: ExecutionState::Spawned,
                ..
            }) = engine.output(execution_id, None).await
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let mut offset = 0;
        while offset < token.len() {
            match engine
                .interact(
                    execution_id,
                    ProcessAction::WriteStdin {
                        bytes: token[offset..].to_vec(),
                    },
                )
                .await
                .map_err(conflict)?
            {
                ExecutionReply::StdinProgress { accepted_bytes }
                    if accepted_bytes <= token.len() - offset =>
                {
                    offset += accepted_bytes
                }
                _ => return Err(conflict("execution release returned invalid progress")),
            }
            if offset < token.len() {
                tokio::task::yield_now().await;
            }
        }
        engine.close_stdin(execution_id).await.map_err(conflict)?;
        Ok(())
    };
    send.await
}

/// Refresh owner state without acquiring authority from an unclaimed row read.
pub(crate) async fn refresh_claim(ctx: &SpawnCtx, claimed: &Operation) -> Result<Operation> {
    let latest = ctx
        .operation_repo
        .get_operation(&claimed.id)
        .await?
        .ok_or_else(|| conflict("owned op disappeared"))?;
    if latest.lease_owner != claimed.lease_owner {
        return Err(conflict("owned observer lost its original claim"));
    }
    Ok(latest)
}

pub(crate) async fn settlement(
    ctx: &SpawnCtx,
    execution_id: &ExecutionId,
) -> Result<Option<Settlement>> {
    let pool = ctx.operation_repo.sqlite_db();
    let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
    let value = sql::settlement(&mut tx, execution_id.as_str()).await?;
    tx.commit().await?;
    value
        .map(|s| serde_json::from_str(&s).map_err(Into::into))
        .transpose()
}

/// Leader exit is not settlement: ask the executor to drain descendants before sampling.
pub(crate) async fn drain_exited(
    ctx: &SpawnCtx,
    op: &Operation,
    execution_id: &ExecutionId,
) -> Result<()> {
    if let Some(engine) = ctx.execution.engine()
        && matches!(
            engine.output(execution_id, None).await,
            Ok(ExecutionReply::Observation {
                state: ExecutionState::Exited { .. },
                ..
            })
        )
    {
        let pool = ctx.operation_repo.sqlite_db();
        let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
        super::repo_sqlite::require_live_claim_tx(&mut tx, op).await?;
        let ticket = engine
            .request(&mut tx, execution_id, Desired::Stop)
            .await
            .map_err(conflict)?;
        tx.commit().await?;
        engine.drive(ticket.committed());
    }
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod ambient_tests;
