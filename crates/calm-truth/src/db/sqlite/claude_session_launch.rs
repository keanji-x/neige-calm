//! Claude launch evidence storage. A `--session-id` launch is recorded before admission and
//! retired by its bind, by an init-free clean end of its output, or by a refusal before its
//! release; Claude owns classification.
use crate::error::Result;
use sqlx::SqliteConnection;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ClaudeSessionLaunch {
    pub worker_session_id: String,
    pub card_id: String,
    pub track_id: String,
    pub thread_id: String,
    pub execution_id: String,
}

/// One unretired launch per worker session; a second insert fails rather than replace it.
pub async fn insert(conn: &mut SqliteConnection, launch: &ClaudeSessionLaunch) -> Result<()> {
    sqlx::query(
        "INSERT INTO claude_session_launches
        (worker_session_id, card_id, track_id, thread_id, execution_id, created_at_ms)
        VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&launch.worker_session_id)
    .bind(&launch.card_id)
    .bind(&launch.track_id)
    .bind(&launch.thread_id)
    .bind(&launch.execution_id)
    .bind(crate::model::now_ms())
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn get(
    conn: &mut SqliteConnection,
    worker_session_id: &str,
) -> Result<Option<ClaudeSessionLaunch>> {
    Ok(sqlx::query_as(
        "SELECT worker_session_id, card_id, track_id, thread_id, execution_id
        FROM claude_session_launches WHERE worker_session_id = ?",
    )
    .bind(worker_session_id)
    .fetch_optional(conn)
    .await?)
}

pub async fn delete(conn: &mut SqliteConnection, worker_session_id: &str) -> Result<()> {
    sqlx::query("DELETE FROM claude_session_launches WHERE worker_session_id = ?")
        .bind(worker_session_id)
        .execute(conn)
        .await?;
    Ok(())
}

/// Retire only the named launch, so an older execution's end never retires a newer launch.
pub async fn retire(
    conn: &mut SqliteConnection,
    worker_session_id: &str,
    execution_id: &str,
) -> Result<bool> {
    Ok(sqlx::query(
        "DELETE FROM claude_session_launches WHERE worker_session_id = ? AND execution_id = ?",
    )
    .bind(worker_session_id)
    .bind(execution_id)
    .execute(conn)
    .await?
    .rows_affected()
        == 1)
}
