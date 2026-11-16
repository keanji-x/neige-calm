//! Durable routing from workspace cleanup to the resident ACP execution.
use sqlx::{Sqlite, Transaction};
use std::path::Path;

pub async fn bind_workspace(
    tx: &mut Transaction<'_, Sqlite>,
    execution: &str,
    worker: &str,
    track: &str,
    cwd: &Path,
) -> crate::error::Result<()> {
    super::workspace_retirement::require_active_workspace_path(tx, cwd).await?;
    let result = sqlx::query(
        "INSERT INTO acp_resident_workspace_bindings (execution_id, worker_session_id, track_id, cwd)
         SELECT e.execution_id, ws.id, ws.track_id, ?4
         FROM workspace_executions e JOIN worker_sessions ws ON ws.id=?2
         WHERE e.execution_id=?1 AND e.desired='run' AND e.settled_at_ms IS NULL AND ws.track_id=?3
         ON CONFLICT (execution_id, worker_session_id) DO UPDATE SET track_id=excluded.track_id
         WHERE acp_resident_workspace_bindings.track_id=excluded.track_id
           AND acp_resident_workspace_bindings.cwd=excluded.cwd",
    )
    .bind(execution)
    .bind(worker)
    .bind(track)
    .bind(cwd.to_str().ok_or_else(|| crate::error::TruthError::Core(
        calm_types::error::CoreError::Conflict("ACP workspace path is not UTF-8".into()),
    ))?)
    .execute(&mut **tx)
    .await?;
    if result.rows_affected() != 1 {
        return Err(crate::error::TruthError::Core(
            calm_types::error::CoreError::Conflict("ACP resident workspace owner changed".into()),
        ));
    }
    Ok(())
}

pub async fn for_worker(pool: &sqlx::SqlitePool, worker: &str) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT execution_id FROM acp_resident_workspace_bindings WHERE worker_session_id=?1 ORDER BY execution_id")
        .bind(worker).fetch_all(pool).await
}

pub async fn workers_for_track(
    tx: &mut Transaction<'_, Sqlite>,
    track: &str,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT DISTINCT worker_session_id FROM acp_resident_workspace_bindings WHERE track_id=?1 ORDER BY worker_session_id")
        .bind(track).fetch_all(&mut **tx).await
}

pub async fn clear_generation(
    tx: &mut Transaction<'_, Sqlite>,
    execution: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM acp_resident_workspace_bindings WHERE execution_id=?1")
        .bind(execution)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
