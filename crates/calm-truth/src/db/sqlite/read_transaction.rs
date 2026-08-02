use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::error::Result;

/// Begin a provably read-only deferred transaction. Callers must only issue
/// SELECTs before commit; the source-scan invariant allowlists this one begin.
pub(super) async fn begin_read_tx(pool: &SqlitePool) -> Result<Transaction<'_, Sqlite>> {
    Ok(pool.begin().await?)
}
