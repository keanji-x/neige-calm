//! The stable database identity: one row, minted by the first open and read back by every
//! later one. Two autocommit statements, no explicit transaction; `INSERT OR IGNORE` on the fixed primary key makes the mint idempotent.

use sqlx::SqlitePool;

use crate::error::Result;
use crate::model::now_ms;

/// Mint the identity if the row does not exist yet, then read it back.
pub(super) async fn ensure_database_identity(pool: &SqlitePool) -> Result<String> {
    sqlx::query(
        "INSERT OR IGNORE INTO database_identity(singleton, id, minted_at_ms) \
         VALUES (1, ?1, ?2)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(now_ms())
    .execute(pool)
    .await?;
    let id: String = sqlx::query_scalar("SELECT id FROM database_identity WHERE singleton = 1")
        .fetch_one(pool)
        .await?;
    Ok(id)
}
