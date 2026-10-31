//! Kept ACP cleanup is independent of delivery and qualified END.
use super::carrier::save_input_carrier_tx;
use super::{RuntimeExecutionFence, RuntimeFrameAfterWrite, current_input_carrier_tx};
use crate::db::sqlite::{
    AcpCleanupObservation, AcpCleanupVersion, AcpKeptCleanup, AcpSubmission, AcpSubmissionInput,
    AcpSubmissionState,
};
use crate::error::{CalmError, Result};
use calm_types::conversation::{HumanInputResolutionDecision, InputDeliveryFact};
use calm_types::harness::HarnessPhaseTag;
use sqlx::{Sqlite, Transaction};

/// The ACP owner holds issuance exclusion and has normally joined the exact
/// receipt's confirmed stop/revoke. This transaction rechecks the durable side;
/// neither Keep admission nor a recovered empty process slot can call it alone.
pub async fn settle_kept_acp_receipt_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    expected: &AcpSubmission,
    registration_digest: &str,
    observation: AcpCleanupObservation,
) -> Result<Option<RuntimeFrameAfterWrite>> {
    let Some(receipt) = crate::db::sqlite::acp_submission_get(
        &mut **tx,
        &expected.worker_session_id,
        &expected.client_id,
    )
    .await?
    else {
        return Ok(None);
    };
    if receipt.turn_id != expected.turn_id
        || receipt.thread_id != expected.thread_id
        || receipt.native_session_id != expected.native_session_id
        || receipt.input_json != expected.input_json
        || !matches!(receipt.state, AcpSubmissionState::Unknown)
        || receipt.outcome_json.is_some()
    {
        return Ok(None);
    }
    let AcpSubmissionInput::Canonical(association) =
        AcpSubmissionInput::decode(&receipt.input_json)?
    else {
        return Ok(None);
    };
    let mut carrier = current_input_carrier_tx(tx, card, &receipt.worker_session_id).await?;
    let attempt = association.proof.attempt();
    let Some(fact) = carrier.checkpoint().attempt(&attempt.submission_id) else {
        return Ok(None);
    };
    if fact.attempt != *attempt
        || !matches!(fact.delivery, InputDeliveryFact::Unknown)
        || fact.correlation.thread_id != receipt.thread_id
        || fact.correlation.local_turn_id.as_deref() != Some(receipt.turn_id.as_str())
        || fact.correlation.native_session_id.as_deref() != Some(receipt.native_session_id.as_str())
        || fact.correlation.native_turn_id.is_some()
    {
        return Ok(None);
    }
    let Some(keep) = matching_keep(carrier.checkpoint(), &receipt.worker_session_id, attempt)
    else {
        return Ok(None);
    };
    let evidence = AcpKeptCleanup {
        version: AcpCleanupVersion::V1,
        worker_session_id: receipt.worker_session_id.clone(),
        client_id: receipt.client_id.clone(),
        turn_id: receipt.turn_id.clone(),
        thread_id: receipt.thread_id.clone(),
        native_session_id: receipt.native_session_id.clone(),
        association: association.clone(),
        registration_digest: registration_digest.to_owned(),
        keep_key: keep.key.clone(),
        keep_principal: keep.principal.clone(),
        keep_fingerprint: keep.fingerprint.clone(),
        observation,
    };
    if let Some(prior) = receipt.kept_cleanup {
        if prior.0 != evidence {
            return Err(CalmError::Conflict("ACP cleanup evidence changed"));
        }
        return Ok(None);
    }
    let identity = super::submission_identity_tx(tx, &attempt.submission_id).await?;
    let plan = super::submission_plan_tx(tx, &attempt.submission_id).await?;
    let stored_proof = calm_types::conversation::SubmissionDispatchProof::from_stored_plan(
        attempt.clone(),
        &plan.plan,
        plan.original_digest,
    )
    .map_err(CalmError::Conflict)?;
    let bound: bool = sqlx::query_scalar(concat!(
"SELECT EXISTS(SELECT 1 FROM worker_sessions ws JOIN acp_managed_sessions a ON ",
"a.worker_session_id=ws.id WHERE ws.id=?1 AND ws.thread_id=?2 AND ws.agent_session_id=?3 AND ",
"a.registration_digest=?4 AND ws.provider=?5)",
))
        .bind(&receipt.worker_session_id).bind(&receipt.thread_id).bind(&receipt.native_session_id).bind(registration_digest).bind(fact.correlation.provider.wire_name()).fetch_one(&mut **tx).await?;
    let phase = carrier.phase()?;
    let has_intent = carrier
        .snapshot()
        .get("interruption_intent")
        .is_some_and(|value| !value.is_null());
    if !bound
        || identity.card_id != card
        || identity.logical_turn_id != association.logical_turn_id
        || stored_proof != association.proof
        || carrier.checkpoint().active.as_ref().is_none_or(|active| {
            active.logical_turn_id != association.logical_turn_id
                || active.submission_ids != [attempt.submission_id.clone()]
        })
        || !((matches!(
            phase,
            HarnessPhaseTag::IssuingTurn | HarnessPhaseTag::TurnRunning
        ) && !has_intent)
            || (phase == HarnessPhaseTag::IssuingInterrupt
                && has_intent
                && super::runtime_frame::interruption_matches_tx(
                    tx,
                    &carrier,
                    &receipt.thread_id,
                    &receipt.turn_id,
                )
                .await?))
        || (phase != HarnessPhaseTag::IssuingTurn
            && carrier
                .snapshot()
                .get("last_turn_id")
                .and_then(serde_json::Value::as_str)
                != Some(receipt.turn_id.as_str()))
        || ["pending_rewind", "wedged_reason"]
            .iter()
            .any(|key| carrier.snapshot().get(*key).is_some_and(|v| !v.is_null()))
        || crate::db::sqlite::acp_submission_other_blocking_tx(
            tx,
            &receipt.worker_session_id,
            &receipt.client_id,
        )
        .await?
    {
        return Ok(None);
    }
    let json = serde_json::to_string(&evidence)?;
    let updated = sqlx::query(concat!(
"UPDATE acp_submissions SET kept_cleanup_json=?3 WHERE worker_session_id=?1 AND client_id=?2 ",
"AND state='unknown' AND kept_cleanup_json IS NULL",
))
        .bind(&receipt.worker_session_id).bind(&receipt.client_id).bind(json).execute(&mut **tx).await?;
    if updated.rows_affected() != 1 {
        return Err(CalmError::Conflict("ACP cleanup receipt changed"));
    }
    carrier.checkpoint.active = None;
    carrier.set_phase(HarnessPhaseTag::Idle)?;
    carrier.snapshot["interruption_intent"] = serde_json::Value::Null;
    save_input_carrier_tx(tx, &mut carrier).await?;
    crate::db::sqlite::session_set_harness_observation_runtime_tx(
        tx,
        &receipt.worker_session_id,
        calm_types::worker::WorkerSessionState::Idle,
        Some(&receipt.thread_id),
        None,
    )
    .await
    .map_err(|e| CalmError::Internal(e.to_string()))?;
    Ok(Some(RuntimeFrameAfterWrite {
        snapshot: carrier.snapshot().clone(),
        execution_fence: RuntimeExecutionFence::from_carrier(&carrier),
    }))
}

fn matching_keep<'a>(
    checkpoint: &'a calm_types::conversation::ConversationInputCheckpoint,
    worker: &str,
    attempt: &calm_types::conversation::InputAttempt,
) -> Option<&'a calm_types::conversation::HumanInputResolutionReceipt> {
    checkpoint
        .receipts
        .iter()
        .find(|keep| match &keep.decision {
            HumanInputResolutionDecision::Keep { result, .. } => {
                result.carrier_id == worker
                    && result.submission_id == attempt.submission_id.as_str()
                    && result.attempt_id == attempt.attempt_id.as_str()
            }
            HumanInputResolutionDecision::Retry { .. } => false,
        })
}

/// Negative observation only: a real cleanup candidate still reloads and checks
/// the full durable contract inside settle_kept_acp_receipt_tx.
pub async fn kept_acp_cleanup_candidate(
    pool: &sqlx::SqlitePool,
    card: &str,
    expected: &AcpSubmission,
) -> Result<bool> {
    let frame = super::runtime_frame_observation(pool, card, &expected.worker_session_id).await?;
    if frame.snapshot.canonical_inputs.active.is_none() {
        return Ok(false);
    }
    let AcpSubmissionInput::Canonical(association) =
        AcpSubmissionInput::decode(&expected.input_json)?
    else {
        return Ok(false);
    };
    Ok(matching_keep(
        &frame.snapshot.canonical_inputs,
        &expected.worker_session_id,
        association.proof.attempt(),
    )
    .is_some())
}
