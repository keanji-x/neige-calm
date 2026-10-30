//! One exact pending Human revision joins the existing logical execution.
use super::*;
use calm_types::conversation::{InputSubmissionId, MessageRevisionRef};

pub async fn claim_supplemental_submission_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &mut CurrentInputCarrier,
    message: &MessageRevisionRef,
) -> Result<ClaimedSubmission> {
    carrier.require_execution_current()?;
    if carrier.phase()? != calm_types::harness::HarnessPhaseTag::TurnRunning {
        return Err(CalmError::Conflict(
            "supplemental input requires a running execution",
        ));
    }
    claims::require_private_tx(tx, carrier.card_id()).await?;
    let active = carrier
        .checkpoint
        .active
        .clone()
        .ok_or(CalmError::Conflict("no active logical turn"))?;
    let index = carrier.checkpoint.pending.iter().position(|input|
        matches!(input, calm_types::conversation::PendingInput::Human { message: actual, .. } if actual == message))
        .ok_or(CalmError::Conflict("message is no longer pending"))?;
    let entries = vec![carrier.checkpoint.pending[index].clone()];
    let plan = pending_plan_for_message_tx(tx, carrier, message).await?;
    claim_validation::validate_claim_tx(tx, carrier, &plan, &entries).await?;
    let last: i64 = sqlx::query_scalar("SELECT max(ordinal) FROM conversation_input_submissions WHERE logical_turn_id=?1 AND card_id=?2")
        .bind(active.logical_turn_id.as_str()).bind(carrier.card_id()).fetch_one(&mut **tx).await?;
    let ordinal = u32::try_from(last)
        .ok()
        .and_then(|v| v.checked_add(1))
        .ok_or(CalmError::Conflict("supplemental ordinal exhausted"))?;
    let submission_id = InputSubmissionId::parse(&crate::model::new_id())
        .map_err(|e| CalmError::Internal(e.to_string()))?;
    let original_digest = claim_storage::insert_submission_at_tx(
        tx,
        carrier.card_id(),
        &active.logical_turn_id,
        &submission_id,
        &plan,
        &entries,
        ordinal,
    )
    .await?;
    carrier.checkpoint.pending.remove(index);
    carrier
        .checkpoint
        .active
        .as_mut()
        .expect("checked active")
        .submission_ids
        .push(submission_id.clone());
    carrier::save_input_carrier_tx(tx, carrier).await?;
    Ok(ClaimedSubmission {
        submission_id,
        logical_turn_id: active.logical_turn_id,
        ordinal,
        plan,
        original_digest,
        snapshot_after_write: carrier.snapshot.clone(),
    })
}
