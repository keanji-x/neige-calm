//! Gate-owned execution construction, durable authorization and lifecycle decisions.
use super::owned_execution::pipe;
use super::{Operation, SpawnCtx, Tx, gate_process::GateFiles};
use crate::error::{CalmError, Result};
use workspace_runtime::execution::*;

pub(crate) const KEY: &str = "gate_execution";
pub(crate) const CONSUMER: &str = "gate";
pub(crate) use super::owned_execution::{Decision, Release, StopReason, engine, refresh_claim};
fn conflict(message: impl std::fmt::Debug) -> CalmError {
    CalmError::Conflict(format!("{message:?}"))
}
pub(crate) const CONFIG: pipe::PipeConfig = pipe::PipeConfig {
    consumer: CONSUMER,
    alias_prefix: "gate",
    record_key: KEY,
};
pub(crate) use pipe::Record;
pub(crate) fn record(op: &Operation) -> Result<Record> {
    pipe::record(CONFIG, op)
}
pub(crate) async fn stop(ctx: &SpawnCtx, op: &Operation, reason: StopReason) -> Result<()> {
    pipe::stop(CONFIG, ctx, op, reason).await
}
pub(crate) async fn release(
    ctx: &SpawnCtx,
    op: &Operation,
    authority: ReleaseAuthority<'_>,
    #[cfg(any(test, feature = "fixtures"))] response: Option<&super::task_gate_run::FaultHook>,
) -> Result<()> {
    pipe::release(
        CONFIG,
        ctx,
        op,
        &authority,
        pipe::ReleaseRequest {
            token: b"go\n",
            timeout: super::gate_lifecycle::RELEASE_TIMEOUT,
        },
        #[cfg(any(test, feature = "fixtures"))]
        response,
    )
    .await
}
pub(crate) async fn retire(tx: &mut Tx<'_>, op: &Operation) -> Result<()> {
    pipe::retire(CONFIG, tx, op).await
}
/// The only gate intent constructor. Files exist before the admission transaction.
pub(crate) async fn admit(
    op: &Operation,
    ctx: &SpawnCtx,
    cwd: &str,
    track: &str,
    files: GateFiles<'_>,
    steps: super::gate_process::WrapperSteps<'_>,
    marker: &str,
) -> Result<Record> {
    engine(ctx)?;
    // Gate cwd permits a symlink (the target sampler verifies its provenance).
    // Private execution roots require a directory opened with O_NOFOLLOW.
    let root = tokio::fs::canonicalize(cwd).await?;
    let mut environment: Vec<(String, String)> = ["PATH", "HOME", "LANG", "LC_ALL", "TERM"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok().map(|v| (key.into(), v)))
        .collect();
    if let serde_json::Value::Object(env) =
        super::terminal_adapter::terminal_worker_env(ctx.repo.as_ref()).await?
    {
        environment.extend(
            env.into_iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k, v.into()))),
        );
    }
    for (key, path) in [
        ("NEIGE_GATE_EXIT_PATH", files.exit),
        ("NEIGE_GATE_STEP_PATH", files.step),
        ("NEIGE_GATE_LOG_PATH", files.log),
    ] {
        environment.push((key.into(), path.display().to_string()));
    }
    environment.push(("NEIGE_GATE_OP".into(), marker.into()));
    tokio::fs::write(files.script, steps.render()).await?;
    tokio::fs::write(files.log, b"").await?;
    pipe::admit(
        CONFIG,
        op,
        ctx,
        root,
        track,
        ExecutionSpecification {
            ambient_environment: calm_session::control::execution::AmbientEnvironment::None,
            program: "/bin/sh".into(),
            args: vec![files.script.display().to_string()],
            environment,
            io_mode: calm_session::control::IoMode::Pipe,
            replay_bytes: 8192,
        },
    )
    .await
}
pub(crate) enum ReleaseAuthority<'a> {
    Task(&'a str),
    ObjectiveIntent(&'a calm_truth::objective_host::IntentId),
}

#[async_trait::async_trait]
impl pipe::ReleaseAuthorization for ReleaseAuthority<'_> {
    async fn authorize_tx(&self, tx: &mut Tx<'_>) -> Result<()> {
        match self {
            Self::Task(task) => super::task_launch::TaskLaunch::authorize_tx(tx, task).await,
            Self::ObjectiveIntent(id) => {
                let Some((row, _)) = calm_truth::objective_host::current_check_tx(tx, id).await?
                else {
                    return Err(conflict("objective check authorization expired"));
                };
                if !matches!(
                    row.status,
                    calm_truth::objective_host::IntentStatus::Dispatched { .. }
                ) {
                    return Err(conflict("objective intent is not dispatched"));
                }
                Ok(())
            }
        }
    }
}

pub struct Projection;
#[async_trait::async_trait]
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
