//! Session-owned sticky backend bindings. PR1 exposes storage, never allocates bindings.
use super::SqliteDb;
use crate::error::Result;
use sqlx::{SqliteConnection, SqlitePool};

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct BackendBinding {
    pub provider: String,
    pub backend_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionBackend {
    Bound(BackendBinding),
    Unpooled,
}

pub async fn insert_tx(
    conn: &mut SqliteConnection,
    worker_session_id: &str,
    provider: &str,
    backend_id: &str,
) -> Result<()> {
    sqlx::query("INSERT INTO worker_session_backends (worker_session_id, provider, backend_id) VALUES (?, ?, ?)")
        .bind(worker_session_id).bind(provider).bind(backend_id).execute(conn).await?;
    Ok(())
}

pub async fn binding_for_session(db: &SqliteDb, session: &str) -> Result<SessionBackend> {
    let binding = sqlx::query_as::<_, BackendBinding>(
        "SELECT provider, backend_id FROM worker_session_backends WHERE worker_session_id = ?",
    )
    .bind(session)
    .fetch_optional(db.read_pool())
    .await?;
    Ok(match binding {
        Some(binding) => SessionBackend::Bound(binding),
        None => SessionBackend::Unpooled,
    })
}

pub async fn live_bound_counts_tx(
    conn: &mut SqliteConnection,
    provider: &str,
) -> Result<Vec<(String, i64)>> {
    Ok(sqlx::query_as(
        "SELECT b.backend_id, COUNT(*) FROM worker_session_backends b
        JOIN worker_sessions s ON s.id = b.worker_session_id
        WHERE b.provider = ? AND s.state NOT IN ('exited', 'failed', 'superseded')
        GROUP BY b.backend_id",
    )
    .bind(provider)
    .fetch_all(conn)
    .await?)
}

/// Only explicit live bindings. Missing entries remain historical/default;
/// callers combine this with the existing attribution owner, not a parallel scan.
pub async fn live_bound_threads(
    pool: &SqlitePool,
    provider: &str,
) -> Result<Vec<(String, String)>> {
    Ok(sqlx::query_as(
        "SELECT s.thread_id, b.backend_id FROM worker_session_backends b
        JOIN worker_sessions s ON s.id = b.worker_session_id
        WHERE b.provider = ? AND s.thread_id IS NOT NULL
        AND s.state NOT IN ('exited', 'failed', 'superseded')",
    )
    .bind(provider)
    .fetch_all(pool)
    .await?)
}
