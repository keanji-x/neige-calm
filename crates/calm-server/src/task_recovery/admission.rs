//! Admission checks are read-only and run under the writer transaction.
//! Every refusal is a typed [`RecoveryRefusal`] decided at its site: the
//! read side takes its code, its continuation and its sentence from the
//! type and never from the message text or the task shape.

use super::refusal::{
    Admission, AdmissionError, RecoveryRefusal, RecoveryRefusalCode, RefusalSite,
    SupportedContinuation,
};
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
            RefusalSite::ActorNotUserOrPlanner,
            RecoveryRefusalCode::NotAuthorized,
            SupportedContinuation::None,
            "task recovery requires a User or Planner",
        )
        .into());
    }
    calm_truth::decision_gate::enforce_role_resolving_session_from_tx(tx, actor, event, scope)
        .await
        .map_err(|error| {
            RecoveryRefusal::forbidden(
                RefusalSite::PlannerSessionUnresolved,
                RecoveryRefusalCode::NotAuthorized,
                SupportedContinuation::None,
                error.to_string(),
            )
            .into()
        })
}

fn conflict(reason: impl Into<String>) -> CalmError {
    CalmError::Conflict(reason.into())
}

/// A Conflict-kind refusal decided at `site`. `reason` is the complete
/// sentence the wire and `calm.plan.list` guidance carry unchanged.
fn refuse(
    site: RefusalSite,
    code: RecoveryRefusalCode,
    continuation: SupportedContinuation,
    reason: impl Into<String>,
) -> AdmissionError {
    RecoveryRefusal::conflict(site, code, continuation, reason).into()
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
            RefusalSite::TrackNotReady,
            RecoveryRefusalCode::TrackNotReady,
            SupportedContinuation::None,
            format!(
                "track is blocked or terminal (lifecycle {}) and does not schedule work; \
                 explicitly request working to resolve a blocker, or separately reopen a \
                 terminal track",
                track.lifecycle.as_db_str()
            ),
        ));
    }
    if task.spawn != TASK_IN_TRACK_ROUTE {
        return Err(refuse(
            RefusalSite::ChildTaskRoute,
            RecoveryRefusalCode::UnsupportedSpawn,
            SupportedContinuation::None,
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
                RefusalSite::PlannerOutsideAutoDeclare,
                RecoveryRefusalCode::UserAuthorizationRequired,
                SupportedContinuation::UserRecovery,
                "Planner recovery requires a Planner declaration under auto-declare; user-owned and declare-and-wait tasks require an explicit User recovery",
            )
            .into());
        }
        if generation >= 2 {
            return Err(RecoveryRefusal::forbidden(
                RefusalSite::PlannerRetryLimit,
                RecoveryRefusalCode::RecoveryLimitReached,
                SupportedContinuation::UserRecovery,
                "Planner recovery limit reached; an explicit User recovery is required",
            )
            .into());
        }
    }
    Ok(())
}

/// The actor-dependent policy, then the actor-independent contract and
/// predecessor checks, with the same `?` at each step as before the split.
pub(super) async fn admit_recovery_tx(
    tx: &mut Tx<'_>,
    track: &Track,
    previous: &Task,
    generation: i64,
    actor: &ActorId,
    resume_blocked: bool,
) -> Admission<TaskRecoveryConstraint> {
    recovery_policy(tx, track, previous, generation, actor, resume_blocked).await?;
    admit_contract_and_predecessor_tx(tx, track, previous).await
}

/// Everything after the policy: claim constraint, its shape, the current
/// declaration and frozen references, the predecessor fence, the frozen
/// file-delivery input. Actor-independent and read-only, so
/// `calm.plan.list` guidance re-runs it behind a policy refusal: the
/// continuation it then advertises is the one the kernel would decide for
/// any actor.
pub(crate) async fn admit_contract_and_predecessor_tx(
    tx: &mut Tx<'_>,
    track: &Track,
    previous: &Task,
) -> Admission<TaskRecoveryConstraint> {
    let constraint = claim_constraint_tx(tx, previous).await?;
    constraint.validate(&previous.track_id).map_err(|reason| {
        refuse(
            RefusalSite::ConstraintShapeInvalid,
            RecoveryRefusalCode::MissingFrozenContract,
            SupportedContinuation::None,
            reason,
        )
    })?;
    check_constraint_tx(tx, track, &previous.key, &constraint).await?;
    require_recoverable_predecessor_tx(tx, previous).await?;
    // File-delivery input checks refuse with their own Conflict messages; at
    // this boundary they all mean the frozen input contract cannot be honoured.
    crate::file_delivery::require_recovery_input_tx(tx, previous)
        .await
        .map_err(|error| match error {
            CalmError::Conflict(reason) => refuse(
                RefusalSite::FileDeliveryInputUnhonoured,
                RecoveryRefusalCode::ContractChanged,
                SupportedContinuation::None,
                reason,
            ),
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
            RefusalSite::FrozenContextTruncated,
            RecoveryRefusalCode::MissingFrozenContract,
            SupportedContinuation::None,
            "failed execution has incomplete frozen context; same-contract recovery is unavailable",
        ));
    }
    let refs = serde_json::from_str(json.as_deref().ok_or_else(|| {
        refuse(
            RefusalSite::FrozenContextMissing,
            RecoveryRefusalCode::MissingFrozenContract,
            SupportedContinuation::None,
            "failed execution has no frozen contract; same-contract recovery is unavailable",
        )
    })?)
    .map_err(|_| {
        refuse(
            RefusalSite::FrozenContextMalformed,
            RecoveryRefusalCode::MissingFrozenContract,
            SupportedContinuation::None,
            "failed execution has malformed frozen context; same-contract recovery is unavailable",
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
            RefusalSite::DeclarationMissing,
            RecoveryRefusalCode::DeclarationWithdrawn,
            SupportedContinuation::None,
            format!(
                "task declaration `{key}` is no longer in the report or is unreadable; \
                 same-contract recovery has nothing current to honour"
            ),
        )
    })?;
    let duplicate = matching.next().is_some();
    if duplicate || declaration.tombstone || !declaration.ready {
        let state = if duplicate {
            "is declared more than once in the report"
        } else if declaration.tombstone {
            "was removed (tombstoned)"
        } else {
            "was withdrawn (ready is false)"
        };
        return Err(refuse(
            RefusalSite::DeclarationNotCurrent,
            RecoveryRefusalCode::DeclarationWithdrawn,
            SupportedContinuation::None,
            format!(
                "task declaration `{key}` {state}; same-contract recovery has nothing current to honour"
            ),
        ));
    }
    if declaration
        .block_index
        .and_then(|i| diagnostics.get(i))
        .is_some_and(|diags| !diags.is_empty())
    {
        return Err(refuse(
            RefusalSite::DeclarationInvalid,
            RecoveryRefusalCode::DeclarationWithdrawn,
            SupportedContinuation::None,
            format!(
                "task declaration `{key}` has validation errors; same-contract recovery has nothing current to honour"
            ),
        ));
    }
    if declaration.declared_by == PLANNER_DECLARATION_AUTHOR
        && automation_policy_tx(tx, track).await?.as_deref() == Some("declare-and-wait")
        && !declaration.released_by_user
    {
        return Err(refuse(
            RefusalSite::ReleaseWithdrawn,
            RecoveryRefusalCode::DeclarationWithdrawn,
            SupportedContinuation::None,
            format!(
                "task execution release for `{key}` was withdrawn under declare-and-wait; \
                 same-contract recovery waits for a current User release"
            ),
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
    constraint.validate(track.id.as_str()).map_err(|reason| {
        refuse(
            RefusalSite::InnerConstraintShapeInvalid,
            RecoveryRefusalCode::MissingFrozenContract,
            SupportedContinuation::None,
            reason,
        )
    })?;
    let declaration = declaration_tx(tx, track, key).await?;
    let TaskRecoveryConstraint::V1 {
        refs,
        spawn,
        declared_by,
    } = constraint;
    let changed = |site: RefusalSite, reason: String| {
        refuse(
            site,
            RecoveryRefusalCode::ContractChanged,
            SupportedContinuation::None,
            reason,
        )
    };
    if &declaration.spawn != spawn || &declaration.declared_by != declared_by {
        return Err(changed(
            RefusalSite::RouteOrAuthorChanged,
            "recovery contract route or author changed; same-contract recovery is unavailable"
                .into(),
        ));
    }
    for frozen in refs {
        let target = crate::track_lifecycle::track_get_tx(tx, &frozen.track_id)
            .await
            .map_err(|error| match error {
                CalmError::NotFound(_) => changed(
                    RefusalSite::FrozenTrackMissing,
                    format!(
                        "recovery frozen context track is missing: {}; same-contract recovery is unavailable",
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
                return Err(changed(
                    RefusalSite::ContextMovedOutsideArea,
                    "recovery context moved outside its authorized area; same-contract recovery is unavailable".into(),
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
            return Err(changed(
                RefusalSite::FrozenReportMissing,
                format!(
                    "recovery frozen context report is missing: {}; same-contract recovery is unavailable",
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
                changed(
                    RefusalSite::FrozenBlockMissing,
                    "recovery frozen context block is missing; same-contract recovery is unavailable".into(),
                )
            })?;
        if frozen.is_root && declaration.block_id != frozen.block_id {
            return Err(changed(
                RefusalSite::RootIdentityChanged,
                "recovery declaration identity changed; same-contract recovery is unavailable"
                    .into(),
            ));
        }
        let current =
            crate::task_context::context_ref(frozen.track_id.as_str(), block, frozen.is_root);
        if current.hash != frozen.hash {
            return Err(changed(
                RefusalSite::RootHashChanged,
                "recovery contract changed; same-contract recovery is unavailable".into(),
            ));
        }
    }
    Ok(())
}

/// Prepared isolated executions require their retained namespace stop proof.
/// Every other prepared worker (shared codex, claude, terminal — whatever
/// card kind) retains the pre-preparation fence: a PTY leader exit, terminal
/// session state or released lease does not prove descendants stopped
/// writing, so once such a worker was prepared the same key is never
/// recoverable; the way forward is a new task key.
///
/// Actor-independent and read-only: `calm.plan.list` guidance re-evaluates it
/// (through `admit_contract_and_predecessor_tx`) behind an actor-dependent
/// policy refusal so a continuation it advertises is one the kernel would
/// decide for any actor.
pub(super) async fn require_recoverable_predecessor_tx(
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
    let fence = |site: RefusalSite, continuation: SupportedContinuation, reason: &str| {
        refuse(
            site,
            RecoveryRefusalCode::PredecessorNotQuiescent,
            continuation,
            reason,
        )
    };
    if operations.iter().any(|operation| {
        operation.kind == crate::isolated_codex::OPERATION_KIND
            && operation.tx_output_json.is_some()
    }) {
        if operations.len() != 1
            || task.gate_attempt != 0
            || task.gate_pid.is_some()
            || task.gate_result_json.is_some()
        {
            return Err(fence(
                RefusalSite::IsolatedAmbiguousOperations,
                SupportedContinuation::None,
                ISOLATED_AMBIGUOUS_OPERATIONS,
            ));
        }
        return crate::isolated_codex::recovery::require_stopped_tx(tx, task, &operations[0].id)
            .await;
    }

    if task.worker_card_id.is_some() {
        return Err(fence(
            RefusalSite::OrdinaryWorkerPrepared,
            SupportedContinuation::NewTask,
            ORDINARY_WORKER_PREPARED_NO_STOP_PROOF,
        ));
    }
    if task.gate_attempt != 0 || task.gate_pid.is_some() || task.gate_result_json.is_some() {
        return Err(fence(
            RefusalSite::VerificationEffectsWithoutWorker,
            SupportedContinuation::NewTask,
            VERIFICATION_EFFECTS_NO_STOP_PROOF,
        ));
    }
    if task
        .status_detail
        .as_deref()
        .map(crate::db::sqlite::status_detail_class)
        != Some("spawn-failed")
    {
        return Err(fence(
            RefusalSite::NotSpawnFailedWithoutStopProof,
            SupportedContinuation::NewTask,
            NOT_SPAWN_FAILED_NO_STOP_PROOF,
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
            return Err(fence(
                RefusalSite::OperationUncertainExternalEffects,
                SupportedContinuation::NewTask,
                OPERATION_UNCERTAIN_EXTERNAL_EFFECTS,
            ));
        }
    }
    // A still-pending predecessor Operation cannot race into a new process:
    // prepare rechecks the failed terminal row/current generation under this
    // same serialized writer boundary and refuses obsolete attempts.
    Ok(())
}

/// A prepared isolated execution whose key also carries other operations or
/// verification effects: the kernel cannot tell which process the stop proof
/// would cover, and no settlement briefing re-opens this.
pub(crate) const ISOLATED_AMBIGUOUS_OPERATIONS: &str = "predecessor isolated execution has ambiguous operations or verification effects on its key; \
     the namespace stop proof cannot be attributed, so same-key recovery is permanently \
     unavailable and no settlement briefing re-opens it";

/// A worker card was prepared for the key (`worker_card_id` is set) with no
/// isolated preparation receipt on the key — whatever the declaration's
/// context shape says — so nothing proves its descendants stopped writing.
/// `calm.plan.list` guidance carries the retained worktree path when a lease
/// exists.
pub(crate) const ORDINARY_WORKER_PREPARED_NO_STOP_PROOF: &str = "an ordinary worker was prepared for this key and has no stop proof; same-key recovery is \
     unavailable. Continue by declaring a new task (new key).";

/// Verification effects (`gate_attempt`, `gate_pid`, `gate_result_json`)
/// exist for the key but no worker card was prepared: a detached verifier
/// descendant may still write.
pub(crate) const VERIFICATION_EFFECTS_NO_STOP_PROOF: &str = "verification effects were recorded for this key with no worker stop proof; same-key recovery \
     is unavailable. Continue by declaring a new task (new key).";

/// The failure is not a spawn failure yet neither a worker card nor
/// verification effects remain: the kernel holds no stop proof and does not
/// claim to know whether a preparation happened.
pub(crate) const NOT_SPAWN_FAILED_NO_STOP_PROOF: &str = "the failed execution was not a spawn failure and has no stop proof; same-key recovery is \
     unavailable. Continue by declaring a new task (new key).";

/// A keyed operation whose recorded shape is not a pre-preparation failure
/// (foreign kind, advanced phase, prepared target, output, artifacts or
/// compensation): its external effects are uncertain.
pub(crate) const OPERATION_UNCERTAIN_EXTERNAL_EFFECTS: &str = "predecessor operation has uncertain external effects; recovery currently requires a failure \
     before worker preparation, so same-key recovery is unavailable. Continue by declaring a new \
     task (new key).";

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
            RefusalSite::AllocationMissing,
            RecoveryRefusalCode::RecoveryLineageMissing,
            SupportedContinuation::None,
            "execution allocation is missing; the accepted recovery cannot start",
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
                RefusalSite::PredecessorRowMissing,
                RecoveryRefusalCode::RecoveryLineageMissing,
                SupportedContinuation::None,
                "recovery predecessor row is missing; the accepted recovery cannot start",
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
            RefusalSite::ProvenanceUnsupported,
            RecoveryRefusalCode::NotAuthorized,
            SupportedContinuation::None,
            "accepted recovery has unsupported authority provenance; it cannot start",
        ));
    }
    if !crate::scheduler::lifecycle_allows_scheduling(track.lifecycle) {
        return Err(refuse(
            RefusalSite::TrackNoLongerSchedules,
            RecoveryRefusalCode::TrackNotReady,
            SupportedContinuation::None,
            format!(
                "track is paused or terminal (lifecycle {}); resume its work before this recovery can start",
                track.lifecycle.as_db_str()
            ),
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
