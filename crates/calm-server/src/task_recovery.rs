//! Execution history of a Track + key task, and the guard every task side effect passes.

mod view;
pub use calm_types::task_recovery::{TaskAttemptView, TaskRecoveryView};
pub use view::task_recovery_view;
pub(crate) use view::task_recovery_view_tx;

use crate::db::sqlite::{task_attempt_current_tx, task_get_tx};
use crate::error::{CalmError, Result};
use crate::model::TaskStatus;
use sqlx::{Sqlite, Transaction};

/// Refuse a new side effect unless `task_id` is its key's current attempt and still live.
pub(crate) async fn require_attempt_startable_tx(
    tx: &mut Transaction<'_, Sqlite>,
    task_id: &str,
) -> Result<()> {
    let conflict = |reason: &str| CalmError::Conflict(reason.into());
    let task = task_get_tx(tx, task_id)
        .await?
        .ok_or_else(|| conflict("execution no longer exists"))?;
    let current = task_attempt_current_tx(tx, &task.track_id, &task.key)
        .await?
        .ok_or_else(|| conflict("current execution allocation is missing"))?;
    if current.attempt_id != task_id
        || !matches!(
            task.status,
            TaskStatus::Dispatched | TaskStatus::Running | TaskStatus::Verifying
        )
    {
        return Err(conflict(
            "execution is obsolete or terminal; refusing new side effects",
        ));
    }
    Ok(())
}

/// Re-admit a checkout worker at every atomic preparation/release boundary.
/// A blocked attempt may have yielded to another writer since its first dispatch.
pub(crate) async fn require_checkout_startable_tx(
    tx: &mut Transaction<'_, Sqlite>,
    task_id: &str,
) -> Result<()> {
    require_attempt_startable_tx(tx, task_id).await?;
    let task = task_get_tx(tx, task_id)
        .await?
        .ok_or_else(|| CalmError::Conflict("execution no longer exists".into()))?;
    if !task.runs_in_track_checkout() {
        return Ok(());
    }
    // Every caller is a new preparation/admission or a Held Pipe release.
    // A scheduler Running acknowledgement does not prove the go token was sent.
    let open: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tracks WHERE id=?1 AND closed_at IS NULL)")
            .bind(&task.track_id)
            .fetch_one(&mut **tx)
            .await?;
    if !open {
        return Err(CalmError::Conflict(
            "track closed before worker preparation or release".into(),
        ));
    }
    if !crate::db::sqlite::checkout_occupancy(tx, &task.track_id, task_id)
        .await?
        .admits(task.access)
    {
        return Err(CalmError::OperationBlocked(
            calm_types::task_execution::PendingBlocker::new(
                calm_types::task_execution::PendingBlockerClass::Checkout,
                "track",
                task.track_id,
                "the current checkout occupant finishes and its delivery settles",
            ),
        ));
    }
    Ok(())
}

pub(crate) async fn require_operation_checkout_tx(
    tx: &mut Transaction<'_, Sqlite>,
    op: &crate::operation::Operation,
) -> Result<()> {
    let Some(kind) = crate::model::TaskKind::from_worker_operation_kind(&op.kind) else {
        return Ok(());
    };
    let id = op
        .idempotency_key
        .as_deref()
        .ok_or_else(|| CalmError::Conflict("checkout worker has no task binding".into()))?;
    let task = task_get_tx(tx, id)
        .await?
        .ok_or_else(|| CalmError::Conflict("checkout worker task is missing".into()))?;
    if task.kind != kind
        || !task.runs_in_track_checkout()
        || op
            .payload
            .get("idempotency_key")
            .and_then(serde_json::Value::as_str)
            != Some(id)
        || op
            .payload
            .get("track_id")
            .and_then(serde_json::Value::as_str)
            != Some(task.track_id.as_str())
    {
        return Err(CalmError::Conflict(
            "checkout worker operation does not match its task".into(),
        ));
    }
    require_checkout_startable_tx(tx, id).await
}
