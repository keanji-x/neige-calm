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
        blocking_reason: None,
    })
}

pub async fn task_recovery_view(
    repo: &dyn RepoEventWrite,
    track_id: &str,
    key: &str,
    actor: ActorId,
    task_budget_default: i64,
) -> Result<TaskRecoveryView> {
    let track_id = TrackId::from(track_id);
    let key = key.to_string();
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            task_recovery_view_tx(tx, &track_id, &key, &actor, task_budget_default).await
        })
    })
    .await
}

pub(crate) async fn task_recovery_view_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track_id: &TrackId,
    key: &str,
    actor: &ActorId,
    task_budget_default: i64,
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
    let Some(mut allocation) = task_attempt_current_tx(tx, track_id.as_str(), key).await? else {
        // A valid authored task can await release/admission before its first row.
        // Resolve existence from the same authoritative source used by projection.
        let (declarations, diagnostics) =
            crate::track_report::task_projection_source_tx(tx, track_id.as_str())
                .await?
                .ok_or_else(|| CalmError::NotFound(format!("task {key}")))?;
        let matching: Vec<_> = declarations
            .iter()
            .filter(|declaration| declaration.key == key && !declaration.tombstone)
            .collect();
        let declaration = match matching.as_slice() {
            [] => return Err(CalmError::NotFound(format!("task {key}"))),
            [declaration] => declaration,
            _ => {
                return Err(CalmError::Conflict(
                    "task declaration key is ambiguous".into(),
                ));
            }
        };
        if declaration
            .block_index
            .and_then(|index| diagnostics.get(index))
            .is_none_or(|diagnostics| !diagnostics.is_empty())
        {
            return Err(CalmError::Conflict("task declaration is invalid".into()));
        }
        return Ok(TaskRecoveryView {
            key: key.to_string(),
            current: None,
            attempts: Vec::new(),
            recovery: TaskRecoveryCapability {
                allowed: false,
                code: "not_started".into(),
                reason: "No execution has been allocated for this task.".into(),
            },
        });
    };
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
                    reason: if matches!(
                        crate::file_delivery::selection(task)?,
                        Some(
                            calm_types::task_execution::FileDelivery::CandidateConsumer { .. }
                                | calm_types::task_execution::FileDelivery::CandidateReviewer { .. }
                        )
                    ) {
                        "Retry this goal in a new workspace with the original immutable candidate file-set input binding, verification identity and original review/decision evidence when required. Previous Worker-created files are not inherited. Failed candidate outputs remain unsupported as recovery inputs.".into()
                    } else if matches!(
                        crate::file_delivery::selection(task)?,
                        Some(calm_types::task_execution::FileDelivery::Consumer { .. })
                    ) {
                        "Retry this goal in a new workspace with the original immutable JSON input binding. Previous Worker-created files are not inherited.".into()
                    } else if crate::isolated_codex::selected(task)? {
                        "Retry this goal as a new execution in a new empty workspace. Previous results stay with the old attempt.".into()
                    } else {
                        "Retry the preparation failure as a new execution under its unchanged contract.".into()
                    },
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
    let blocking_reason = current_blocking_reason_tx(
        tx,
        &track,
        current,
        current_task.as_ref(),
        task_budget_default,
    )
    .await?;
    let mut current = task_attempt_view(current, current_task.as_ref())?;
    current.blocking_reason = blocking_reason;
    let mut attempts = Vec::with_capacity(allocations.len());
    for allocation in allocations {
        let task = task_get_tx(tx, &allocation.attempt_id).await?;
        if task.is_none() && allocation.attempt_id != current.attempt_id {
            return Err(CalmError::Internal(
                "historical execution row is missing".into(),
            ));
        }
        let mut entry = task_attempt_view(&allocation, task.as_ref())?;
        if entry.attempt_id == current.attempt_id {
            entry.blocking_reason.clone_from(&current.blocking_reason);
        }
        attempts.push(entry);
    }
    Ok(TaskRecoveryView {
        key: key.to_string(),
        current: Some(current),
        attempts,
        recovery,
    })
}

pub(crate) async fn current_blocking_reason_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track: &crate::model::Track,
    allocation: &TaskAttemptAllocation,
    task: Option<&Task>,
    task_budget_default: i64,
) -> Result<Option<String>> {
    if task.is_some_and(|task| task.status != TaskStatus::Pending) {
        return Ok(None);
    }
    if !crate::scheduler::lifecycle_allows_scheduling(track.lifecycle) {
        return Ok(Some(format!(
            "Track is {:?}; resume its work before this task can start",
            track.lifecycle
        )));
    }
    let Some((declarations, diagnostics)) =
        crate::track_report::task_projection_source_tx(tx, track.id.as_str()).await?
    else {
        return Ok(Some(
            "Task report is missing; restore its declaration before execution".into(),
        ));
    };
    let matching: Vec<_> = declarations
        .iter()
        .filter(|declaration| declaration.key == allocation.key)
        .collect();
    if matching.is_empty() {
        return Ok(Some(
            "Task declaration is missing; restore it before execution".into(),
        ));
    }
    if matching.iter().all(|declaration| declaration.tombstone) {
        return Ok(Some("Task declaration was withdrawn".into()));
    }
    if matching
        .iter()
        .all(|declaration| !declaration.ready || declaration.tombstone)
    {
        return Ok(Some(
            "Task declaration is not ready; authorize it before execution".into(),
        ));
    }
    let configured_default: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key=?1")
            .bind(crate::routes::settings::TASK_BUDGET_DEFAULT_KEY)
            .fetch_optional(&mut **tx)
            .await?;
    let task_budget_default = crate::routes::settings::effective_task_budget_default(
        configured_default.as_deref(),
        task_budget_default,
    );
    let verdicts = crate::db::sqlite::evaluate_schedulability_with_task_budget_default(
        tx,
        track.id.as_str(),
        &declarations,
        &diagnostics,
        task_budget_default,
    )
    .await?;
    let verdicts: Vec<_> = verdicts
        .iter()
        .filter(|verdict| verdict.key == allocation.key)
        .collect();
    for verdict in &verdicts {
        if let Some(reason) = &verdict.pending_reason {
            let message = match reason {
                crate::db::sqlite::TaskPendingReason::DependencyBlocked { message, .. }
                | crate::db::sqlite::TaskPendingReason::BudgetQueued { message, .. }
                | crate::db::sqlite::TaskPendingReason::NotAdmitted { message, .. } => message,
            };
            return Ok(Some(message.clone()));
        }
        if let Some(diagnostic) = verdict.diagnostics.first() {
            return Ok(Some(diagnostic.message.clone()));
        }
    }
    if matches!(allocation.origin, TaskAttemptOrigin::Recovery { .. }) {
        match admission::check_recovery_attempt_tx(tx, &allocation.attempt_id).await {
            Ok(()) => {}
            Err(CalmError::Conflict(reason) | CalmError::Forbidden(reason)) => {
                return Ok(Some(reason));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(task
        .is_none()
        .then(|| "Task declaration is eligible; waiting for scheduler projection".into()))
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
