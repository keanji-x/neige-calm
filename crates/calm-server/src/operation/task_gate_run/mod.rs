//! `task-gate-run` operation (#2464): the worker asks, the kernel commits the worker's checkout as
//! the attempt's one commit above the lease base and runs the task's declared gate on it, in the
//! lease checkout, outside the sandbox. One operation per run (`"{task.id}#r{N}"`). The attempt's
//! first gate verdict reuses the last run when §4 of the design holds (`task_verify_adapter/reuse.rs`);
//! otherwise the kernel's gate after the report decides.
//!
//! The held wrapper's first step is the checkpoint (`checkpoint.rs`); its release waits for the
//! park (the forge-action order), so before the park no checkpoint and no step has run, and a
//! re-drive stops the same never-released execution. No run path reads the exit file: the worker is alive
//! and same-user (D9).

mod admission;
mod checkpoint;
pub(crate) mod finalize;

#[cfg(test)]
mod tests;

pub(crate) use admission::{Admitted, admit_run_tx};
pub(crate) use checkpoint::refs_digest;
pub use finalize::GateRunResult;

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::gate_execution::{self as execution, Decision, Release, StopReason};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use workspace_runtime::execution::*;

use super::gate_lifecycle::{GatePaths, GateSpec};
use super::gate_process::{GateEvidence, GateFiles};

use super::{
    CompensationStateVersioned, CompensationStep, Operation, ParkedRecovery, PhaseTag,
    ProviderAdapter, RecoveryMode, SpawnArtifacts, SpawnCtx, SpawnOutcome, TxOutput,
};
use crate::error::{CalmError, Result};
use crate::model::now_ms;

pub const TASK_GATE_RUN_KIND: &str = "task-gate-run";

/// Admitted runs (op rows) per attempt (#2464 O2).
pub const GATE_RUNS_PER_ATTEMPT: i64 = 5;

const TASK_GATE_RUN_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::SpawnStarted,
    PhaseTag::Parked,
    PhaseTag::Succeeded,
];

/// `recover_parked`'s reasons. A reason that names `gate-infra` is infra whatever class the driver
/// files it under (P11 before P13); see [`finalize::terminal_result`].
const RUN_KERNEL_RESTARTED: &str = "gate-infra: the kernel restarted during the run";

/// The key of run `run` of `task_id`; also its processes' `NEIGE_GATE_OP` marker.
pub fn gate_run_key(task_id: &str, run: i64) -> String {
    format!("{task_id}#r{run}")
}

/// Parse `"{task.id}#r{N}"`. Task keys are `[a-z0-9._-]`, so the LAST `#r` separates.
pub(crate) fn parse_run_key(key: &str) -> Option<(&str, i64)> {
    let (task_id, n) = key.rsplit_once("#r")?;
    let run = n.parse::<i64>().ok()?;
    (run >= 1 && !task_id.is_empty()).then_some((task_id, run))
}

/// How long one call of the run tool waits before it answers `running` (D3): half the worker idle
/// window, at most 90 s. That stays under Claude's 120 s Bash default, Codex's 300 s `tools/call`
/// timeout and the idle window however it is configured, so a polling worker is never reaped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateRunWait(Duration);

impl GateRunWait {
    const CEILING: Duration = Duration::from_secs(90);

    /// The bound for the default 3600 s idle window.
    pub const DEFAULT: Self = Self::for_idle(Duration::from_secs(3600));

    pub const fn for_idle(idle: Duration) -> Self {
        let half = Duration::from_millis((idle.as_millis() / 2) as u64);
        if half.as_millis() < Self::CEILING.as_millis() {
            Self(half)
        } else {
            Self(Self::CEILING)
        }
    }

    pub fn duration(self) -> Duration {
        self.0
    }

    /// `90 seconds`, as the prompt and the tool description render it.
    pub fn render(self) -> String {
        let secs = self.0.as_secs();
        if secs == 1 {
            "1 second".to_string()
        } else {
            format!("{secs} seconds")
        }
    }
}

/// A pure function of the admission (`admit_run_tx`): the run number is reserved by the key, the
/// message is resolved (the worker's or the kernel's) so the checkpoint never reads the call.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskGateRunPayload {
    pub track_id: String,
    pub task_id: String,
    pub card_id: String,
    pub run: i64,
    pub message: String,
}

/// What `prepare_tx` froze: the lease identity, its base and branch, the run's ref and the gate.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct FrozenRun {
    pub task_id: String,
    pub track_id: String,
    pub card_id: String,
    pub run: i64,
    pub cwd: String,
    pub branch: String,
    pub base_sha: String,
    pub canonical_path: String,
    pub git_common_dir: String,
    pub ref_name: String,
    pub message: String,
    pub gate: GateSpec,
}

impl FrozenRun {
    pub(crate) fn from_output(output: &TxOutput) -> Result<Self> {
        serde_json::from_value(output.data.clone()).map_err(|e| {
            CalmError::Internal(format!("task-gate-run tx_output.data unparseable: {e}"))
        })
    }

    fn key(&self) -> String {
        gate_run_key(&self.task_id, self.run)
    }
}

/// `refs/neige/gate-runs/<track>/<card>/r<N>`: one worker card serves one attempt, and an attempt
/// id (`<track>:<key>`) cannot appear in a ref name.
pub(crate) fn gate_run_ref_name(track_id: &str, card_id: &str, run: i64) -> String {
    format!("{}{card_id}/r{run}", gate_run_ref_prefix(track_id))
}

/// The prefix every run ref of one Track shares (`git_candidate::refs` deletes them with it).
pub(crate) fn gate_run_ref_prefix(track_id: &str) -> String {
    format!("refs/neige/gate-runs/{track_id}/")
}

#[cfg(any(test, feature = "fixtures"))]
type Hook = std::sync::Arc<dyn Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

#[cfg(any(test, feature = "fixtures"))]
pub(super) type FaultHook =
    std::sync::Arc<dyn Fn() -> futures::future::BoxFuture<'static, Result<()>> + Send + Sync>;

#[derive(Clone)]
pub struct TaskGateRunAdapter {
    refs: Option<String>,
    gate_logs_dir: PathBuf,
    #[cfg(any(test, feature = "fixtures"))]
    hooks: TestHooks,
}

/// Fixture-only pauses (R4c, R6, R8d).
#[cfg(any(test, feature = "fixtures"))]
#[derive(Clone, Default)]
struct TestHooks {
    before_spawn: Option<Hook>,
    before_release: Option<Hook>,
    before_completion: Option<Hook>,
    release_response: Option<FaultHook>,
    observer_read: Option<FaultHook>,
}

impl TaskGateRunAdapter {
    pub fn new(gate_logs_dir: PathBuf) -> Self {
        Self {
            gate_logs_dir,
            refs: None,
            #[cfg(any(test, feature = "fixtures"))]
            hooks: TestHooks::default(),
        }
    }

    /// Fixture-only delay at the start of `spawn_side_effect`, inside the operation drive.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn with_before_spawn(mut self, hook: Hook) -> Self {
        self.hooks.before_spawn = Some(hook);
        self
    }

    /// Fixture-only hook at the release boundary of the observer, just before the go token.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn with_before_release(mut self, hook: Hook) -> Self {
        self.hooks.before_release = Some(hook);
        self
    }

    /// Fixture-only pause in the observer after the verdict, before its completion commits.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn with_before_completion(mut self, hook: Hook) -> Self {
        self.hooks.before_completion = Some(hook);
        self
    }

    /// Fixture-only fault after real release I/O, before the observer receives its result.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn with_release_response(mut self, hook: FaultHook) -> Self {
        self.hooks.release_response = Some(hook);
        self
    }

    /// Fixture-only transient read fault or pause at the observer's repository boundary.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn with_observer_read(mut self, hook: FaultHook) -> Self {
        self.hooks.observer_read = Some(hook);
        self
    }

    fn paths(&self, task_id: &str, run: i64) -> GatePaths {
        GatePaths::new(&self.gate_logs_dir, &format!("{task_id}-r{run}"))
    }
}

/// The log of run `run` of `task_id`, for the tool's answer.
pub(crate) fn gate_run_log_path(gate_logs_dir: &Path, task_id: &str, run: i64) -> PathBuf {
    GatePaths::new(gate_logs_dir, &format!("{task_id}-r{run}")).log
}

/// The step a running run's wrapper last started, from its step file: informational only, a
/// same-user process could write it.
pub(crate) fn running_step(gate_logs_dir: &Path, frozen: &FrozenRun) -> Option<String> {
    let paths = GatePaths::new(
        gate_logs_dir,
        &format!("{}-r{}", frozen.task_id, frozen.run),
    );
    let evidence = GateEvidence {
        log_path: paths.log,
        step_path: Some(paths.step),
        steps: checkpoint::run_steps(frozen),
    };
    let number = evidence.started_step_number()?;
    Some(evidence.steps[number - 1].name.clone())
}

#[async_trait]
impl ProviderAdapter for TaskGateRunAdapter {
    fn kind(&self) -> &'static str {
        TASK_GATE_RUN_KIND
    }

    fn phases(&self) -> &'static [PhaseTag] {
        TASK_GATE_RUN_PHASES
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: TaskGateRunPayload = serde_json::from_value(input.clone())?;
        if payload.task_id.trim().is_empty() || payload.run < 1 {
            return Err(CalmError::BadRequest(
                "task-gate-run needs a task_id and a run >= 1".into(),
            ));
        }
        Ok(())
    }

    /// D10 again, in the prepare transaction: a row that moved since the admission fails the op
    /// (P14). Freezes the lease identity, its base and branch, and the gate.
    async fn prepare_tx<'tx>(
        &self,
        tx: &mut super::Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> Result<TxOutput> {
        let payload: TaskGateRunPayload = serde_json::from_value(input.clone())?;
        super::refuse_if_context_stale(tx, Some(&payload.task_id)).await?;
        if op.idempotency_key.as_deref() != Some(&gate_run_key(&payload.task_id, payload.run)) {
            return Err(CalmError::BadRequest(format!(
                "task-gate-run idempotency key must be \"{{task_id}}#r{{N}}\" of its payload, got {:?}",
                op.idempotency_key
            )));
        }
        let target =
            admission::run_target_tx(tx, &payload.task_id, &payload.card_id, &payload.track_id)
                .await?;
        let frozen = FrozenRun {
            ref_name: gate_run_ref_name(&payload.track_id, &payload.card_id, payload.run),
            task_id: payload.task_id,
            track_id: payload.track_id,
            card_id: payload.card_id,
            run: payload.run,
            cwd: target.cwd,
            branch: target.branch,
            base_sha: target.base_sha,
            canonical_path: target.canonical_path,
            git_common_dir: target.git_common_dir,
            message: payload.message,
            gate: target.gate,
        };
        let mut output = TxOutput::new("task", Some(frozen.task_id.clone()), json!({}));
        output.data = serde_json::to_value(&frozen)?;
        Ok(output)
    }

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<super::AppServerInteractOutcome> {
        Ok(super::AppServerInteractOutcome::NotApplicable)
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
        if let Some(hook) = &self.hooks.before_spawn {
            hook().await;
        }
        let frozen = FrozenRun::from_output(output)?;
        let redrive = output.data.get(execution::KEY).is_some();
        let refs = checkpoint::refs_digest(
            Path::new(&frozen.cwd),
            tokio::time::Instant::now() + super::task_verify_adapter::SAMPLE_TIMEOUT,
        )
        .await;
        if !redrive {
            if op.spawn_artifacts.is_some() {
                return Err(legacy_error(op));
            }
            execution::engine(ctx)?;
            super::admit_task_side_effect(ctx.repo.as_ref(), &frozen.task_id).await?;
            let paths = self.paths(&frozen.task_id, frozen.run);
            paths.unlink_stale(&self.gate_logs_dir).await?;
            execution::admit(
                op,
                ctx,
                &frozen.cwd,
                &frozen.track_id,
                GateFiles {
                    script: &paths.script,
                    log: &paths.log,
                    exit: &paths.exit,
                    step: &paths.step,
                },
                crate::operation::gate_process::WrapperSteps::Logged(&checkpoint::run_steps(
                    &frozen,
                )),
                &frozen.key(),
            )
            .await?;
        } else {
            execution::record(op)?;
        }
        #[cfg(feature = "fixtures")]
        crate::test_seams::crash_point("task-gate-run-pre-park");
        let mut adapter = self.clone();
        adapter.refs = refs;
        let (ctx, id) = (ctx.clone(), op.id.clone());
        Ok(SpawnOutcome::Parked {
            deadline_ms: frozen.gate.parked_deadline_ms(now_ms()),
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
        if op
            .tx_output
            .as_ref()
            .is_none_or(|o| o.data.get(execution::KEY).is_none())
        {
            return Err(legacy_error(op));
        }
        let record = execution::record(op)?;
        if matches!(mode, RecoveryMode::Boot) {
            // The boot caller returns without waiting for executor availability or drain.
            let (adapter, observer_ctx, id) = (self.clone(), ctx.clone(), op.id.clone());
            tokio::spawn(async move {
                adapter.observe(observer_ctx, id, true).await;
            });
            if record.decision == Decision::Observing {
                execution::stop(ctx, op, StopReason::KernelRestarted).await?;
                return Ok(ParkedRecovery::LeaveParked);
            }
        }
        let settled =
            crate::operation::owned_execution::settlement(ctx, &record.execution_id).await?;
        if settled.is_none() {
            let frozen = FrozenRun::from_output(super::required_output(op)?)?;
            let timed_out = op.parked_deadline_ms.is_some_and(|d| now_ms() > d)
                || matches!(record.release,Release::Releasing { authorized_at_ms } | Release::Released { authorized_at_ms } if now_ms() >= authorized_at_ms+frozen.gate.timeout_secs_clamped()*1000);
            if timed_out && record.decision == Decision::Observing {
                execution::stop(ctx, op, StopReason::Timeout).await?;
            } else {
                crate::operation::owned_execution::drain_exited(ctx, op, &record.execution_id)
                    .await?;
            }
            return Ok(ParkedRecovery::LeaveParked);
        }
        // Every completion-capable claimant observes the durable release outcome.
        // A settled child cannot prove that an in-flight release was acknowledged.
        if record.decision == Decision::Observing
            && !matches!(record.release, Release::Released { .. })
        {
            execution::stop(ctx, op, StopReason::ReleaseRefused).await?;
            return Ok(ParkedRecovery::LeaveParked);
        }
        let frozen = FrozenRun::from_output(super::required_output(op)?)?;
        let paths = self.paths(&frozen.task_id, frozen.run);
        let evidence = GateEvidence {
            log_path: paths.log,
            step_path: Some(paths.step),
            steps: checkpoint::run_steps(&frozen),
        };
        let status = match settled.unwrap() {
            Settlement::Journal(
                ExecutionState::Drained { status, .. }
                | ExecutionState::OriginalGroupEmpty { status, .. },
            ) => status,
            _ => None,
        };
        let reason = match &record.decision {
            Decision::Stopping { reason } => Some(reason.clone()),
            _ => None,
        };
        let result =
            finalize::finalize_run(&frozen, &evidence, status, reason, self.refs.clone()).await;
        #[cfg(any(test, feature = "fixtures"))]
        if let Some(hook) = &self.hooks.before_completion {
            hook().await;
        }
        Ok(ParkedRecovery::Complete(Box::new((
            super::ParkedOutcome::Succeeded {
                result: serde_json::to_value(result)?,
            },
            None,
        ))))
    }

    async fn complete_owned_parked_tx(
        &self,
        tx: &mut super::Tx<'_>,
        op: &Operation,
    ) -> Result<Vec<crate::event::BroadcastEnvelope>> {
        execution::retire(tx, op).await?;
        Ok(Vec::new())
    }

    fn owned_parked_completed(&self, op: &Operation, ctx: &SpawnCtx) {
        if let Ok(record) = execution::record(op) {
            ctx.execution.drive([record.execution_id]);
        }
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.to_string(),
            steps: vec![CompensationStep::new(
                "stop_run_group",
                json!({
                    "artifacts": op.spawn_artifacts,
                    "marker": FrozenRun::from_output(output)?.key(),
                }),
            )],
        })
    }

    /// Kill the run's recorded group and prove it stopped; a group that cannot be proven stopped
    /// leaves the op `stuck`.
    async fn compensate_step(
        &self,
        step: &CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<()> {
        match step.op.as_str() {
            "stop_run_group" => {
                let Some(artifacts) = step.args.get("artifacts").filter(|v| !v.is_null()) else {
                    return Ok(());
                };
                let artifacts: SpawnArtifacts = serde_json::from_value(artifacts.clone())?;
                Err(CalmError::Conflict(format!(
                    "drain under the old kernel or manually stop pgid {}",
                    artifacts.pgid
                )))
            }
            other => Err(CalmError::Internal(format!(
                "task-gate-run unknown compensation step {other}"
            ))),
        }
    }
}

fn legacy_error(op: &Operation) -> CalmError {
    CalmError::Conflict(format!(
        "pre-C2b gate unresolved; drain under the old kernel or manually stop pgid {}",
        op.spawn_artifacts.as_ref().map_or(0, |a| a.pgid)
    ))
}

impl TaskGateRunAdapter {
    async fn observe(self, ctx: SpawnCtx, id: String, mut restart: bool) {
        #[cfg(feature = "fixtures")]
        crate::test_seams::crash_point("task-gate-run-post-park");
        #[cfg(any(test, feature = "fixtures"))]
        if let Some(hook) = &self.hooks.before_release {
            hook().await;
        }
        loop {
            let read = async {
                #[cfg(any(test, feature = "fixtures"))]
                if let Some(hook) = &self.hooks.observer_read {
                    hook().await?;
                }
                ctx.operation_repo.get_operation(&id).await
            }
            .await;
            let current = match read {
                Ok(Some(current)) => current,
                Ok(None) => return,
                Err(error) => {
                    tracing::warn!(op_id=%id,%error,"gate observer read failed; retrying");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };
            if matches!(
                current.phase.tag(),
                PhaseTag::Succeeded | PhaseTag::Failed | PhaseTag::Stuck
            ) {
                if let Ok(record) = execution::record(&current) {
                    ctx.execution.drive([record.execution_id]);
                }
                return;
            }
            if let Ok(Some(op)) = ctx.operation_repo.claim_parked(&id).await {
                let result = async {
                    let action: Result<()> = async {
                        let record = execution::record(&op)?;
                        if record.decision == Decision::Observing {
                            if restart {
                                execution::stop(&ctx, &op, StopReason::KernelRestarted).await?;
                                restart = false;
                            } else if record.release == Release::Held {
                                let frozen = FrozenRun::from_output(super::required_output(&op)?)?;
                                if op.parked_deadline_ms.is_some_and(|d| now_ms() > d) {
                                    execution::stop(&ctx, &op, StopReason::Timeout).await?;
                                } else {
                                    let released = execution::release(
                                        &ctx,
                                        &op,
                                        execution::ReleaseAuthority::Task(&frozen.task_id),
                                        #[cfg(any(test, feature = "fixtures"))]
                                        self.hooks.release_response.as_ref(),
                                    )
                                    .await;
                                    if released.is_err() {
                                        // An uncertain release remains durable as Releasing.
                                        let latest = execution::refresh_claim(&ctx, &op).await?;
                                        execution::stop(&ctx, &latest, StopReason::ReleaseRefused)
                                            .await?;
                                    }
                                }
                            }
                        }
                        Ok(())
                    }
                    .await;
                    // A failed decision write must never fall through to completion.
                    action?;
                    let latest = execution::refresh_claim(&ctx, &op).await?;
                    super::owned_parked::reconcile(
                        &self,
                        &latest,
                        RecoveryMode::PreDeadlineProbe,
                        &ctx,
                    )
                    .await
                }
                .await;
                if let Err(error) = result {
                    tracing::warn!(op_id=%id,%error,"gate execution retained");
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

// Base commit 3fc2edb3e's admission.rs persisted this kernel-owned text when the worker
// omitted commit_message. Old payloads have no provenance field: only this exact text for
// the exact attempt and run is excluded from explicit inputs after an upgrade.
fn legacy_generated_message(attempt_id: &str, run: i64) -> String {
    format!("neige: attempt {attempt_id} gate run {run}")
}

/// Latest explicit admitted input, independent of checkpoint progress and Git history. Run numbers
/// follow admission order under the caller's write lock; the typed lookup fences attempt keys.
pub(crate) async fn latest_input_message_tx(
    tx: &mut super::Tx<'_>,
    attempt_id: &str,
) -> Result<Option<crate::git_candidate::commit_message::CommitMessage>> {
    use super::gate_ops::{GateOpKind, gate_ops_of_attempt_tx};
    let ops = gate_ops_of_attempt_tx(tx, attempt_id).await?;
    for op in ops.iter().rev().filter(|op| op.kind == GateOpKind::Run) {
        let message = admitted_message_tx(tx, &op.key).await?;
        if message.as_str() != legacy_generated_message(attempt_id, op.number) {
            return Ok(Some(message));
        }
    }
    Ok(None)
}

/// Joining compares against the admitted run itself, even before it has checkpointed.
pub(super) async fn admitted_message_tx(
    tx: &mut super::Tx<'_>,
    key: &str,
) -> Result<crate::git_candidate::commit_message::CommitMessage> {
    let payload: String = sqlx::query_scalar(
        "SELECT payload_json FROM operations WHERE kind = ?1 AND idempotency_key = ?2",
    )
    .bind(TASK_GATE_RUN_KIND)
    .bind(key)
    .fetch_one(&mut **tx)
    .await?;
    let payload: TaskGateRunPayload = serde_json::from_str(&payload)?;
    crate::git_candidate::commit_message::CommitMessage::parse(&payload.message)
        .map_err(CalmError::Internal)
}
