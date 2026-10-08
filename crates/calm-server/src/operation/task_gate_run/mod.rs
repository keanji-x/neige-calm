//! `task-gate-run` operation (#2464): the worker asks, the kernel commits the worker's checkout as
//! the attempt's one commit above the lease base and runs the task's declared gate on it, in the
//! lease checkout, outside the sandbox. One operation per run (`"{task.id}#r{N}"`); the result is
//! advisory, the kernel's gate after the report stays the verdict.
//!
//! The held wrapper's first step is the checkpoint (`checkpoint.rs`); its release waits for the
//! park (the forge-action order), so before the park no checkpoint and no step has run, and a
//! re-drive replaces a never-released wrapper. No run path reads the exit file: the worker is alive
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

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

use super::gate_lifecycle::{GatePaths, GateSpec, RELEASE_TIMEOUT, spawn_held_identified};
use super::gate_process::{GateEvidence, observe_verdict};
use super::task_verify_adapter::target::stop_group;
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
const RUN_ENDED_UNOBSERVED: &str = "gate-infra: the run ended without a kernel-observed verdict";
const RUN_KERNEL_RESTARTED: &str = "gate-infra: the kernel restarted during the run";
const RUN_PARKED_DEADLINE: &str = "gate timeout (parked deadline exceeded)";

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

pub struct TaskGateRunAdapter {
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
}

impl TaskGateRunAdapter {
    pub fn new(gate_logs_dir: PathBuf) -> Self {
        Self {
            gate_logs_dir,
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

    /// Stop a recorded group (a re-drive), take the refs digest, spawn the held wrapper and record
    /// it. Nothing is released here: the observer releases after the park commits.
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
        let marker = frozen.key();
        // A re-drive: the earlier wrapper was never released; its group must be proven stopped
        // before a second one exists, or the op does not go on.
        if let Some(artifacts) = &op.spawn_artifacts {
            stop_group(artifacts, &marker).await?;
        }
        super::admit_task_side_effect(ctx.repo.as_ref(), &frozen.task_id).await?;
        let refs = checkpoint::refs_digest(
            Path::new(&frozen.cwd),
            tokio::time::Instant::now() + super::task_verify_adapter::SAMPLE_TIMEOUT,
        )
        .await;
        let paths = self.paths(&frozen.task_id, frozen.run);
        paths.unlink_stale(&self.gate_logs_dir).await?;
        let steps = checkpoint::run_steps(&frozen);
        let (child, artifacts) = spawn_held_identified(
            ctx.repo.as_ref(),
            Path::new(&frozen.cwd),
            &steps,
            &paths,
            &marker,
        )
        .await?;
        if let Err(error) = ctx.record_spawn_artifacts(op, &artifacts).await {
            super::gate_process::kill(&artifacts);
            return Err(error);
        }
        #[cfg(feature = "fixtures")]
        crate::test_seams::crash_point("task-gate-run-pre-park");

        let deadline_ms = frozen.gate.parked_deadline_ms(now_ms());
        let observer = Observer {
            child,
            artifacts,
            evidence: GateEvidence {
                log_path: paths.log.clone(),
                step_path: Some(paths.step.clone()),
                steps,
            },
            frozen,
            refs,
            op: op.clone(),
            ctx: ctx.clone(),
            #[cfg(any(test, feature = "fixtures"))]
            hooks: self.hooks.clone(),
        };
        Ok(SpawnOutcome::Parked {
            deadline_ms,
            observer: Box::pin(observer.run()),
        })
    }

    /// D9: never reads the exit file. A dead leader is infra (P11); an overdue live one is a
    /// timeout (P13); a live one found at boot is stopped and infra (P12).
    async fn recover_parked(
        &self,
        op: &Operation,
        artifacts: &SpawnArtifacts,
        alive: bool,
        mode: RecoveryMode,
        _ctx: &SpawnCtx,
    ) -> Result<ParkedRecovery> {
        if !alive {
            return Ok(ParkedRecovery::Fail {
                reason: RUN_ENDED_UNOBSERVED.into(),
            });
        }
        Ok(match mode {
            RecoveryMode::PreDeadlineProbe => ParkedRecovery::LeaveParked,
            RecoveryMode::PastDeadline => ParkedRecovery::Fail {
                reason: RUN_PARKED_DEADLINE.into(),
            },
            RecoveryMode::Boot => {
                let frozen = op
                    .tx_output
                    .as_ref()
                    .ok_or_else(|| CalmError::Internal("task-gate-run op missing tx_output".into()))
                    .and_then(FrozenRun::from_output)?;
                if let Err(error) = stop_group(artifacts, &frozen.key()).await {
                    tracing::warn!(op_id = %op.id, %error, "gate run: the restarted kernel could not prove the run's group stopped");
                }
                ParkedRecovery::Fail {
                    reason: RUN_KERNEL_RESTARTED.into(),
                }
            }
        })
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
                let marker = step.arg_string("marker", "task-gate-run")?;
                stop_group(&artifacts, &marker).await
            }
            other => Err(CalmError::Internal(format!(
                "task-gate-run unknown compensation step {other}"
            ))),
        }
    }
}

/// The post-park half of one run: release, observe, stop, finalize, complete, reap.
struct Observer {
    child: tokio::process::Child,
    artifacts: SpawnArtifacts,
    evidence: GateEvidence,
    frozen: FrozenRun,
    refs: Option<String>,
    op: Operation,
    ctx: SpawnCtx,
    #[cfg(any(test, feature = "fixtures"))]
    hooks: TestHooks,
}

impl Observer {
    async fn run(mut self) {
        #[cfg(feature = "fixtures")]
        crate::test_seams::crash_point("task-gate-run-post-park");
        #[cfg(any(test, feature = "fixtures"))]
        if let Some(hook) = &self.hooks.before_release {
            hook().await;
        }
        // Released through `TaskLaunch`: an attempt no longer current, or terminal, is never
        // released; dropping the stdin makes the held wrapper exit 75 having run nothing (P4).
        if let Err(error) = self.release().await {
            crate::operation::task_verify_adapter::target::append_log_line(
                &self.evidence.log_path,
                &format!("gate-infra: the run was not released: {error}"),
            )
            .await;
        }
        let observation = observe_verdict(
            self.child,
            self.artifacts.clone(),
            self.evidence,
            self.frozen.run,
            self.frozen.gate.timeout_secs_clamped(),
        )
        .await;
        #[cfg(any(test, feature = "fixtures"))]
        if let Some(hook) = &self.hooks.before_completion {
            hook().await;
        }
        // The leader stays an unreaped zombie until the completion below commits, so a concurrent
        // parked probe never reads this run as dead without a verdict (H1).
        let stopped = stop_group(&self.artifacts, &self.frozen.key()).await;
        let result = finalize::finalize_run(&self.frozen, &observation, stopped, self.refs).await;
        if let Err(error) = finalize::complete_run_op(&self.ctx, &self.op.id, &result).await {
            tracing::error!(op_id = %self.op.id, %error, "gate run observer: completion tx failed; recovery will fail the run");
        }
        observation.reap().await;
    }

    async fn release(&mut self) -> Result<()> {
        let mut stdin =
            self.child.stdin.take().ok_or_else(|| {
                CalmError::Internal("gate run wrapper stdin handle missing".into())
            })?;
        let launch = super::task_launch::TaskLaunch::new(&self.frozen.task_id, &self.op);
        let release = launch.run(self.ctx.repo.as_ref(), async move {
            // Newline-terminated: POSIX `read` returns non-zero on EOF-before-newline.
            stdin.write_all(b"go\n").await.map_err(|error| {
                CalmError::Internal(format!("gate run release write failed: {error}"))
            })?;
            drop(stdin);
            Ok(())
        });
        match tokio::time::timeout(RELEASE_TIMEOUT, release).await {
            Ok(result) => result,
            Err(_) => {
                super::gate_process::kill(&self.artifacts);
                Err(CalmError::Internal(
                    "the release did not complete within 60s".into(),
                ))
            }
        }
    }
}
