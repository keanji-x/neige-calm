//! Consumer-delegated disposal; callers authorize the scope and hold its admission fence.
use crate::{
    db::sqlite::{SqliteDb, begin_immediate_tx},
    error::{CalmError, Result},
    event::BroadcastEnvelope,
    execution_store::SqliteExecutionStore,
    operation::OperationRuntime,
};
use calm_truth::db::sqlite::workspace_execution as sql;
use workspace_runtime::execution::{ExecutionId, ExecutionMetadata};

/// Registered alongside the consumer projection. Preparing and stopping all owners share
/// one transaction. The owner must fence new admission/release in that transaction and
/// atomically append any business outcome when its disposal decision wins.
#[async_trait::async_trait]
pub trait DisposalOwner: Send + Sync {
    async fn prepare_disposal_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        metadata: &ExecutionMetadata,
    ) -> Result<()>;
    /// Return only envelopes already persisted in this transaction. The coordinator
    /// broadcasts them after commit, so a failed owner stop cannot publish an outcome.
    async fn stop_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        metadata: &ExecutionMetadata,
    ) -> Result<Vec<BroadcastEnvelope>>;
    /// True only after durable owner completion. Never take over a live owner's lease.
    async fn finish(
        &self,
        runtime: &OperationRuntime,
        metadata: &ExecutionMetadata,
    ) -> Result<bool>;
}

fn conflict(error: impl std::fmt::Display) -> CalmError {
    CalmError::Conflict(format!(
        "owned execution disposal retained resources: {error}"
    ))
}

/// An execution inventory is not scope authorization: callers must already hold the scope's
/// issue/admission fence and must recheck that inventory in their final destructive writer.
/// Unknown consumers, corrupt records and unconfirmed owner completion retain all resources.
pub async fn dispose(
    store: &SqliteExecutionStore,
    runtime: &OperationRuntime,
    ids: &[ExecutionId],
) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let ctx = runtime.reclaim_context();
    let engine = ctx
        .execution
        .engine()
        .ok_or_else(|| conflict("missing Engine"))?;
    let pool = ctx.operation_repo.sqlite_db();
    let mut tx = begin_immediate_tx(&pool).await?;
    let mut owners = Vec::new();
    let mut unique = std::collections::BTreeSet::new();
    for id in ids {
        if !unique.insert(id.clone()) {
            continue;
        }
        let metadata: ExecutionMetadata = serde_json::from_str(
            &sql::intent_tx(&mut tx, id.as_str())
                .await
                .map_err(conflict)?,
        )
        .map_err(conflict)?;
        if metadata.id != *id {
            return Err(conflict("execution identity mismatch"));
        }
        let owner = store
            .projection(&metadata.consumer)
            .map_err(|e| conflict(e.0))?
            .disposal()
            .ok_or_else(|| conflict("consumer has no disposal ports"))?;
        owner
            .prepare_disposal_tx(&mut tx, &metadata)
            .await
            .map_err(conflict)?;
        owners.push((metadata, owner));
    }
    let mut events = Vec::new();
    for (metadata, owner) in &owners {
        events.extend(owner.stop_tx(&mut tx, metadata).await.map_err(conflict)?);
    }
    tx.commit().await?;
    for event in events {
        ctx.events.emit_envelope(event);
    }
    ctx.execution
        .drive(owners.iter().map(|(metadata, _)| metadata.id.clone()));
    // Include owner completion and executor retirement in one budget, not only settlement.
    tokio::time::timeout(workspace_runtime::execution::QUIESCE_DEADLINE, async {
        for (metadata, owner) in &owners {
            engine
                .wait_settled(&metadata.id)
                .await
                .map_err(|e| conflict(format!("{e:?}")))?;
            loop {
                if owner.finish(runtime, metadata).await.map_err(conflict)?
                    && retired(&pool, &metadata.id).await?
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        }
        Ok(())
    })
    .await
    .map_err(conflict)?
}

async fn retired(pool: &SqliteDb, id: &ExecutionId) -> Result<bool> {
    let row = sql::load(pool.read_pool(), id.as_str())
        .await?
        .ok_or_else(|| conflict("execution disappeared before retirement"))?;
    Ok(row.desired == "retire" && row.settlement_json.is_some() && row.secrets_json.is_none())
}
