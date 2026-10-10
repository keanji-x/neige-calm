//! Compensation steps of a Claude restart that replaced a live child (#2516).

use serde::{Deserialize, Serialize};

use super::{CompensationStep, SpawnCtx};
use crate::db::sqlite::{
    session_complete_tx, session_projection_by_id_tx, session_restore_from_superseded_runtime_tx,
};
use crate::error::{CalmError, Result};
use crate::session_projection_repo::WorkerSessionState;

/// The runtime of the live child a restart replaces, and the state it had: superseded at prepare,
/// and the card's runtime again when the restart fails while that child still runs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct LivePredecessor {
    pub(crate) worker_session_id: String,
    pub(crate) status: WorkerSessionState,
}

fn live_predecessor_arg(step: &CompensationStep) -> Result<Option<LivePredecessor>> {
    Ok(step
        .args
        .get("live_predecessor")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .flatten())
}

fn arg_string(step: &CompensationStep, key: &str) -> Result<String> {
    step.args
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "claude restart compensation step missing {key} arg"
            ))
        })
}

/// The state of a restart's own runtime, whether or not the card still links it.
pub(crate) async fn own_runtime_status(
    ctx: &SpawnCtx,
    worker_session_id: &str,
) -> Result<Option<WorkerSessionState>> {
    let worker_session_id = worker_session_id.to_owned();
    crate::db::write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        Box::pin(async move {
            Ok(session_projection_by_id_tx(tx, &worker_session_id)
                .await?
                .map(|runtime| runtime.status))
        })
    })
    .await
}

fn is_live(status: WorkerSessionState) -> bool {
    matches!(
        status,
        WorkerSessionState::Starting | WorkerSessionState::Running
    )
}

/// This restart's own runtime fails, never the card's active one: the drive interleaves
/// operations by phase, so a newer restart of the card may own that by now.
pub(crate) async fn fail_replacement_runtime(
    ctx: &SpawnCtx,
    step: &CompensationStep,
) -> Result<()> {
    let worker_session_id = arg_string(step, "worker_session_id")?;
    crate::db::write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let Some(runtime) = session_projection_by_id_tx(tx, &worker_session_id).await? else {
                return Ok(());
            };
            if is_live(runtime.status) {
                session_complete_tx(tx, &runtime.id, WorkerSessionState::Failed).await?;
            }
            Ok(())
        })
    })
    .await
}

/// A restart that failed before its stop left the old child running: its runtime is the card's
/// runtime again, in the state it had. An undecided probe is no proof the child is gone, so the
/// step fails and is retried rather than leave a running child behind an ended runtime.
pub(crate) async fn restore_live_predecessor(
    ctx: &SpawnCtx,
    step: &CompensationStep,
) -> Result<()> {
    let terminal_id = arg_string(step, "terminal_id")?;
    let Some(LivePredecessor {
        worker_session_id,
        status,
    }) = live_predecessor_arg(step)?
    else {
        return Ok(());
    };
    match ctx
        .terminal_renderer
        .child_running(ctx.daemon.proc_supervisor_sock.as_deref(), &terminal_id)
        .await
    {
        Some(true) => {}
        Some(false) => return Ok(()),
        None => {
            return Err(CalmError::Internal(format!(
                "could not tell whether terminal {terminal_id} still runs its child"
            )));
        }
    }
    crate::db::write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        Box::pin(async move {
            session_restore_from_superseded_runtime_tx(tx, &worker_session_id, status).await?;
            Ok(())
        })
    })
    .await
}

/// Whether [`restore_live_predecessor`] made the replaced child's runtime the card's again: then
/// that child still runs and its terminal row stays live. `true` when there was none to restore.
pub(crate) async fn predecessor_restored(ctx: &SpawnCtx, step: &CompensationStep) -> Result<bool> {
    let Some(predecessor) = live_predecessor_arg(step)? else {
        return Ok(true);
    };
    Ok(ctx
        .repo
        .session_projection_by_id(&predecessor.worker_session_id)
        .await?
        .is_some_and(|runtime| is_live(runtime.status)))
}
