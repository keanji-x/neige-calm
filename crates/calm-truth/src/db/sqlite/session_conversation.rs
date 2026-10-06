//! What a fresh start on a card would lose (#2184): the send boundary starts a card only when
//! no carrier holds a thread and no transcript exists.
use sqlx::SqlitePool;

use crate::session_projection_repo::{CardConversation, Result};

/// A row preserves nothing only in the shape a failed start's compensation leaves it in
/// (`session_fail_if_active_tx`): `failed`, completed, with no `thread_id` and no snapshot
/// `last_thread_id`. Any other row, and any transcript row, is a conversation. A snapshot that
/// is not valid JSON may hold a thread, so it counts as one. `planner-harness-start` and its
/// frozen payload key for the card are the start adapter's; `succeeded`, `failed` and `stuck`
/// are the phases an operation is never driven out of.
pub(super) async fn card_conversation(
    pool: &SqlitePool,
    card_id: &str,
) -> Result<CardConversation> {
    let (thread_to_preserve, start_in_flight): (bool, bool) = sqlx::query_as(
        r#"SELECT
             EXISTS(SELECT 1 FROM worker_sessions
                    WHERE card_id = ?1
                      AND NOT (state = 'failed'
                               AND completed_at_ms IS NOT NULL
                               AND trim(COALESCE(thread_id, '')) = ''
                               AND (handle_state_json IS NULL
                                    OR (json_valid(handle_state_json)
                                        AND trim(COALESCE(json_extract(handle_state_json, '$.last_thread_id'), '')) = ''))))
               OR EXISTS(SELECT 1 FROM harness_items WHERE card_id = ?1),
             EXISTS(SELECT 1 FROM operations
                    WHERE kind = 'planner-harness-start'
                      AND phase NOT IN ('succeeded', 'failed', 'stuck')
                      AND json_extract(payload_json, '$.spec_card_id') = ?1)"#,
    )
    .bind(card_id)
    .fetch_one(pool)
    .await?;
    Ok(if thread_to_preserve {
        CardConversation::ThreadToPreserve
    } else if start_in_flight {
        CardConversation::StartInFlight
    } else {
        CardConversation::NoThreadToPreserve
    })
}
