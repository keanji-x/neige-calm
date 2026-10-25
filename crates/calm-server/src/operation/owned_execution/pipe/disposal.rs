//! Scope-authorized Pipe stop writes do not acquire or replace an observer's lease.
use super::*;
use crate::{
    execution_quiesce::owned::DisposalOwner,
    operation::{OperationRuntime, Phase, RecoveryMode, owned_parked},
};

async fn owner_tx(
    config: PipeConfig,
    tx: &mut Tx<'_>,
    metadata: &ExecutionMetadata,
) -> Result<Operation> {
    let row = sqlx::query("SELECT * FROM operations WHERE id=?1")
        .bind(&metadata.owner.0)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| conflict("execution owner disappeared"))?;
    let op = crate::operation::repo_sqlite::operation_from_row(&row)?;
    let record = record(config, &op)?;
    if metadata.consumer.0 != config.consumer
        || metadata.alias.0 != format!("{}:{}", config.alias_prefix, op.id)
        || metadata.id != record.execution_id
    {
        return Err(conflict("execution record and metadata disagree"));
    }
    if !matches!(
        op.phase,
        Phase::SpawnStarted | Phase::Parked | Phase::Succeeded | Phase::Failed
    ) {
        return Err(conflict("execution owner is not in a disposable phase"));
    }
    Ok(op)
}

impl PipeConfig {
    /// True only when this writer wins the disposal decision. The business owner
    /// must converge its task and append events in this same transaction.
    pub(crate) async fn stop_for_disposal_tx(
        &self,
        tx: &mut Tx<'_>,
        metadata: &ExecutionMetadata,
    ) -> Result<bool> {
        let op = owner_tx(*self, tx, metadata).await?;
        // Natural owner completion won before this transaction: preserve it.
        if matches!(op.phase, Phase::Succeeded | Phase::Failed) {
            return Ok(false);
        }
        let old = record(*self, &op)?;
        let won = old.decision == Decision::Observing;
        if won {
            let mut new = old.clone();
            new.decision = Decision::Stopping {
                reason: StopReason::Disposed,
            };
            // This is an authorized scope writer, not a replacement observer. Exact record CAS
            // invalidates stale release/completion snapshots; the live lease remains untouched.
            crate::operation::repo_sqlite::require_original_operation_tx(tx, &op).await?;
            sql::compare_exchange(
                tx,
                &op.id,
                self.record_key,
                &serde_json::to_string(&old)?,
                &serde_json::to_string(&new)?,
            )
            .await?;
        }
        calm_truth::db::sqlite::workspace_execution::request(tx, metadata.id.as_str(), "stop")
            .await?;
        Ok(won)
    }
}

#[async_trait::async_trait]
impl DisposalOwner for PipeConfig {
    async fn prepare_disposal_tx(
        &self,
        tx: &mut Tx<'_>,
        metadata: &ExecutionMetadata,
    ) -> Result<()> {
        owner_tx(*self, tx, metadata).await.map(|_| ())
    }

    async fn stop_tx(
        &self,
        tx: &mut Tx<'_>,
        metadata: &ExecutionMetadata,
    ) -> Result<Vec<crate::event::BroadcastEnvelope>> {
        self.stop_for_disposal_tx(tx, metadata).await?;
        Ok(Vec::new())
    }

    async fn finish(
        &self,
        runtime: &OperationRuntime,
        metadata: &ExecutionMetadata,
    ) -> Result<bool> {
        let ctx = runtime.reclaim_context();
        let op = ctx
            .operation_repo
            .get_operation(&metadata.owner.0)
            .await?
            .ok_or_else(|| conflict("execution owner disappeared before completion"))?;
        if record(*self, &op)?.execution_id != metadata.id {
            return Err(conflict("execution owner changed before completion"));
        }
        if matches!(op.phase, Phase::Succeeded | Phase::Failed) {
            return Ok(true);
        }
        if matches!(op.phase, Phase::Parked)
            && let Some(claimed) = ctx.operation_repo.claim_parked(&op.id).await?
        {
            let adapter = runtime.adapter(&claimed.kind)?;
            owned_parked::reconcile(
                adapter.as_ref(),
                &claimed,
                RecoveryMode::PreDeadlineProbe,
                ctx,
            )
            .await?;
        }
        // A live observer finishes under its original claim; a later call checks its commit.
        Ok(false)
    }
}
