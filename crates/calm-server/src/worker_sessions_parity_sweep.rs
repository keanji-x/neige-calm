use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use sqlx::{Row, SqlitePool};

use crate::error::Result;

const SWEEP_INTERVAL: Duration = Duration::from_secs(300);

#[derive(Debug)]
pub(crate) struct ParityDivergence {
    runtime_id: String,
    runtime_status: String,
    session_state: Option<String>,
    runtime_thread_id: Option<String>,
    session_thread_id: Option<String>,
    runtime_session_id: Option<String>,
    session_agent_session_id: Option<String>,
    runtime_active_turn_id: Option<String>,
    session_active_turn_id: Option<String>,
    runtime_terminal_run_id: Option<String>,
    session_terminal_run_id: Option<String>,
    runtime_handle_state_json: Option<String>,
    session_handle_state_json: Option<String>,
    runtime_created_at_ms: i64,
    session_created_at_ms: Option<i64>,
    runtime_updated_at_ms: i64,
    session_updated_at_ms: Option<i64>,
    runtime_completed_at_ms: Option<i64>,
    session_completed_at_ms: Option<i64>,
}

#[derive(Debug)]
struct ReverseOrphan {
    session_id: String,
    state: String,
    thread_id: Option<String>,
}

#[derive(Debug)]
struct CardSessionDuplicate {
    session_id: String,
    n: i64,
}

impl ParityDivergence {
    pub(crate) fn runtime_id(&self) -> &str {
        &self.runtime_id
    }
}

pub fn spawn(pool: SqlitePool, counter: Arc<AtomicU64>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(SWEEP_INTERVAL);
        tick.tick().await;
        loop {
            tick.tick().await;
            if let Err(e) = sweep(&pool, counter.as_ref()).await {
                tracing::warn!(
                    target: "worker_sessions::parity",
                    error = %e,
                    "worker_sessions parity sweep failed"
                );
            }
        }
    });
}

pub async fn sweep(pool: &SqlitePool, counter: &AtomicU64) -> Result<usize> {
    let divergences = diff(pool).await?;
    let reverse_orphans = reverse_orphans(pool).await?;
    let card_session_duplicates = card_session_duplicates(pool).await?;
    let divergence_count =
        divergences.len() + reverse_orphans.len() + card_session_duplicates.len();
    if divergence_count == 0 {
        return Ok(0);
    }

    counter.fetch_add(divergence_count as u64, Ordering::Relaxed);
    for divergence in &divergences {
        tracing::warn!(
            target: "worker_sessions::parity",
            runtime_id = %divergence.runtime_id,
            runtime_status = %divergence.runtime_status,
            session_state = ?divergence.session_state,
            runtime_thread_id = ?divergence.runtime_thread_id,
            session_thread_id = ?divergence.session_thread_id,
            runtime_session_id = ?divergence.runtime_session_id,
            session_agent_session_id = ?divergence.session_agent_session_id,
            runtime_active_turn_id = ?divergence.runtime_active_turn_id,
            session_active_turn_id = ?divergence.session_active_turn_id,
            runtime_terminal_run_id = ?divergence.runtime_terminal_run_id,
            session_terminal_run_id = ?divergence.session_terminal_run_id,
            runtime_handle_state_json = ?divergence.runtime_handle_state_json,
            session_handle_state_json = ?divergence.session_handle_state_json,
            runtime_created_at_ms = divergence.runtime_created_at_ms,
            session_created_at_ms = ?divergence.session_created_at_ms,
            runtime_updated_at_ms = divergence.runtime_updated_at_ms,
            session_updated_at_ms = ?divergence.session_updated_at_ms,
            runtime_completed_at_ms = ?divergence.runtime_completed_at_ms,
            session_completed_at_ms = ?divergence.session_completed_at_ms,
            "worker_sessions parity divergence"
        );
    }
    for orphan in &reverse_orphans {
        tracing::warn!(
            target: "worker_sessions::parity",
            session_id = %orphan.session_id,
            state = %orphan.state,
            thread_id = ?orphan.thread_id,
            "worker_sessions reverse orphan"
        );
    }
    for duplicate in &card_session_duplicates {
        tracing::warn!(
            target: "worker_sessions::parity",
            session_id = %duplicate.session_id,
            card_count = duplicate.n,
            "cards share worker session"
        );
    }
    Ok(divergence_count)
}

pub(crate) async fn diff(pool: &SqlitePool) -> Result<Vec<ParityDivergence>> {
    // Layer-2/3 parity is intentionally one-way: runtimes LEFT JOIN
    // worker_sessions. Orphan worker_sessions rows remain tolerated until PR9b.
    let rows = sqlx::query(
        r#"SELECT r.id AS runtime_id,
                  r.status AS runtime_status,
                  ws.state AS session_state,
                  r.thread_id AS runtime_thread_id,
                  ws.thread_id AS session_thread_id,
                  r.session_id AS runtime_session_id,
                  ws.agent_session_id AS session_agent_session_id,
                  r.active_turn_id AS runtime_active_turn_id,
                  ws.active_turn_id AS session_active_turn_id,
                  r.terminal_run_id AS runtime_terminal_run_id,
                  ws.terminal_run_id AS session_terminal_run_id,
                  r.handle_state_json AS runtime_handle_state_json,
                  ws.handle_state_json AS session_handle_state_json,
                  r.created_at_ms AS runtime_created_at_ms,
                  ws.created_at_ms AS session_created_at_ms,
                  r.updated_at_ms AS runtime_updated_at_ms,
                  ws.updated_at_ms AS session_updated_at_ms,
                  r.completed_at_ms AS runtime_completed_at_ms,
                  ws.completed_at_ms AS session_completed_at_ms
           FROM runtimes r
           LEFT JOIN worker_sessions ws ON ws.id = r.id
           WHERE ws.id IS NULL
              OR ws.state != r.status
              OR NOT (ws.thread_id IS r.thread_id)
              OR NOT (ws.agent_session_id IS r.session_id)
              OR NOT (ws.active_turn_id IS r.active_turn_id)
              OR NOT (ws.terminal_run_id IS r.terminal_run_id)
              OR NOT (ws.handle_state_json IS r.handle_state_json)
              OR ws.created_at_ms != r.created_at_ms
              OR ws.updated_at_ms != r.updated_at_ms
              OR NOT (ws.completed_at_ms IS r.completed_at_ms)
           ORDER BY r.created_at_ms ASC, r.id ASC"#,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| ParityDivergence {
            runtime_id: row.get("runtime_id"),
            runtime_status: row.get("runtime_status"),
            session_state: row.get("session_state"),
            runtime_thread_id: row.get("runtime_thread_id"),
            session_thread_id: row.get("session_thread_id"),
            runtime_session_id: row.get("runtime_session_id"),
            session_agent_session_id: row.get("session_agent_session_id"),
            runtime_active_turn_id: row.get("runtime_active_turn_id"),
            session_active_turn_id: row.get("session_active_turn_id"),
            runtime_terminal_run_id: row.get("runtime_terminal_run_id"),
            session_terminal_run_id: row.get("session_terminal_run_id"),
            runtime_handle_state_json: row.get("runtime_handle_state_json"),
            session_handle_state_json: row.get("session_handle_state_json"),
            runtime_created_at_ms: row.get("runtime_created_at_ms"),
            session_created_at_ms: row.get("session_created_at_ms"),
            runtime_updated_at_ms: row.get("runtime_updated_at_ms"),
            session_updated_at_ms: row.get("session_updated_at_ms"),
            runtime_completed_at_ms: row.get("runtime_completed_at_ms"),
            session_completed_at_ms: row.get("session_completed_at_ms"),
        })
        .collect())
}

async fn reverse_orphans(pool: &SqlitePool) -> Result<Vec<ReverseOrphan>> {
    let rows = sqlx::query(
        r#"SELECT ws.id, ws.state, ws.thread_id FROM worker_sessions ws
           LEFT JOIN runtimes r ON r.id = ws.id WHERE r.id IS NULL"#,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| ReverseOrphan {
            session_id: row.get("id"),
            state: row.get("state"),
            thread_id: row.get("thread_id"),
        })
        .collect())
}

async fn card_session_duplicates(pool: &SqlitePool) -> Result<Vec<CardSessionDuplicate>> {
    let rows = sqlx::query(
        r#"SELECT session_id, COUNT(*) AS n FROM cards
           WHERE session_id IS NOT NULL GROUP BY session_id HAVING n > 1"#,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| CardSessionDuplicate {
            session_id: row.get("session_id"),
            n: row.get("n"),
        })
        .collect())
}
