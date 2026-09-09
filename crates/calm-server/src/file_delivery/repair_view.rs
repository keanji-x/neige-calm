//! Public lineage and live round stage, without private paths or Operation payloads.
use super::{repair::Receipt, *};
use crate::{
    db::sqlite::{task_attempt_current_tx, task_get_tx},
    operation::Tx,
};
use serde_json::{Value, json};
pub(crate) async fn view_tx(tx: &mut Tx<'_>, receipt: &Receipt) -> Result<Value> {
    let mut view = receipt.public();
    let stage = stage_tx(tx, receipt).await;
    match stage {
        Ok(stage) => view["stage"] = json!(stage),
        Err(CalmError::Conflict(reason) | CalmError::Forbidden(reason)) => {
            view["stage"] = json!("unavailable");
            view["reason"] = json!(reason);
        }
        Err(error) => return Err(error),
    }
    Ok(view)
}
async fn stage_tx(tx: &mut Tx<'_>, receipt: &Receipt) -> Result<&'static str> {
    let Some(allocation) =
        task_attempt_current_tx(tx, &receipt.track_id, &receipt.repair.key).await?
    else {
        return Ok("repair-unavailable");
    };
    let Some(task) = task_get_tx(tx, &allocation.attempt_id).await? else {
        return Ok("repair-unavailable");
    };
    use crate::model::TaskStatus;
    match task.status {
        TaskStatus::Pending => return Ok("repair-pending"),
        TaskStatus::Failed => return Ok("repair-failed"),
        TaskStatus::Done => {}
        _ => return Ok("repair-running"),
    }
    let publication: Option<String> = sqlx::query_scalar(
        "SELECT operation_id FROM task_file_candidates WHERE producer_attempt_id=?1",
    )
    .bind(&task.id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(publication) = publication else {
        return Ok("awaiting-c2-publication");
    };
    let candidate = super::candidate::load_tx(tx, &publication).await?;
    let verification:Option<(String,String)>=sqlx::query_as("SELECT o.id,o.phase FROM operations o JOIN task_candidate_verification_allocations a ON a.operation_key=o.operation_key WHERE a.publication_operation_id=?1 AND o.kind='candidate-verify'").bind(&publication).fetch_optional(&mut **tx).await?;
    let Some((id, phase)) = verification else {
        return Ok("awaiting-c2-checks");
    };
    if phase == "failed" {
        return Ok("c2-checks-failed");
    }
    if phase != "succeeded" {
        return Ok("awaiting-c2-checks");
    }
    super::candidate_verify::qualified_tx(tx, &id, &candidate).await?;
    let Some(allocation) =
        task_attempt_current_tx(tx, &receipt.track_id, &receipt.reviewer.key).await?
    else {
        return Ok("review-unavailable");
    };
    let Some(review) = task_get_tx(tx, &allocation.attempt_id).await? else {
        return Ok("review-unavailable");
    };
    match review.status {
        TaskStatus::Pending => return Ok("review-pending"),
        TaskStatus::Failed => return Ok("review-failed"),
        TaskStatus::Done => {}
        _ => return Ok("review-running"),
    }
    let phase: Option<String> = sqlx::query_scalar(
        "SELECT phase FROM operations WHERE kind='codex-isolated-worker' AND idempotency_key=?1",
    )
    .bind(&review.id)
    .fetch_optional(&mut **tx)
    .await?;
    if phase.as_deref() == Some("failed") {
        return Ok("review-operation-failed");
    }
    if phase.as_deref() != Some("succeeded") {
        return Ok("awaiting-review-settlement");
    }
    let evidence = super::candidate_review::evidence_tx(tx, &candidate, &id).await?;
    if !evidence.report.passed {
        return Ok("review-blocking");
    }
    match super::candidate_qualification::qualified_tx(tx, &candidate, &id).await {
        Ok(_) => Ok("qualified-c2"),
        Err(CalmError::Conflict(_) | CalmError::Forbidden(_)) => Ok("awaiting-planner-acceptance"),
        Err(error) => Err(error),
    }
}
