//! Done Reviewer settlement is a review decision notice, never retry authority.
use crate::{
    error::{CalmError, Result},
    model::{Task, TaskStatus},
    operation::Tx,
};
use calm_types::task_execution::FileDelivery;
use serde_json::{Value, json};

pub(crate) fn is_review(task: &Task) -> Result<bool> {
    Ok(task.status == TaskStatus::Done
        && matches!(
            crate::file_delivery::selection(task)?,
            Some(FileDelivery::CandidateReviewer { .. })
        ))
}
/// Check the exact terminal Operation and namespace stop, independently of retry.
/// In particular, a Done report can precede an Operation failure or cancellation.
pub(crate) async fn outcome_tx(tx: &mut Tx<'_>, task: &Task, id: &str) -> Result<String> {
    if !is_review(task)? {
        return Err(CalmError::Conflict(
            "not a completed candidate reviewer".into(),
        ));
    }
    let phase: Option<String> = sqlx::query_scalar("SELECT phase FROM operations WHERE id=?1 AND kind='codex-isolated-worker' AND idempotency_key=?2 AND phase IN ('succeeded','failed')")
        .bind(id).bind(&task.id).fetch_optional(&mut **tx).await?;
    let phase =
        phase.ok_or_else(|| CalmError::Conflict("review operation has not settled".into()))?;
    super::recovery::confirmed_record_tx(tx, task, id)
        .await
        .map_err(|error| match error {
            CalmError::Conflict(_) => {
                CalmError::Conflict("review namespace stop is not confirmed".into())
            }
            error => error,
        })?;
    Ok(phase)
}
async fn authority_tx(tx: &mut Tx<'_>, task: &Task, id: &str) -> Result<()> {
    let current = crate::db::sqlite::task_attempt_current_tx(tx, &task.track_id, &task.key).await?;
    if current.is_none_or(|a| a.attempt_id != task.id) {
        return Err(CalmError::Conflict("review attempt is obsolete".into()));
    }
    crate::task_recovery::validate_frozen_contract_tx(tx, task).await?;
    // Notice replay and queued delivery are authority boundaries too. Validate
    // original C1/R1 here once, without widening nested source/freeze readers.
    crate::file_delivery::repair::validate_task_tx(tx, task).await?;
    let (binding, state, prepared) = crate::file_delivery::candidate_input::load_tx(tx, &task.id)
        .await?
        .ok_or_else(|| CalmError::Conflict("review input missing".into()))?;
    if state != "prepared" || prepared.as_deref() != Some(id) {
        return Err(CalmError::Conflict(
            "review settlement does not own prepared input".into(),
        ));
    }
    crate::file_delivery::candidate_input::validate_source_tx(tx, task, &binding).await
}
pub(crate) async fn relevant_tx(tx: &mut Tx<'_>, task: &Task, id: &str) -> Result<bool> {
    match async {
        outcome_tx(tx, task, id).await?;
        authority_tx(tx, task, id).await
    }
    .await
    {
        Ok(()) => Ok(true),
        Err(CalmError::Conflict(_) | CalmError::Forbidden(_)) => Ok(false),
        Err(error) => Err(error),
    }
}
/// Recomputed at actual delivery too: a queued notice may since have been withdrawn.
pub(crate) async fn briefing_tx(tx: &mut Tx<'_>, task: &Task, id: &str) -> Result<Value> {
    let outcome = match outcome_tx(tx, task, id).await {
        Ok(phase) => Some(phase),
        Err(CalmError::Conflict(_)) => None,
        Err(error) => return Err(error),
    };
    let reason = match authority_tx(tx, task, id).await {
        Ok(()) => None,
        Err(CalmError::Conflict(reason) | CalmError::Forbidden(reason)) => Some(reason),
        Err(error) => return Err(error),
    };
    let binding = crate::file_delivery::candidate_input::load_tx(tx, &task.id).await?;
    let subject=binding.map(|(binding,_,_)|json!({"producer_attempt_id":binding.candidate.source.task_id,"publication_operation_id":binding.candidate.publication_operation_id,"verification_operation_id":binding.verification_operation_id}));
    Ok(
        json!({"review_attempt_id":task.id,"review_operation_id":id,"operation_state":outcome,
        "current_authority":reason.is_none() && outcome.is_some(),"authority_reason":reason,"subject":subject,
        "delivery":crate::file_delivery::view_tx(tx,task).await?,
        "decision":"Read calm.plan.list for the exact machine, review and Operation outcomes. A successful settled review may now be considered for the producer's explicit calm.task.verdict; a failed Operation cannot qualify the candidate. Recheck current authority and all evidence. If delivery.repair exists, use its linked repair_key/review_key: this original R1 notice neither creates another round nor accepts C2. C2 needs its own fresh checks, settled complete R2 and explicit producer acceptance. Do not recover this Done Reviewer: settlement grants neither Recover nor code acceptance."}),
    )
}
pub(crate) fn render(briefing: &Value) -> Result<String> {
    Ok(format!(
        "Candidate review execution settled (kernel snapshot):\n{}\nEnd candidate review settlement.",
        serde_json::to_string_pretty(briefing)?
    ))
}
