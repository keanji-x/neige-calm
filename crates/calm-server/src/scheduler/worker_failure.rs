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

/// Settle the attempts whose worker card is about to be deleted (#2493): the attempts the card's
/// sessions are bound to, resolved once before anything moves (the failures below would hide a
/// `dispatched`/`running` one from a later read). Each such attempt that is still
/// `dispatched`/`running` is failed, and each bound attempt's lease is released and committed as
/// `interrupted` (whatever its status: a timed-out worker's lease is held until its cleanup reaps
/// it, and that cleanup marker leaves with the card's sessions). The caller commits these kernel
/// events with the card deletion.
pub(crate) async fn settle_attempt_for_deleted_card_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card: &crate::model::Card,
) -> Result<Vec<(ActorId, EventScope, Event)>> {
    let bound = crate::db::sqlite::card_bindings_tx(tx, card.id.as_str()).await?;
    let mut events = Vec::new();
    for binding in bound {
        let Some(attempt_id) = binding.attempt_id else {
            continue;
        };
        if matches!(
            binding.attempt_status,
            Some(TaskStatus::Dispatched | TaskStatus::Running)
        ) {
            let task = task_get_tx(tx, &attempt_id)
                .await?
                .ok_or_else(|| CalmError::NotFound(format!("task {attempt_id}")))?;
            let track = track_find_tx(tx, task.track_id.as_str())
                .await?
                .ok_or_else(|| CalmError::NotFound(format!("track {}", task.track_id)))?;
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
        events.extend(
            release_workspace_lease_for_attempt_tx(
                tx,
                &attempt_id,
                ReleaseDelivery::Commit(
                    crate::git_candidate::delivery::AttemptOutcome::Interrupted,
                ),
            )
            .await?,
        );
    }
    Ok(events)
}
