//! A single task-bound Operation owns preparation, one turn and final stop.
use super::{
    OPERATION_KIND, WorkerPayload,
    config::{Backend, provider_error},
    record::{Admission, ProviderRecord, RecordVersion, RunRecord},
};
use crate::db::{RouteRepo, write_in_tx_typed};
use crate::dedicated_codex::{DedicatedIdentity, DedicatedRequest, NativeMcp, SessionRecord};
use crate::error::{CalmError, Result};
use crate::operation::*;
use crate::state::WriteContext;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

#[derive(Clone)]
pub struct IsolatedCodexAdapter {
    pub(crate) backend: Option<Arc<Backend>>,
    pub(crate) repo: Arc<dyn RouteRepo>,
    pub(crate) socket: Option<PathBuf>,
    pub(crate) write: WriteContext,
}
impl IsolatedCodexAdapter {
    pub fn new(
        backend: Option<Arc<Backend>>,
        repo: Arc<dyn RouteRepo>,
        socket: Option<PathBuf>,
        write: WriteContext,
    ) -> Self {
        Self {
            backend,
            repo,
            socket,
            write,
        }
    }
    pub(crate) fn backend(&self) -> Result<&Arc<Backend>> {
        self.backend.as_ref().ok_or_else(|| {
            CalmError::Conflict(
                "isolated Codex is unavailable; configure --isolated-codex-config".into(),
            )
        })
    }
    pub(crate) async fn record(&self, op: &Operation) -> Result<RunRecord> {
        let id = op.id.clone();
        write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move { super::journal::load_tx(tx, &id).await })
        })
        .await
    }
    pub(crate) fn checkpoint(
        &self,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<super::journal::OperationCheckpoint> {
        Ok(super::journal::OperationCheckpoint {
            repo: self.repo.clone(),
            operation: op.clone(),
            events: ctx.events.clone(),
            task_timeout_ms: self.backend()?.task_timeout_ms,
        })
    }
    pub(crate) async fn prepare_endpoint(&self, op: &Operation) -> Result<RunRecord> {
        let record = self.record(op).await?;
        if matches!(record.provider, ProviderRecord::Prepared(_)) {
            return Ok(record);
        }
        let backend = self.backend()?;
        let root = backend.workspace_root.clone();
        let id = op.id.clone();
        let path = tokio::task::spawn_blocking(move || super::workspace::prepare(&root, &id))
            .await
            .map_err(|_| {
                CalmError::Internal("isolated workspace preparation interrupted".into())
            })??;
        if path != record.request.workspace {
            return Err(CalmError::Conflict(
                "isolated workspace request changed".into(),
            ));
        }
        let native = NativeMcp {
            socket: self
                .socket
                .clone()
                .ok_or_else(|| CalmError::Conflict("native MCP unavailable".into()))?,
            card_token: record.native_token.clone(),
        };
        let endpoint = backend
            .controller
            .prepare(record.request.clone(), &backend.seed, &native)
            .await
            .map_err(provider_error)?;
        let session = SessionRecord::prepared(endpoint);
        let owned = op.clone();
        write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move { super::journal::prepared_tx(tx, &owned, session).await })
        })
        .await?;
        self.record(op).await
    }
}
#[async_trait]
impl ProviderAdapter for IsolatedCodexAdapter {
    fn kind(&self) -> &'static str {
        OPERATION_KIND
    }
    fn phases(&self) -> &'static [PhaseTag] {
        &[
            PhaseTag::Pending,
            PhaseTag::TxCommitted,
            PhaseTag::SpawnStarted,
            PhaseTag::Parked,
        ]
    }
    fn owns_parked_resource(&self) -> bool {
        true
    }
    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: WorkerPayload = serde_json::from_value(input.clone())?;
        if payload.actor != crate::ids::ActorId::KernelDispatcher
            || payload.task_id.is_empty()
            || payload.track_id.is_empty()
            || payload.idempotency_key != payload.task_id
        {
            return Err(CalmError::BadRequest(
                "invalid isolated task operation identity".into(),
            ));
        }
        Ok(())
    }
    #[allow(
        deprecated,
        reason = "existing card transaction helper requires its write-through role cache"
    )]
    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> Result<TxOutput> {
        self.validate(input).await?;
        let backend = self.backend()?;
        if self.socket.is_none() {
            return Err(CalmError::Conflict(
                "isolated Codex requires native MCP".into(),
            ));
        }
        let task = super::admission::validate_start_tx(tx, op).await?;
        let card_id = crate::model::new_id();
        let session_id = crate::model::new_id();
        let workspace = backend.workspace_root.join(&op.id);
        let context: Value = serde_json::from_str(&task.context_json)?;
        let prompt = format!(
            "This task starts in a new empty workspace. Files you create remain with this execution. Before ending the turn, report through the native calm.task.complete or calm.task.fail tool using the exact task ID below.\n\n{}",
            codex_adapter::render_task_worker_prompt(
                &task.id,
                &task.goal,
                &context,
                task.acceptance_criteria.as_deref()
            )
        );
        let (mut card, terminal, token) = crate::db::sqlite::card_with_codex_create_tx(
            tx,
            card_id.clone(),
            &session_id,
            Some(&op.id),
            task.track_id.clone().into(),
            None,
            None,
            workspace.to_string_lossy().into_owned(),
            json!({}),
            None,
            None,
            None,
            crate::model::CardRole::Worker,
            true,
            self.write.role_cache(),
            crate::routes::theme::RequestTheme::default_dark(),
        )
        .await?;
        let mut payload =
            card.payload.as_object().cloned().ok_or_else(|| {
                CalmError::Internal("worker card payload must be an object".into())
            })?;
        payload.insert("idempotency_key".into(), json!(task.id));
        payload.insert("goal".into(), json!(task.goal));
        payload.insert("context".into(), context);
        payload.insert("prompt".into(), json!(prompt));
        card = crate::db::sqlite::card_update_tx(
            tx,
            &card_id,
            crate::model::CardPatch {
                title: Some(task.key.clone()),
                kind: None,
                sort: None,
                payload: Some(Value::Object(payload)),
                deletable: None,
            },
        )
        .await?;
        let record = RunRecord {
            version: RecordVersion::V1,
            track_id: task.track_id.clone(),
            request: DedicatedRequest {
                identity: DedicatedIdentity {
                    run_id: op.id.clone(),
                    attempt_id: task.id.clone(),
                    card_id: card_id.clone(),
                    session_id: session_id.clone(),
                },
                workspace: workspace.clone(),
                developer_instructions: prompt,
            },
            native_token: token.ok_or_else(|| {
                CalmError::Internal("isolated worker native token missing".into())
            })?,
            admission: Admission::Open,
            provider: ProviderRecord::Unprepared,
        };
        let mut output = TxOutput::new("card", Some(card_id.clone()), serde_json::to_value(card)?);
        output.data = json!({"isolated_execution":record,"card_id":card_id,"worker_session_id":session_id,
            "task_id":task.id,"track_id":task.track_id,"terminal_id":terminal.id,"cwd":workspace});
        Ok(output)
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
        _: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        // A committed turn acknowledgement is observation recovery, not a new launch.
        if let Some(parked) = super::observe::acknowledged_recovery(self, op, ctx).await? {
            return Ok(parked);
        }
        let owned = op.clone();
        write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                super::journal::require_owner_tx(tx, &owned).await?;
                super::admission::validate_start_tx(tx, &owned)
                    .await
                    .map(|_| ())
            })
        })
        .await?;
        let record = self.prepare_endpoint(op).await?;
        let checkpoint = self.checkpoint(op, ctx)?;
        let backend = self.backend()?;
        let mut session = backend
            .controller
            .connect(record.session()?.clone(), &checkpoint)
            .await
            .map_err(provider_error)?;
        if session.record().phase == crate::dedicated_codex::RequestPhase::CreatingThread {
            session
                .reconcile(&checkpoint)
                .await
                .map_err(provider_error)?;
        }
        session
            .create_thread(&checkpoint)
            .await
            .map_err(provider_error)?;
        session
            .begin_turn(
                &op.id,
                &record.request.developer_instructions,
                &checkpoint,
                &super::turn::Guard {
                    repo: self.repo.clone(),
                    operation: op.clone(),
                },
            )
            .await
            .map_err(provider_error)?;
        let deadline = crate::model::now_ms().saturating_add(backend.task_timeout_ms);
        Ok(SpawnOutcome::Parked {
            deadline_ms: deadline,
            observer: super::observe::observer(self.clone(), op.clone(), ctx.clone(), session),
        })
    }
    async fn recover_owned_parked(
        &self,
        op: &Operation,
        mode: RecoveryMode,
        ctx: &SpawnCtx,
    ) -> Result<ParkedRecovery> {
        super::observe::reconcile(self, op, mode, ctx).await
    }
    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        _: &TxOutput,
        _: &Operation,
    ) -> Result<CompensationStateVersioned> {
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.into(),
            steps: vec![CompensationStep::new("stop-isolated", json!({}))],
        })
    }
    async fn compensate_step(
        &self,
        step: &CompensationStep,
        _: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()> {
        if step.op != "stop-isolated" {
            return Err(CalmError::Internal(
                "unknown isolated compensation step".into(),
            ));
        }
        super::observe::fail(
            self,
            op,
            ctx,
            "Isolated execution could not finish startup or recovery; stopping its owned runtime.",
        )
        .await?;
        super::observe::stop(self, op, ctx).await
    }
}
