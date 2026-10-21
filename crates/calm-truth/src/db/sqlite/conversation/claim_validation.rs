//! Canonical source/current-revision checks for Initial submissions.
use super::{CurrentInputCarrier, message_tx, revision_tx};
use crate::error::{CalmError, Result};
use calm_types::conversation::{
    AcceptedInputRef, MessageAuthor, PendingInput, PrivateHistoryPayload, SubmissionRenderInput,
    SubmissionRenderPlan, SubmissionRenderSegment,
};
use sqlx::{Sqlite, Transaction};

pub(super) async fn validate_claim_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &CurrentInputCarrier,
    plan: &SubmissionRenderPlan,
    entries: &[PendingInput],
) -> Result<Vec<AcceptedInputRef>> {
    let refs = plan.input_refs().map_err(CalmError::Conflict)?;
    let expected = entries
        .iter()
        .map(PendingInput::accepted_input_ref)
        .collect::<Option<Vec<_>>>();
    if expected.as_ref() != Some(&refs) {
        return Err(CalmError::Conflict(
            "render plan does not select the exact pending revisions in order",
        ));
    }
    for (position, entry) in entries.iter().enumerate() {
        let input = plan
            .segments()
            .iter()
            .filter_map(|segment| match segment {
                SubmissionRenderSegment::Input { input, .. } => Some(input),
                _ => None,
            })
            .nth(position)
            .expect("validated plan positions select this input");
        match (entry, input) {
            (PendingInput::Human { message, .. }, SubmissionRenderInput::Human { author, .. }) => {
                let current = message_tx(tx, &message.message_id).await?;
                if current.card_id != carrier.card_id
                    || current.current != *message
                    || current.withdrawn_at_ms.is_some()
                {
                    return Err(CalmError::Conflict(
                        "human pending revision changed or belongs to another conversation",
                    ));
                }
                if current.author
                    != (MessageAuthor::AuthenticatedHuman {
                        registered_user_id: author.registered_user_id.clone(),
                    })
                {
                    return Err(CalmError::Conflict(
                        "human render attribution does not match canonical provenance",
                    ));
                }
                let name: Option<String> =
                    sqlx::query_scalar("SELECT display_name FROM application_users WHERE id=?1")
                        .bind(&author.registered_user_id)
                        .fetch_optional(&mut **tx)
                        .await?;
                if name.as_ref() != Some(&author.display_name) {
                    return Err(CalmError::Conflict(
                        "human render label changed before claim",
                    ));
                }
                if !matches!(
                    revision_tx(tx, message).await?.payload,
                    PrivateHistoryPayload::Present { .. }
                ) {
                    return Err(CalmError::Conflict("erased human input cannot be claimed"));
                }
            }
            (
                PendingInput::System { observation, .. },
                SubmissionRenderInput::System {
                    payload: PrivateHistoryPayload::Present { value },
                    ..
                },
            ) => {
                if observation != value {
                    return Err(CalmError::Conflict(
                        "pending input and submitted source/payload kind disagree",
                    ));
                }
            }
            (PendingInput::Human { .. }, SubmissionRenderInput::System { .. })
            | (PendingInput::System { .. }, SubmissionRenderInput::Human { .. })
            | (
                PendingInput::System { .. },
                SubmissionRenderInput::System {
                    payload: PrivateHistoryPayload::ErasedByPrivateHistoryOperation { .. },
                    ..
                },
            ) => {
                return Err(CalmError::Conflict(
                    "pending input and submitted source/payload kind disagree",
                ));
            }
            // Successors claim only through the relation-backed InitialClaim path.
            (
                PendingInput::ExplicitSuccessor { .. },
                SubmissionRenderInput::Human { .. } | SubmissionRenderInput::System { .. },
            ) => {
                return Err(CalmError::Conflict(
                    "pending input and submitted source/payload kind disagree",
                ));
            }
        }
        let claimed: bool = match &refs[position] {
            AcceptedInputRef::Human { message } => sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM conversation_submission_inputs WHERE message_id=?1 AND effective=1)")
                .bind(message.message_id.as_str()).fetch_one(&mut **tx).await?,
            AcceptedInputRef::System { observation_id } => sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM conversation_submission_inputs WHERE observation_id=?1 AND effective=1)")
                .bind(observation_id.as_str()).fetch_one(&mut **tx).await?,
        };
        if claimed {
            return Err(CalmError::Conflict(
                "accepted input already has an effective claim",
            ));
        }
        i64::try_from(entry.queue_position())
            .map_err(|_| CalmError::Conflict("pending input order exceeds storage range"))?;
    }
    if plan.segments().iter().any(|segment| {
        matches!(
            segment,
            SubmissionRenderSegment::Context {
                payload: PrivateHistoryPayload::ErasedByPrivateHistoryOperation { .. }
            }
        )
    }) {
        return Err(CalmError::Conflict(
            "erased turn context cannot be dispatched",
        ));
    }
    Ok(refs)
}
