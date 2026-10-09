//! Canonical Worker failure CAS/events shared by startup and owned execution loss.
use super::*;

pub(crate) async fn fail_worker_task_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    task: &Task,
    track: &Track,
    class: &str,
    reason: &str,
) -> Result<Vec<(ActorId, EventScope, Event)>> {
    let scope = EventScope::Track {
        track: track.id.clone(),
        area: track.area_id.clone(),
    };
    let rows = task_fail_from_worker_tx(
        tx,
        &task.id,
        track.id.as_str(),
        TaskReporter::Kernel,
        &status_detail_with_reason(class, reason),
        now_ms(),
    )
    .await?;
    if rows == 0 {
        return Err(race_lost_err());
    }
    let preparation = class == "spawn-failed";
    let reason = if preparation {
        format!("worker spawn failed: {reason}")
    } else {
        format!("worker execution failed: {reason}")
    };
    Ok(vec![(
        ActorId::KernelDispatcher,
        scope,
        Event::TaskFailed {
            idempotency_key: task.id.clone(),
            reason,
            details: None,
            agent_message: None,
        },
    )])
}

/// Settle active executions whose owning card is about to be deleted. The caller
/// commits these kernel events together with the card deletion and lease release.
pub(crate) async fn fail_tasks_for_deleted_card_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card: &crate::model::Card,
) -> Result<Vec<(ActorId, EventScope, Event)>> {
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM tasks WHERE (worker_card_id = ?1 OR worker_card_id IS NULL) AND track_id = ?2 \
         AND status IN ('dispatched', 'running') ORDER BY id",
    )
    .bind(card.id.as_str())
    .bind(card.track_id.as_str())
    .fetch_all(&mut **tx)
    .await?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let track = track_find_tx(tx, card.track_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {}", card.track_id)))?;
    let mut events = Vec::new();
    for id in ids {
        let task = task_get_tx(tx, &id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("task {id}")))?;
        if task.worker_card_id.is_none()
            && !crate::db::sqlite::worker_op_targets_card_tx(tx, &task.id, card.id.as_str()).await?
        {
            continue;
        }
        events.extend(
            fail_worker_task_tx(
                tx,
                &task,
                &track,
                "worker-card-deleted",
                "the execution card was deleted",
            )
            .await?,
        );
    }
    Ok(events)
}
