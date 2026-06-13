use sqlx::{Row, SqlitePool};

pub async fn assert_runtimes_worker_sessions_parity(pool: &SqlitePool) {
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
    .await
    .expect("query runtimes/worker_sessions parity");

    if rows.is_empty() {
        return;
    }

    let details = rows
        .iter()
        .map(|row| {
            format!(
                "runtime_id={} status={:?}/{:?} thread={:?}/{:?} session={:?}/{:?} turn={:?}/{:?} terminal={:?}/{:?} handle={:?}/{:?} created={:?}/{:?} updated={:?}/{:?} completed={:?}/{:?}",
                row.get::<String, _>("runtime_id"),
                row.try_get::<String, _>("runtime_status").ok(),
                row.try_get::<Option<String>, _>("session_state").ok().flatten(),
                row.try_get::<Option<String>, _>("runtime_thread_id").ok().flatten(),
                row.try_get::<Option<String>, _>("session_thread_id").ok().flatten(),
                row.try_get::<Option<String>, _>("runtime_session_id").ok().flatten(),
                row.try_get::<Option<String>, _>("session_agent_session_id").ok().flatten(),
                row.try_get::<Option<String>, _>("runtime_active_turn_id").ok().flatten(),
                row.try_get::<Option<String>, _>("session_active_turn_id").ok().flatten(),
                row.try_get::<Option<String>, _>("runtime_terminal_run_id").ok().flatten(),
                row.try_get::<Option<String>, _>("session_terminal_run_id").ok().flatten(),
                row.try_get::<Option<String>, _>("runtime_handle_state_json").ok().flatten(),
                row.try_get::<Option<String>, _>("session_handle_state_json").ok().flatten(),
                row.try_get::<i64, _>("runtime_created_at_ms").ok(),
                row.try_get::<Option<i64>, _>("session_created_at_ms").ok().flatten(),
                row.try_get::<i64, _>("runtime_updated_at_ms").ok(),
                row.try_get::<Option<i64>, _>("session_updated_at_ms").ok().flatten(),
                row.try_get::<Option<i64>, _>("runtime_completed_at_ms").ok().flatten(),
                row.try_get::<Option<i64>, _>("session_completed_at_ms").ok().flatten(),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    panic!("runtimes/worker_sessions parity divergence:\n{details}");
}
