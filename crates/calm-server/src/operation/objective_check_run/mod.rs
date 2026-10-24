//! User-approved command checks, executed by the kernel at a frozen revision.
pub(crate) mod checkout;
mod lifecycle;

use super::gate_execution::{self as execution, Decision, Release, StopReason};
use super::*;
use calm_truth::objective_host::{self, IntentId, IntentStatus};
use objective::{Intent, check::CheckConfig};
use std::path::PathBuf;
use workspace_runtime::execution::{ExecutionState, Settlement};

pub const KIND: &str = "objective-check-run";
const PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::SpawnStarted,
    PhaseTag::Parked,
    PhaseTag::Succeeded,
];
const TIMEOUT_SECS: i64 = 1800;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Payload {
    pub intent_id: IntentId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Frozen {
    pub intent_id: IntentId,
    pub intent: Intent,
    pub command: String,
    pub revision: String,
    pub timeout_secs: i64,
    pub repo: PathBuf,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<PathBuf>,
}
impl Frozen {
    fn from_output(output: &TxOutput) -> Result<Self> {
        Ok(serde_json::from_value(output.data.clone())?)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum CheckRunResult {
    Measured {
        exit_code: i32,
    },
    Metric {
        output: objective::check::MetricOutput,
    },
    Failed {
        reason: String,
    },
}

#[cfg(any(test, feature = "fixtures"))]
type Hook = Arc<dyn Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

#[derive(Clone)]
pub struct ObjectiveCheckRunAdapter {
    root: PathBuf,
    logs: PathBuf,
    timeout_secs: i64,
    #[cfg(any(test, feature = "fixtures"))]
    before_spawn: Option<Hook>,
    #[cfg(any(test, feature = "fixtures"))]
    before_release: Option<Hook>,
}
impl ObjectiveCheckRunAdapter {
    /// Both directories are kernel-owned, outside worker writable roots.
    pub fn new(root: PathBuf, logs: PathBuf) -> Self {
        Self {
            root,
            logs,
            timeout_secs: TIMEOUT_SECS,
            #[cfg(any(test, feature = "fixtures"))]
            before_spawn: None,
            #[cfg(any(test, feature = "fixtures"))]
            before_release: None,
        }
    }
    #[cfg(any(test, feature = "fixtures"))]
    pub fn with_timeout(mut self, seconds: i64) -> Self {
        self.timeout_secs = seconds.max(1);
        self
    }
    #[cfg(any(test, feature = "fixtures"))]
    pub fn with_before_spawn(mut self, hook: Hook) -> Self {
        self.before_spawn = Some(hook);
        self
    }
    #[cfg(any(test, feature = "fixtures"))]
    pub fn with_before_release(mut self, hook: Hook) -> Self {
        self.before_release = Some(hook);
        self
    }
    fn paths(&self, op: &Operation) -> gate_lifecycle::GatePaths {
        gate_lifecycle::GatePaths::new(&self.logs, &op.id)
    }
}

#[async_trait]
impl ProviderAdapter for ObjectiveCheckRunAdapter {
    fn kind(&self) -> &'static str {
        KIND
    }
    fn phases(&self) -> &'static [PhaseTag] {
        PHASES
    }
    async fn validate(&self, input: &Value) -> Result<()> {
        let _: Payload = serde_json::from_value(input.clone())?;
        Ok(())
    }
    async fn preparation_track(&self, input: &Value, _ctx: &SpawnCtx) -> Result<Option<String>> {
        let payload: Payload = serde_json::from_value(input.clone())?;
        Ok(Some(payload.intent_id.track_id.to_string()))
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> Result<TxOutput> {
        let Payload { intent_id } = serde_json::from_value(input.clone())?;
        let (row, definition) = objective_host::current_check_tx(tx, &intent_id)
            .await?
            .ok_or_else(|| CalmError::Conflict("objective check authorization expired".into()))?;
        if !crate::objective_dispatch::supports(&definition) {
            return Err(CalmError::Conflict(
                "objective check has no executor".into(),
            ));
        }
        let metric = matches!(definition.config, CheckConfig::Metric(_));
        let command = match definition.config {
            CheckConfig::Command(params) => params.command,
            CheckConfig::Metric(params) => params.command,
            _ => {
                return Err(CalmError::Conflict(
                    "objective-check-run requires command or metric".into(),
                ));
            }
        };
        let Intent::RunCheck { revision, .. } = &row.intent else {
            unreachable!()
        };
        let workspace =
            crate::db::sqlite::track_workspace_read_tx(tx, intent_id.track_id.as_str()).await?;
        if row.status == IntentStatus::Pending {
            objective_host::intent_mark_dispatched_tx(tx, &intent_id).await?;
        }
        let frozen = Frozen {
            stdout: metric.then(|| self.paths(op).script.with_extension("stdout")),
            cwd: self.root.join(intent_id.track_id.as_str()).join(format!(
                "{}-{}-{}",
                intent_id.seq.0, intent_id.ordinal, op.id
            )),
            repo: workspace.path.into(),
            revision: revision.as_str().into(),
            command,
            timeout_secs: self.timeout_secs,
            intent: row.intent,
            intent_id,
        };
        let mut output = TxOutput::new(
            "track",
            Some(frozen.intent_id.track_id.to_string()),
            json!({}),
        );
        output.data = serde_json::to_value(frozen)?;
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
    fn owns_parked_resource(&self) -> Option<&'static str> {
        Some(execution::KEY)
    }
    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        #[cfg(any(test, feature = "fixtures"))]
        if let Some(hook) = &self.before_spawn {
            hook().await;
        }
        let frozen = Frozen::from_output(output)?;
        let redrive = output.data.get(execution::KEY).is_some();
        if !redrive {
            execution::engine(ctx)?;
            checkout::create(&frozen).await?;
            let paths = self.paths(op);
            paths.unlink_stale(&self.logs).await?;
            let step = gate_lifecycle::GateStep {
                name: if frozen.stdout.is_some() {
                    "metric"
                } else {
                    "command"
                }
                .into(),
                cmd: frozen.command.clone(),
            };
            let logged = [step.clone()];
            let steps = match &frozen.stdout {
                Some(path) => gate_process::WrapperSteps::CaptureStdout {
                    step,
                    path: path.clone(),
                },
                None => gate_process::WrapperSteps::Logged(&logged),
            };
            execution::admit(
                op,
                ctx,
                frozen
                    .cwd
                    .to_str()
                    .ok_or_else(|| CalmError::Internal("non-UTF8 objective checkout".into()))?,
                frozen.intent_id.track_id.as_str(),
                gate_process::GateFiles {
                    script: &paths.script,
                    log: &paths.log,
                    exit: &paths.exit,
                    step: &paths.step,
                },
                steps,
                &op.id,
            )
            .await?;
        }
        let (adapter, ctx, id) = (self.clone(), ctx.clone(), op.id.clone());
        Ok(SpawnOutcome::Parked {
            deadline_ms: now_ms() + frozen.timeout_secs * 1000,
            observer: Box::pin(async move {
                adapter.observe(ctx, id, redrive).await;
            }),
        })
    }
    async fn recover_owned_parked(
        &self,
        op: &Operation,
        mode: RecoveryMode,
        ctx: &SpawnCtx,
    ) -> Result<ParkedRecovery> {
        let record = execution::record(op)?;
        if mode == RecoveryMode::Boot {
            let (adapter, observer_ctx, id) = (self.clone(), ctx.clone(), op.id.clone());
            tokio::spawn(async move {
                adapter.observe(observer_ctx, id, true).await;
            });
            if record.decision == Decision::Observing {
                execution::stop(ctx, op, StopReason::KernelRestarted).await?;
                return Ok(ParkedRecovery::LeaveParked);
            }
        }
        let settlement = owned_execution::settlement(ctx, &record.execution_id).await?;
        let Some(settlement) = settlement else {
            if record.decision == Decision::Observing
                && op.parked_deadline_ms.is_some_and(|d| now_ms() >= d)
            {
                execution::stop(ctx, op, StopReason::Timeout).await?;
            } else {
                owned_execution::drain_exited(ctx, op, &record.execution_id).await?;
            }
            return Ok(ParkedRecovery::LeaveParked);
        };
        let result = match (&record.decision, &record.release, settlement) {
            (
                Decision::Observing,
                Release::Released { .. },
                Settlement::Journal(
                    ExecutionState::Drained {
                        status: Some(code), ..
                    }
                    | ExecutionState::OriginalGroupEmpty {
                        status: Some(code), ..
                    },
                ),
            ) => match Frozen::from_output(required_output(op)?)?.stdout {
                Some(path) => metric_result(code, &path),
                None => CheckRunResult::Measured { exit_code: code },
            },
            (Decision::Stopping { reason }, _, _) => CheckRunResult::Failed {
                reason: format!("{reason:?}"),
            },
            _ => CheckRunResult::Failed {
                reason: "execution has no acknowledged wait status".into(),
            },
        };
        Ok(ParkedRecovery::Complete(Box::new((
            ParkedOutcome::Succeeded {
                result: serde_json::to_value(result)?,
            },
            None,
        ))))
    }
    async fn complete_owned_parked_tx(
        &self,
        tx: &mut Tx<'_>,
        op: &Operation,
    ) -> Result<Vec<BroadcastEnvelope>> {
        // owned_parked::reconcile fenced the live claim before changing phase,
        // in this same transaction. Completion has already cleared its lease here.
        execution::retire(tx, op).await?;
        lifecycle::finish_tx(tx, op).await
    }
    fn owned_parked_completed(&self, op: &Operation, ctx: &SpawnCtx) {
        if let Ok(record) = execution::record(op) {
            ctx.execution.drive([record.execution_id]);
        }
        if let Ok(frozen) = required_output(op).and_then(Frozen::from_output) {
            let (db, id) = (ctx.operation_repo.sqlite_db(), op.id.clone());
            tokio::spawn(async move {
                if let Err(error) = checkout::cleanup_operation(&db, &id, &frozen).await {
                    tracing::warn!(%error, "objective checkout cleanup deferred");
                }
            });
        }
    }
    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        _output: &TxOutput,
        _op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        // A recorded execution is retained by owned_execution, never compensated blindly.
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.into(),
            steps: vec![],
        })
    }
    async fn compensate_step(
        &self,
        _step: &CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<()> {
        Err(CalmError::Internal(
            "objective check has no compensation steps".into(),
        ))
    }
}

impl ObjectiveCheckRunAdapter {
    async fn observe(self, ctx: SpawnCtx, id: String, mut restart: bool) {
        #[cfg(any(test, feature = "fixtures"))]
        if let Some(hook) = &self.before_release {
            hook().await;
        }
        loop {
            let Ok(Some(current)) = ctx.operation_repo.get_operation(&id).await else {
                return;
            };
            if current.phase != Phase::Parked {
                return;
            }
            if let Ok(Some(op)) = ctx.operation_repo.claim_parked(&id).await {
                let result: Result<()> = async {
                    let record = execution::record(&op)?;
                    if record.decision == Decision::Observing {
                        if restart {
                            execution::stop(&ctx, &op, StopReason::KernelRestarted).await?;
                            restart = false;
                        } else if record.release == Release::Held {
                            let frozen = Frozen::from_output(required_output(&op)?)?;
                            if op.parked_deadline_ms.is_some_and(|d| now_ms() >= d) {
                                execution::stop(&ctx, &op, StopReason::Timeout).await?;
                            } else if execution::release(
                                &ctx,
                                &op,
                                execution::ReleaseAuthority::ObjectiveIntent(&frozen.intent_id),
                                #[cfg(any(test, feature = "fixtures"))]
                                None,
                            )
                            .await
                            .is_err()
                            {
                                let latest = execution::refresh_claim(&ctx, &op).await?;
                                execution::stop(&ctx, &latest, StopReason::ReleaseRefused).await?;
                            }
                        }
                    }
                    let latest = execution::refresh_claim(&ctx, &op).await?;
                    owned_parked::reconcile(&self, &latest, RecoveryMode::PreDeadlineProbe, &ctx)
                        .await
                }
                .await;
                if let Err(error) = result {
                    tracing::warn!(%id, %error, "objective execution retained");
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}

// The numeric parser belongs to objective; this layer owns bounded process evidence.
fn metric_result(exit_code: i32, path: &std::path::Path) -> CheckRunResult {
    let parsed: std::result::Result<_, String> = (|| {
        if exit_code != 0 {
            return Err(format!("metric command exited with {exit_code}"));
        }
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .and_then(|file| {
                file.take((gate_process::CAPTURE_STDOUT_LIMIT + 1) as u64)
                    .read_to_end(&mut bytes)
            })
            .map_err(|e| format!("metric stdout: {e}"))?;
        if bytes.len() > gate_process::CAPTURE_STDOUT_LIMIT {
            return Err("metric stdout exceeded 64 KiB".into());
        }
        let stdout = std::str::from_utf8(&bytes).map_err(|e| format!("metric stdout: {e}"))?;
        objective::check::parse_metric_output(stdout).map_err(|e| format!("metric stdout: {e:?}"))
    })();
    match parsed {
        Ok(output) => CheckRunResult::Metric { output },
        Err(reason) => CheckRunResult::Failed { reason },
    }
}
