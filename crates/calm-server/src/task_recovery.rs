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
