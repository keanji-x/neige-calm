//! Borrow the authenticated owner's transaction. Checkpoint receipts alone suppress
//! held input; neither the delivery evidence nor the original execution is rewritten.
use super::carrier::save_input_carrier_tx;
use super::{current_input_fact_carrier_tx, digest};
use crate::error::{CalmError, Result};
use calm_types::conversation::{
    HumanInputResolutionDecision, HumanInputResolutionReceipt, KeepConversationInputRequest,
    KeepConversationInputResult,
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
    if let Some(decision) = replay_tx(tx, card_id, human, key, &fingerprint).await? {
        return match decision {
            HumanInputResolutionDecision::Keep { result, .. } => Ok(result),
            HumanInputResolutionDecision::Retry { .. } => Err(CalmError::IdempotencyKeyReused(
                "this key names another Human decision".into(),
            )),
        };
    }
    let (mut carrier, index) = resolution_target_tx(tx, card_id, request).await?;
    if super::retry::held_unavailable(&carrier, index).is_some() {
        return Err(CalmError::Conflict("input is no longer held"));
    }
    let attempt = &carrier.checkpoint.attempts[index];
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
    carrier
        .checkpoint
        .receipts
        .push(HumanInputResolutionReceipt {
            key: key.into(),
            principal: human.into(),
            fingerprint,
            decision: HumanInputResolutionDecision::Keep {
                request: request.clone(),
                result: result.clone(),
            },
        });
    save_input_carrier_tx(tx, &mut carrier).await?;
    Ok(result)
}

/// One cross-action replay authority, including retained original carriers.
pub(super) async fn replay_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    human: &str,
    key: &str,
    fingerprint: &str,
) -> Result<Option<HumanInputResolutionDecision>> {
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
                return Ok(Some(receipt.decision));
            }
        }
    }
    Ok(None)
}

pub(super) async fn resolution_target_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    request: &KeepConversationInputRequest,
) -> Result<(super::CurrentInputCarrier, usize)> {
    let carrier = current_input_fact_carrier_tx(tx, card_id, &request.carrier_id).await?;
    let index = carrier
        .checkpoint
        .attempts
        .iter()
        .position(|attempt| {
            attempt.attempt.submission_id.as_str() == request.submission_id
                && attempt.attempt.attempt_id.as_str() == request.attempt_id
        })
        .ok_or(CalmError::Conflict("held input identity changed"))?;
    if carrier.checkpoint.attempts[index].revision.to_string() != request.revision {
        return Err(CalmError::Conflict("held input revision changed"));
    }
    Ok((carrier, index))
}

/// Caller retains a live Human source and checks resource access before replay.
pub async fn retry_conversation_input_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    human: &str,
    key: &str,
    request: &calm_types::conversation::RetryConversationInputRequest,
    execution: &calm_types::conversation::ConversationExecutionProvenance,
) -> Result<calm_types::conversation::RetryConversationInputResult> {
    let decision = super::retry::resolve_retry_tx(tx, card, human, key, &request.into(), execution)
        .await?
        .map_err(|reason| CalmError::Conflict(format!("retry unavailable: {reason:?}")))?;
    match decision.public_result() {
        calm_types::conversation::ConversationInputResolutionResult::Retry { result } => Ok(result),
        calm_types::conversation::ConversationInputResolutionResult::Keep { .. } => Err(
            CalmError::IdempotencyKeyReused("this key names another Human decision".into()),
        ),
    }
}

/// Receipt replay still requires the facade's current Human/resource admission.
/// It deliberately does not require an execution carrier that can start work.
pub async fn replay_retry_conversation_input_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    human: &str,
    key: &str,
    request: &calm_types::conversation::RetryConversationInputRequest,
) -> Result<Option<calm_types::conversation::RetryConversationInputResult>> {
    super::retry::replay_retry_tx(tx, card, human, key, &request.into())
        .await?
        .map(|decision| match decision.public_result() {
            calm_types::conversation::ConversationInputResolutionResult::Retry { result } => {
                Ok(result)
            }
            calm_types::conversation::ConversationInputResolutionResult::Keep { .. } => Err(
                CalmError::IdempotencyKeyReused("this key names another Human decision".into()),
            ),
        })
        .transpose()
}
