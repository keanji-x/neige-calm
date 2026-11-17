//! Terminal-free, single-turn task execution over the shared native backends.
use super::codex_adapter::CodexWorkerOperationPayload as Payload;
use super::workspace_lease::worker::preparation::WorkerAdmission;
use super::workspace_lease::worker::{record_catch_up, record_declared_head};
use super::workspace_lease::{WorkerLeasePlan, acquire_workspace_lease_tx};
use super::*;
use crate::db::sqlite::{task_get_tx, task_worker_create_tx};
use crate::db::{write_in_tx_typed, write_with_actor_events_typed};
use crate::harness::{
    HarnessRegistry,
    profile::{HarnessProfile, PlannerBinding},
    wiring::PlannerWiring,
};
use crate::model::{NewCard, Task};
use crate::planner_model::{
    CardModelSelection, acquisition::CatalogAccess, child_selection::ChildSelectionContext,
    task_selection::resolve_task_selection,
};
use crate::session_projection_repo::AgentProvider;
use provider::TurnModelSelection;

mod runner;
pub(crate) mod settlement;

#[derive(Serialize, Deserialize)]
struct StoredSelection {
    model: Option<String>,
    effort: Option<String>,
}

#[derive(Clone)]
pub(crate) struct AgentWorkerAdapter {
    pub repo: Arc<dyn crate::db::Repo>,
    pub wiring: PlannerWiring,
    pub selection: ChildSelectionContext,
    pub backends: Arc<crate::agent_backend_pool::codex::CodexBackends>,
    pub harness: HarnessRegistry,
    pub roles: crate::card_role_cache::CardRoleCache,
    pub areas: crate::track_area_cache::TrackAreaCache,
    pub socket: Option<std::path::PathBuf>,
    pub root: std::path::PathBuf,
    pub gate_wait: super::task_gate_run::GateRunWait,
    pub liveness: crate::scheduler::WorkerLiveness,
    resolved: Option<(Task, AgentProvider, TurnModelSelection, CardModelSelection)>,
}
impl AgentWorkerAdapter {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        repo: Arc<dyn crate::db::Repo>,
        wiring: PlannerWiring,
        selection: ChildSelectionContext,
        backends: Arc<crate::agent_backend_pool::codex::CodexBackends>,
        harness: HarnessRegistry,
        roles: crate::card_role_cache::CardRoleCache,
        areas: crate::track_area_cache::TrackAreaCache,
        socket: Option<std::path::PathBuf>,
        root: std::path::PathBuf,
        gate_wait: super::task_gate_run::GateRunWait,
    ) -> Self {
        Self {
            repo,
            wiring,
            selection,
            backends,
            harness,
            roles,
            areas,
            socket,
            root,
            gate_wait,
            liveness: crate::scheduler::WorkerLiveness::DEFAULT,
            resolved: None,
        }
    }
    pub(crate) fn with_liveness(mut self, liveness: crate::scheduler::WorkerLiveness) -> Self {
        self.liveness = liveness;
        self
    }
}
#[async_trait]
impl WorkerAdmission for AgentWorkerAdapter {
    async fn publish_worker(
        &self,
        tx: &mut Tx<'_>,
        op: &Operation,
        plan: WorkerLeasePlan,
    ) -> Result<TxOutput> {
        let payload: Payload = serde_json::from_value(op.payload.clone())?;
        let (task, provider, turn, card_selection) = self
            .resolved
            .as_ref()
            .ok_or_else(|| CalmError::Internal("worker selection was not prepared".into()))?;
        if task_get_tx(tx, &task.id).await?.as_ref() != Some(task) {
            return Err(CalmError::Conflict(
                "worker declaration changed during selection".into(),
            ));
        }
        let card_id = new_id();
        let worker = new_id();
        let cwd = plan.path.to_string_lossy().into_owned();
        let digest = if provider.protocol() == calm_types::runtime::AgentProtocol::Acp {
            Some(
                self.wiring
                    .acp
                    .configured(provider)?
                    .native_session_digest(&plan.path)?,
            )
        } else {
            None
        };
        let prompt = super::task_prompt::render_task_worker_prompt_tx(
            tx,
            &task.id,
            &payload.goal,
            &payload.context,
            payload.acceptance_criteria.as_deref(),
            &plan,
            self.gate_wait,
        )
        .await?;
        let mut data = json!({"schemaVersion":1,"harness_profile":"task_worker", "worker_provider":provider,
            "idempotency_key":task.id,"cwd":cwd,"prompt":prompt});
        card_selection.copy_to_payload(data.as_object_mut().expect("object"));
        let card = task_worker_create_tx(
            tx,
            card_id.clone(),
            worker.clone(),
            op.id.clone(),
            NewCard {
                track_id: task.track_id.clone().into(),
                kind: "codex".into(),
                sort: None,
                title: Some(task.key.clone()),
                payload: data,
            },
            provider.clone(),
            digest,
            &self.roles,
        )
        .await?;
        crate::db::sqlite::session_set_handle_state_tx(
            tx,
            &worker,
            Some(serde_json::to_value(
                crate::harness::HarnessSnapshot::initial(0, vec![]),
            )?),
        )
        .await?;
        if *provider == AgentProvider::Codex {
            self.backends
                .select_new_worker_tx(tx)
                .await?
                .persist_tx(tx, &worker)
                .await?;
        }
        let (lease, event) =
            acquire_workspace_lease_tx(tx, &card_id, &task.track_id, &op.id, &plan).await?;
        let mut output = TxOutput::new("card", Some(card_id.clone()), serde_json::to_value(&card)?);
        output.data = json!({"card_id":card_id,"worker_session_id":worker,"track_id":task.track_id,"cwd":cwd,
            "lease_id":lease.lease_id,"branch":plan.branch,"base_sha":plan.base.base_sha,
            "canonical_path":plan.base.canonical_path,"prompt":prompt,"provider":provider,"selection":StoredSelection {model:turn.model.clone(),effort:turn.effort.clone()}});
        record_declared_head(&mut output.data, plan.declared_head());
        record_catch_up(&mut output.data, plan.catch_up.as_ref())?;
        output.post_commit_events.extend(plan.superseded);
        output.post_commit_events.push(event);
        let scope =
            crate::routes::cards::card_scope_tx(tx, card.id.clone(), card.track_id.clone()).await?;
        output.post_commit_events.extend(
            super::workspace_lease::append_workspace_events_tx(
                tx,
                vec![(
                    crate::ids::ActorId::KernelDispatcher,
                    scope,
                    crate::event::Event::CardAdded(card),
                )],
            )
            .await?,
        );
        Ok(output)
    }
}
#[async_trait]
impl ProviderAdapter for AgentWorkerAdapter {
    fn kind(&self) -> &'static str {
        "agent-worker"
    }
    fn phases(&self) -> &'static [PhaseTag] {
        &[
            PhaseTag::Pending,
            PhaseTag::TxCommitted,
            PhaseTag::SpawnStarted,
            PhaseTag::SpawnSucceeded,
            PhaseTag::Succeeded,
        ]
    }
    async fn preparation_track(&self, input: &Value, _ctx: &SpawnCtx) -> Result<Option<String>> {
        Ok(Some(
            serde_json::from_value::<Payload>(input.clone())?.track_id,
        ))
    }
    async fn validate(&self, input: &Value) -> Result<()> {
        let p: Payload = serde_json::from_value(input.clone())?;
        if p.idempotency_key.is_empty() {
            return Err(CalmError::BadRequest("worker attempt is required".into()));
        }
        Ok(())
    }
    async fn prepare_external(
        &self,
        input: &Value,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<Option<Box<dyn PreparedTx>>> {
        let p: Payload = serde_json::from_value(input.clone())?;
        let task = self
            .repo
            .task_get(&p.idempotency_key)
            .await?
            .ok_or_else(|| CalmError::NotFound("worker task".into()))?;
        let track = self
            .repo
            .track_get(&p.track_id)
            .await?
            .ok_or_else(|| CalmError::NotFound("worker track".into()))?;
        let pool = ctx.operation_repo.sqlite_db();
        let planner: Option<String> = sqlx::query_scalar(
            "SELECT payload FROM cards WHERE track_id=?1 AND role='planner' LIMIT 1",
        )
        .bind(&p.track_id)
        .fetch_optional(pool.read_pool())
        .await?;
        let planner = planner
            .map(|v| serde_json::from_str::<Value>(&v))
            .transpose()?
            .and_then(|v| v.get("planner_provider").cloned())
            .map(serde_json::from_value::<AgentProvider>)
            .transpose()?
            .unwrap_or(AgentProvider::Codex);
        let access = CatalogAccess::new(
            &self.selection.availability,
            &self.wiring.claude,
            &self.wiring.acp,
            &self.selection.codex,
        );
        let declared = task
            .provider
            .as_ref()
            .map(|v| serde_json::from_value::<AgentProvider>(json!(v)))
            .transpose()?;
        let (provider, turn, card) = resolve_task_selection(
            &access,
            task.kind,
            declared,
            task.model.clone(),
            task.reasoning_effort.clone(),
            &planner,
            track.workspace.agent_cwd(),
        )
        .await?;
        let mut prepared = self.clone();
        prepared.resolved = Some((task, provider, turn, card));
        super::workspace_lease::worker::preparation::collect(
            prepared,
            op,
            ctx,
            &p.track_id,
            &p.idempotency_key,
            &self.root,
        )
        .await
    }
    async fn prepare_tx<'tx>(
        &self,
        _tx: &mut Tx<'tx>,
        _input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        Err(CalmError::Internal(
            "worker checkout requires external preparation".into(),
        ))
    }
    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        let p: Payload = serde_json::from_value(op.payload.clone())?;
        super::admit_task_side_effect(ctx.repo.as_ref(), &p.idempotency_key).await?;
        Ok(AppServerInteractOutcome::NotApplicable)
    }
    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        runner::spawn(self, output, op, ctx).await?;
        Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
    }
    async fn reconcile_sessions(&self, ctx: &SpawnCtx) -> Result<()> {
        settlement::recover(self, ctx).await
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
            reason: reason.into(),
            steps: vec![CompensationStep::new("cleanup_agent_worker", json!({}))],
        })
    }
    async fn compensate_step(
        &self,
        step: &CompensationStep,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()> {
        if step.completed {
            return Ok(());
        }
        if step.op != "cleanup_agent_worker" {
            return Err(CalmError::Internal(
                "unknown agent worker compensation step".into(),
            ));
        }
        settlement::compensate(self, output, op, ctx).await
    }
}
