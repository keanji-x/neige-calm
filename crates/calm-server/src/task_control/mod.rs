//! Shared task input admission. Transport identity is admitted by each caller before this service.
mod events;
pub(crate) use events::write_worker_events;
pub(crate) mod cancel;
use crate::{
    db::RouteRepo,
    error::{CalmError, Result},
    model::Task,
    operation::Tx,
};
use calm_types::{
    provider_worker::{ProviderWorkerCapabilities, WorkerMessageTransport},
    track_fs_dto::TaskMessageSource,
};
use std::sync::{Arc, atomic::AtomicBool};

pub(crate) fn is_native(task: &Task) -> Result<bool> {
    Ok(task
        .agent_provider()
        .map_err(CalmError::BadRequest)?
        .and_then(ProviderWorkerCapabilities::for_provider)
        .is_some_and(|c| c.message_transport == WorkerMessageTransport::NativeSession))
}
/// Reuse the protocol driver's full owner proof for messages and user control, inside the writer.
pub(crate) async fn admit_native_tx(
    repo: Arc<dyn RouteRepo>,
    tx: &mut Tx<'_>,
    task: &Task,
) -> Result<()> {
    if !is_native(task)? {
        return Err(CalmError::Conflict(
            "task has no native Worker input transport".into(),
        ));
    }
    let card = task
        .worker_card_id
        .clone()
        .ok_or_else(|| CalmError::Conflict("Worker card is not bound".into()))?;
    let row:Option<(String,String,String,String)>=sqlx::query_as("SELECT w.id,w.spawn_op_id,l.lease_id,l.path FROM cards c JOIN worker_sessions w ON w.id=c.session_id JOIN workspace_leases l ON l.card_id=c.id AND l.lease_owner=w.spawn_op_id WHERE c.id=?1 AND c.track_id=?2 AND l.state='held'")
        .bind(&card).bind(&task.track_id).fetch_optional(&mut **tx).await?;
    let (worker, operation, lease, cwd) = row.ok_or_else(|| {
        CalmError::Conflict("native Worker lease or session is no longer active".into())
    })?;
    crate::acp_worker::admission::Admission {
        repo,
        worker,
        card,
        track: task.track_id.clone(),
        attempt: task.id.clone(),
        operation,
        lease,
        cwd: cwd.into(),
        authorized: Arc::new(AtomicBool::new(true)),
        native: None,
        credential: None,
        turn: None,
    }
    .check_tx(tx, false)
    .await?;
    Ok(())
}
pub(crate) async fn message_tx(
    repo: Arc<dyn RouteRepo>,
    tx: &mut Tx<'_>,
    task: &Task,
    key: &str,
    text: &str,
    source: TaskMessageSource,
) -> Result<String> {
    crate::terminal_interaction::validate_message_text(text)
        .map_err(|e| CalmError::BadRequest(e.to_string()))?;
    if key.trim().is_empty() {
        return Err(CalmError::BadRequest(
            "idempotency_key must not be empty".into(),
        ));
    }
    if is_native(task)? {
        admit_native_tx(repo, tx, task).await?;
    } else {
        crate::operation::worker_attempt::current_running_attempt_tx(tx, task).await?;
    }
    Ok(
        crate::db::sqlite::planner_message_insert_with_source_tx(tx, &task.id, key, text, source)
            .await?,
    )
}

/// Capture inside the same task writer; post-commit fencing never resolves a successor card binding.
pub(crate) async fn capture_worker_identity_tx(
    tx: &mut Tx<'_>,
    task: &Task,
) -> Result<Option<(String, Option<String>)>> {
    Ok(sqlx::query_as("SELECT w.id,w.active_turn_id FROM cards c JOIN worker_sessions w ON w.id=c.session_id WHERE c.id=?1 AND c.track_id=?2 AND w.spawn_op_id IN (SELECT id FROM operations WHERE idempotency_key=?3)")
        .bind(task.worker_card_id.as_deref()).bind(&task.track_id).bind(&task.id).fetch_optional(&mut **tx).await?)
}
