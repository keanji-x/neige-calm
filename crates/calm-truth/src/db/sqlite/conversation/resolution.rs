//! Borrow the authenticated owner's transaction. Checkpoint receipts alone suppress
//! held input; neither the delivery evidence nor the original execution is rewritten.
use super::carrier::save_input_carrier_tx;
use super::{current_input_fact_carrier_tx, digest};
use crate::error::{CalmError, Result};
use calm_types::conversation::{
    HumanKeepReceipt, InputDeliveryFact, KeepConversationInputRequest, KeepConversationInputResult,
};
use calm_types::harness::snapshot::CanonicalHarnessSnapshot;
use sqlx::{Sqlite, Transaction};

/// Caller must check current Human visibility/resource access before this lookup,
/// and retain/recheck its live session through the surrounding transaction.
pub async fn keep_conversation_input_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    human: &str,
    key: &str,
    request: &KeepConversationInputRequest,
) -> Result<KeepConversationInputResult> {
    let fingerprint = digest(&("human_keep_v1", card_id, human, request))?;
    // Retained original carriers own replay even after supersession. This never
    // loads a historical body queue or chooses a replacement execution carrier.
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT handle_state_json FROM worker_sessions WHERE card_id=?1 AND
         CASE WHEN json_valid(handle_state_json) THEN
         json_type(handle_state_json,'$.canonical_inputs') IS NOT NULL ELSE 0 END",
    )
    .bind(card_id)
    .fetch_all(&mut **tx)
    .await?;
    for raw in rows {
        let frame: CanonicalHarnessSnapshot = serde_json::from_str(&raw)?;
        for receipt in frame.canonical_inputs.receipts {
            if receipt.key == key {
                if receipt.principal != human || receipt.fingerprint != fingerprint {
                    return Err(CalmError::IdempotencyKeyReused(
                        "this key names another Human decision".into(),
                    ));
                }
                return Ok(receipt.result);
            }
        }
    }
    let mut carrier = current_input_fact_carrier_tx(tx, card_id, &request.carrier_id).await?;
    let attempt = carrier
        .checkpoint
        .attempts
        .iter()
        .find(|attempt| {
            attempt.attempt.submission_id.as_str() == request.submission_id
                && attempt.attempt.attempt_id.as_str() == request.attempt_id
        })
        .ok_or(CalmError::Conflict("held input identity changed"))?;
    if attempt.revision.to_string() != request.revision {
        return Err(CalmError::Conflict("held input revision changed"));
    }
    if carrier.checkpoint.is_kept(&attempt.attempt)
        || !matches!(
            attempt.delivery,
            InputDeliveryFact::CrossingUnknown | InputDeliveryFact::Unknown
        )
    {
        return Err(CalmError::Conflict("input is no longer held"));
    }
    let revision = attempt
        .revision
        .checked_add(1)
        .ok_or(CalmError::Conflict("input attempt revision exhausted"))?;
    let result = KeepConversationInputResult {
        carrier_id: request.carrier_id.clone(),
        submission_id: request.submission_id.clone(),
        attempt_id: request.attempt_id.clone(),
        revision: revision.to_string(),
        waiting_for_settlement: attempt.correlation.native_turn_id.is_none(),
    };
    carrier
        .checkpoint
        .attempts
        .iter_mut()
        .find(|attempt| attempt.attempt.attempt_id.as_str() == request.attempt_id)
        .expect("selected attempt")
        .revision = revision;
    carrier.checkpoint.receipts.push(HumanKeepReceipt {
        key: key.into(),
        principal: human.into(),
        fingerprint,
        request: request.clone(),
        result: result.clone(),
    });
    save_input_carrier_tx(tx, &mut carrier).await?;
    Ok(result)
}
