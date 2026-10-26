//! Terminal task policy over the shared op-owned Pipe lifecycle. Legacy rows never enter here.
use super::*;
use crate::operation::{ParkedOutcome, ParkedRecovery, RecoveryMode, owned_execution as owned};
use owned::{Decision, Release, StopReason, pipe};
use std::{path::PathBuf, time::Duration};
use workspace_runtime::execution::*;

pub(crate) const KEY: &str = "terminal_worker_execution";
pub(crate) const CONSUMER: &str = "terminal_worker";
pub(crate) const CONFIG: pipe::PipeConfig = pipe::PipeConfig {
    consumer: CONSUMER,
    alias_prefix: "terminal_worker",
    record_key: KEY,
};
pub(super) const PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::SpawnStarted,
    PhaseTag::SpawnSucceeded,
    PhaseTag::Parked,
    PhaseTag::Succeeded,
];
/// The persistent preparation boundary; the execution record is written later at admission.
pub(crate) fn is_pipe(output: &TxOutput) -> bool {
    output.data.get("terminal_worker_pipe").is_some()
}
#[derive(Serialize, Deserialize)]
struct Prepared {
    log_path: PathBuf,
    script_path: PathBuf,
    exit_path: PathBuf,
    step_path: PathBuf,
}
impl Prepared {
    fn read(output: &TxOutput) -> Result<Self> {
        Ok(serde_json::from_value(
            output.data["terminal_worker_pipe"].clone(),
        )?)
    }
}
struct Authority<'a>(&'a str);
#[async_trait]
impl pipe::ReleaseAuthorization for Authority<'_> {
    async fn authorize_tx(&self, tx: &mut Tx<'_>) -> Result<()> {
        crate::task_recovery::require_attempt_startable_tx(tx, self.0).await?;
        let task = crate::db::sqlite::task_get_tx(tx, self.0)
            .await?
            .ok_or_else(|| CalmError::Conflict("terminal task disappeared".into()))?;
        if task.kind != crate::model::TaskKind::Terminal
            || !matches!(
                task.status,
                crate::model::TaskStatus::Dispatched | crate::model::TaskStatus::Running
            )
        {
            return Err(CalmError::Conflict(
                "terminal worker is no longer a task writer".into(),
            ));
        }
        Ok(())
    }
}
fn task_id(op: &Operation) -> Result<&str> {
    let id = op
        .payload
        .get("idempotency_key")
        .and_then(Value::as_str)
        .ok_or_else(|| CalmError::Conflict("terminal worker needs task identity".into()))?;
    if op.idempotency_key.as_deref() != Some(id) {
        return Err(CalmError::Conflict(
            "terminal worker task identity mismatch".into(),
        ));
    }
    Ok(id)
}
fn environment(env: &Value) -> Result<Vec<(String, String)>> {
    let mut result = std::collections::BTreeMap::new();
    if let Some(map) = env.as_object() {
        for value in map.values() {
            value
                .as_str()
                .ok_or_else(|| CalmError::BadRequest("invalid worker environment".into()))?;
        }
    }
    result.extend(crate::routes::terminal::terminal_child_envs(env));
    let path = crate::kernel_bin_path::kernel_led_path()?;
    result.insert("PATH".into(), path.path_utf8()?.into());
    Ok(result.into_iter().collect())
}
fn ambient() -> AmbientEnvironment {
    AmbientEnvironment::SupervisorAllowed {
        proxy_policy: AmbientProxyPolicy::RejectSecret,
    }
}
struct PreparedEnvironment {
    adapter: TerminalWorkerAdapter,
    claim: crate::operation::preparation_claim::PreparationClaim,
    env: Value,
}
#[async_trait]
impl crate::operation::PreparedTx for PreparedEnvironment {
    async fn apply(
        self: Box<Self>,
        tx: &mut Tx<'_>,
        op: &Operation,
    ) -> Result<crate::operation::PreparedTxResult> {
        self.claim.require_operation(op)?;
        self.adapter
            .prepare_pipe_tx(tx, &op.payload, op, self.env)
            .await
            .map(crate::operation::PreparedTxResult::Ready)
    }
}
impl TerminalWorkerAdapter {
    pub(super) async fn prepare_pipe_environment(
        &self,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<Option<Box<dyn crate::operation::PreparedTx>>> {
        let engine = owned::engine(ctx)?;
        let mut env = terminal_worker_env(self.repo.as_ref()).await?;
        let (key, value) = crate::claude_code_env::DISABLE_AUTO_MEMORY;
        env[key] = json!(value);
        engine
            .preflight_environment(&ExecutionSpecification {
                ambient_environment: ambient(),
                program: "/bin/sh".into(),
                args: vec![],
                environment: environment(&env)?,
                io_mode: calm_session::control::IoMode::Pipe,
                replay_bytes: 8192,
            })
            .await
            .map_err(|e| {
                CalmError::BadRequest(format!("terminal worker environment refused: {e:?}"))
            })?;
        Ok(Some(Box::new(PreparedEnvironment {
            adapter: self.clone(),
            claim: crate::operation::preparation_claim::PreparationClaim::new(op)?,
            env,
        })))
    }

    pub(super) async fn prepare_pipe_tx(
        &self,
        tx: &mut Tx<'_>,
        input: &Value,
        op: &Operation,
        env: Value,
    ) -> Result<TxOutput> {
        let payload: TerminalWorkerOperationPayload = serde_json::from_value(input.clone())?;
        crate::workspace_service::reclaim::operation::terminal_admission_tx(tx, &payload.track_id)
            .await?;
        super::super::refuse_if_context_stale(tx, Some(&payload.idempotency_key)).await?;
        let cwd = terminal_cwd_or_track_workspace(
            tx,
            &payload.track_id,
            explicit_terminal_cwd(payload.cwd),
        )
        .await?;
        let title = super::super::task_key_for_card_title(tx, &payload.idempotency_key).await;
        let card = crate::db::sqlite::card_create_with_id_tx(tx, new_id(), crate::model::NewCard {
            track_id: payload.track_id.clone().into(), kind: "terminal".into(), sort: None, title,
            payload: json!({"idempotency_key":payload.idempotency_key,"role_request":"terminal","cmd":payload.cmd,"cwd":cwd,
                "terminal_presentation":{"source":"task","in_workbench":false}}),
        }, CardRole::Worker, true, &self.card_role_cache).await?;
        calm_truth::db::sqlite::track_workspace_freeze_tx(
            tx,
            &payload.track_id,
            crate::model::now_ms(),
        )
        .await?;
        let prefix = self.log_directory.join(&op.id);
        let paths = Prepared {
            log_path: prefix.with_extension("log"),
            script_path: prefix.with_extension("sh"),
            exit_path: prefix.with_extension("exit"),
            step_path: prefix.with_extension("step"),
        };
        let mut output = TxOutput::new(
            "card",
            Some(card.id.to_string()),
            serde_json::to_value(&card)?,
        );
        output.data = json!({"card_id":card.id,"track_id":payload.track_id,"cmd":payload.cmd,"cwd":cwd,"env":env,"terminal_worker_pipe":paths});
        Ok(output)
    }
    pub(super) async fn spawn_pipe(
        &self,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        if output.data.get(KEY).is_none() {
            owned::engine(ctx)?;
            let id = task_id(op)?;
            let cwd = output.output_string("cwd", "terminal worker")?;
            let track = output.output_string("track_id", "terminal worker")?;
            let cmd = output.output_string("cmd", "terminal worker")?;
            let mut environment = environment(&output.data["env"])?;
            let paths = Prepared::read(output)?;
            tokio::fs::create_dir_all(
                paths
                    .log_path
                    .parent()
                    .ok_or_else(|| CalmError::Internal("worker log parent missing".into()))?,
            )
            .await?;
            let steps = [super::super::gate_lifecycle::GateStep {
                name: "terminal-worker".into(),
                cmd: format!("/bin/sh -c {}", crate::shell_command::sh_single_quote(&cmd)),
            }];
            tokio::fs::write(
                &paths.script_path,
                super::super::gate_process::WrapperSteps::Logged(&steps).render(),
            )
            .await?;
            tokio::fs::write(&paths.log_path, b"").await?;
            for (key, path) in [
                ("NEIGE_GATE_LOG_PATH", &paths.log_path),
                ("NEIGE_GATE_EXIT_PATH", &paths.exit_path),
                ("NEIGE_GATE_STEP_PATH", &paths.step_path),
            ] {
                environment.push((key.into(), path.display().to_string()));
            }
            pipe::admit(
                CONFIG,
                op,
                ctx,
                tokio::fs::canonicalize(cwd).await?,
                &track,
                ExecutionSpecification {
                    ambient_environment: ambient(),
                    program: "/bin/sh".into(),
                    args: vec![paths.script_path.display().to_string()],
                    environment,
                    io_mode: calm_session::control::IoMode::Pipe,
                    replay_bytes: 8192,
                },
                Some(&Authority(id)),
            )
            .await?;
        } else {
            pipe::record(CONFIG, op)?;
        }
        log_terminal_worker_card_added(
            ctx,
            &self.card_role_cache,
            &self.track_area_cache,
            &output_card_id(output)?,
            &crate::ids::TrackId::from(output.output_string("track_id", "terminal worker")?),
        )
        .await?;
        let (adapter, ctx, id) = (self.clone(), ctx.clone(), op.id.clone());
        Ok(SpawnOutcome::Parked {
            deadline_ms: i64::MAX,
            observer: Box::pin(async move {
                adapter.observe_pipe(ctx, id).await;
            }),
        })
    }
    async fn act_pipe(&self, op: &Operation, ctx: &SpawnCtx) -> Result<()> {
        let record = pipe::record(CONFIG, op)?;
        if record.decision != Decision::Observing {
            return Ok(());
        }
        match record.release {
            Release::Held => {
                let result = pipe::release(
                    CONFIG,
                    ctx,
                    op,
                    &Authority(task_id(op)?),
                    pipe::ReleaseRequest {
                        token: b"go\n",
                        timeout: super::super::gate_lifecycle::RELEASE_TIMEOUT,
                    },
                    #[cfg(any(test, feature = "fixtures"))]
                    None,
                )
                .await;
                if result.is_err() {
                    let latest = owned::refresh_claim(ctx, op).await?;
                    pipe::stop(CONFIG, ctx, &latest, StopReason::ReleaseRefused).await?;
                }
            }
            Release::Releasing { .. } => {
                pipe::stop(CONFIG, ctx, op, StopReason::ReleaseRefused).await?;
            }
            Release::Released { .. } => {}
        }
        Ok(())
    }
    pub(super) async fn recover_pipe(
        &self,
        op: &Operation,
        mode: RecoveryMode,
        ctx: &SpawnCtx,
    ) -> Result<ParkedRecovery> {
        // OQ7(ii): boot only resumes observation. It never stops Released or resends Releasing.
        if matches!(mode, RecoveryMode::Boot) {
            let (adapter, ctx, id) = (self.clone(), ctx.clone(), op.id.clone());
            tokio::spawn(async move {
                adapter.observe_pipe(ctx, id).await;
            });
        }
        self.act_pipe(op, ctx).await?;
        let latest = owned::refresh_claim(ctx, op).await?;
        let record = pipe::record(CONFIG, &latest)?;
        if owned::settlement(ctx, &record.execution_id)
            .await?
            .is_none()
        {
            owned::drain_exited(ctx, &latest, &record.execution_id).await?;
            return Ok(ParkedRecovery::LeaveParked);
        }
        // The completion callback rechecks this exact record in the same transaction as task completion.
        // Releasing/decision changes require a fresh parked claim/snapshot.
        if record != pipe::record(CONFIG, op)? {
            return Ok(ParkedRecovery::LeaveParked);
        }
        Ok(ParkedRecovery::Complete(Box::new((
            ParkedOutcome::Succeeded {
                result: super::super::required_output(op)?.result.clone(),
            },
            None,
        ))))
    }
    pub(super) async fn complete_pipe_tx(
        &self,
        tx: &mut Tx<'_>,
        op: &Operation,
    ) -> Result<Vec<BroadcastEnvelope>> {
        let record = pipe::record(CONFIG, op)?;
        pipe::retire(CONFIG, tx, op).await?;
        let settled =
            calm_truth::db::sqlite::gate_execution::settlement(tx, record.execution_id.as_str())
                .await?
                .ok_or_else(|| CalmError::Conflict("terminal execution not settled".into()))?;
        let status = match serde_json::from_str::<Settlement>(&settled)? {
            Settlement::Journal(
                ExecutionState::Drained { status, .. }
                | ExecutionState::OriginalGroupEmpty { status, .. },
            ) if matches!(record.release, Release::Released { .. })
                && record.decision == Decision::Observing =>
            {
                status
            }
            _ => None,
        };
        let output = super::super::required_output(op)?;
        let paths = Prepared::read(output)?;
        let tail = super::super::gate_process::try_read_log_tail(&paths.log_path, 8 * 1024)?;
        crate::scheduler::complete_terminal_task_tx(
            tx,
            task_id(op)?,
            &output.output_string("track_id", "terminal worker")?,
            &output_card_id(output)?,
            status,
            false,
            &tail.text,
            tail.truncated,
        )
        .await
    }
    async fn observe_pipe(self, ctx: SpawnCtx, id: String) {
        loop {
            match ctx.operation_repo.get_operation(&id).await {
                Ok(Some(op))
                    if matches!(
                        op.phase.tag(),
                        PhaseTag::Succeeded | PhaseTag::Failed | PhaseTag::Stuck
                    ) =>
                {
                    return;
                }
                Ok(None) => return,
                _ => {}
            }
            if let Ok(Some(op)) = ctx.operation_repo.claim_parked(&id).await
                && let Err(error) = super::super::owned_parked::reconcile(
                    &self,
                    &op,
                    RecoveryMode::PreDeadlineProbe,
                    &ctx,
                )
                .await
            {
                tracing::warn!(op_id=%id,%error,"terminal worker retained for retry");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    pub(super) async fn compensate_pipe(&self, op: &Operation, ctx: &SpawnCtx) -> Result<()> {
        if super::super::required_output(op)?.data.get(KEY).is_some() {
            pipe::stop(CONFIG, ctx, op, StopReason::ReleaseRefused).await?;
            let record = pipe::record(CONFIG, op)?;
            if owned::settlement(ctx, &record.execution_id)
                .await?
                .is_none()
            {
                return Err(CalmError::Conflict(
                    "terminal worker stop is not settled".into(),
                ));
            }
        }
        let card_id = output_card_id(super::super::required_output(op)?)?;
        let roles = self.card_role_cache.clone();
        crate::db::write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
            Box::pin(async move {
                let exists: bool =
                    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cards WHERE id=?1)")
                        .bind(&card_id)
                        .fetch_one(&mut **tx)
                        .await?;
                if exists {
                    crate::db::sqlite::card_delete_tx(tx, &card_id, &roles).await?;
                }
                Ok(())
            })
        })
        .await
    }
}
/// No terminal or session projection is created for a Pipe worker.
pub(crate) struct Projection;
#[async_trait]
impl crate::execution_store::ExecutionProjection for Projection {
    fn disposal(&self) -> Option<&dyn crate::execution_quiesce::owned::DisposalOwner> {
        static OWNER: crate::scheduler::card_disposal::CardDisposalOwner =
            crate::scheduler::card_disposal::CardDisposalOwner(CONFIG);
        Some(&OWNER)
    }
    async fn spawned(
        &self,
        _tx: &mut Tx<'_>,
        _alias: &str,
        _now: i64,
    ) -> std::result::Result<(), sqlx::Error> {
        Ok(())
    }
    async fn settled(
        &self,
        _tx: &mut Tx<'_>,
        _alias: &str,
        _settlement: &Settlement,
        _now: i64,
    ) -> std::result::Result<(), sqlx::Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
