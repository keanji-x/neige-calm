//! Admission checks are read-only and run under the writer transaction.

use crate::db::sqlite::{task_attempt_current_tx, task_attempt_get_tx, task_get_tx};
use crate::error::{CalmError, Result};
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, TrackId};
use crate::model::{Task, TaskStatus, Track, TrackLifecycle};
use calm_types::report_blocks::tasks::{PLANNER_DECLARATION_AUTHOR, TaskDeclaration};
use calm_types::task_recovery::{TASK_IN_TRACK_ROUTE, TaskAttemptOrigin, TaskRecoveryConstraint};
use sqlx::{Sqlite, Transaction};

type Tx<'a> = Transaction<'a, Sqlite>;

pub(super) async fn authorize_tx(
    tx: &mut Tx<'_>,
    actor: &ActorId,
    scope: &EventScope,
    event: &Event,
) -> Result<()> {
    if !matches!(
        actor,
        ActorId::User | ActorId::AiPlanner(_) | ActorId::AiPlannerSession(_)
    ) {
        return Err(CalmError::Forbidden(
            "task recovery requires a User or Planner".into(),
        ));
    }
    calm_truth::decision_gate::enforce_role_resolving_session_from_tx(tx, actor, event, scope)
        .await
        .map_err(|error| CalmError::Forbidden(error.to_string()))
}

fn conflict(reason: impl Into<String>) -> CalmError {
    CalmError::Conflict(reason.into())
}

pub(super) async fn recovery_policy(
    tx: &mut Tx<'_>,
    track: &Track,
    task: &Task,
    generation: i64,
    actor: &ActorId,
    resume_blocked: bool,
) -> Result<()> {
    let explicit_resume = track.lifecycle == TrackLifecycle::Blocked
        && resume_blocked
        && crate::track_lifecycle::validate_transition(
            track.lifecycle,
            TrackLifecycle::Working,
            actor,
        )
        .is_ok();
    if !crate::scheduler::lifecycle_allows_scheduling(track.lifecycle) && !explicit_resume {
        return Err(conflict(
            "track is blocked or terminal; explicitly request working to resolve a blocker, or separately reopen a terminal track",
        ));
    }
    if task.spawn != TASK_IN_TRACK_ROUTE {
        return Err(conflict("child-task recovery is not supported"));
    }
    if !matches!(actor, ActorId::User) {
        if task.declared_by != PLANNER_DECLARATION_AUTHOR
            || !automation_policy_tx(tx, track)
                .await?
                .as_deref()
                .is_none_or(|policy| policy == "auto-declare")
        {
            return Err(CalmError::Forbidden("Planner recovery requires a Planner declaration under auto-declare; user-owned and declare-and-wait tasks require an explicit User recovery".into()));
        }
        if generation >= 2 {
            return Err(CalmError::Forbidden(
                "Planner recovery limit reached; an explicit User recovery is required".into(),
            ));
        }
    }
    Ok(())
}

pub(super) async fn admit_recovery_tx(
    tx: &mut Tx<'_>,
    track: &Track,
    previous: &Task,
    generation: i64,
    actor: &ActorId,
    resume_blocked: bool,
) -> Result<TaskRecoveryConstraint> {
    recovery_policy(tx, track, previous, generation, actor, resume_blocked).await?;
    let constraint = claim_constraint_tx(tx, previous).await?;
    constraint.validate(&previous.track_id).map_err(conflict)?;
    check_constraint_tx(tx, track, &previous.key, &constraint).await?;
    require_recoverable_predecessor_tx(tx, previous).await?;
    Ok(constraint)
}

/// Shared exact claim-source validation; isolated first starts also require it.
pub(crate) async fn validate_isolated_start_tx(tx: &mut Tx<'_>, task: &Task) -> Result<()> {
    require_attempt_startable_tx(tx, &task.id).await?;
    let track = crate::track_lifecycle::track_get_tx(tx, &task.track_id.clone().into()).await?;
    if task.status != TaskStatus::Dispatched
        || task.context_stale_at_ms.is_some()
        || !crate::scheduler::lifecycle_allows_scheduling(track.lifecycle)
    {
        return Err(conflict(
            "isolated task is not authorized for a first start",
        ));
    }
    let constraint = claim_constraint_tx(tx, task).await?;
    check_constraint_tx(tx, &track, &task.key, &constraint).await
}

async fn claim_constraint_tx(tx: &mut Tx<'_>, task: &Task) -> Result<TaskRecoveryConstraint> {
    let (json, truncated): (Option<String>, i64) = sqlx::query_as(
        "SELECT claim_context_json, context_closure_truncated FROM tasks WHERE id=?1",
    )
    .bind(&task.id)
    .fetch_one(&mut **tx)
    .await?;
    if truncated != 0 {
        return Err(conflict(
            "failed execution has incomplete frozen context; same-contract recovery is unavailable",
        ));
    }
    let refs = serde_json::from_str(json.as_deref().ok_or_else(|| {
        conflict("failed execution has no frozen contract; same-contract recovery is unavailable")
    })?)
    .map_err(|_| conflict("failed execution has malformed frozen context"))?;
    let constraint = TaskRecoveryConstraint::V1 {
        refs,
        spawn: task.spawn.clone(),
        declared_by: task.declared_by.clone(),
    };
    Ok(constraint)
}

async fn automation_policy_tx(tx: &mut Tx<'_>, track: &Track) -> Result<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT automation_policy FROM tracks WHERE id=?1")
            .bind(track.id.as_str())
            .fetch_one(&mut **tx)
            .await?,
    )
}

async fn declaration_tx(tx: &mut Tx<'_>, track: &Track, key: &str) -> Result<TaskDeclaration> {
    let (_, blocks) = crate::track_report::report_blocks_snapshot_tx(tx, track.id.as_str()).await?;
    let (declarations, diagnostics) =
        calm_types::report_blocks::tasks::project_task_declarations(&blocks);
    let mut matching = declarations.into_iter().filter(|decl| decl.key == key);
    let declaration = matching
        .next()
        .ok_or_else(|| conflict("task declaration was withdrawn or is invalid"))?;
    if matching.next().is_some() || declaration.tombstone || !declaration.ready {
        return Err(conflict(
            "task declaration is duplicate, withdrawn, or not ready",
        ));
    }
    if declaration
        .block_index
        .and_then(|i| diagnostics.get(i))
        .is_some_and(|diags| !diags.is_empty())
    {
        return Err(conflict("task declaration has validation errors"));
    }
    if declaration.declared_by == PLANNER_DECLARATION_AUTHOR
        && automation_policy_tx(tx, track).await?.as_deref() == Some("declare-and-wait")
        && !declaration.released_by_user
    {
        return Err(conflict("task execution release was withdrawn"));
    }
    Ok(declaration)
}

async fn check_constraint_tx(
    tx: &mut Tx<'_>,
    track: &Track,
    key: &str,
    constraint: &TaskRecoveryConstraint,
) -> Result<()> {
    constraint.validate(track.id.as_str()).map_err(conflict)?;
    let declaration = declaration_tx(tx, track, key).await?;
    let TaskRecoveryConstraint::V1 {
        refs,
        spawn,
        declared_by,
    } = constraint;
    if &declaration.spawn != spawn || &declaration.declared_by != declared_by {
        return Err(conflict("recovery contract route or author changed"));
    }
    for frozen in refs {
        let target = crate::track_lifecycle::track_get_tx(tx, &frozen.track_id)
            .await
            .map_err(|error| match error {
                CalmError::NotFound(_) => conflict(format!(
                    "recovery frozen context track is missing: {}",
                    frozen.track_id
                )),
                other => other,
            })?;
        if target.area_id != track.area_id {
            let kind: String = sqlx::query_scalar("SELECT kind FROM areas WHERE id=?1")
                .bind(target.area_id.as_str())
                .fetch_one(&mut **tx)
                .await?;
            if kind != "system" {
                return Err(conflict(
                    "recovery context moved outside its authorized area",
                ));
            }
        }
        let report_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM cards WHERE track_id=?1 AND kind='track-report')",
        )
        .bind(frozen.track_id.as_str())
        .fetch_one(&mut **tx)
        .await?;
        if !report_exists {
            return Err(conflict(format!(
                "recovery frozen context report is missing: {}",
                frozen.track_id
            )));
        }
        let (_, blocks) =
            crate::track_report::report_blocks_snapshot_tx(tx, frozen.track_id.as_str()).await?;
        let block = blocks
            .iter()
            .find(|block| block.id == frozen.block_id)
            .ok_or_else(|| conflict("recovery frozen context block is missing"))?;
        if frozen.is_root && declaration.block_id != frozen.block_id {
            return Err(conflict("recovery declaration identity changed"));
        }
        let current =
            crate::task_context::context_ref(frozen.track_id.as_str(), block, frozen.is_root);
        if current.hash != frozen.hash {
            return Err(conflict(
                "recovery contract changed; same-contract recovery is unavailable",
            ));
        }
    }
    Ok(())
}

/// Prepared isolated executions require their retained namespace stop proof.
/// Legacy workers retain the pre-preparation fence: a PTY leader exit, terminal
/// session state or released lease does not prove descendants stopped writing.
async fn require_recoverable_predecessor_tx(tx: &mut Tx<'_>, task: &Task) -> Result<()> {
    // Keyed Operation rows are permanent (migration 0093). Prepared targets and
    // tx_output remain evidence even if worker cards/sessions were later deleted.
    // Read every operation sharing the execution key, including a foreign kind
    // that could have caused a scheduler payload collision; do not infer no work
    // merely because the expected worker adapter cannot be found.
    let operations: Vec<PredecessorOperation> = sqlx::query_as(
        "SELECT id,kind,phase,phase_detail_json,target_type,target_id,tx_output_json,spawn_artifacts_json,compensation_state \
         FROM operations WHERE idempotency_key=?1 \
         OR (kind='task-verify' AND json_extract(payload_json,'$.task_id')=?1)",
    ).bind(&task.id).fetch_all(&mut **tx).await?;
    if operations.iter().any(|operation| {
        operation.kind == crate::isolated_codex::OPERATION_KIND
            && operation.tx_output_json.is_some()
    }) {
        if operations.len() != 1
            || task.gate_attempt != 0
            || task.gate_pid.is_some()
            || task.gate_result_json.is_some()
        {
            return Err(conflict(
                "predecessor isolated execution has ambiguous operations or verification effects",
            ));
        }
        return crate::isolated_codex::recovery::require_stopped_tx(tx, task, &operations[0].id)
            .await;
    }

    if task.worker_card_id.is_some()
        || task.gate_attempt != 0
        || task.gate_pid.is_some()
        || task.gate_result_json.is_some()
    {
        return Err(conflict(PREDECESSOR_WRITE_FENCE_UNAVAILABLE));
    }
    if task
        .status_detail
        .as_deref()
        .map(crate::db::sqlite::status_detail_class)
        != Some("spawn-failed")
    {
        return Err(conflict(PREDECESSOR_WRITE_FENCE_UNAVAILABLE));
    }
    for operation in operations {
        let worker_kind = matches!(
            operation.kind.as_str(),
            "codex-worker"
                | "claude-worker"
                | "terminal-worker"
                | crate::isolated_codex::OPERATION_KIND
        );
        let failed_before_prepare = operation.phase == "failed"
            && operation
                .phase_detail_json
                .as_deref()
                .and_then(|detail| serde_json::from_str::<serde_json::Value>(detail).ok())
                .is_some_and(|detail| {
                    detail.get("from_phase").and_then(serde_json::Value::as_str) == Some("pending")
                });
        // Before preparation the payload only identifies its parent Track.
        // prepare_tx_and_advance atomically replaces that with the worker card
        // target and tx_output; a missing card row must not erase this evidence.
        let initial_target = operation.target_type == "track"
            && operation.target_id.as_deref() == Some(task.track_id.as_str());
        if !worker_kind
            || !(operation.phase == "pending" || failed_before_prepare)
            || !initial_target
            || operation.tx_output_json.is_some()
            || operation.spawn_artifacts_json.is_some()
            || operation.compensation_state.is_some()
        {
            return Err(conflict(
                "predecessor operation has uncertain external effects; recovery currently requires a failure before worker preparation",
            ));
        }
    }
    // A still-pending predecessor Operation cannot race into a new process:
    // prepare rechecks the failed terminal row/current generation under this
    // same serialized writer boundary and refuses obsolete attempts.
    Ok(())
}

const PREDECESSOR_WRITE_FENCE_UNAVAILABLE: &str = "predecessor has no supported descendant write fence; leader exit and session completion do not prove all writes stopped. Recovery is currently limited to failures before worker preparation";

#[derive(sqlx::FromRow)]
struct PredecessorOperation {
    id: String,
    kind: String,
    phase: String,
    phase_detail_json: Option<String>,
    target_type: String,
    target_id: Option<String>,
    tx_output_json: Option<String>,
    spawn_artifacts_json: Option<String>,
    compensation_state: Option<String>,
}

/// Recovered pending rows may be deleted/rebuilt. Their original constraint and
/// admitted actor remain authoritative through claim and Operation preparation.
pub(crate) async fn check_recovery_attempt_tx(tx: &mut Tx<'_>, task_id: &str) -> Result<()> {
    let allocation = task_attempt_get_tx(tx, task_id)
        .await?
        .ok_or_else(|| conflict("execution allocation is missing"))?;
    let TaskAttemptOrigin::Recovery {
        previous_attempt_id,
        actor,
        constraint,
        ..
    } = allocation.origin
    else {
        return Ok(());
    };
    let track =
        crate::track_lifecycle::track_get_tx(tx, &TrackId::from(allocation.track_id.clone()))
            .await?;
    let previous = task_get_tx(tx, &previous_attempt_id)
        .await?
        .ok_or_else(|| conflict("recovery predecessor is missing"))?;
    // Allocation + its scoped decision event is the accepted delegation.
    // The admitting actor remains immutable provenance; retiring its session or
    // card does not withdraw work already accepted by the kernel. New commands
    // still pass authorize_tx with their live identity at the service boundary.
    if !matches!(
        actor,
        ActorId::User | ActorId::AiPlanner(_) | ActorId::AiPlannerSession(_)
    ) {
        return Err(conflict(
            "accepted recovery has unsupported authority provenance",
        ));
    }
    if !crate::scheduler::lifecycle_allows_scheduling(track.lifecycle) {
        return Err(conflict(
            "track is paused or terminal; resume its work before this recovery can start",
        ));
    }
    // Current author/ready/release/policy and frozen contract are the withdrawal
    // fences. declare-and-wait may be satisfied by an explicit current release;
    // the initial Planner retry limit was consumed at allocation admission.
    check_constraint_tx(tx, &track, &allocation.key, &constraint).await?;
    require_recoverable_predecessor_tx(tx, &previous).await
}

pub(crate) async fn require_attempt_startable_tx(tx: &mut Tx<'_>, task_id: &str) -> Result<()> {
    let task = task_get_tx(tx, task_id)
        .await?
        .ok_or_else(|| conflict("execution no longer exists"))?;
    let current = task_attempt_current_tx(tx, &task.track_id, &task.key)
        .await?
        .ok_or_else(|| conflict("current execution allocation is missing"))?;
    if current.attempt_id != task_id
        || !matches!(
            task.status,
            TaskStatus::Dispatched | TaskStatus::Running | TaskStatus::Verifying
        )
    {
        return Err(conflict(
            "execution is obsolete or terminal; refusing new side effects",
        ));
    }
    check_recovery_attempt_tx(tx, task_id).await
}
