use super::{OPERATION_KIND, WorkerPayload, selected, worker_payload};
use crate::db::sqlite::task_get_tx;
use crate::error::{CalmError, Result};
use crate::model::Task;
use crate::operation::{Operation, Tx};

pub(crate) async fn validate_start_tx(tx: &mut Tx<'_>, op: &Operation) -> Result<Task> {
    let request: WorkerPayload = serde_json::from_value(op.payload.clone())?;
    let task = task_get_tx(tx, &request.task_id)
        .await?
        .ok_or_else(|| CalmError::Conflict("isolated task no longer exists".into()))?;
    if op.kind != OPERATION_KIND
        || op.idempotency_key.as_deref() != Some(&task.id)
        || worker_payload(&task) != request
        || !selected(&task)?
    {
        return Err(CalmError::Conflict(
            "isolated task operation identity changed".into(),
        ));
    }
    require_backend_tx(tx, &task.id).await?;
    crate::task_recovery::validate_isolated_start_tx(tx, &task).await?;
    Ok(task)
}

pub(crate) async fn require_backend_tx(tx: &mut Tx<'_>, task_id: &str) -> Result<()> {
    let kinds:Vec<String>=sqlx::query_scalar("SELECT kind FROM operations WHERE idempotency_key=?1 AND kind IN ('codex-worker','claude-worker','terminal-worker','codex-isolated-worker')")
        .bind(task_id).fetch_all(&mut **tx).await?;
    if kinds.as_slice() != [OPERATION_KIND] {
        return Err(CalmError::Conflict(
            "task is bound to a different or ambiguous execution backend".into(),
        ));
    }
    Ok(())
}
