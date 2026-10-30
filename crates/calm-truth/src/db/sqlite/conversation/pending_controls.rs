//! Exact pending Human mutations. Message versions and the sole queue commit together.
use super::carrier::save_input_carrier_tx;
use super::*;
use crate::db::PendingHumanMessage;
use calm_types::conversation::{
    HumanPayload, MessageAuthor, MessageRevisionRef, PendingInput, PrivateHistoryPayload,
};

#[cfg(test)]
mod tests;

pub async fn pending_human_messages_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    pending: &[PendingInput],
) -> Result<Vec<PendingHumanMessage>> {
    let mut messages = Vec::new();
    for entry in pending {
        let PendingInput::Human {
            message,
            queued_at_ms,
            ..
        } = entry
        else {
            continue;
        };
        let current = message_tx(tx, &message.message_id).await?;
        if current.card_id != card
            || current.current != *message
            || current.withdrawn_at_ms.is_some()
        {
            return Err(CalmError::Conflict(
                "pending Human revision is no longer current",
            ));
        }
        let PrivateHistoryPayload::Present { value } = revision_tx(tx, message).await?.payload
        else {
            continue;
        };
        let MessageAuthor::AuthenticatedHuman { registered_user_id } = current.author;
        messages.push(PendingHumanMessage {
            reference: message.clone(),
            author_user_id: registered_user_id,
            payload: value,
            queued_at_ms: *queued_at_ms,
        });
    }
    Ok(messages)
}

pub enum PendingHumanChange {
    Edit { text: String },
    Withdraw,
}

/// An absent row is already claimed/withdrawn; a stale row carries its real current revision.
pub enum PendingHumanWrite {
    Applied {
        previous: MessageRevisionRef,
        current: Option<MessageRevisionRef>,
        payload: HumanPayload,
    },
    Stale(PendingHumanMessage),
    Gone,
}

pub async fn mutate_pending_human_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &mut CurrentInputCarrier,
    actor: &str,
    expected: &MessageRevisionRef,
    change: PendingHumanChange,
) -> Result<PendingHumanWrite> {
    let identity = message_tx(tx, &expected.message_id).await?;
    if identity.card_id != carrier.card_id()
        || identity.author
            != (MessageAuthor::AuthenticatedHuman {
                registered_user_id: actor.into(),
            })
    {
        return Err(CalmError::Forbidden(
            "only the original author can change pending Human input".into(),
        ));
    }
    let Some(index) = carrier
        .checkpoint
        .pending
        .iter()
        .position(|input| input.message_id() == Some(&expected.message_id))
    else {
        return Ok(PendingHumanWrite::Gone);
    };
    let entry = carrier.checkpoint.pending[index].clone();
    let PendingInput::Human {
        message,
        queued_at_ms,
        ..
    } = &entry
    else {
        unreachable!()
    };
    if identity.withdrawn_at_ms.is_some() || identity.current != *message {
        return Ok(PendingHumanWrite::Gone);
    }
    let PrivateHistoryPayload::Present { value: mut payload } =
        revision_tx(tx, message).await?.payload
    else {
        return Ok(PendingHumanWrite::Gone);
    };
    if message != expected {
        return Ok(PendingHumanWrite::Stale(PendingHumanMessage {
            reference: message.clone(),
            author_user_id: actor.into(),
            payload,
            queued_at_ms: *queued_at_ms,
        }));
    }
    // The pending checkpoint and effective claim are both fenced in this transaction.
    let claimed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM conversation_submission_inputs WHERE message_id=?1 AND effective=1)")
        .bind(expected.message_id.as_str()).fetch_one(&mut **tx).await?;
    if claimed {
        return Err(CalmError::Conflict(
            "pending message already has an execution claim",
        ));
    }
    let current = match change {
        PendingHumanChange::Edit { text } => {
            if text.trim().is_empty() && payload.bound_attachments.is_empty() {
                return Err(CalmError::BadRequest(
                    "a message must contain text or attachments",
                ));
            }
            let revision = expected
                .revision
                .get()
                .checked_add(1)
                .and_then(std::num::NonZeroU32::new)
                .ok_or(CalmError::Conflict("message revision exhausted"))?;
            payload.text = text;
            let reference = MessageRevisionRef {
                message_id: expected.message_id.clone(),
                revision,
            };
            super::messages::insert_revision_tx(
                tx,
                &StoredMessageRevision {
                    reference: reference.clone(),
                    body_sha256: digest(&payload)?,
                    payload: PrivateHistoryPayload::Present {
                        value: payload.clone(),
                    },
                    created_at_ms: crate::model::now_ms(),
                },
            )
            .await?;
            sqlx::query(
                "UPDATE conversation_messages SET current_revision=?1
                 WHERE id=?2 AND current_revision=?3 AND withdrawn_at_ms IS NULL",
            )
            .bind(i64::from(revision.get()))
            .bind(expected.message_id.as_str())
            .bind(i64::from(expected.revision.get()))
            .execute(&mut **tx)
            .await?;
            let PendingInput::Human { message, .. } = &mut carrier.checkpoint.pending[index] else {
                unreachable!()
            };
            *message = reference.clone();
            Some(reference)
        }
        PendingHumanChange::Withdraw => {
            sqlx::query(
                "UPDATE conversation_messages SET withdrawn_at_ms=?1
                 WHERE id=?2 AND current_revision=?3 AND withdrawn_at_ms IS NULL",
            )
            .bind(crate::model::now_ms())
            .bind(expected.message_id.as_str())
            .bind(i64::from(expected.revision.get()))
            .execute(&mut **tx)
            .await?;
            carrier.checkpoint.pending.remove(index);
            None
        }
    };
    save_input_carrier_tx(tx, carrier).await?;
    Ok(PendingHumanWrite::Applied {
        previous: expected.clone(),
        current,
        payload,
    })
}
