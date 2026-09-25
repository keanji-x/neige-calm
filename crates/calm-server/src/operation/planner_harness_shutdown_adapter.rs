use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

use crate::claude_planner::config::ClaudePlannerHost;
use crate::claude_planner::stop::stop;
use crate::db::Repo;
use crate::db::sqlite::{session_mark_superseded_runtime_tx, session_projection_by_id_tx};
use crate::error::{CalmError, Result};
use crate::harness::HarnessRegistry;
use crate::session_projection_repo::{AgentProvider, WorkerSessionKind};
use crate::shared_codex_appserver::SharedCodexAppServer;

use super::{
    AppServerInteractOutcome, CompensationStateVersioned, Operation, PhaseTag, ProviderAdapter,
    SpawnCtx, SpawnHandle, SpawnOutcome, Tx, TxOutput,
};

const SHUTDOWN_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::Succeeded,
];

#[derive(Clone)]
pub struct PlannerHarnessShutdownAdapter {
    harness_registry: HarnessRegistry,
    daemon: Arc<SharedCodexAppServer>,
    repo: Arc<dyn Repo>,
    claude_host: Arc<ClaudePlannerHost>,
}

impl PlannerHarnessShutdownAdapter {
    pub fn new(
        harness_registry: HarnessRegistry,
        daemon: Arc<SharedCodexAppServer>,
        repo: Arc<dyn Repo>,
        claude_host: Arc<ClaudePlannerHost>,
    ) -> Self {
        Self {
            harness_registry,
            daemon,
            repo,
            claude_host,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlannerHarnessShutdownOperationPayload {
    /// Wire key frozen as `runtime_id`: stored `operations.payload_json` rows keep that key and this field has
    /// no default, so without the rename a parked operation would fail to deserialize on resume.
    #[serde(rename = "runtime_id")]
    pub worker_session_id: String,
}

#[async_trait]
impl ProviderAdapter for PlannerHarnessShutdownAdapter {
    fn kind(&self) -> &'static str {
        "planner-harness-shutdown"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        SHUTDOWN_PHASES
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: PlannerHarnessShutdownOperationPayload =
            serde_json::from_value(input.clone())?;
        if payload.worker_session_id.trim().is_empty() {
            return Err(CalmError::BadRequest("runtime_id is required".into()));
        }
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        let payload: PlannerHarnessShutdownOperationPayload =
            serde_json::from_value(input.clone())?;
        let runtime = session_projection_by_id_tx(tx, &payload.worker_session_id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("runtime {}", payload.worker_session_id)))?;
        session_mark_superseded_runtime_tx(tx, &payload.worker_session_id).await?;
        let mut output = TxOutput::new(
            "runtime",
            Some(runtime.id.clone()),
            serde_json::to_value(&runtime)?,
        );
        output.data = json!({ "runtime_id": runtime.id });
        Ok(output)
    }

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        let worker_session_id = output.output_string("runtime_id", "planner harness")?;
        let runtime = self
            .repo
            .session_projection_by_id(&worker_session_id)
            .await?;
        let claude = runtime.as_ref().is_some_and(|runtime| {
            runtime.kind == WorkerSessionKind::SharedPlanner
                && runtime.agent_provider == Some(AgentProvider::Claude)
        });
        let registered = self.harness_registry.remove(&worker_session_id);
        let was_registered = registered.is_some();
        if let Some(harness) = registered {
            harness.shutdown().await?;
        }
        if claude {
            // #1791 §5.1: a Claude Planner is stopped by id, registered or not (a replay repeats it). A failure is
            // logged and left to the boot sweep or the next destructive step's scoped sweep; the superseded row's
            // token no longer authenticates.
            if let Err(error) = stop(&self.claude_host.instance, &worker_session_id).await {
                tracing::warn!(%worker_session_id, %error, "planner harness shutdown: the Claude Planner stop did not confirm");
            }
        } else if !was_registered
            && let Some(runtime) = runtime
            && let Some(thread_id) = runtime.thread_id.as_deref()
        {
            let cached_turn = self.daemon.active_turn_id_for_thread(thread_id);
            if let Err(e) = self.daemon.interrupt_active_turn(thread_id).await {
                tracing::warn!(
                    runtime_id = %worker_session_id,
                    thread_id,
                    error = %e,
                    "planner harness shutdown replay thread interrupt failed"
                );
            }
            if cached_turn.is_none()
                && let Some(persisted_turn) = runtime.active_turn_id.as_deref()
                && let Err(e) = self.daemon.turn_interrupt(thread_id, persisted_turn).await
            {
                tracing::warn!(
                    runtime_id = %worker_session_id,
                    thread_id,
                    turn_id = persisted_turn,
                    error = %e,
                    "planner harness shutdown replay persisted-turn interrupt failed"
                );
            }
        }
        Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        _output: &TxOutput,
        _op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.to_string(),
            steps: vec![],
        })
    }

    async fn compensate_step(
        &self,
        _step: &super::CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<()> {
        Ok(())
    }
}
