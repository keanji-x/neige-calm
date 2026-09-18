//! Admission checks are read-only and run under the writer transaction.
//! Every refusal is a typed [`RecoveryRefusal`]; the read side takes its
//! code from the type and never from the message text.

use super::refusal::{Admission, AdmissionError, RecoveryRefusal, RecoveryRefusalCode};
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
) -> Admission<()> {
    if !matches!(
        actor,
        ActorId::User | ActorId::AiPlanner(_) | ActorId::AiPlannerSession(_)
    ) {
        return Err(RecoveryRefusal::forbidden(
            RecoveryRefusalCode::NotAuthorized,
            "task recovery requires a User or Planner",
        )
        .into());
    }
    calm_truth::decision_gate::enforce_role_resolving_session_from_tx(tx, actor, event, scope)
        .await
        .map_err(|error| {
            RecoveryRefusal::forbidden(RecoveryRefusalCode::NotAuthorized, error.to_string()).into()
        })
}

fn conflict(reason: impl Into<String>) -> CalmError {
    CalmError::Conflict(reason.into())
}

fn refuse(code: RecoveryRefusalCode, reason: impl Into<String>) -> AdmissionError {
    RecoveryRefusal::conflict(code, reason).into()
}

pub(super) async fn recovery_policy(
    tx: &mut Tx<'_>,
    track: &Track,
    task: &Task,
    generation: i64,
    actor: &ActorId,
    resume_blocked: bool,
) -> Admission<()> {
    let explicit_resume = track.lifecycle == TrackLifecycle::Blocked
        && resume_blocked
        && crate::track_lifecycle::validate_transition(
            track.lifecycle,
            TrackLifecycle::Working,
            actor,
        )
        .is_ok();
    if !crate::scheduler::lifecycle_allows_scheduling(track.lifecycle) && !explicit_resume {
        return Err(refuse(
            RecoveryRefusalCode::TrackNotReady,
            "track is blocked or terminal; explicitly request working to resolve a blocker, or separately reopen a terminal track",
        ));
    }
    if task.spawn != TASK_IN_TRACK_ROUTE {
        return Err(refuse(
            RecoveryRefusalCode::UnsupportedSpawn,
            "child-task recovery is not supported",
        ));
    }
    if !matches!(actor, ActorId::User) {
        if task.declared_by != PLANNER_DECLARATION_AUTHOR
            || !automation_policy_tx(tx, track)
                .await?
                .as_deref()
                .is_none_or(|policy| policy == "auto-declare")
        {
            return Err(RecoveryRefusal::forbidden(
                RecoveryRefusalCode::UserAuthorizationRequired,
                "Planner recovery requires a Planner declaration under auto-declare; user-owned and declare-and-wait tasks require an explicit User recovery",
            )
            .into());
        }
        if generation >= 2 {
            return Err(RecoveryRefusal::forbidden(
                RecoveryRefusalCode::RecoveryLimitReached,
                "Planner recovery limit reached; an explicit User recovery is required",
            )
            .into());
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
) -> Admission<TaskRecoveryConstraint> {
    recovery_policy(tx, track, previous, generation, actor, resume_blocked).await?;
    let constraint = claim_constraint_tx(tx, previous).await?;
    constraint
        .validate(&previous.track_id)
        .map_err(|reason| refuse(RecoveryRefusalCode::MissingFrozenContract, reason))?;
    check_constraint_tx(tx, track, &previous.key, &constraint).await?;
    require_recoverable_predecessor_tx(tx, previous).await?;
    // File-delivery input checks refuse with their own Conflict messages; at
    // this boundary they all mean the frozen input contract cannot be honoured.
    crate::file_delivery::require_recovery_input_tx(tx, previous)
        .await
        .map_err(|error| match error {
            CalmError::Conflict(reason) => refuse(RecoveryRefusalCode::ContractChanged, reason),
            other => AdmissionError::Other(other),
        })?;
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
    validate_frozen_contract_tx(tx, task).await
}

/// Read-only contract authority shared by starts and post-execution publication.
/// Callers keep their own lifecycle/current-attempt/status guards.
pub(crate) async fn validate_frozen_contract_tx(tx: &mut Tx<'_>, task: &Task) -> Result<()> {
    let track = crate::track_lifecycle::track_get_tx(tx, &task.track_id.clone().into()).await?;
    if task.context_stale_at_ms.is_some() {
        return Err(conflict("frozen task context is stale"));
    }
    let constraint = claim_constraint_tx(tx, task).await?;
    check_constraint_tx(tx, &track, &task.key, &constraint).await?;
    Box::pin(crate::file_delivery::repair::validate_contract_tx(tx, task)).await?;
    Ok(())
}

async fn claim_constraint_tx(tx: &mut Tx<'_>, task: &Task) -> Admission<TaskRecoveryConstraint> {
    let (json, truncated): (Option<String>, i64) = sqlx::query_as(
        "SELECT claim_context_json, context_closure_truncated FROM tasks WHERE id=?1",
    )
    .bind(&task.id)
    .fetch_one(&mut **tx)
    .await?;
    if truncated != 0 {
        return Err(refuse(
            RecoveryRefusalCode::MissingFrozenContract,
            "failed execution has incomplete frozen context; same-contract recovery is unavailable",
        ));
    }
    let refs = serde_json::from_str(json.as_deref().ok_or_else(|| {
        refuse(
            RecoveryRefusalCode::MissingFrozenContract,
            "failed execution has no frozen contract; same-contract recovery is unavailable",
        )
    })?)
    .map_err(|_| {
        refuse(
            RecoveryRefusalCode::MissingFrozenContract,
            "failed execution has malformed frozen context",
        )
    })?;
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

async fn declaration_tx(tx: &mut Tx<'_>, track: &Track, key: &str) -> Admission<TaskDeclaration> {
    let (_, blocks) = crate::track_report::report_blocks_snapshot_tx(tx, track.id.as_str()).await?;
    let (declarations, diagnostics) =
        calm_types::report_blocks::tasks::project_task_declarations(&blocks);
    let mut matching = declarations.into_iter().filter(|decl| decl.key == key);
    let declaration = matching.next().ok_or_else(|| {
        refuse(
            RecoveryRefusalCode::DeclarationWithdrawn,
            "task declaration was withdrawn or is invalid",
        )
    })?;
    if matching.next().is_some() || declaration.tombstone || !declaration.ready {
        return Err(refuse(
            RecoveryRefusalCode::DeclarationWithdrawn,
            "task declaration is duplicate, withdrawn, or not ready",
        ));
    }
    if declaration
        .block_index
        .and_then(|i| diagnostics.get(i))
        .is_some_and(|diags| !diags.is_empty())
    {
        return Err(refuse(
            RecoveryRefusalCode::DeclarationWithdrawn,
            "task declaration has validation errors",
        ));
    }
    if declaration.declared_by == PLANNER_DECLARATION_AUTHOR
        && automation_policy_tx(tx, track).await?.as_deref() == Some("declare-and-wait")
        && !declaration.released_by_user
    {
        return Err(refuse(
            RecoveryRefusalCode::DeclarationWithdrawn,
            "task execution release was withdrawn",
        ));
    }
    Ok(declaration)
}

pub(super) async fn check_constraint_tx(
    tx: &mut Tx<'_>,
    track: &Track,
    key: &str,
    constraint: &TaskRecoveryConstraint,
) -> Admission<()> {
    constraint
        .validate(track.id.as_str())
        .map_err(|reason| refuse(RecoveryRefusalCode::MissingFrozenContract, reason))?;
    let declaration = declaration_tx(tx, track, key).await?;
    let TaskRecoveryConstraint::V1 {
        refs,
        spawn,
        declared_by,
    } = constraint;
    if &declaration.spawn != spawn || &declaration.declared_by != declared_by {
        return Err(refuse(
            RecoveryRefusalCode::ContractChanged,
            "recovery contract route or author changed",
        ));
    }
    for frozen in refs {
        let target = crate::track_lifecycle::track_get_tx(tx, &frozen.track_id)
            .await
            .map_err(|error| match error {
                CalmError::NotFound(_) => refuse(
                    RecoveryRefusalCode::ContractChanged,
                    format!(
                        "recovery frozen context track is missing: {}",
                        frozen.track_id
                    ),
                ),
                other => AdmissionError::Other(other),
            })?;
        if target.area_id != track.area_id {
            let kind: String = sqlx::query_scalar("SELECT kind FROM areas WHERE id=?1")
                .bind(target.area_id.as_str())
                .fetch_one(&mut **tx)
                .await?;
            if kind != "system" {
                return Err(refuse(
                    RecoveryRefusalCode::ContractChanged,
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
            return Err(refuse(
                RecoveryRefusalCode::ContractChanged,
                format!(
                    "recovery frozen context report is missing: {}",
                    frozen.track_id
                ),
            ));
        }
        let (_, blocks) =
            crate::track_report::report_blocks_snapshot_tx(tx, frozen.track_id.as_str()).await?;
        let block = blocks
            .iter()
            .find(|block| block.id == frozen.block_id)
            .ok_or_else(|| {
                refuse(
                    RecoveryRefusalCode::ContractChanged,
                    "recovery frozen context block is missing",
                )
            })?;
        if frozen.is_root && declaration.block_id != frozen.block_id {
            return Err(refuse(
                RecoveryRefusalCode::ContractChanged,
                "recovery declaration identity changed",
            ));
        }
        let current =
            crate::task_context::context_ref(frozen.track_id.as_str(), block, frozen.is_root);
        if current.hash != frozen.hash {
            return Err(refuse(
                RecoveryRefusalCode::ContractChanged,
                "recovery contract changed; same-contract recovery is unavailable",
            ));
        }
    }
    Ok(())
}

/// Prepared isolated executions require their retained namespace stop proof.
/// Ordinary workers (shared codex, claude, terminal) retain the pre-preparation
/// fence: a PTY leader exit, terminal session state or released lease does not
/// prove descendants stopped writing, so once a worker was prepared the same
/// key is never recoverable; the way forward is a new task on the retained tree.
///
/// Actor-independent and read-only: `calm.plan.list` guidance re-evaluates it
/// behind an actor-dependent policy refusal so a continuation it advertises is
/// one the kernel would admit.
pub(crate) async fn require_recoverable_predecessor_tx(
    tx: &mut Tx<'_>,
    task: &Task,
) -> Admission<()> {
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
            return Err(refuse(
                RecoveryRefusalCode::PredecessorNotQuiescent,
                "predecessor isolated execution has ambiguous operations or verification effects",
            ));
        }
        return crate::isolated_codex::recovery::require_stopped_tx(tx, task, &operations[0].id)
            .await;
    }

    if task.worker_card_id.is_some() {
        return Err(refuse(
            RecoveryRefusalCode::PredecessorNotQuiescent,
            ORDINARY_WORKER_PREPARED_NO_STOP_PROOF,
        ));
    }
    if task.gate_attempt != 0 || task.gate_pid.is_some() || task.gate_result_json.is_some() {
        return Err(refuse(
            RecoveryRefusalCode::PredecessorNotQuiescent,
            VERIFICATION_EFFECTS_NO_STOP_PROOF,
        ));
    }
    if task
        .status_detail
        .as_deref()
        .map(crate::db::sqlite::status_detail_class)
        != Some("spawn-failed")
    {
        return Err(refuse(
            RecoveryRefusalCode::PredecessorNotQuiescent,
            FAILURE_AFTER_PREPARATION_NO_STOP_PROOF,
        ));
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
            return Err(refuse(
                RecoveryRefusalCode::PredecessorNotQuiescent,
                "predecessor operation has uncertain external effects; recovery currently requires a failure before worker preparation",
            ));
        }
    }
    // A still-pending predecessor Operation cannot race into a new process:
    // prepare rechecks the failed terminal row/current generation under this
    // same serialized writer boundary and refuses obsolete attempts.
    Ok(())
}

/// Reason for an ordinary (non-isolated) worker that was prepared before the
/// attempt failed (`worker_card_id` is set). Same-key recovery is permanently
/// unavailable; the continuation is a new task key. `calm.plan.list`
/// guidance carries the retained worktree path when a lease exists.
pub(crate) const ORDINARY_WORKER_PREPARED_NO_STOP_PROOF: &str = "an ordinary worker was prepared for this key and has no stop proof; same-key recovery is unavailable. Continue by declaring a new task (new key).";

/// Reason when verification effects (`gate_attempt`, `gate_pid`,
/// `gate_result_json`) exist for the key but no worker card was prepared:
/// a detached verifier descendant may still write.
pub(crate) const VERIFICATION_EFFECTS_NO_STOP_PROOF: &str = "verification effects were recorded for this key with no worker stop proof; same-key recovery is unavailable. Continue by declaring a new task (new key).";

/// Reason when the failure is not a preparation failure yet neither a worker
/// card nor verification effects remain (the card row may have been deleted):
/// the kernel cannot prove nothing was prepared.
pub(crate) const FAILURE_AFTER_PREPARATION_NO_STOP_PROOF: &str = "the failure was recorded after preparation could have started and the kernel holds no worker stop proof; same-key recovery is unavailable. Continue by declaring a new task (new key).";

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
pub(crate) async fn check_recovery_attempt_tx(tx: &mut Tx<'_>, task_id: &str) -> Admission<()> {
    let allocation = task_attempt_get_tx(tx, task_id).await?.ok_or_else(|| {
        refuse(
            RecoveryRefusalCode::RecoveryLineageMissing,
            "execution allocation is missing",
        )
    })?;
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
        .ok_or_else(|| {
            refuse(
                RecoveryRefusalCode::RecoveryLineageMissing,
                "recovery predecessor is missing",
            )
        })?;
    // Allocation + its scoped decision event is the accepted delegation.
    // The admitting actor remains immutable provenance; retiring its session or
    // card does not withdraw work already accepted by the kernel. New commands
    // still pass authorize_tx with their live identity at the service boundary.
    if !matches!(
        actor,
        ActorId::User | ActorId::AiPlanner(_) | ActorId::AiPlannerSession(_)
    ) {
        return Err(refuse(
            RecoveryRefusalCode::NotAuthorized,
            "accepted recovery has unsupported authority provenance",
        ));
    }
    if !crate::scheduler::lifecycle_allows_scheduling(track.lifecycle) {
        return Err(refuse(
            RecoveryRefusalCode::TrackNotReady,
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
    Ok(check_recovery_attempt_tx(tx, task_id).await?)
}
