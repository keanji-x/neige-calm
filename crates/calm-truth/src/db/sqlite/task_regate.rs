//! #2405: the one row write of a gate re-run — a failed gated attempt goes back to `verifying`
//! on the same row, holding a reserved attempt number the scheduler never runs.

use sqlx::Sqlite;
use sqlx::Transaction;

use crate::error::Result;

/// Guarded `failed → verifying` of a gated attempt whose last gate failed. The caller decides
/// admission (task-verify `regate`); this write re-checks what the row itself can prove.
///
/// `reserved_attempt` is one above every gate attempt this row has used (its `gate_attempt`, its
/// verdict's attempt, every task-verify op of the task). It is a placeholder: no op `#g{reserved}`
/// exists, so the scheduler submits `#g{reserved + 1}` and that op's `prepare_tx` bump moves the
/// row onto it. A lower number would let the scheduler find the old op and copy its verdict back.
/// `gate_result_json` is cleared so a `verifying` row never carries a verdict; the previous one
/// stays in its `task.gate_result` event. `0` rows = the row moved since the caller read it.
pub async fn task_regate_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    track_id: &str,
    observed_gate_attempt: i64,
    reserved_attempt: i64,
    now: i64,
) -> Result<u64> {
    let res = sqlx::query(
        r#"UPDATE tasks
           SET status = 'verifying',
               status_detail = NULL,
               finished_at_ms = NULL,
               gate_result_json = NULL,
               gate_attempt = ?1,
               updated_at_ms = ?2
           WHERE id = ?3 AND track_id = ?4 AND status = 'failed'
             AND gate_json IS NOT NULL AND gate_attempt = ?5
             AND context_stale_at_ms IS NULL"#,
    )
    .bind(reserved_attempt)
    .bind(now)
    .bind(id)
    .bind(track_id)
    .bind(observed_gate_attempt)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}
