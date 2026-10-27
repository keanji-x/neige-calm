//! Session-owned sticky backend bindings. PR1 exposes storage, never allocates bindings.
use super::SqliteDb;
use crate::error::Result;
use sqlx::{SqliteConnection, SqlitePool};

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

/// The sole Codex scope lookup; historical rows belong to the implicit default.
pub enum CodexScopeKey<'a> {
    Session(&'a str),
    Thread(&'a str),
}
#[derive(Debug, PartialEq, Eq)]
pub enum CodexScope {
    Bound(String),
    Absent,
}

pub async fn codex_backend_scope(db: &SqliteDb, key: CodexScopeKey<'_>) -> Result<CodexScope> {
    let (session, thread) = match key {
        CodexScopeKey::Session(id) => (Some(id), None),
        CodexScopeKey::Thread(id) => (None, Some(id)),
    };
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT COALESCE(b.backend_id, 'default')
         FROM worker_sessions s LEFT JOIN worker_session_backends b ON b.worker_session_id = s.id
         WHERE s.provider = 'codex' AND (s.id = ?1 OR s.thread_id = ?2)",
    )
    .bind(session)
    .bind(thread)
    .fetch_all(db.read_pool())
    .await?;
    match rows.as_slice() {
        [] => Ok(CodexScope::Absent),
        [id] => Ok(CodexScope::Bound(id.clone())),
        _ => Err(crate::TruthError::Internal(
            "conflicting Codex backend scopes".into(),
        )),
    }
}
