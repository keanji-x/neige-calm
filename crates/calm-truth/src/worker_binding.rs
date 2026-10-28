//! A worker session's persisted binding to its card and attempt, read by the reaper's
//! dead-worker fence and by the Repo read that resolves a spawn op's attempt.
use sqlx::{Sqlite, SqliteExecutor, Transaction};

use crate::error::Result;

/// Whether `session_id` is `card_id`'s current session in `track_id`. Read under the caller's
/// write lock so a replacement that committed first is seen.
pub async fn session_is_card_current_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    session_id: &str,
    track_id: &str,
) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM cards WHERE id = ?1 AND session_id = ?2 AND track_id = ?3)",
    )
    .bind(card_id)
    .bind(session_id)
    .bind(track_id)
    .fetch_one(&mut **tx)
    .await?)
}

/// `worker_sessions.spawn_op_id` resolves to `operations.idempotency_key`, the immutable task id
/// the worker operation was submitted with.
pub async fn operation_idempotency_key<'e>(
    executor: impl SqliteExecutor<'e>,
    op_id: &str,
) -> Result<Option<String>> {
    let row: Option<Option<String>> =
        sqlx::query_scalar("SELECT idempotency_key FROM operations WHERE id = ?1")
            .bind(op_id)
            .fetch_optional(executor)
            .await?;
    Ok(row.flatten())
}
