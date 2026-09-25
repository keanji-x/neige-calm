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

impl PlannerHarnessShutdownAdapter {
    /// #1791 §5.1: a Claude Planner is stopped by id, registered or not (a replay repeats it). A failure is logged
    /// and left to the boot sweep or the next destructive step's scoped sweep; the superseded row's token no longer
    /// authenticates.
    async fn stop_claude_planner(&self, worker_session_id: &str) {
        if let Err(error) = stop(&self.claude_host.instance, worker_session_id).await {
            tracing::warn!(
                %worker_session_id,
                %error,
                "planner harness shutdown: the Claude Planner stop did not confirm"
            );
        }
    }
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
        // A registered harness is shut down first, whatever a row read would say; its backend names its provider.
        if let Some(harness) = self.harness_registry.remove(&worker_session_id) {
            let claude = harness.provider() == AgentProvider::Claude;
            harness.shutdown().await?;
            if claude {
                self.stop_claude_planner(&worker_session_id).await;
            }
            return Ok(SpawnOutcome::Ready(SpawnHandle::NoOp));
        }
        if let Some(runtime) = self
            .repo
            .session_projection_by_id(&worker_session_id)
            .await?
        {
            if runtime.kind == WorkerSessionKind::SharedPlanner
                && runtime.agent_provider == Some(AgentProvider::Claude)
            {
                self.stop_claude_planner(&worker_session_id).await;
                return Ok(SpawnOutcome::Ready(SpawnHandle::NoOp));
            }
            let Some(thread_id) = runtime.thread_id.as_deref() else {
                return Ok(SpawnOutcome::Ready(SpawnHandle::NoOp));
            };
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::db::sqlite::SqlxRepo;
    use crate::event::EventBus;
    use crate::harness::{HarnessConfig, HarnessSnapshot, PlannerHarness, PlannerHarnessParams};
    use crate::ids::{CardId, TrackId};
    use crate::operation::{OperationCompletionBus, Phase, SqlxOperationRepo};
    use crate::state::DaemonClient;
    use crate::terminal_renderer::TerminalRendererRegistry;

    /// #1791 PR4 review: a registered harness is shut down before anything reads the row, so a
    /// row read that would fail can never keep the op from shutting it down (the Codex base order).
    #[tokio::test]
    async fn a_registered_harness_is_shut_down_even_when_its_row_cannot_be_read() {
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        let daemon = SharedCodexAppServer::new_stub(repo_dyn.clone());
        let registry = HarnessRegistry::new();
        let id = "rt-shutdown-read-fails".to_string();
        let handle = PlannerHarness::run(PlannerHarnessParams {
            worker_session_id: id.clone(),
            track_id: TrackId::from("track-x".to_string()),
            card_id: CardId::from("card-x".to_string()),
            thread_id: None,
            repo: repo_dyn.clone(),
            events: EventBus::new(),
            card_role_cache: crate::card_role_cache::CardRoleCache::new(),
            track_area_cache: crate::track_area_cache::TrackAreaCache::new(),
            backend: daemon.clone().into(),
            config: HarnessConfig::default(),
            snapshot: HarnessSnapshot::initial(0, vec![]),
        });
        registry.insert(id.clone(), handle);
        let adapter = PlannerHarnessShutdownAdapter::new(
            registry.clone(),
            daemon,
            repo_dyn.clone(),
            Arc::new(
                crate::claude_planner::config::ClaudePlannerHost::unconfigured_scratch().unwrap(),
            ),
        );
        let route_repo: Arc<dyn crate::db::RouteRepo> = repo.clone();
        let op_repo = Arc::new(SqlxOperationRepo::new(repo.pool().clone()));
        let ctx = SpawnCtx::new(
            route_repo.clone(),
            op_repo,
            Arc::new(DaemonClient::new_stub()),
            TerminalRendererRegistry::new_with_repo(route_repo),
            EventBus::new(),
            OperationCompletionBus::new(),
        );
        let payload = serde_json::to_value(PlannerHarnessShutdownOperationPayload {
            worker_session_id: id.clone(),
        })
        .unwrap();
        let mut output = TxOutput::new("runtime", Some(id.clone()), json!({}));
        output.data = payload.clone();
        let op = Operation {
            id: "op-shutdown".into(),
            operation_key: "op-shutdown".into(),
            kind: "planner-harness-shutdown".into(),
            idempotency_key: None,
            payload_hash: String::new(),
            target_type: "runtime".into(),
            target_id: Some(id.clone()),
            target: json!({}),
            payload,
            tx_output: None,
            phase: Phase::TxCommitted,
            phase_detail: None,
            attempt: 1,
            last_error: None,
            compensation_state: None,
            lease_owner: None,
            lease_until_ms: None,
            spawn_artifacts: None,
            parked_at_ms: None,
            parked_deadline_ms: None,
        };

        sqlx::query("ALTER TABLE worker_sessions RENAME TO worker_sessions_hidden")
            .execute(repo.pool())
            .await
            .unwrap();
        let outcome = adapter.spawn_side_effect(&output, &op, &ctx).await;
        sqlx::query("ALTER TABLE worker_sessions_hidden RENAME TO worker_sessions")
            .execute(repo.pool())
            .await
            .unwrap();
        assert!(outcome.is_ok(), "{:?}", outcome.err());
        assert!(
            registry.get(&id).is_none(),
            "the harness was shut down and removed"
        );
    }
}
