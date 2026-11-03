//! The first progress-obligation owner: child-track dispatch-to-running handoff.
//! Running child progress and worker liveness are deliberately separate contracts.
use super::progress_obligation::{
    ProgressObligation, progress_obligation_release_tx, progress_obligation_require_tx,
};
use crate::error::Result;
use calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE;
use sqlx::{Sqlite, Transaction};

pub const CHILD_BOOTSTRAP_PURPOSE: &str = "child-track-bootstrap";
pub const CHILD_BOOTSTRAP_WINDOW_MS: i64 = 15 * 60 * 1000;
pub const CHILD_BOOTSTRAP_OVERDUE: &str = "child-track-bootstrap-overdue";

/// Called in the claim transaction. Reverse discovery uses the same enrollment contract.
pub async fn task_require_child_bootstrap_tx(
    tx: &mut Transaction<'_, Sqlite>,
    task_id: &str,
) -> Result<()> {
    let anchor: Option<i64> = sqlx::query_scalar(
        "SELECT updated_at_ms FROM current_tasks WHERE id=?1 AND spawn=?2 AND status='dispatched'",
    )
    .bind(task_id)
    .bind(TASK_CHILD_TRACK_ROUTE)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(anchor) = anchor {
        progress_obligation_require_tx(
            tx,
            &ProgressObligation {
                execution_id: task_id.to_string(),
                purpose: CHILD_BOOTSTRAP_PURPOSE.to_string(),
                deadline_at_ms: anchor.saturating_add(CHILD_BOOTSTRAP_WINDOW_MS),
            },
        )
        .await?;
    }
    Ok(())
}

/// Start from the owned tasks, not just the obligation table: a missing enrollment
/// must not make a dispatched child invisible. This also enrolls pre-upgrade rows.
pub async fn task_reconcile_child_bootstrap_obligations_tx(
    tx: &mut Transaction<'_, Sqlite>,
) -> Result<()> {
    let missing: Vec<String> = sqlx::query_scalar(
        "SELECT t.id FROM current_tasks t WHERE t.spawn=?1 AND t.status='dispatched' \
         AND NOT EXISTS(SELECT 1 FROM progress_obligations o WHERE o.execution_id=t.id AND o.purpose=?2)",
    ).bind(TASK_CHILD_TRACK_ROUTE).bind(CHILD_BOOTSTRAP_PURPOSE).fetch_all(&mut **tx).await?;
    for task_id in missing {
        task_require_child_bootstrap_tx(tx, &task_id).await?;
    }
    // Includes canceled, deleted and superseded attempts; none retains wake authority.
    let obsolete: Vec<String> = sqlx::query_scalar(
        "SELECT o.execution_id FROM progress_obligations o WHERE o.purpose=?1 AND o.state!='released' \
         AND NOT EXISTS(SELECT 1 FROM current_tasks t JOIN tracks p ON p.id=t.track_id \
                        WHERE t.id=o.execution_id AND t.spawn=?2 AND t.status='dispatched')",
    ).bind(CHILD_BOOTSTRAP_PURPOSE).bind(TASK_CHILD_TRACK_ROUTE).fetch_all(&mut **tx).await?;
    for execution_id in obsolete {
        progress_obligation_release_tx(tx, &execution_id, CHILD_BOOTSTRAP_PURPOSE).await?;
    }
    Ok(())
}

/// A committed notice is retried through the existing Planner ingress while its
/// execution remains in this owner's scope. No new events are minted on retry.
#[derive(sqlx::FromRow)]
pub struct ParentProgressNotice {
    pub track_id: String,
    pub event_id: i64,
}

pub async fn task_pending_child_bootstrap_notices(
    db: &super::SqliteDb,
) -> Result<Vec<ParentProgressNotice>> {
    Ok(sqlx::query_as(
        "SELECT t.track_id,e.id AS event_id FROM progress_obligations o \
         JOIN current_tasks t ON t.id=o.execution_id \
         JOIN tracks p ON p.id=t.track_id \
         JOIN events e ON e.scope_track=t.track_id AND e.kind='track.wake_requested' \
           AND json_extract(e.payload,'$.source')='progress-obligation' \
           AND json_extract(e.payload,'$.key')=o.execution_id||':'||o.purpose \
         WHERE o.purpose=?1 AND o.state='notified' AND t.spawn=?2 AND t.status='dispatched' \
         ORDER BY e.id",
    )
    .bind(CHILD_BOOTSTRAP_PURPOSE)
    .bind(TASK_CHILD_TRACK_ROUTE)
    .fetch_all(db.read_pool())
    .await?)
}

/// Task-owned eligibility and presentation change, in the notification transaction.
#[derive(sqlx::FromRow)]
pub struct ChildBootstrapTarget {
    pub track_id: String,
    pub area_id: String,
    pub task_key: String,
}

pub async fn task_claim_child_bootstrap_notice_tx(
    tx: &mut Transaction<'_, Sqlite>,
    obligation: &ProgressObligation,
    now: i64,
) -> Result<Option<ChildBootstrapTarget>> {
    let target = sqlx::query_as::<_, ChildBootstrapTarget>(
        "SELECT t.track_id,p.area_id,t.key AS task_key FROM current_tasks t \
         JOIN tracks p ON p.id=t.track_id WHERE t.id=?1 AND t.status='dispatched' AND t.spawn=?2",
    )
    .bind(&obligation.execution_id)
    .bind(TASK_CHILD_TRACK_ROUTE)
    .fetch_optional(&mut **tx)
    .await?;
    if target.is_none()
        || !super::progress_obligation::progress_obligation_notify_tx(tx, obligation, now).await?
    {
        return Ok(None);
    }
    sqlx::query("UPDATE tasks SET status_detail=?1 WHERE id=?2 AND status_detail IS NULL")
        .bind(CHILD_BOOTSTRAP_OVERDUE)
        .bind(&obligation.execution_id)
        .execute(&mut **tx)
        .await?;
    Ok(target)
}
