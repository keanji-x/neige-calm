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
    let (json, truncated): (Option<String>, i64) = sqlx::query_as(
        "SELECT claim_context_json, context_closure_truncated FROM tasks WHERE id=?1",
    )
    .bind(&previous.id)
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
        spawn: previous.spawn.clone(),
        declared_by: previous.declared_by.clone(),
    };
    constraint.validate(&previous.track_id).map_err(conflict)?;
    check_constraint_tx(tx, track, &previous.key, &constraint).await?;
    require_quiescence_tx(tx, previous).await?;
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
        let target = crate::track_lifecycle::track_get_tx(tx, &frozen.track_id).await?;
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

/// A failed task or released workspace alone is not proof that writes stopped.
async fn require_quiescence_tx(tx: &mut Tx<'_>, task: &Task) -> Result<()> {
    if task.gate_pid.is_some()
        || task.status_detail.as_deref().is_some_and(|detail| {
            detail.starts_with("gate-infra") || detail.starts_with("gate-timeout")
        })
    {
        return Err(conflict(
            "predecessor verifier has uncertain write-stop evidence; reconcile it before recovery",
        ));
    }
    let operations: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT phase,target_type,target_id,spawn_artifacts_json FROM operations WHERE \
         (idempotency_key=?1 AND kind IN ('codex-worker','claude-worker','terminal-worker')) \
         OR (kind='task-verify' AND json_extract(payload_json,'$.task_id')=?1)",
    )
    .bind(&task.id)
    .fetch_all(&mut **tx)
    .await?;
    let mut cards = std::collections::BTreeSet::new();
    if let Some(card) = &task.worker_card_id {
        cards.insert(card.clone());
    }
    for (phase, target_type, card, artifacts) in operations {
        // Pending has not prepared anything. Its prepare fence will reject the
        // obsolete attempt. Mid-spawn or compensation requires reconciliation.
        if !matches!(phase.as_str(), "pending" | "failed" | "succeeded") {
            return Err(conflict(
                "predecessor operation has uncertain external effects; reconcile it before recovery",
            ));
        }
        if phase == "failed" && card.is_none() && artifacts.is_some() {
            return Err(conflict(
                "predecessor operation has uncertain external effects; reconcile it before recovery",
            ));
        }
        if target_type == "card"
            && let Some(card) = card
        {
            cards.insert(card);
        }
    }
    for card in cards {
        let terminals: Vec<(Option<i64>, i64)> =
            sqlx::query_as("SELECT exit_code,signal_killed FROM terminals WHERE card_id=?1")
                .bind(&card)
                .fetch_all(&mut **tx)
                .await?;
        let terminal_exited = !terminals.is_empty()
            && terminals
                .iter()
                .all(|(exit, killed)| exit.is_some_and(|code| code >= 0) || *killed != 0);
        let sessions: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT state,active_turn_id,handle_state_json FROM worker_sessions WHERE card_id=?1",
        )
        .bind(&card)
        .fetch_all(&mut **tx)
        .await?;
        let sessions_quiet = !sessions.is_empty()
            && sessions.iter().all(|(state, turn, handle)| {
                let cleanup_pending = handle
                    .as_deref()
                    .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
                    .is_some_and(|value| value.get("timeout_cleanup").is_some());
                matches!(state.as_str(), "exited" | "failed" | "superseded")
                    && turn.is_none()
                    && !cleanup_pending
            });
        if !(terminal_exited && (task.kind == crate::model::TaskKind::Terminal || sessions_quiet)) {
            return Err(conflict(
                "predecessor has no verified write-stop evidence; stop/reconcile its provider before recovery",
            ));
        }
    }
    Ok(())
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
    recovery_policy(
        tx,
        &track,
        &previous,
        allocation.generation - 1,
        &actor,
        false,
    )
    .await?;
    let scope = EventScope::Track {
        track: track.id.clone(),
        area: track.area_id.clone(),
    };
    let event = Event::PlanUpdated {
        track_id: track.id.clone(),
        changed_keys: vec![allocation.key.clone()],
        agent_message: None,
    };
    authorize_tx(tx, &actor, &scope, &event).await?;
    check_constraint_tx(tx, &track, &allocation.key, &constraint).await?;
    require_quiescence_tx(tx, &previous).await
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
