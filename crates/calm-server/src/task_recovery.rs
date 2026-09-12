//! Explicit, bounded continuation of a failed execution under the same report contract.
//! This is an execution foundation; it does not claim to preserve candidate files.

mod admission;
mod view;
pub(crate) use admission::{
    check_recovery_attempt_tx, require_attempt_startable_tx, validate_frozen_contract_tx,
    validate_isolated_start_tx,
};
pub use calm_types::task_recovery::{TaskAttemptView, TaskRecoveryCapability, TaskRecoveryView};
pub(crate) use view::current_blocking_reason_tx;
pub use view::task_recovery_view;
pub(crate) use view::task_recovery_view_tx;

use crate::db::sqlite::{
    task_attempt_current_tx, task_get_tx, task_recovery_allocate_tx, task_recovery_lookup_tx,
};
use crate::db::{RepoEventWrite, write_with_actor_events_typed};
use crate::error::{CalmError, Result};
use crate::event::{Event, EventBus, EventScope};
use crate::ids::{ActorId, TrackId};
use crate::model::{Task, TaskKind, TaskStatus, TrackLifecycle};
use crate::state::WriteContext;
use calm_types::task_recovery::{TaskRecoveryReceipt, TaskRecoveryRequest};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

const REPLAY: &str = "task recovery: returning previously committed receipt";

pub struct RecoveryContext<'a> {
    pub repo: &'a dyn RepoEventWrite,
    pub events: &'a EventBus,
    pub write: &'a WriteContext,
}

/// What a Planner may expect from the executor a recovered attempt runs on.
pub(crate) struct ExecutorStatement {
    pub environment: Value,
    pub recover_changes: &'static str,
}

const LEGACY_ENVIRONMENT_NOTE: &str =
    "recovery re-runs on the same executor as the failed attempt; its environment is unchanged";
const LEGACY_RECOVER_CHANGES: &str = "Recovery re-runs on the same executor as the failed attempt with its unchanged environment and capabilities. It cannot resolve a failure caused by a missing capability; change the task's goal or inputs instead.";

/// Executor statement for the attempt `task` describes. The route is decided by
/// `isolated_codex::selected`, the same pure predicate over the frozen task row
/// that `scheduler::build_worker_payload` branches on, so the statement cannot
/// disagree with the adapter that will run the attempt. Only the isolated Codex
/// route carries the fixed envelope; legacy routes name their executor and
/// promise nothing about workspace freshness, network or tools.
pub(crate) fn executor_statement(task: &Task) -> Result<ExecutorStatement> {
    if crate::isolated_codex::selected(task)? {
        return Ok(ExecutorStatement {
            environment: {
                let context = serde_json::from_str(&task.context_json)?;
                let selection =
                    calm_types::task_execution::IsolatedCodexSelection::from_context(&context)
                        .map_err(CalmError::BadRequest)?
                        .ok_or_else(|| {
                            CalmError::Conflict("isolated recovery selection missing".into())
                        })?;
                crate::dedicated_codex::executor_environment_with_plugins(&selection.plugin_tools)
            },
            recover_changes: crate::dedicated_codex::RECOVER_CHANGES,
        });
    }
    let executor = match task.kind {
        TaskKind::Codex => "shared-codex",
        TaskKind::Claude => "claude",
        TaskKind::Terminal => "terminal",
    };
    Ok(ExecutorStatement {
        environment: json!({"executor": executor, "note": LEGACY_ENVIRONMENT_NOTE}),
        recover_changes: LEGACY_RECOVER_CHANGES,
    })
}

/// Statement for the replacement attempt a receipt names. The replacement row
/// is projected in the recovery transaction; when admission capacity withheld
/// it (`awaiting_projection`), the previous attempt carries the same frozen
/// contract and therefore the same route.
pub(crate) async fn executor_statement_for_receipt(
    repo: &dyn RepoEventWrite,
    receipt: &TaskRecoveryReceipt,
) -> Result<ExecutorStatement> {
    let task = match repo.task_get(&receipt.attempt_id).await? {
        Some(task) => task,
        None => repo
            .task_get(&receipt.previous_attempt_id)
            .await?
            .ok_or_else(|| {
                CalmError::Internal("recovered attempt has no projected execution row".into())
            })?,
    };
    executor_statement(&task)
}

pub async fn recover_failed_task(
    context: RecoveryContext<'_>,
    track_id: &str,
    key: &str,
    request: TaskRecoveryRequest,
    actor: ActorId,
) -> Result<TaskRecoveryReceipt> {
    recover_with_binding(context, track_id, key, request, actor, None).await
}

pub(crate) async fn recover_failed_task_bound(
    context: RecoveryContext<'_>,
    track_id: &str,
    key: &str,
    request: TaskRecoveryRequest,
    actor: ActorId,
    binding: crate::semantic_recovery::BoundTurn,
) -> Result<TaskRecoveryReceipt> {
    recover_with_binding(context, track_id, key, request, actor, Some(binding)).await
}

async fn recover_with_binding(
    context: RecoveryContext<'_>,
    track_id: &str,
    key: &str,
    request: TaskRecoveryRequest,
    actor: ActorId,
    binding: Option<crate::semantic_recovery::BoundTurn>,
) -> Result<TaskRecoveryReceipt> {
    if !calm_types::report_blocks::tasks::key_is_valid(key)
        || request.expected_attempt_id.trim().is_empty()
        || request.idempotency_key.trim().is_empty()
        || request.idempotency_key.len() > 200
        || request.reason.trim().is_empty()
        || request.reason.len() > 4000
    {
        return Err(CalmError::BadRequest("recovery requires a valid key, expected_attempt_id, idempotency_key (1-200 bytes), and reason (1-4000 bytes)".into()));
    }
    let track_id = TrackId::from(track_id);
    let key = key.to_string();
    let fingerprint = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            "task-recovery-v1",
            &track_id,
            &key,
            &request,
            &actor,
        ))?)
    );
    // The eventized transaction intentionally rejects empty batches. A replay
    // rolls its read-only transaction back and returns the authenticated receipt.
    let replay = Arc::new(Mutex::new(None));
    let replay_out = Arc::clone(&replay);
    let result = write_with_actor_events_typed(
        context.repo,
        None,
        context.events,
        context.write,
        move |tx| {
            Box::pin(async move {
                if let Some(binding) = &binding {
                    crate::semantic_recovery::authenticate_tx(tx, binding).await?;
                }
                let track = crate::track_lifecycle::track_get_tx(tx, &track_id).await?;
                let scope = EventScope::Track {
                    track: track.id.clone(),
                    area: track.area_id.clone(),
                };
                let event = Event::PlanUpdated {
                    track_id: track.id.clone(),
                    changed_keys: vec![key.clone()],
                    agent_message: Some(request.reason.clone()),
                };
                admission::authorize_tx(tx, &actor, &scope, &event).await?;
                if let Some(receipt) = task_recovery_lookup_tx(
                    tx,
                    track_id.as_str(),
                    &key,
                    &request.idempotency_key,
                    &fingerprint,
                )
                .await?
                {
                    *replay_out.lock().expect("recovery receipt lock") = Some(receipt);
                    return Err(CalmError::Conflict(REPLAY.into()));
                }
                let allocation = task_attempt_current_tx(tx, track_id.as_str(), &key)
                    .await?
                    .ok_or_else(|| CalmError::NotFound(format!("task {key}")))?;
                if allocation.attempt_id != request.expected_attempt_id {
                    return Err(CalmError::Conflict(
                        "recovery expected attempt is no longer current; refresh task history"
                            .into(),
                    ));
                }
                let previous = task_get_tx(tx, &allocation.attempt_id)
                    .await?
                    .ok_or_else(|| {
                        CalmError::Conflict("current execution is not projected".into())
                    })?;
                if previous.status != TaskStatus::Failed {
                    return Err(CalmError::Conflict(
                        "only a failed execution can be recovered".into(),
                    ));
                }
                let constraint = admission::admit_recovery_tx(
                    tx,
                    &track,
                    &previous,
                    allocation.generation,
                    &actor,
                    true,
                )
                .await?;
                let receipt = task_recovery_allocate_tx(
                    tx,
                    track_id.as_str(),
                    &key,
                    &request,
                    &fingerprint,
                    &constraint,
                    &actor,
                )
                .await?;
                let mut emitted = Vec::new();
                if crate::track_lifecycle::validate_transition(
                    track.lifecycle,
                    TrackLifecycle::Working,
                    &actor,
                )
                .is_ok()
                    && let Some(transitions) =
                        crate::track_lifecycle::apply_requested_transition_in_tx(
                            tx,
                            &track.id,
                            TrackLifecycle::Working,
                            &actor,
                            request.reason,
                        )
                        .await?
                {
                    emitted.extend(
                        transitions
                            .into_iter()
                            .map(|event| (actor.clone(), scope.clone(), event)),
                    );
                }
                let projection =
                    crate::track_report::tasks_rebuild_tx(tx, track_id.as_str()).await?;
                emitted.extend(projection.kernel_events);
                emitted.push((actor, scope, event));
                Ok((receipt, emitted))
            })
        },
    )
    .await;
    match result {
        Ok((receipt, _)) => Ok(receipt),
        Err(CalmError::Conflict(message)) if message == REPLAY => replay
            .lock()
            .expect("recovery receipt lock")
            .take()
            .ok_or_else(|| CalmError::Internal("recovery replay lost its receipt".into())),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
pub(crate) mod launch_test_support;
