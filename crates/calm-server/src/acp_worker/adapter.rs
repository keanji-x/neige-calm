//! Thin task adapter: shared checkout preparation, a Worker session, and the common ACP driver.
use crate::{
    acp_session::host::AcpHost,
    card_role_cache::CardRoleCache,
    db::RouteRepo,
    error::{CalmError, Result},
    model::{NewCard, TaskAccess, new_id, now_ms},
    operation::{
        self, AppServerInteractOutcome, CompensationStateVersioned, CompensationStep, Operation,
        PhaseTag, ProviderAdapter, SpawnCtx, SpawnHandle, SpawnOutcome, Tx, TxOutput,
    },
};
use calm_types::{
    acp_worker::{AcpWorkerState, ReadOnlyWorkerTarget, WorkerRequestedSelection},
    provider_worker::{ProviderWorkerCapabilities, WorkerOperationKind, task_worker_contract},
    worker::{
        LivenessTag, SessionMode, WorkerProviderKind, WorkerSession, WorkerSessionId,
        WorkerSessionState,
    },
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

#[derive(Clone)]
pub struct AcpWorkerAdapter {
    repo: Arc<dyn RouteRepo>,
    host: Arc<AcpHost>,
    roles: CardRoleCache,
    root: PathBuf,
    wait: operation::task_gate_run::GateRunWait,
}
impl AcpWorkerAdapter {
    pub fn new(
        repo: Arc<dyn RouteRepo>,
        host: Arc<AcpHost>,
        roles: CardRoleCache,
        root: PathBuf,
    ) -> Self {
        host.completion.bind_host(Arc::downgrade(&host));
        Self {
            repo,
            host,
            roles,
            root,
            wait: operation::task_gate_run::GateRunWait::DEFAULT,
        }
    }
    pub fn with_gate_run_wait(mut self, wait: operation::task_gate_run::GateRunWait) -> Self {
        self.wait = wait;
        self
    }
}
#[async_trait::async_trait]
impl operation::workspace_lease::worker::preparation::WorkerAdmission for AcpWorkerAdapter {
    async fn publish_worker(
        &self,
        tx: &mut Tx<'_>,
        op: &Operation,
        plan: operation::workspace_lease::WorkerLeasePlan,
    ) -> Result<TxOutput> {
        let payload: operation::codex_adapter::CodexWorkerOperationPayload =
            serde_json::from_value(op.payload.clone())?;
        let task = crate::db::sqlite::task_get_tx(tx, &payload.idempotency_key)
            .await?
            .ok_or_else(|| CalmError::NotFound("ACP Worker task".into()))?;
        let provider = task
            .agent_provider()
            .map_err(CalmError::BadRequest)?
            .ok_or_else(|| CalmError::BadRequest("ACP Worker provider is required".into()))?;
        let cap = ProviderWorkerCapabilities::for_provider(provider.clone())
            .filter(|cap| cap.operation == WorkerOperationKind::AcpWorker)
            .ok_or_else(|| CalmError::BadRequest("provider has no ACP Worker capability".into()))?;
        let _ = self.host.registration(&provider)?;
        let prompt = operation::task_prompt::render_task_worker_prompt_tx(
            tx,
            &task.id,
            &task.goal,
            &serde_json::from_str(&task.context_json)?,
            task.acceptance_criteria.as_deref(),
            &plan,
            self.wait,
        )
        .await?;
        let mut state = AcpWorkerState::prepared(
            task.id.clone(),
            WorkerRequestedSelection {
                provider: provider.clone(),
                model: task.model.clone(),
                reasoning_effort: task.reasoning_effort.clone(),
            },
        );
        if task.access == TaskAccess::ReadOnly {
            state.read_only_target = Some(ReadOnlyWorkerTarget {
                canonical_path: plan.base.canonical_path.to_string_lossy().into_owned(),
                git_common_dir: plan.base.git_common_dir.to_string_lossy().into_owned(),
                head: plan.base.base_sha.clone(),
            });
        }
        let now = now_ms();
        let worker = new_id();
        let prompt = format!(
            "{}\n\n{}",
            crate::planner_card::render_system_prompt(
                crate::planner_card::WORKER_ACP_SYSTEM_PROMPT,
                &task.track_id
            ),
            prompt
        );
        let card=crate::db::sqlite::card_create_tx(tx,NewCard{track_id:task.track_id.clone().into(),kind:"codex".into(),sort:None,title:Some(task.key.clone()),payload:json!({"schemaVersion":1,"agent_provider":provider,"idempotency_key":task.id,"goal":task.goal,"prompt":prompt,"acp_worker":true,"track_id":task.track_id,"task_key":task.key})},&self.roles).await?;
        crate::db::sqlite::session_insert_tx(
            tx,
            WorkerSession {
                id: WorkerSessionId::from(worker.clone()),
                track_id: task.track_id.clone().into(),
                provider: WorkerProviderKind::from(&provider),
                mode: SessionMode::Resumable,
                contract: task_worker_contract(task.access, task.result_contract.as_deref()),
                parent_session_id: None,
                requester_session_id: None,
                state: WorkerSessionState::Starting,
                mcp_token_hash: None,
                thread_id: Some(worker.clone()),
                agent_session_id: None,
                active_turn_id: None,
                terminal_run_id: None,
                card_id: Some(card.id.clone()),
                handle_state_json: Some(serde_json::to_value(&state)?),
                liveness: LivenessTag::Unknown,
                liveness_probed_at_ms: None,
                exit_code: None,
                exit_interpretation: None,
                spawn_op_id: Some(op.id.clone()),
                last_activity_ms: None,
                last_thread_status: None,
                created_at_ms: now,
                updated_at_ms: now,
                completed_at_ms: None,
            },
        )
        .await?;
        let installed =
            sqlx::query("UPDATE cards SET session_id=?1 WHERE id=?2 AND session_id IS NULL")
                .bind(&worker)
                .bind(card.id.as_str())
                .execute(&mut **tx)
                .await?
                .rows_affected();
        if installed != 1 {
            return Err(CalmError::Conflict(
                "ACP Worker card carrier changed".into(),
            ));
        }
        let (lease, event) = operation::workspace_lease::acquire_workspace_lease_tx(
            tx,
            card.id.as_str(),
            &task.track_id,
            &op.id,
            &plan,
        )
        .await?;
        // The scheduler may not yet have observed spawn's return when the first MCP call arrives.
        sqlx::query("UPDATE tasks SET worker_card_id=?1 WHERE id=?2 AND status='dispatched' AND worker_card_id IS NULL").bind(card.id.as_str()).bind(&task.id).execute(&mut **tx).await?;
        let mut output = TxOutput::new(
            "card",
            Some(card.id.to_string()),
            serde_json::to_value(&card)?,
        );
        output.data = json!({"card_id":card.id,"runtime_id":worker,"track_id":task.track_id,"attempt_id":task.id,"provider":cap.provider,"cwd":plan.path,"lease_id":lease.lease_id,"branch":plan.branch,"base_sha":plan.base.base_sha,"canonical_path":plan.base.canonical_path,"git_common_dir":plan.base.git_common_dir,"prompt":prompt});
        operation::workspace_lease::worker::record_catch_up(
            &mut output.data,
            plan.catch_up.as_ref(),
        )?;
        let scope =
            operation::workspace_lease::workspace_scope_tx(tx, card.id.as_str(), &task.track_id)
                .await?;
        let published = operation::workspace_lease::append_workspace_events_tx(
            tx,
            vec![
                (
                    crate::ids::ActorId::KernelDispatcher,
                    scope.clone(),
                    crate::event::Event::CardAdded(card.clone()),
                ),
                (
                    crate::ids::ActorId::KernelDispatcher,
                    scope,
                    crate::event::Event::WorkerSessionStarted {
                        worker_session_id: worker,
                        card_id: card.id.to_string(),
                        kind: calm_types::runtime::WorkerSessionKind::CodexCard,
                        agent_provider: Some(provider),
                        status: WorkerSessionState::Starting,
                    },
                ),
            ],
        )
        .await?;
        output.post_commit_events.extend(published);
        output.post_commit_events.extend(plan.superseded);
        output.post_commit_events.push(event);
        Ok(output)
    }
}
#[async_trait::async_trait]
impl ProviderAdapter for AcpWorkerAdapter {
    fn kind(&self) -> &'static str {
        WorkerOperationKind::AcpWorker.as_str()
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
    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: operation::codex_adapter::CodexWorkerOperationPayload =
            serde_json::from_value(input.clone())?;
        if payload.idempotency_key.trim().is_empty() {
            return Err(CalmError::BadRequest(
                "ACP Worker attempt is required".into(),
            ));
        }
        let task = self
            .repo
            .task_get(&payload.idempotency_key)
            .await?
            .ok_or_else(|| CalmError::NotFound("ACP Worker attempt".into()))?;
        if task.track_id != payload.track_id
            || task.worker_operation_kind() != Some(WorkerOperationKind::AcpWorker.as_str())
        {
            return Err(CalmError::Conflict(
                "ACP Worker operation does not match the frozen task".into(),
            ));
        }
        Ok(())
    }
    async fn preparation_track(&self, input: &Value, _: &SpawnCtx) -> Result<Option<String>> {
        Ok(Some(
            serde_json::from_value::<operation::codex_adapter::CodexWorkerOperationPayload>(
                input.clone(),
            )?
            .track_id,
        ))
    }
    async fn prepare_external(
        &self,
        input: &Value,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<Option<Box<dyn operation::PreparedTx>>> {
        let payload: operation::codex_adapter::CodexWorkerOperationPayload =
            serde_json::from_value(input.clone())?;
        operation::workspace_lease::worker::preparation::collect(
            self.clone(),
            op,
            ctx,
            &payload.track_id,
            &payload.idempotency_key,
            &self.root,
        )
        .await
    }
    async fn prepare_tx<'tx>(&self, _: &mut Tx<'tx>, _: &Value, _: &Operation) -> Result<TxOutput> {
        Err(CalmError::Internal(
            "ACP Worker checkout requires external preparation".into(),
        ))
    }
    async fn app_server_interact(
        &self,
        _: &mut TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }
    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        self.host
            .native_workers
            .clone()
            .start(self.host.clone(), output, op, ctx)
            .await?;
        if let Ok(held) = ctx.held_track() {
            held.release();
        }
        Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
    }
    fn fence_worker_session(&self, session: &str, turn: Option<&str>) -> bool {
        self.host.native_workers.fence(session, turn)
    }
    async fn stop_worker_session(
        &self,
        runtime: &crate::session_projection_repo::WorkerSessionProjection,
    ) -> Result<bool> {
        self.host
            .native_workers
            .stop(&self.host, &runtime.id, runtime.active_turn_id.as_deref())
            .await?;
        Ok(true)
    }
    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        _: &Operation,
    ) -> Result<CompensationStateVersioned> {
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.into(),
            steps: vec![CompensationStep::new(
                "cleanup_acp_worker",
                output.data.clone(),
            )],
        })
    }
    async fn compensate_step(
        &self,
        step: &CompensationStep,
        _: &TxOutput,
        _: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()> {
        let session = step.args["runtime_id"]
            .as_str()
            .ok_or_else(|| CalmError::Conflict("ACP Worker cleanup identity missing".into()))?;
        self.host
            .native_workers
            .stop(&self.host, session, None)
            .await?;
        super::lifecycle::fail_and_finalize(ctx, session, None, "ACP Worker startup failed").await
    }
}
