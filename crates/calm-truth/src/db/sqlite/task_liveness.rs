//! Running agent tasks' liveness facts: when the task entered `running`, its hard deadline, and
//! when its worker last made transcript progress.

use sqlx::Sqlite;
use sqlx::Transaction;

use crate::error::Result;

/// Backfill for agent tasks already running when the running deadline or start was introduced:
/// each missing value is stamped as if the task entered `running` at `now`. A deadline the row
/// already carries is kept, so such a row ends no later than it would have before.
pub async fn task_stamp_missing_running_liveness_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    now: i64,
    running_deadline_ms: i64,
) -> Result<u64> {
    let res = sqlx::query(
        r#"UPDATE tasks
           SET running_deadline_ms = COALESCE(running_deadline_ms, ?1),
               running_started_at_ms = COALESCE(running_started_at_ms, ?2),
               updated_at_ms = ?2
           WHERE id = ?3
             AND kind IN ('codex', 'claude')
             AND status = 'running'
             AND (running_deadline_ms IS NULL OR running_started_at_ms IS NULL)"#,
    )
    .bind(running_deadline_ms)
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// What decides whether a running agent task's worker is still alive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunningLivenessFacts {
    pub started_at_ms: i64,
    /// The hard cap, stamped once when the task entered `running`.
    pub deadline_ms: i64,
    /// When the worker card's transcript capture last advanced; `None` before its first record.
    pub last_progress_ms: Option<i64>,
}

/// `None` unless the task is `running` with both its start and deadline stamped.
pub async fn task_running_liveness_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    worker_card_id: Option<&str>,
) -> Result<Option<RunningLivenessFacts>> {
    let row: Option<(i64, i64, Option<i64>)> = sqlx::query_as(
        r#"SELECT running_started_at_ms,
                  running_deadline_ms,
                  (SELECT MAX(updated_at_ms) FROM worker_flow_cursors WHERE card_id = ?2)
           FROM tasks
           WHERE id = ?1
             AND status = 'running'
             AND running_started_at_ms IS NOT NULL
             AND running_deadline_ms IS NOT NULL"#,
    )
    .bind(id)
    .bind(worker_card_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(
        |(started_at_ms, deadline_ms, last_progress_ms)| RunningLivenessFacts {
            started_at_ms,
            deadline_ms,
            last_progress_ms,
        },
    ))
}
