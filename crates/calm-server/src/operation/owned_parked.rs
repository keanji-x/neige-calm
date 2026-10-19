//! Existing parked saga completion with an adapter-owned physical resource.
use super::{
    Operation, ParkedCompletion, ParkedRecovery, ProviderAdapter, RecoveryMode, SpawnCtx,
    complete_parked_tx,
};
use crate::db::sqlite::begin_immediate_tx;
use crate::error::{CalmError, Result};

/// Both live observer and the existing boot/sweep loop use this one lease fence.
/// The adapter must retain ownership until it has real stop/quiescence evidence.
pub(crate) async fn reconcile(
    adapter: &dyn ProviderAdapter,
    op: &Operation,
    mode: RecoveryMode,
    ctx: &SpawnCtx,
) -> Result<()> {
    let owner = op
        .lease_owner
        .as_deref()
        .ok_or_else(|| CalmError::Conflict("owned parked recovery has no lease".into()))?;
    let outcome = adapter.recover_owned_parked(op, mode, ctx).await;
    let pool = ctx.operation_repo.sqlite_db();
    let result = async {
        match outcome? {
            ParkedRecovery::Complete(outcome) => {
                let mut tx = begin_immediate_tx(&pool).await?;
                super::repo_sqlite::require_live_claim_tx(&mut tx, op).await?;
                let completion = complete_parked_tx(&mut tx, &op.id, &outcome).await?;
                let events = if matches!(completion, ParkedCompletion::Completed(_)) {
                    adapter.complete_owned_parked_tx(&mut tx, op).await?
                } else {
                    Vec::new()
                };
                tx.commit().await?;
                for event in events {
                    ctx.events.emit_envelope(event);
                }
                if let ParkedCompletion::Completed(result) = completion {
                    ctx.completion.complete(result);
                }
                Ok(())
            }
            ParkedRecovery::LeaveParked => Ok(()),
            ParkedRecovery::Fail { reason } => Err(CalmError::Conflict(format!(
                "owned resource remains unresolved: {reason}"
            ))),
        }
    }
    .await;
    // A stale observer can neither complete nor alter a replacement's claim.
    let sql = if result.is_err() {
        "UPDATE operations SET last_error=?3,updated_at_ms=?4 WHERE id=?1 AND lease_owner=?2 AND phase='parked'"
    } else {
        "UPDATE operations SET lease_owner=NULL,lease_until_ms=NULL WHERE id=?1 AND lease_owner=?2 AND phase='parked'"
    };
    let mut query = sqlx::query(sql).bind(&op.id).bind(owner);
    if let Err(error) = &result {
        query = query
            .bind(format!("owned execution retained: {error}"))
            .bind(crate::model::now_ms());
    }
    query.execute(pool.writer_pool()).await?;
    result
}

#[cfg(test)]
#[path = "owned_parked_tests.rs"]
mod tests;
