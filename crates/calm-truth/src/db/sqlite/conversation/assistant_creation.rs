//! Original Assistant entry acceptance; independent of subsequent queue/execution state.
use crate::error::{CalmError, Result};
use calm_types::conversation_receipt::ConversationInitialInputReceipt;
use sqlx::{Row, Sqlite, Transaction};

pub struct AssistantCreationRecord {
    pub card_id: String,
    pub track_id: String,
    pub request_sha256: String,
    pub source_user_id: String,
    pub presentation_sequence: i64,
    pub receipt: ConversationInitialInputReceipt,
}

pub async fn assistant_creation_get_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
) -> Result<Option<AssistantCreationRecord>> {
    sqlx::query("SELECT * FROM conversation_assistant_creations WHERE card_id=?1")
        .bind(card)
        .fetch_optional(&mut **tx)
        .await?
        .map(|row| {
            Ok(AssistantCreationRecord {
                card_id: row.try_get("card_id")?,
                track_id: row.try_get("track_id")?,
                request_sha256: row.try_get("request_sha256")?,
                source_user_id: row.try_get("source_user_id")?,
                presentation_sequence: row.try_get("presentation_sequence")?,
                receipt: serde_json::from_str(&row.try_get::<String, _>("receipt_json")?)?,
            })
        })
        .transpose()
}

pub async fn assistant_creation_insert_tx(
    tx: &mut Transaction<'_, Sqlite>,
    record: &AssistantCreationRecord,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO conversation_assistant_creations
        (card_id,track_id,request_sha256,source_user_id,origin_runtime_id,message_id,
         presentation_sequence,receipt_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
    )
    .bind(&record.card_id)
    .bind(&record.track_id)
    .bind(&record.request_sha256)
    .bind(&record.source_user_id)
    .bind(&record.receipt.initial_runtime_id)
    .bind(&record.receipt.entry_id)
    .bind(record.presentation_sequence)
    .bind(serde_json::to_string(&record.receipt)?)
    .execute(&mut **tx)
    .await
    .map_err(|error| CalmError::Internal(error.to_string()))?;
    Ok(())
}

/// The first input's frozen context is hydrated into its render plan, never a Human body.
pub(super) async fn creation_context_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    message: &str,
) -> Result<Option<String>> {
    // Only entries owning opening material consult this creation namespace.
    // Ordinary Planner plans have no dependency on Assistant receipt storage.
    let Some(context) = sqlx::query_scalar::<_, Option<String>>(
        "SELECT json_extract(payload,'$.assistant_opening_context') FROM cards WHERE id=?1",
    )
    .bind(card)
    .fetch_one(&mut **tx)
    .await?
    else {
        return Ok(None);
    };
    let original: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM conversation_assistant_creations WHERE card_id=?1 AND message_id=?2)",
    ).bind(card).bind(message).fetch_one(&mut **tx).await?;
    Ok(original.then_some(context))
}
