//! Stopped deployment preparation and the earliest repository admission fence.
//! This slice has no completion, classification, export or application authority.
pub(crate) mod boot;
mod evidence;
pub mod state;

use super::{SqliteDb, database_identity, infra};
use crate::error::{CalmError, Result};

/// Install numbered schema only, without constructing a repository, seeding
/// caches, running semantic backfills or touching provider/runtime owners.
pub async fn install_schema(url: &str) -> Result<SqliteDb> {
    let db = SqliteDb::connect(url).await?;
    infra::check_no_unknown_future_migrations(db.read_pool(), &crate::MIGRATOR).await?;
    crate::MIGRATOR
        .run(db.read_pool())
        .await
        .map_err(|error| CalmError::Internal(format!("migrate: {error}")))?;
    Ok(db)
}

/// The operator must stop all deployment writers first. Success means the
/// required marker committed; it does not assert quiescence or qualify data.
pub async fn prepare(url: &str) -> Result<state::RequiredUpgrade> {
    let db = install_schema(url).await?;
    let result = async {
        let mut connection = db.read_pool().acquire().await?;
        let marker = state::read(&mut connection).await?;
        let deployment = if marker.is_some() {
            sqlx::query_scalar("SELECT id FROM database_identity WHERE singleton=1")
                .fetch_optional(&mut *connection)
                .await?
                .ok_or_else(|| state::refusal("required marker has no deployment identity"))?
        } else {
            database_identity::ensure_database_identity(db.writer_pool()).await?
        };
        drop(connection);
        state::prepare(&db, &deployment).await
    }
    .await;
    db.close().await;
    result
}

#[cfg(test)]
mod tests;
