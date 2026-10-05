use sqlx::{Sqlite, Transaction};

use crate::error::Result;

/// `(created_at_ms, input_segments)` of the newest turn-input row a Planner session wrote for its
/// card (#2130 §5): the issuance projection or a steer row, in creation (`id`) order. `turn_id` is
/// not required (NULL until the echo); a row without segments is a non-projected echo and skipped.
/// Served by the `(card_id, id)` index, so the read inside a write lock does not scan.
pub async fn transcript_latest_turn_input_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    worker_session_id: &str,
) -> Result<Option<(i64, String)>> {
    Ok(sqlx::query_as(
        "SELECT created_at_ms, input_segments FROM harness_items \
         WHERE card_id = ?1 AND worker_session_id = ?2 \
           AND method = 'item/completed' AND item_type IN ('userMessage', 'user_message') \
           AND input_segments IS NOT NULL \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(card_id)
    .bind(worker_session_id)
    .fetch_optional(&mut **tx)
    .await?)
}
