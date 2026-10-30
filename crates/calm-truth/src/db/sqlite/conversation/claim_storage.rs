//! Immutable submission facts. Only canonical claim owns these writes.
use super::digest;
use crate::error::{CalmError, Result};
use crate::model::now_ms;
use calm_types::conversation::{
    AcceptedInputRef, ConversationExecutionProvenance, InputSubmissionId, LogicalTurnId,
    PendingInput, SubmissionRenderDigest, SubmissionRenderPlan,
};
use sqlx::{Sqlite, Transaction};

pub(super) async fn insert_submission_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    turn_id: &LogicalTurnId,
    submission_id: &InputSubmissionId,
    plan: &SubmissionRenderPlan,
    entries: &[PendingInput],
) -> Result<SubmissionRenderDigest> {
    insert_submission_at_tx(tx, card_id, turn_id, submission_id, plan, entries, 0).await
}

pub(super) async fn insert_submission_at_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    turn_id: &LogicalTurnId,
    submission_id: &InputSubmissionId,
    plan: &SubmissionRenderPlan,
    entries: &[PendingInput],
    ordinal: u32,
) -> Result<SubmissionRenderDigest> {
    let original_digest = SubmissionRenderDigest::parse(&digest(plan)?)
        .map_err(|error| CalmError::Internal(error.to_string()))?;
    sqlx::query("INSERT INTO conversation_input_submissions(id,card_id,logical_turn_id,ordinal,kind,render_plan_json,
         render_plan_sha256,render_plan_root_submission_id,created_at_ms)
         VALUES(?1,?2,?3,?7,?8,?4,?5,NULL,?6)")
        .bind(submission_id.as_str()).bind(card_id).bind(turn_id.as_str())
        .bind(serde_json::to_string(plan)?).bind(original_digest.as_str())
        .bind(now_ms()).bind(i64::from(ordinal)).bind(if ordinal == 0 { "initial" } else { "supplemental" }).execute(&mut **tx).await?;
    for (position, entry) in entries.iter().enumerate() {
        let reference = entry.accepted_input_ref().ok_or(CalmError::Conflict(
            "initial claim requires accepted input provenance",
        ))?;
        let (kind, message, revision, observation) = match &reference {
            AcceptedInputRef::Human { message } => (
                "human",
                Some(message.message_id.as_str()),
                Some(i64::from(message.revision.get())),
                None,
            ),
            AcceptedInputRef::System { observation_id } => {
                ("system", None, None, Some(observation_id.as_str()))
            }
        };
        let addressable = match entry {
            PendingInput::System { addressable, .. } => Some(i64::from(*addressable)),
            PendingInput::Human { .. } => None,
            PendingInput::ExplicitSuccessor { .. } => {
                return Err(CalmError::Conflict(
                    "ordinary claim cannot reinterpret a retry intent",
                ));
            }
        };
        sqlx::query("INSERT INTO conversation_submission_inputs(submission_id,input_position,input_kind,message_id,
         revision,observation_id,queue_position,queued_at_ms,envelope_id,system_addressable,effective)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,1)")
            .bind(submission_id.as_str()).bind(i64::try_from(position).map_err(|_| CalmError::Conflict("submission input count exceeds storage range"))?)
            .bind(kind).bind(message).bind(revision).bind(observation)
            .bind(i64::try_from(entry.queue_position()).map_err(|_| CalmError::Conflict("pending input order exceeds storage range"))?)
            .bind(entry.queued_at_ms()).bind(entry.envelope_id()).bind(addressable)
            .execute(&mut **tx).await?;
    }
    Ok(original_digest)
}

/// The same Initial turn writer serves ordinary claims and reference claims;
/// callers retain admission and reserve identities in their own transaction.
pub(super) async fn insert_turn_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    carrier_id: &str,
    logical_turn_id: &LogicalTurnId,
    submission_id: &InputSubmissionId,
    provenance: &ConversationExecutionProvenance,
) -> Result<()> {
    let ConversationExecutionProvenance::LegacySingleUserDeployment {
        deployment_owner_user_id,
        provider,
        profile,
    } = provenance;
    let profile = serde_json::to_value(profile)?;
    sqlx::query("INSERT INTO conversation_turns(id,card_id,initiating_runtime_id,initial_submission_id,execution_kind,
         deployment_owner_user_id,provider,profile,created_at_ms)
         VALUES(?1,?2,?3,?4,'legacy_single_user_deployment',?5,?6,?7,?8)")
        .bind(logical_turn_id.as_str()).bind(card_id).bind(carrier_id).bind(submission_id.as_str())
        .bind(deployment_owner_user_id).bind(provider.wire_name()).bind(profile.as_str().ok_or_else(|| CalmError::Internal("invalid persisted harness profile".into()))?)
        .bind(now_ms()).execute(&mut **tx).await?;
    Ok(())
}

pub(super) async fn insert_retry_submission_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    turn: &LogicalTurnId,
    submission: &InputSubmissionId,
    id: &calm_types::conversation::RetryIntentId,
    intent: &super::retry::StoredRetryIntent,
) -> Result<SubmissionRenderDigest> {
    sqlx::query(
        "INSERT INTO conversation_input_submissions(id,card_id,logical_turn_id,ordinal,kind,
        render_plan_json,render_plan_root_submission_id,retry_intent_id,render_plan_sha256,
        created_at_ms) VALUES(?1,?2,?3,0,'initial',NULL,?4,?5,?6,?7)",
    )
    .bind(submission.as_str())
    .bind(card)
    .bind(turn.as_str())
    .bind(intent.root_submission_id.as_str())
    .bind(id.as_str())
    .bind(intent.plan.original_digest.as_str())
    .bind(now_ms())
    .execute(&mut **tx)
    .await?;
    let copied = sqlx::query("INSERT INTO conversation_submission_inputs(submission_id,input_position,input_kind,message_id,
        revision,observation_id,queue_position,queued_at_ms,envelope_id,system_addressable,effective)
        SELECT ?1,input_position,input_kind,message_id,revision,observation_id,queue_position,queued_at_ms,envelope_id,system_addressable,1 FROM conversation_submission_inputs WHERE submission_id=?2 AND effective=0 AND retirement_reason='human_resolution' AND retired_by_retry_intent_id=?3")
        .bind(submission.as_str()).bind(intent.predecessor_submission_id.as_str()).bind(id.as_str()).execute(&mut **tx).await?;
    if usize::try_from(copied.rows_affected()).ok()
        != Some(
            intent
                .plan
                .plan
                .input_refs()
                .map_err(CalmError::Conflict)?
                .len(),
        )
    {
        return Err(CalmError::Conflict(
            "retry predecessor claims no longer match its intent",
        ));
    }
    Ok(intent.plan.original_digest.clone())
}
