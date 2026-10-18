//! Execution persistence SQL. Domain transactions reuse these writes; no lifecycle policy here.
use sqlx::{Sqlite, SqlitePool, Transaction};

/// Sole mapping between Terminal identity and the execution alias namespace.
pub fn terminal_alias(id: &str) -> String {
    format!("term:{id}")
}
pub const TERMINAL_ALIAS_SQL: &str = "'term:' || terminals.id";

#[derive(sqlx::FromRow)]
pub struct ExecutionRow {
    pub intent_json: String,
    pub secrets_json: Option<String>,
    pub desired: String,
    pub settlement_json: Option<String>,
}

pub async fn insert(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    track: &str,
    alias: &str,
    intent: &str,
    secrets: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO workspace_executions
            (execution_id,track_id,alias,intent_json,secrets_json,desired)
         VALUES (?,?,?,?,?,'run')",
    )
    .bind(id)
    .bind(track)
    .bind(alias)
    .bind(intent)
    .bind(secrets)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn request(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    desired: &str,
) -> Result<(), sqlx::Error> {
    let result = sqlx::query(
        "UPDATE workspace_executions SET desired = CASE
            WHEN desired = 'retire' OR ? = 'retire' THEN 'retire'
            WHEN desired = 'stop' OR ? = 'stop' THEN 'stop' ELSE 'run' END
         WHERE execution_id = ?",
    )
    .bind(desired)
    .bind(desired)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if result.rows_affected() == 0 {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}

pub async fn load(pool: &SqlitePool, id: &str) -> Result<Option<ExecutionRow>, sqlx::Error> {
    sqlx::query_as("SELECT intent_json,secrets_json,desired,settlement_json FROM workspace_executions WHERE execution_id = ?")
        .bind(id).fetch_optional(pool).await
}

pub async fn settle(
    pool: &SqlitePool,
    id: &str,
    settlement: &str,
    at_ms: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE workspace_executions SET settled_at_ms = ?, settlement_json = ? WHERE execution_id = ? AND settled_at_ms IS NULL")
        .bind(at_ms).bind(settlement).bind(id).execute(pool).await?;
    Ok(())
}

pub async fn finish(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    // The CHECK rejects finishing before settlement; exact replays remain harmless.
    let result =
        sqlx::query("UPDATE workspace_executions SET secrets_json = NULL WHERE execution_id = ?")
            .bind(id)
            .execute(pool)
            .await?;
    if result.rows_affected() == 0 {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}

pub async fn pending(pool: &SqlitePool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT execution_id FROM workspace_executions
         WHERE settled_at_ms IS NULL OR (desired = 'retire' AND secrets_json IS NOT NULL)
         ORDER BY execution_id",
    )
    .fetch_all(pool)
    .await
}

/// Occupancy scopes; lifecycle policy belongs to callers.
pub enum Scope<'a> {
    Track(&'a str),
    Area(&'a str),
    Alias(&'a str),
}

pub async fn unsettled_executions(
    conn: &mut sqlx::SqliteConnection,
    scope: Scope<'_>,
) -> crate::error::Result<Vec<workspace::execution::ExecutionId>> {
    let (predicate, value) = match scope {
        Scope::Track(id) => ("e.track_id = ?", id),
        Scope::Area(id) => (
            "e.track_id IN (SELECT id FROM tracks WHERE area_id = ?)",
            id,
        ),
        Scope::Alias(alias) => ("e.alias = ?", alias),
    };
    let query = format!(
        "SELECT e.execution_id FROM workspace_executions e WHERE e.settled_at_ms IS NULL AND {predicate} ORDER BY e.execution_id"
    );
    let ids: Vec<String> = sqlx::query_scalar(&query)
        .bind(value)
        .fetch_all(conn)
        .await?;
    ids.into_iter()
        .map(|id| id.try_into().map_err(crate::error::CalmError::Internal))
        .collect()
}

/// Current alias slot, excluding historical retired executions.
pub async fn current(
    tx: &mut Transaction<'_, Sqlite>,
    alias: &str,
) -> crate::error::Result<Option<workspace::execution::ExecutionId>> {
    let id: Option<String> = sqlx::query_scalar(
        "SELECT execution_id FROM workspace_executions WHERE alias = ? AND desired <> 'retire'",
    )
    .bind(alias)
    .fetch_optional(&mut **tx)
    .await?;
    id.map(|id| id.try_into().map_err(crate::error::CalmError::Internal))
        .transpose()
}
