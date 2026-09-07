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
    let pool = ctx.operation_repo.sqlite_pool();
    let result=async {
        match outcome? {
            ParkedRecovery::Complete(outcome)=>{
                let mut tx=begin_immediate_tx(&pool).await?;
                let owned:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM operations WHERE id=?1 AND lease_owner=?2 AND phase='parked')")
                    .bind(&op.id).bind(owner).fetch_one(&mut *tx).await?;
                if !owned {return Err(CalmError::Conflict("owned parked completion lost lease".into()));}
                let completion=complete_parked_tx(&mut tx,&op.id,&outcome).await?;
                let events = if matches!(completion, ParkedCompletion::Completed(_)) {
                    adapter.complete_owned_parked_tx(&mut tx, op).await?
                } else { Vec::new() };
                tx.commit().await?;
                for event in events { ctx.events.emit_envelope(event); }
                if let ParkedCompletion::Completed(result)=completion {ctx.completion.complete(result);}
                Ok(())
            }
            ParkedRecovery::LeaveParked=>Ok(()),
            ParkedRecovery::Fail{reason}=>Err(CalmError::Conflict(format!("owned resource remains unresolved: {reason}"))),
        }
    }.await;
    // A stale observer can neither complete nor release a replacement's claim.
    sqlx::query("UPDATE operations SET lease_owner=NULL,lease_until_ms=NULL WHERE id=?1 AND lease_owner=?2 AND phase='parked'")
        .bind(&op.id).bind(owner).execute(&pool).await?;
    result
}
