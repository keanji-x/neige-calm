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
    let mut events = vec![(
        ActorId::KernelDispatcher,
        scope.clone(),
        Event::TaskFailed {
            idempotency_key: task.id.clone(),
            reason,
            details: None,
            agent_message: None,
        },
    )];
    if let Some(auto_events) = auto_transition_if_current_in_tx(
        tx,
        &track.id,
        TrackLifecycle::Working,
        TrackLifecycle::Reviewing,
        &ActorId::KernelDispatcher,
        Some(
            if preparation {
                "[auto] worker spawn failed"
            } else {
                "[auto] worker execution failed"
            }
            .into(),
        ),
    )
    .await?
    {
        events.extend(
            auto_events
                .into_iter()
                .map(|event| (ActorId::KernelDispatcher, scope.clone(), event)),
        );
    }
    Ok(events)
}
