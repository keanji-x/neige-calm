//! #1830 S2 D5: whether a track's checkout is free for its next codex or claude worker. The
//! scheduler's claim and the `trackBusy` pending reason read this one predicate.

use sqlx::SqliteConnection;

use super::task::TASK_COLUMNS;
use crate::error::Result;
use crate::model::Task;

/// No attempt other than `except_attempt` is using the track's checkout. Three terms, each
/// covering what the others miss: no in-tree worker task is `dispatched`/`running`/`verifying`
/// (a gate reading the tree after the release; a claim before its lease exists); no lease is
/// `held`/`releasing` unless its owner op is `stuck` (a canceled worker not yet killed); no
/// delivery is unsettled (a commit not yet landed).
pub async fn track_idle(
    conn: &mut SqliteConnection,
    track_id: &str,
    except_attempt: &str,
) -> Result<bool> {
    let sql = format!(
        "SELECT {TASK_COLUMNS} FROM current_tasks WHERE track_id = ?1 AND id <> ?2 \
         AND status IN ('dispatched','running','verifying')"
    );
    let in_flight = sqlx::query_as::<_, Task>(&sql)
        .bind(track_id)
        .bind(except_attempt)
        .fetch_all(&mut *conn)
        .await?;
    for task in in_flight {
        if task.runs_in_track_checkout()? {
            return Ok(false);
        }
    }
    // `'stuck'` is the operations phase of an owner whose outcome is unknown (`PhaseTag::Stuck`).
    let lease_held: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM workspace_leases wl \
         LEFT JOIN operations o ON o.id = wl.lease_owner \
         WHERE wl.track_id = ?1 AND wl.state IN ('held','releasing') \
         AND o.idempotency_key IS NOT ?2 \
         AND o.phase IS NOT 'stuck')",
    )
    .bind(track_id)
    .bind(except_attempt)
    .fetch_one(&mut *conn)
    .await?;
    if lease_held {
        return Ok(false);
    }
    let delivery_unsettled: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM task_git_deliveries \
         WHERE track_id = ?1 AND settlement IS NULL AND producer_attempt_id <> ?2)",
    )
    .bind(track_id)
    .bind(except_attempt)
    .fetch_one(&mut *conn)
    .await?;
    Ok(!delivery_unsettled)
}
