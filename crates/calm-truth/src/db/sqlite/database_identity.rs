//! #1722 S1b — the stable database identity (`database_identity`, migration
//! 0110).
//!
//! One row, minted by the first open after the migration and read back by
//! every later one. It names the *database*: a client that keys per-database
//! state (read receipts, baselines) on it keeps that state across process
//! restarts, where `db_instance_id` — a fresh UUID per boot — would discard
//! it. Both are served by `GET /api/version`; neither replaces the other.
//!
//! Two autocommit statements, no explicit transaction (#930: production must
//! not open a deferred transaction, and there is nothing here to make
//! atomic). `INSERT OR IGNORE` on the fixed primary key is what makes the mint
//! idempotent: the first writer's row stays, every later writer's insert is
//! ignored, and the `SELECT` reads the one row back. Two boots racing on the
//! same file serialize on sqlite's write lock and read the same id.

use sqlx::SqlitePool;

use crate::error::Result;
use crate::model::now_ms;

/// Mint the identity if the row does not exist yet, then read it back.
///
/// Called once per [`super::SqlxRepo::open`], after migrations. The uuid is
/// generated before the statement and discarded when the row already exists.
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
