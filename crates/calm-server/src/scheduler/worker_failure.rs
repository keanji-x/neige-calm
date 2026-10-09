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

/// `status_detail` class of a worker execution the kernel could not get past a provider's
/// startup screen (#1755).
pub const WORKER_STARTUP_BLOCKED: &str = "worker-startup-blocked";

/// The live (`dispatched`/`running`) executions of `card_id` in `track_id`: stamped with the
/// card, or not stamped yet and proven by the worker-spawn op that created the card.
async fn live_executions_of_card_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card_id: &str,
    track_id: &str,
) -> Result<Vec<Task>> {
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM tasks WHERE (worker_card_id = ?1 OR worker_card_id IS NULL) AND track_id = ?2 \
         AND status IN ('dispatched', 'running') ORDER BY id",
    )
    .bind(card_id)
    .bind(track_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut tasks = Vec::new();
    for id in ids {
        let task = task_get_tx(tx, &id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("task {id}")))?;
        if task.worker_card_id.is_none()
            && !crate::db::sqlite::worker_op_targets_card_tx(tx, &task.id, card_id).await?
        {
            continue;
        }
        tasks.push(task);
    }
    Ok(tasks)
}

async fn track_of_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track_id: &str,
) -> Result<Track> {
    track_find_tx(tx, track_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {track_id}")))
}

/// Settle active executions whose owning card is about to be deleted. The caller
/// commits these kernel events together with the card deletion and lease release.
pub(crate) async fn fail_tasks_for_deleted_card_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card: &crate::model::Card,
) -> Result<Vec<(ActorId, EventScope, Event)>> {
    let tasks = live_executions_of_card_tx(tx, card.id.as_str(), card.track_id.as_str()).await?;
    if tasks.is_empty() {
        return Ok(Vec::new());
    }
    let track = track_of_tx(tx, card.track_id.as_str()).await?;
    let mut events = Vec::new();
    for task in tasks {
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

/// #1755: the kernel could not get the worker on `card_id` past its provider's startup screen.
/// Its live execution fails as an owned execution loss ([`WORKER_STARTUP_BLOCKED`]: `reason`) and
/// the worker is marked for the sweep's reap, as a liveness timeout is. Empty when none of the
/// card's executions is live any more.
pub(crate) async fn fail_worker_startup_blocked_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card_id: &str,
    track_id: &str,
    reason: &str,
) -> Result<Vec<(ActorId, EventScope, Event)>> {
    let tasks = live_executions_of_card_tx(tx, card_id, track_id).await?;
    if tasks.is_empty() {
        return Ok(Vec::new());
    }
    let track = track_of_tx(tx, track_id).await?;
    let mut events = Vec::new();
    for task in tasks {
        events
            .extend(fail_worker_task_tx(tx, &task, &track, WORKER_STARTUP_BLOCKED, reason).await?);
        let mark = super::mark_running_timeout_cleanup_tx(
            tx,
            card_id,
            &task.id,
            now_ms(),
            super::WorkerCleanupReason::StartupBlocked,
        )
        .await?;
        events.extend(mark.released);
    }
    Ok(events)
}
