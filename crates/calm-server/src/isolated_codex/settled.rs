//! A durable wake after failed execution cleanup, independent of live session rows.
use crate::db::{RepoEventWrite, write_in_tx_typed};
use crate::error::Result;
use crate::event::{BroadcastEnvelope, Event, EventScope};
use crate::ids::{ActorId, TrackId};
use crate::model::TaskStatus;
use crate::operation::{Operation, Tx};

/// Called only after the owned parked completion CAS, in that same transaction.
pub(super) async fn record_tx(tx: &mut Tx<'_>, op: &Operation) -> Result<Vec<BroadcastEnvelope>> {
    let record = super::journal::load_tx(tx, &op.id).await?;
    let Some(task) =
        crate::db::sqlite::task_get_tx(tx, &record.request.identity.attempt_id).await?
    else {
        return Ok(Vec::new());
    };
    if task.status != TaskStatus::Failed {
        return Ok(Vec::new());
    }
    let current = crate::db::sqlite::task_attempt_current_tx(tx, &task.track_id, &task.key).await?;
    if current.is_none_or(|allocation| allocation.attempt_id != task.id) {
        return Ok(Vec::new());
    }
    super::recovery::require_stopped_tx(tx, &task, &op.id).await?;
    let track = crate::track_lifecycle::track_get_tx(tx, &task.track_id.clone().into()).await?;
    let actor = ActorId::KernelDispatcher;
    let scope = EventScope::Track {
        track: track.id,
        area: track.area_id,
    };
    let event = Event::TaskExecutionSettled {
        task_id: task.id,
        operation_id: op.id.clone(),
    };
    let id =
        crate::db::sqlite::append_decision_event_in_tx(tx, &actor, &scope, None, &event).await?;
    Ok(vec![BroadcastEnvelope {
        id,
        event_version: crate::event::SYNC_EVENT_VERSION,
        actor,
        scope,
        event,
    }])
}

/// Shared by live delivery and boot replay. This hint is useful when a User
/// could request recovery, even if the Planner needs that User's authorization.
/// Never turn an obsolete/withdrawn execution into a claim of current readiness.
pub(crate) async fn relevant(
    repo: &dyn RepoEventWrite,
    track_id: &TrackId,
    task_id: &str,
    operation_id: &str,
) -> Result<bool> {
    let track_id = track_id.clone();
    let task_id = task_id.to_string();
    let operation_id = operation_id.to_string();
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let Some(task) = crate::db::sqlite::task_get_tx(tx, &task_id).await? else {
                return Ok(false);
            };
            if task.track_id != track_id.as_str() || task.status != TaskStatus::Failed {
                return Ok(false);
            }
            let Some(current) =
                crate::db::sqlite::task_attempt_current_tx(tx, track_id.as_str(), &task.key)
                    .await?
            else {
                return Ok(false);
            };
            if current.attempt_id != task_id {
                return Ok(false);
            }
            // Verify the event's Operation identity, not merely some stopped run.
            if let Err(error) = super::recovery::require_stopped_tx(tx, &task, &operation_id).await
            {
                return match error {
                    crate::error::CalmError::Conflict(_) => Ok(false),
                    error => Err(error),
                };
            }
            let view = crate::task_recovery::task_recovery_view_tx(
                tx,
                &track_id,
                &task.key,
                &ActorId::User,
                crate::scheduler::DEFAULT_TRACK_TASK_BUDGET,
            )
            .await?;
            Ok(view.recovery.allowed)
        })
    })
    .await
}
