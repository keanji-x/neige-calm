//! Immutable submission facts. Only canonical claim owns these writes.
use super::digest;
use crate::error::{CalmError, Result};
use crate::model::now_ms;
use calm_types::conversation::{
    AcceptedInputRef, InputSubmissionId, LogicalTurnId, PendingInput, SubmissionRenderDigest,
    SubmissionRenderPlan,
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
    let original_digest = SubmissionRenderDigest::parse(&digest(plan)?)
        .map_err(|error| CalmError::Internal(error.to_string()))?;
    sqlx::query("INSERT INTO conversation_input_submissions(id,card_id,logical_turn_id,ordinal,kind,render_plan_json,
         render_plan_sha256,created_at_ms)
         VALUES(?1,?2,?3,0,'initial',?4,?5,?6)")
        .bind(submission_id.as_str()).bind(card_id).bind(turn_id.as_str())
        .bind(serde_json::to_string(plan)?).bind(original_digest.as_str())
        .bind(now_ms()).execute(&mut **tx).await?;
    for (position, entry) in entries.iter().enumerate() {
        let reference = entry.input_ref();
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
            _ => None,
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
