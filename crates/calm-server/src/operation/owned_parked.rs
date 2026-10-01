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
    let pool = ctx.operation_repo.sqlite_pool();
    let recovery = adapter.recover_owned_parked(op, mode, ctx);
    tokio::pin!(recovery);
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(20));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let outcome = loop {
        tokio::select! {
            result=&mut recovery=>break result,
            _=heartbeat.tick()=>{
                let now=crate::model::now_ms();
                let renewed=sqlx::query("UPDATE operations SET lease_until_ms=?3 WHERE id=?1 \
                    AND lease_owner=?2 AND phase='parked' AND lease_until_ms>=?4")
                    .bind(&op.id).bind(owner).bind(now+super::OPERATION_LEASE_MS).bind(now)
                    .execute(&pool).await?.rows_affected();
                if renewed!=1 {break Err(CalmError::Conflict("owned resource recovery lost its operation lease".into()));}
            }
        }
    };
    let result = async {
        match outcome? {
            ParkedRecovery::Complete(outcome) => {
                let mut tx = begin_immediate_tx(&pool).await?;
                require_owner_tx(&mut tx, &op.id, owner).await?;
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
    // A stale observer can neither complete nor release a replacement's claim.
    sqlx::query("UPDATE operations SET lease_owner=NULL,lease_until_ms=NULL WHERE id=?1 AND lease_owner=?2 AND phase='parked'")
        .bind(&op.id).bind(owner).execute(&pool).await?;
    result
}

/// The same authority check protects final completion and every new child issuance.
pub(crate) async fn require_owner_tx(tx: &mut super::Tx<'_>, op: &str, owner: &str) -> Result<()> {
    let owned: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM operations WHERE id=?1 \
        AND lease_owner=?2 AND phase='parked' AND lease_until_ms>=?3)",
    )
    .bind(op)
    .bind(owner)
    .bind(crate::model::now_ms())
    .fetch_one(&mut **tx)
    .await?;
    if !owned {
        return Err(CalmError::Conflict(
            "owned execution lost its operation lease".into(),
        ));
    }
    Ok(())
}
