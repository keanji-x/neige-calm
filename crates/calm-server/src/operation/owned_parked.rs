//! Existing parked saga completion with an adapter-owned physical resource.
use super::{
    Operation, ParkedCompletion, ParkedRecovery, ProviderAdapter, RecoveryMode, SpawnCtx,
    complete_parked_tx,
};
use crate::db::sqlite::begin_immediate_tx;
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, EventScope, SYNC_EVENT_VERSION};
use crate::ids::ActorId;

/// The owner supplies every event fact; generic completion infers neither actor nor scope.
#[derive(Clone, Debug)]
pub struct OwnerEvent {
    pub actor: ActorId,
    pub scope: EventScope,
    pub event: Event,
}

pub(crate) async fn append_owner_event_tx(
    tx: &mut super::Tx<'_>,
    owner: OwnerEvent,
) -> Result<BroadcastEnvelope> {
    let OwnerEvent {
        actor,
        scope,
        event,
    } = owner;
    let id =
        crate::db::sqlite::append_decision_event_in_tx(tx, &actor, &scope, None, &event).await?;
    Ok(BroadcastEnvelope {
        id,
        event_version: SYNC_EVENT_VERSION,
        actor,
        scope,
        event,
    })
}

/// Both live observer and the existing boot/sweep loop use this one lease fence.
/// The adapter must retain ownership until it has real stop/quiescence evidence.
// Keep event storage out of callers' recovery futures, including native recovery branches.
pub(crate) fn reconcile<'a>(
    adapter: &'a dyn ProviderAdapter,
    op: &'a Operation,
    mode: RecoveryMode,
    ctx: &'a SpawnCtx,
) -> futures::future::BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        let owner = op
            .lease_owner
            .as_deref()
            .ok_or_else(|| CalmError::Conflict("owned parked recovery has no lease".into()))?;
        let outcome = adapter.recover_owned_parked(op, mode, ctx).await;
        let pool = ctx.operation_repo.sqlite_db();
        let result = async {
            match outcome? {
                ParkedRecovery::Complete(complete) => {
                    let (outcome, owner_event) = *complete;
                    let mut tx = begin_immediate_tx(&pool).await?;
                    super::repo_sqlite::require_live_claim_tx(&mut tx, op).await?;
                    let completion = complete_parked_tx(&mut tx, &op.id, &outcome).await?;
                    let mut events = if matches!(completion, ParkedCompletion::Completed(_)) {
                        adapter.complete_owned_parked_tx(&mut tx, op).await?
                    } else {
                        Vec::new()
                    };
                    if matches!(completion, ParkedCompletion::Completed(_)) {
                        let owner_event = match (&outcome, owner_event) {
                            (super::ParkedOutcome::Failed { last_error, .. }, None) => {
                                adapter.failure_event_tx(&mut tx, op, last_error).await?
                            }
                            (_, event) => event,
                        };
                        if let Some(event) = owner_event {
                            events.push(append_owner_event_tx(&mut tx, event).await?);
                        }
                    }
                    tx.commit().await?;
                    for event in events {
                        ctx.events.emit_envelope(event);
                    }
                    if let ParkedCompletion::Completed(result) = completion {
                        adapter.owned_parked_completed(op, ctx);
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
    })
}

#[cfg(test)]
mod tests;
