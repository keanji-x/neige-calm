//! Op-owned Pipe admission and record lifecycle. Business authorization stays with the caller.
mod disposal;
use super::{Decision, Release, StopReason, engine};
use crate::{
    error::{CalmError, Result},
    model::now_ms,
    operation::{Operation, SpawnCtx, Tx},
};
use calm_truth::db::sqlite::gate_execution as sql;
use serde::{Deserialize, Serialize};
use workspace_runtime::execution::*;

/// Required owner identity; no consumer identities are special-cased here.
#[derive(Clone, Copy)]
pub(crate) struct PipeConfig {
    pub consumer: &'static str,
    pub alias_prefix: &'static str,
    pub record_key: &'static str,
}

#[async_trait::async_trait]
pub(crate) trait ReleaseAuthorization: Sync {
    async fn authorize_tx(&self, tx: &mut Tx<'_>) -> Result<()>;
}
pub(crate) struct ReleaseRequest<'a> {
    pub token: &'a [u8],
    pub timeout: std::time::Duration,
}
#[cfg(any(test, feature = "fixtures"))]
pub(crate) type ReleaseResponseHook =
    std::sync::Arc<dyn Fn() -> futures::future::BoxFuture<'static, Result<()>> + Send + Sync>;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    version: u8,
    pub execution_id: ExecutionId,
    pub release: Release,
    pub decision: Decision,
}
fn conflict(message: impl std::fmt::Debug) -> CalmError {
    CalmError::Conflict(format!("{message:?}"))
}
pub(crate) fn record(config: PipeConfig, op: &Operation) -> Result<Record> {
    let value = op
        .tx_output
        .as_ref()
        .and_then(|o| o.data.get(config.record_key))
        .ok_or_else(|| conflict("gate execution record missing"))?;
    let record: Record = serde_json::from_value(value.clone())?;
    if record.version != 1 {
        return Err(conflict("unsupported gate execution record"));
    }
    Ok(record)
}
/// Admit the caller's Pipe program and atomically persist its owner record under the live claim.
pub(crate) async fn admit(
    config: PipeConfig,
    op: &Operation,
    ctx: &SpawnCtx,
    root: std::path::PathBuf,
    track: &str,
    program: ExecutionSpecification,
    authority: Option<&dyn ReleaseAuthorization>,
) -> Result<Record> {
    let engine = engine(ctx)?;
    if !matches!(program.io_mode, calm_session::control::IoMode::Pipe) {
        return Err(conflict("owned Pipe execution requires Pipe IO"));
    }
    let id = ExecutionId::fresh()?;
    let intent = ExecutionIntent {
        metadata: ExecutionMetadata {
            consumer: config.consumer.into(),
            id: id.clone(),
            alias: ProcessAlias(format!("{}:{}", config.alias_prefix, op.id)),
            owner: OperationRef(op.id.clone()),
            track: TrackId(track.into()),
            source: PrivateSource {
                root,
                cwd: workspace::identity::WorkspaceCwd::root(),
                registration: ObjectRegistrationId::fresh()?,
                nonce: TransferNonce::fresh()?,
                directory: ExternalDirectoryId::fresh()?,
                incarnation: ExternalDirectoryIncarnationId::fresh()?,
                authority: ExecutionAuthority {
                    grant_id: ExecutionGrantId::fresh()?,
                    authorization_binding: AuthorizationBindingId::fresh()?,
                    authorization_version: 1,
                },
            },
            program,
            specification: SpecificationId::fresh()?,
            ticket_id: LaunchTicketId::fresh()?,
        },
        secrets: ExecutionSecrets {
            ticket: CapabilityToken::fresh()?,
            tokens: ExecutionControlTokens::fresh()?,
        },
    };
    let record = Record {
        version: 1,
        execution_id: id,
        release: Release::Held,
        decision: Decision::Observing,
    };
    let pool = ctx.operation_repo.sqlite_db();
    let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
    crate::operation::repo_sqlite::require_live_claim_tx(&mut tx, op).await?;
    if let Some(authority) = authority {
        authority.authorize_tx(&mut tx).await?;
    }
    let ticket = engine.admit(&mut tx, intent).await.map_err(conflict)?;
    sql::insert(
        &mut tx,
        &op.id,
        config.record_key,
        &serde_json::to_string(&record)?,
    )
    .await?;
    tx.commit().await?;
    engine.drive(ticket.committed());
    Ok(record)
}

/// All record transitions are fenced by the parked claim and compare the entire v1 record.
pub(crate) async fn transition(
    config: PipeConfig,
    ctx: &SpawnCtx,
    op: &Operation,
    old: &Record,
    new: &Record,
    authority: Option<&dyn ReleaseAuthorization>,
) -> Result<()> {
    let pool = ctx.operation_repo.sqlite_db();
    let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
    crate::operation::repo_sqlite::require_live_claim_tx(&mut tx, op).await?;
    if let Some(authority) = authority {
        authority.authorize_tx(&mut tx).await?;
    }
    sql::compare_exchange(
        &mut tx,
        &op.id,
        config.record_key,
        &serde_json::to_string(old)?,
        &serde_json::to_string(new)?,
    )
    .await?;
    if matches!(new.decision, Decision::Stopping { .. }) {
        calm_truth::db::sqlite::workspace_execution::request(
            &mut tx,
            new.execution_id.as_str(),
            "stop",
        )
        .await?;
    }
    tx.commit().await?;
    ctx.execution.drive([new.execution_id.clone()]);
    Ok(())
}

pub(crate) async fn stop(
    config: PipeConfig,
    ctx: &SpawnCtx,
    op: &Operation,
    reason: StopReason,
) -> Result<()> {
    let old = record(config, op)?;
    if matches!(old.decision, Decision::Stopping { .. }) {
        return Ok(());
    }
    let mut new = old.clone();
    new.decision = Decision::Stopping { reason };
    transition(config, ctx, op, &old, &new, None).await
}

/// Called only by the post-park observer under a parked claim.
pub(crate) async fn release(
    config: PipeConfig,
    ctx: &SpawnCtx,
    op: &Operation,
    authority: &dyn ReleaseAuthorization,
    request: ReleaseRequest<'_>,
    #[cfg(any(test, feature = "fixtures"))] response: Option<&ReleaseResponseHook>,
) -> Result<()> {
    let old = record(config, op)?;
    if old.decision
        == (Decision::Stopping {
            reason: StopReason::Disposed,
        })
    {
        return Err(conflict("execution owner is being disposed"));
    }
    if old.release != Release::Held || old.decision != Decision::Observing {
        return Ok(());
    }
    let mut new = old.clone();
    new.release = Release::Releasing {
        authorized_at_ms: now_ms(),
    };
    transition(config, ctx, op, &old, &new, Some(authority)).await?;
    let send = async {
        super::send_release(ctx, &old.execution_id, request.token).await?;
        // Fault only after the real token and close reached the executor.
        #[cfg(any(test, feature = "fixtures"))]
        if let Some(response) = response {
            response().await?;
        }
        Ok::<(), CalmError>(())
    };
    tokio::time::timeout(request.timeout, send)
        .await
        .map_err(conflict)??;
    // Only a CloseStdin acknowledgement makes settlement status eligible for a verdict.
    // Persist it under the original claim; an expired/replaced claimant cannot ack.
    let mut acked = new.clone();
    acked.release = Release::Released {
        authorized_at_ms: match new.release {
            Release::Releasing { authorized_at_ms } => authorized_at_ms,
            _ => unreachable!(),
        },
    };
    transition(config, ctx, op, &new, &acked, None).await
}

/// Completion rechecks the decision in the same transaction as phase write and Retire.
pub(crate) async fn retire(config: PipeConfig, tx: &mut Tx<'_>, op: &Operation) -> Result<()> {
    let snapshot = record(config, op)?;
    sql::require_record(
        tx,
        &op.id,
        config.record_key,
        &serde_json::to_string(&snapshot)?,
    )
    .await?;
    calm_truth::db::sqlite::workspace_execution::request(
        tx,
        snapshot.execution_id.as_str(),
        "retire",
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod disposal_tests;
