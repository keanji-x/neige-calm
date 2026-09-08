//! Durable wakes after failed execution or completed Reviewer cleanup.
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
    let review = super::review_settled::is_review(&task)?;
    if task.status != TaskStatus::Failed && !review {
        return Ok(Vec::new());
    }
    let current = crate::db::sqlite::task_attempt_current_tx(tx, &task.track_id, &task.key).await?;
    if current.is_none_or(|allocation| allocation.attempt_id != task.id) {
        return Ok(Vec::new());
    }
    if review {
        super::review_settled::outcome_tx(tx, &task, &op.id).await?;
    } else {
        super::recovery::require_stopped_tx(tx, &task, &op.id).await?;
    }
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM events WHERE kind='task.execution_settled' AND json_extract(payload,'$.operation_id')=?1)")
        .bind(&op.id).fetch_one(&mut **tx).await?;
    if exists {
        return Ok(Vec::new());
    }
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

/// Shared by live delivery and boot replay. Failed executions retain their User
/// recovery predicate. Done Reviewers expose terminal outcomes under current input
/// authority without granting recovery. Obsolete/withdrawn hints remain quiet.
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
            if task.track_id != track_id.as_str() {
                return Ok(false);
            }
            if super::review_settled::is_review(&task)? {
                return super::review_settled::relevant_tx(tx, &task, &operation_id).await;
            }
            if task.status != TaskStatus::Failed {
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

/// Live and replay share the same typed review settlement notice.
pub(crate) async fn review_observation(
    repo: &dyn RepoEventWrite,
    track: &TrackId,
    task: &str,
    op: &str,
) -> Result<Option<crate::harness::Observation>> {
    let track = track.clone();
    let task = task.to_owned();
    let op = op.to_owned();
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let Some(task) = crate::db::sqlite::task_get_tx(tx, &task).await? else {
                return Ok(None);
            };
            if task.track_id != track.as_str() || !super::review_settled::is_review(&task)? {
                return Ok(None);
            }
            let briefing = super::review_settled::briefing_tx(tx, &task, &op).await?;
            Ok(Some(crate::harness::Observation::SystemContext {
                text: super::review_settled::render(&briefing)?,
            }))
        })
    })
    .await
}

/// Compensation completes outside owned-parked's callback. Repair that durable
/// terminal-to-notice gap on the existing boot/periodic scheduler sweep, only for
/// Done Reviewers; no ordinary Done wake or new retry authority is introduced.
pub(crate) async fn backfill_reviews(
    repo: &dyn crate::db::Repo,
    bus: &crate::event::EventBus,
) -> Result<()> {
    use crate::operation::{OperationRepo, SqlxOperationRepo};
    let Some(pool) = repo.sqlite_pool() else {
        return Ok(());
    };
    let ids: Vec<String> = sqlx::query_scalar("SELECT o.id FROM operations o JOIN current_tasks t ON t.id=o.idempotency_key WHERE o.kind='codex-isolated-worker' AND o.phase IN ('succeeded','failed') AND t.status='done' AND json_extract(t.context_json,'$.neige_execution.file_delivery.role')='candidate_reviewer' AND NOT EXISTS(SELECT 1 FROM events e WHERE e.kind='task.execution_settled' AND json_extract(e.payload,'$.operation_id')=o.id)").fetch_all(&pool).await?;
    let operations = SqlxOperationRepo::new(pool);
    for id in ids {
        let Some(op) = operations.get_operation(&id).await? else {
            continue;
        };
        let result = write_in_tx_typed(repo, move |tx| {
            Box::pin(async move { record_tx(tx, &op).await })
        })
        .await;
        match result {
            Ok(events) => {
                for event in events {
                    bus.emit_envelope(event);
                }
            }
            Err(error) => {
                tracing::warn!(operation_id=%id,%error,"review settlement backfill remains unresolved")
            }
        }
    }
    Ok(())
}
