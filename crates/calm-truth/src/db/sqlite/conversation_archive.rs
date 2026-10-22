//! Person-owned archive metadata. The caller owns card admission and its event transaction.
use crate::error::{CalmError, Result};
use sqlx::{Sqlite, Transaction};

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

/// Transaction-local archive admission shared by canonical enqueue and other
/// conversation mutations. Factual receipt replay does not call this guard.
pub async fn require_conversation_unarchived_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
) -> Result<()> {
    let archived: Option<i64> = sqlx::query_scalar(
        "SELECT archived_at_ms FROM assistant_conversation_archives WHERE card_id=?1",
    )
    .bind(card_id)
    .fetch_optional(&mut **tx)
    .await?;
    if archived.is_some() {
        return Err(CalmError::Conflict(
            "This conversation is archived. Restore it before continuing.",
        ));
    }
    Ok(())
}
