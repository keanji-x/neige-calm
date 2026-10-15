//! The shared attempt ownership and current-execution admission for worker tools.
use crate::db::sqlite::{task_attempt_current_tx, task_get_tx};
use crate::error::{CalmError, Result};
use crate::model::{Task, TaskStatus};
use crate::operation::Tx;

pub(crate) async fn owned_attempt_tx(
    tx: &mut Tx<'_>,
    attempt_id: &str,
    card_id: &str,
    track_id: &str,
) -> Result<Task> {
    let not_yours = || {
        CalmError::BadRequest(format!(
            "needs an attempt owned by this card; {attempt_id} is not one"
        ))
    };
    let task = task_get_tx(tx, attempt_id).await?.ok_or_else(not_yours)?;
    if task.track_id != track_id || task.worker_card_id.as_deref() != Some(card_id) {
        return Err(not_yours());
    }
    Ok(task)
}

pub(crate) async fn current_running_attempt_tx(tx: &mut Tx<'_>, task: &Task) -> Result<()> {
    let current = task_attempt_current_tx(tx, &task.track_id, &task.key).await?;
    if task.status != TaskStatus::Running
        || current.as_ref().map(|c| c.attempt_id.as_str()) != Some(task.id.as_str())
    {
        return Err(CalmError::BadRequest(format!(
            "needs the current running attempt; {} is not one",
            task.id
        )));
    }
    Ok(())
}
