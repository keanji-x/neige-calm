//! Settings reads composed into the caller's publication transaction.
use crate::error::Result;
use sqlx::{Sqlite, Transaction};

pub async fn settings_get_tx(
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
) -> Result<Option<String>> {
    Ok(sqlx::query_scalar("SELECT value FROM settings WHERE key=?")
        .bind(key)
        .fetch_optional(&mut **tx)
        .await?)
}
