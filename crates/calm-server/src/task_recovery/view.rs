//! Gate-free execution summaries and current recovery capability.

use super::admission;
use crate::db::sqlite::{task_attempt_current_tx, task_attempt_get_tx, task_get_tx};
use crate::db::{RepoEventWrite, write_in_tx_typed};
use crate::error::{CalmError, Result};
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, TrackId};
use crate::model::{Task, TaskStatus};
use calm_types::task_recovery::{
    TaskAttemptAllocation, TaskAttemptOrigin, TaskAttemptView, TaskRecoveryCapability,
    TaskRecoveryView,
};

fn task_attempt_view(
    allocation: &TaskAttemptAllocation,
    task: Option<&Task>,
) -> Result<TaskAttemptView> {
    let status = match task {
        Some(task) => serde_json::to_value(task.status)?
            .as_str()
            .ok_or_else(|| CalmError::Internal("task status must serialize as a string".into()))?
            .to_string(),
        None => "awaiting_projection".into(),
    };
    Ok(TaskAttemptView {
        attempt_id: allocation.attempt_id.clone(),
        generation: allocation.generation,
        status,
        status_detail: task.and_then(|task| task.status_detail.clone()),
        worker_card_id: task.and_then(|task| task.worker_card_id.clone()),
        created_at_ms: allocation.created_at_ms,
        finished_at_ms: task.and_then(|task| task.finished_at_ms),
    })
}

pub async fn task_recovery_view(
    repo: &dyn RepoEventWrite,
    track_id: &str,
    key: &str,
    actor: ActorId,
) -> Result<TaskRecoveryView> {
    let track_id = TrackId::from(track_id);
    let key = key.to_string();
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move { task_recovery_view_tx(tx, &track_id, &key, &actor).await })
    })
    .await
}

pub(crate) async fn task_recovery_view_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track_id: &TrackId,
    key: &str,
    actor: &ActorId,
) -> Result<TaskRecoveryView> {
    let track = crate::track_lifecycle::track_get_tx(tx, track_id).await?;
    let event = Event::PlanUpdated {
        track_id: track.id.clone(),
        changed_keys: vec![key.to_string()],
        agent_message: None,
    };
    let scope = EventScope::Track {
        track: track.id.clone(),
        area: track.area_id.clone(),
    };
    admission::authorize_tx(tx, actor, &scope, &event).await?;
    let mut allocation = task_attempt_current_tx(tx, track_id.as_str(), key)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("task {key}")))?;
    let mut allocations = Vec::new();
    loop {
        let previous = match &allocation.origin {
            TaskAttemptOrigin::Initial => None,
            TaskAttemptOrigin::Recovery {
                previous_attempt_id,
                ..
            } => {
                let previous = task_attempt_get_tx(tx, previous_attempt_id)
                    .await?
                    .ok_or_else(|| {
                        CalmError::Internal("execution history predecessor is missing".into())
                    })?;
                if previous.track_id != track_id.as_str()
                    || previous.key != key
                    || previous.generation != allocation.generation - 1
                {
                    return Err(CalmError::Internal(
                        "execution history predecessor is inconsistent".into(),
                    ));
                }
                Some(previous)
            }
        };
        allocations.push(allocation);
        let Some(previous) = previous else {
            break;
        };
        allocation = previous;
    }
    allocations.reverse();
    let current = allocations
        .last()
        .ok_or_else(|| CalmError::NotFound(format!("task {key}")))?;
    let current_task = task_get_tx(tx, &current.attempt_id).await?;
    let recovery = match &current_task {
        Some(task) if task.status == TaskStatus::Failed => {
            match admission::admit_recovery_tx(tx, &track, task, current.generation, actor, true)
                .await
            {
                Ok(_) => TaskRecoveryCapability {
                    allowed: true,
                    code: "available".into(),
                    reason:
                        "Retry the preparation failure as a new execution under its unchanged contract."
                            .into(),
                },
                Err(error @ (CalmError::Forbidden(_) | CalmError::Conflict(_))) => {
                    let reason = match error {
                        CalmError::Forbidden(reason) | CalmError::Conflict(reason) => reason,
                        _ => unreachable!(),
                    };
                    TaskRecoveryCapability {
                        allowed: false,
                        code: capability_code(&reason).into(),
                        reason,
                    }
                }
                Err(error) => return Err(error),
            }
        }
        _ => TaskRecoveryCapability {
            allowed: false,
            code: "not_failed".into(),
            reason: "Only a failed current execution can be recovered.".into(),
        },
    };
    let current = task_attempt_view(current, current_task.as_ref())?;
    let mut attempts = Vec::with_capacity(allocations.len());
    for allocation in allocations {
        let task = task_get_tx(tx, &allocation.attempt_id).await?;
        if task.is_none() && allocation.attempt_id != current.attempt_id {
            return Err(CalmError::Internal(
                "historical execution row is missing".into(),
            ));
        }
        attempts.push(task_attempt_view(&allocation, task.as_ref())?);
    }
    Ok(TaskRecoveryView {
        key: key.to_string(),
        current,
        attempts,
        recovery,
    })
}

fn capability_code(reason: &str) -> &'static str {
    if reason.contains("limit reached") {
        "recovery_limit_reached"
    } else if reason.contains("explicit User recovery") {
        "user_authorization_required"
    } else if reason.contains("track")
        && (reason.contains("blocked") || reason.contains("continue work"))
    {
        "track_not_ready"
    } else if reason.contains("child-task") {
        "unsupported_spawn"
    } else if reason.contains("predecessor") {
        "predecessor_not_quiescent"
    } else if reason.contains("withdrawn") || reason.contains("not ready") {
        "declaration_withdrawn"
    } else if reason.contains("no frozen")
        || reason.contains("incomplete frozen")
        || reason.contains("malformed frozen")
    {
        "missing_frozen_contract"
    } else {
        "contract_changed"
    }
}
