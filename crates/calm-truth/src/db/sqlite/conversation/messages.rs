//! The only V2 human body storage; revisions never fall back to envelopes.
use super::digest;
use crate::error::{CalmError, Result};
use crate::model::{new_id, now_ms};
use calm_types::conversation::{
    ConversationMessageId, HumanPayload, MessageAuthor, MessageRevisionRef, PrivateHistoryPayload,
};
use sqlx::{Row, Sqlite, Transaction};
use std::num::NonZeroU32;

#[derive(Clone, Debug, PartialEq)]
pub struct StoredMessageRevision {
    pub reference: MessageRevisionRef,
    pub body_sha256: String,
    pub payload: PrivateHistoryPayload<HumanPayload>,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredMessage {
    pub current: MessageRevisionRef,
    pub card_id: String,
    pub author: MessageAuthor,
    pub created_at_ms: i64,
    pub withdrawn_at_ms: Option<i64>,
}

/// Fact insert used only inside canonical acceptance. This deliberately
/// does not pretend that an inserted body is pending: pending exists only in the
/// committed harness snapshot written by the same transaction owner.
pub async fn insert_message_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    author: &MessageAuthor,
    payload: HumanPayload,
) -> Result<StoredMessageRevision> {
    let reference = MessageRevisionRef {
        message_id: ConversationMessageId::parse(&new_id()).map_err(invalid_identity)?,
        revision: NonZeroU32::MIN,
    };
    let MessageAuthor::AuthenticatedHuman { registered_user_id } = author;
    super::super::area_membership::application_user_require_registered(tx, registered_user_id)
        .await?;
    let now = now_ms();
    sqlx::query(
        "INSERT INTO conversation_messages(id,card_id,current_revision,author_kind,author_user_id,created_at_ms)
         VALUES (?1,?2,1,'authenticated_human',?3,?4)",
    )
    .bind(reference.message_id.as_str())
    .bind(card_id)
    .bind(registered_user_id)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    let stored = StoredMessageRevision {
        reference,
        body_sha256: digest(&payload)?,
        payload: PrivateHistoryPayload::Present { value: payload },
        created_at_ms: now,
    };
    insert_revision_tx(tx, &stored).await?;
    Ok(stored)
}

pub async fn message_tx(
    tx: &mut Transaction<'_, Sqlite>,
    message_id: &ConversationMessageId,
) -> Result<StoredMessage> {
    let row = sqlx::query(
        "SELECT card_id,current_revision,author_kind,author_user_id,created_at_ms,withdrawn_at_ms
         FROM conversation_messages WHERE id=?1",
    )
    .bind(message_id.as_str())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| CalmError::NotFound(format!("message {message_id}")))?;
    let author = match row.try_get::<&str, _>("author_kind")? {
        "authenticated_human" => MessageAuthor::AuthenticatedHuman {
            registered_user_id: row.try_get("author_user_id")?,
        },
        _ => {
            return Err(CalmError::Internal(
                "invalid canonical message author".into(),
            ));
        }
    };
    Ok(StoredMessage {
        current: MessageRevisionRef {
            message_id: message_id.clone(),
            revision: revision_number(row.try_get("current_revision")?)?,
        },
        card_id: row.try_get("card_id")?,
        author,
        created_at_ms: row.try_get("created_at_ms")?,
        withdrawn_at_ms: row.try_get("withdrawn_at_ms")?,
    })
}

pub async fn revision_tx(
    tx: &mut Transaction<'_, Sqlite>,
    reference: &MessageRevisionRef,
) -> Result<StoredMessageRevision> {
    let row: Option<(String, String, i64)> = sqlx::query_as(
        "SELECT body_sha256,payload_json,created_at_ms FROM conversation_message_revisions
         WHERE message_id=?1 AND revision=?2",
    )
    .bind(reference.message_id.as_str())
    .bind(i64::from(reference.revision.get()))
    .fetch_optional(&mut **tx)
    .await?;
    let (body_sha256, payload_json, created_at_ms) = row.ok_or_else(|| {
        CalmError::NotFound(format!(
            "message revision {}:{}",
            reference.message_id, reference.revision
        ))
    })?;
    decode_revision(reference.clone(), body_sha256, &payload_json, created_at_ms)
}

pub(crate) fn decode_revision(
    reference: MessageRevisionRef,
    body_sha256: String,
    payload_json: &str,
    created_at_ms: i64,
) -> Result<StoredMessageRevision> {
    let payload: PrivateHistoryPayload<HumanPayload> = serde_json::from_str(payload_json)?;
    if let PrivateHistoryPayload::Present { value } = &payload
        && digest(value)? != body_sha256
    {
        return Err(CalmError::Internal(
            "canonical message digest mismatch".into(),
        ));
    }
    Ok(StoredMessageRevision {
        reference,
        body_sha256,
        payload,
        created_at_ms,
    })
}

async fn insert_revision_tx(
    tx: &mut Transaction<'_, Sqlite>,
    revision: &StoredMessageRevision,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO conversation_message_revisions(message_id,revision,body_sha256,payload_json,
         created_at_ms)
         VALUES(?1,?2,?3,?4,?5)",
    )
    .bind(revision.reference.message_id.as_str())
    .bind(i64::from(revision.reference.revision.get()))
    .bind(&revision.body_sha256)
    .bind(serde_json::to_string(&revision.payload)?)
    .bind(revision.created_at_ms)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn invalid_identity(error: impl std::fmt::Display) -> CalmError {
    CalmError::Internal(error.to_string())
}
fn revision_number(value: i64) -> Result<NonZeroU32> {
    u32::try_from(value)
        .ok()
        .and_then(NonZeroU32::new)
        .ok_or_else(|| CalmError::Internal("invalid canonical revision".into()))
}
