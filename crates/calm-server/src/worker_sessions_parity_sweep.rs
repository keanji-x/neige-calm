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
    if divergences.is_empty() {
        return Ok(0);
    }

    counter.fetch_add(divergences.len() as u64, Ordering::Relaxed);
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
    Ok(divergences.len())
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
