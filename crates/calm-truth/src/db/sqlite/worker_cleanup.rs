//! Operation cleanup projection. Native state is interpreted only by its owning codec.
use super::{acp_worker, session_get_tx};
use crate::{
    error::{CalmError, Result},
    session_repo::Tx,
};
use calm_types::{provider_worker::WorkerOperationKind, worker::WorkerSessionId};

#[derive(Debug, PartialEq, Eq)]
pub enum WorkerCleanupVerdict {
    /// The terminal launch owner must still check its durable launch obligation.
    Terminal,
    Confirmed,
    Pending,
}

pub async fn worker_operation_cleanup_tx(
    tx: &mut Tx<'_>,
    kind: WorkerOperationKind,
    session: Option<&str>,
    attempt: Option<&str>,
) -> Result<WorkerCleanupVerdict> {
    match kind {
        WorkerOperationKind::CodexWorker
        | WorkerOperationKind::ClaudeWorker
        | WorkerOperationKind::TerminalWorker => Ok(WorkerCleanupVerdict::Terminal),
        WorkerOperationKind::AcpWorker => {
            let (Some(session), Some(attempt)) = (session, attempt) else {
                return Ok(WorkerCleanupVerdict::Pending);
            };
            let row = session_get_tx(tx, &WorkerSessionId::from(session))
                .await?
                .ok_or(CalmError::Conflict("native cleanup owner disappeared"))?;
            let state = acp_worker::acp_worker_state_tx(tx, session, attempt).await?;
            let claim = super::provider_execution::load(&mut **tx, session).await?;
            Ok(
                if state.cleanup_confirmed
                    && row.active_turn_id.is_none()
                    && row.mcp_token_hash.is_none()
                    && claim.is_none()
                {
                    WorkerCleanupVerdict::Confirmed
                } else {
                    WorkerCleanupVerdict::Pending
                },
            )
        }
    }
}
