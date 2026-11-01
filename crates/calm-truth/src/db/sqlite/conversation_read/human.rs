//! Shared Human presentation projection, independent of native/system display bodies.
use crate::error::{CalmError, Result};
use calm_types::conversation::{ConversationMessageId, MessageRevisionRef, PrivateHistoryPayload};
use calm_types::conversation_display::*;
use sqlx::{Row, SqliteConnection, sqlite::SqliteRow};

pub(super) const COLUMNS: &str = r#"m.id AS message_id,m.current_revision,m.author_kind,m.author_user_id,m.withdrawn_at_ms,
                u.display_name,r.body_sha256,r.payload_json,r.created_at_ms AS revision_created"#;
pub(super) const JOINS: &str = r#"LEFT JOIN conversation_messages m ON m.id=p.message_id AND m.card_id=p.card_id
         LEFT JOIN application_users u ON u.id=m.author_user_id
         LEFT JOIN conversation_message_revisions r ON r.message_id=m.id AND r.revision=m.current_revision"#;

pub(super) fn decode(row: &SqliteRow, card: &str) -> Result<ConversationDisplaySource> {
    let removed = row
        .try_get::<Option<String>, _>("erased_by_operation_id")?
        .is_some();
    Ok({
        let message_id: String = row.try_get("message_id")?;
        let identity = ConversationDisplayIdentity::Human {
            message_id: message_id.clone(),
        };
        if row.try_get::<&str, _>("author_kind")? != "authenticated_human" {
            return Err(CalmError::Conflict(
                "canonical Legacy display requires its declared adoption",
            ));
        }
        if removed || row.try_get::<Option<i64>, _>("withdrawn_at_ms")?.is_some() {
            ConversationDisplaySource::Removed { identity }
        } else {
            let revision = u32::try_from(row.try_get::<i64, _>("current_revision")?)
                .ok()
                .and_then(std::num::NonZeroU32::new)
                .ok_or_else(|| CalmError::Internal("invalid message revision".into()))?;
            let reference = MessageRevisionRef {
                message_id: ConversationMessageId::parse(&message_id)
                    .map_err(|e| CalmError::Internal(e.to_string()))?,
                revision,
            };
            let stored = super::super::conversation::messages::decode_revision(
                reference,
                row.try_get("body_sha256")?,
                row.try_get("payload_json")?,
                row.try_get("revision_created")?,
            )?;
            match stored.payload {
                PrivateHistoryPayload::Present { value } => ConversationDisplaySource::Human {
                    message_id,
                    revision: revision.get(),
                    author: ConversationHumanAuthor {
                        user_id: row.try_get("author_user_id")?,
                        display_name: row.try_get("display_name")?,
                    },
                    text: value.text,
                    attachments: value
                        .bound_attachments
                        .iter()
                        .map(|a| a.wire(&card.into()))
                        .collect(),
                    created_at_ms: stored.created_at_ms,
                },
                PrivateHistoryPayload::ErasedByPrivateHistoryOperation { .. } => {
                    ConversationDisplaySource::Removed { identity }
                }
            }
        }
    })
}

fn candidate_query() -> String {
    format!(
        "SELECT p.sequence,p.erased_by_operation_id,{COLUMNS}
         FROM conversation_presentations p {JOINS}
         WHERE p.card_id=?1 AND p.source_kind='human_message' AND p.sequence>?2
         ORDER BY p.sequence LIMIT ?3"
    )
}

/// Advance only Human candidates, preserving the first Human even when its text
/// cannot name the conversation. Tombstones advance; malformed references fail closed.
/// The caller owns the snapshot/admission contract, shared with runtime metadata.
pub(in crate::db::sqlite) async fn first_title_on_connection(
    connection: &mut SqliteConnection,
    card: &str,
) -> Result<Option<String>> {
    const BATCH: i64 = 32;
    let sql = candidate_query();
    let mut after = 0;
    loop {
        let rows = sqlx::query(&sql)
            .bind(card)
            .bind(after)
            .bind(BATCH)
            .fetch_all(&mut *connection)
            .await?;
        for row in &rows {
            after = row.try_get("sequence")?;
            if let ConversationDisplaySource::Human { text, .. } = decode(row, card)? {
                return Ok(crate::conversation_title::conversation_title(&text));
            }
        }
        if rows.len() < BATCH as usize {
            return Ok(None);
        }
    }
}

#[cfg(test)]
mod tests;
