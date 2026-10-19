//! Person-owned archive metadata. The caller owns card admission and its event transaction.
use crate::error::Result;
use sqlx::{Sqlite, Transaction};

/// The archive owner's query, usable on a read pool or the caller's writer transaction.
pub async fn conversation_archived_at<'e, E: sqlx::SqliteExecutor<'e>>(
    executor: E,
    card_id: &str,
) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT archived_at_ms FROM assistant_conversation_archives WHERE card_id=?1",
    )
    .bind(card_id)
    .fetch_optional(executor)
    .await?)
}

pub async fn conversation_archive_set_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    archived: bool,
) -> Result<bool> {
    let changed = if archived {
        sqlx::query("INSERT INTO assistant_conversation_archives(card_id,archived_at_ms) VALUES(?1,?2) ON CONFLICT(card_id) DO NOTHING")
            .bind(card_id).bind(crate::model::now_ms()).execute(&mut **tx).await?
    } else {
        sqlx::query("DELETE FROM assistant_conversation_archives WHERE card_id=?1")
            .bind(card_id)
            .execute(&mut **tx)
            .await?
    };
    Ok(changed.rows_affected() != 0)
}
