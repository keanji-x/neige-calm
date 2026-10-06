//! What a fresh start on a card would lose (#2184): the send boundary starts a card only when
//! no carrier holds a thread and no transcript exists.
use sqlx::SqlitePool;

use crate::session_projection_repo::{CardConversation, Result};

/// The server's start adapter's operation kind (`PLANNER_HARNESS_START`).
pub const PLANNER_START_OPERATION_KIND: &str = "planner-harness-start";
/// The frozen payload key that start names its card under (`planner_card_id`'s serde name).
pub const PLANNER_START_CARD_KEY: &str = "spec_card_id";
/// The operation phases an operation is never driven out of. A `parked` one is still in flight:
/// the operation runtime's parked sweep completes or fails it, so it answers 503 like any other.
pub const TERMINAL_OPERATION_PHASES: [&str; 3] = ["succeeded", "failed", "stuck"];

/// A row preserves nothing only in the shape a failed start's compensation leaves it in
/// (`session_fail_if_active_tx`): `failed`, completed, with no `thread_id` and no snapshot
/// `last_thread_id`. Any other row, and any transcript row, is a conversation. A snapshot that
/// is not valid JSON may hold a thread, so it counts as one. A start is in flight while one of
/// the card's starts is outside [`TERMINAL_OPERATION_PHASES`].
pub(super) async fn card_conversation(
    pool: &SqlitePool,
    card_id: &str,
) -> Result<CardConversation> {
    let [succeeded, failed, stuck] = TERMINAL_OPERATION_PHASES;
    let (thread_to_preserve, start_in_flight, any_row): (bool, bool, bool) = sqlx::query_as(
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
                    WHERE kind = ?2
                      AND phase NOT IN (?3, ?4, ?5)
                      AND json_extract(payload_json, ?6) = ?1),
             EXISTS(SELECT 1 FROM worker_sessions WHERE card_id = ?1)"#,
    )
    .bind(card_id)
    .bind(PLANNER_START_OPERATION_KIND)
    .bind(succeeded)
    .bind(failed)
    .bind(stuck)
    .bind(format!("$.{PLANNER_START_CARD_KEY}"))
    .fetch_one(pool)
    .await?;
    Ok(if thread_to_preserve {
        CardConversation::ThreadToPreserve
    } else if start_in_flight {
        CardConversation::StartInFlight
    } else if any_row {
        CardConversation::OnlyFailedStarts
    } else {
        CardConversation::NeverStarted
    })
}
