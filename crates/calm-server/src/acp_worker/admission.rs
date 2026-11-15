//! Task-specific owner/lease admission, rechecked at each protocol write boundary.
use crate::{
    db::{RouteRepo, write_in_tx_typed},
    error::{CalmError, Result},
    model::TaskStatus,
    operation::Tx,
};
use calm_types::{
    acp_worker::AcpWorkerState,
    provider_worker::{ProviderWorkerCapabilities, task_worker_contract},
    worker::WorkerSessionId,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone)]
pub(crate) struct Admission {
    pub repo: Arc<dyn RouteRepo>,
    pub worker: String,
    pub card: String,
    pub track: String,
    pub attempt: String,
    pub operation: String,
    pub lease: String,
    pub cwd: PathBuf,
    pub authorized: Arc<AtomicBool>,
    pub native: Option<String>,
    pub credential: Option<String>,
    pub turn: Option<String>,
}
impl Admission {
    pub(crate) async fn check_tx(
        &self,
        tx: &mut Tx<'_>,
        allow_ended: bool,
    ) -> Result<AcpWorkerState> {
        let denied = || CalmError::Conflict("ACP Worker owner, attempt, or lease changed".into());
        let task = crate::db::sqlite::task_get_tx(tx, &self.attempt)
            .await?
            .ok_or_else(denied)?;
        let allocation = crate::db::sqlite::task_attempt_current_tx(tx, &self.track, &task.key)
            .await?
            .ok_or_else(denied)?;
        let row =
            crate::db::sqlite::session_get_tx(tx, &WorkerSessionId::from(self.worker.clone()))
                .await?
                .ok_or_else(denied)?;
        let provider = task
            .agent_provider()
            .map_err(CalmError::BadRequest)?
            .ok_or_else(denied)?;
        let cap = ProviderWorkerCapabilities::for_provider(provider.clone()).ok_or_else(denied)?;
        let owned:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM operations o JOIN workspace_leases l ON l.lease_owner=o.id JOIN cards c ON c.id=o.target_id WHERE o.id=?1 AND o.kind=?2 AND o.target_type='card' AND o.target_id=?3 AND o.idempotency_key=?4 AND json_extract(o.payload_json,'$.actor.kind')='KernelDispatcher' AND l.lease_id=?5 AND l.card_id=c.id AND l.track_id=?6 AND l.path=?7 AND l.state IN ('held','releasing') AND c.session_id=?8 AND c.track_id=?6 AND c.role='worker')")
            .bind(&self.operation).bind(cap.operation.as_str()).bind(&self.card).bind(&self.attempt).bind(&self.lease).bind(&self.track).bind(self.cwd.to_string_lossy().as_ref()).bind(&self.worker).fetch_one(&mut **tx).await?;
        let state = crate::db::sqlite::acp_worker_state_tx(tx, &self.worker, &self.attempt).await?;
        if self
            .credential
            .as_ref()
            .is_some_and(|expected| state.credential_generation.as_ref() != Some(expected))
            || !owned
            || allocation.attempt_id != self.attempt
            || task.track_id != self.track
            || task
                .worker_card_id
                .as_deref()
                .is_some_and(|card| card != self.card)
            || row.card_id.as_ref().map(|id| id.as_str()) != Some(&self.card)
            || row.track_id.as_str() != self.track
            || calm_types::runtime::AgentProvider::try_from(row.provider).ok()
                != Some(provider.clone())
            || !cap.admits(row.contract, row.mode)
            || row.contract != task_worker_contract(task.access, task.result_contract.as_deref())
            || row.spawn_op_id.as_deref() != Some(&self.operation)
            || !row.state.is_active_authority()
            || state.requested_selection.provider != provider
            || state.requested_selection.model != task.model
            || state.requested_selection.reasoning_effort != task.reasoning_effort
            || self.native.as_ref().is_some_and(|native| {
                state.native_session_id.as_ref() != Some(native)
                    || row.agent_session_id.as_ref() != Some(native)
            })
            || self
                .turn
                .as_ref()
                .is_some_and(|turn| row.active_turn_id.as_ref() != Some(turn))
            || (!allow_ended
                && (!matches!(task.status, TaskStatus::Dispatched | TaskStatus::Running)
                    || state.release_intent
                    || state.timeout_cleanup.is_some()))
        {
            return Err(denied());
        }
        Ok(state)
    }
    pub(crate) async fn revoke(&self) -> Result<()> {
        if let Some(hash) = &self.credential {
            crate::acp_session::server::revoke_worker(self.repo.as_ref(), &self.worker, hash)
                .await?;
        }
        Ok(())
    }
    pub(crate) async fn check(&self, allow_ended: bool) -> Result<AcpWorkerState> {
        let this = self.clone();
        write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move { this.check_tx(tx, allow_ended).await })
        })
        .await
    }
    pub(crate) fn gate(&self, allow_ended: bool) -> provider::send_boundary::PreSendGate<'static> {
        let this = self.clone();
        provider::send_boundary::PreSendGate::new(move || {
            let this = this.clone();
            Box::pin(async move {
                this.check(allow_ended)
                    .await
                    .map_err(|error| provider::send_boundary::SendRefusal(error.to_string()))?;
                if !allow_ended && !this.authorized.load(Ordering::Acquire) {
                    return Err(provider::send_boundary::SendRefusal(
                        "ACP Worker turn is fenced".into(),
                    ));
                }
                Ok(())
            })
        })
    }
}
