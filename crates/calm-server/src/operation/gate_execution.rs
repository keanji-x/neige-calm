//! Gate-owned execution construction, durable authorization and lifecycle decisions.
use super::{Operation, SpawnCtx, Tx, gate_process::GateFiles};
use crate::{
    error::{CalmError, Result},
    model::now_ms,
};
use calm_truth::db::sqlite::gate_execution as sql;
use serde::{Deserialize, Serialize};
use workspace_runtime::execution::*;

pub(crate) const KEY: &str = "gate_execution";
pub(crate) const CONSUMER: &str = "gate";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Release {
    Held,
    Released { authorized_at_ms: i64 },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum StopReason {
    Timeout,
    KernelRestarted,
    ReleaseRefused,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Decision {
    Observing,
    Stopping { reason: StopReason },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Record {
    version: u8,
    pub execution_id: ExecutionId,
    pub release: Release,
    pub decision: Decision,
}
fn conflict(message: impl std::fmt::Debug) -> CalmError {
    CalmError::Conflict(format!("{message:?}"))
}
pub(crate) fn record(op: &Operation) -> Result<Record> {
    let value = op
        .tx_output
        .as_ref()
        .and_then(|o| o.data.get(KEY))
        .ok_or_else(|| conflict("gate execution record missing"))?;
    let record: Record = serde_json::from_value(value.clone())?;
    if record.version != 1 {
        return Err(conflict("unsupported gate execution record"));
    }
    Ok(record)
}
pub(crate) fn engine(ctx: &SpawnCtx) -> Result<&crate::execution_boot::ExecutionEngine> {
    ctx.execution
        .engine()
        .filter(|e| e.available())
        .ok_or(CalmError::ExecutionUnavailable)
}

/// The only gate intent constructor. Files exist before the admission transaction.
pub(crate) async fn admit(
    op: &Operation,
    ctx: &SpawnCtx,
    cwd: &str,
    track: &str,
    files: GateFiles<'_>,
    steps: &[super::gate_lifecycle::GateStep],
    marker: &str,
) -> Result<Record> {
    let engine = engine(ctx)?;
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
    tokio::fs::write(
        files.script,
        super::gate_process::render_gate_wrapper(steps),
    )
    .await?;
    tokio::fs::write(files.log, b"").await?;
    let id = ExecutionId::fresh()?;
    let intent = ExecutionIntent {
        metadata: ExecutionMetadata {
            consumer: CONSUMER.into(),
            id: id.clone(),
            alias: ProcessAlias(format!("gate:{}", op.id)),
            owner: OperationRef(op.id.clone()),
            track: TrackId(track.into()),
            source: PrivateSource {
                root: cwd.into(),
                cwd: workspace::identity::WorkspaceCwd::root(),
                registration: ObjectRegistrationId::fresh()?,
                nonce: TransferNonce::fresh()?,
                directory: ExternalDirectoryId::fresh()?,
                incarnation: ExternalDirectoryIncarnationId::fresh()?,
                authority: ExecutionAuthority {
                    grant_id: ExecutionGrantId::fresh()?,
                    authorization_binding: AuthorizationBindingId::fresh()?,
                    authorization_version: 1,
                },
            },
            program: ExecutionSpecification {
                program: "/bin/sh".into(),
                args: vec![files.script.display().to_string()],
                environment,
                io_mode: calm_session::control::IoMode::Pipe,
                replay_bytes: 8192,
            },
            specification: SpecificationId::fresh()?,
            ticket_id: LaunchTicketId::fresh()?,
        },
        secrets: ExecutionSecrets {
            ticket: CapabilityToken::fresh()?,
            tokens: ExecutionControlTokens::fresh()?,
        },
    };
    let record = Record {
        version: 1,
        execution_id: id,
        release: Release::Held,
        decision: Decision::Observing,
    };
    let pool = ctx.operation_repo.sqlite_db();
    let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
    super::repo_sqlite::require_live_claim_tx(&mut tx, op).await?;
    let ticket = engine.admit(&mut tx, intent).await.map_err(conflict)?;
    sql::insert(&mut tx, &op.id, &serde_json::to_string(&record)?).await?;
    tx.commit().await?;
    engine.drive(ticket.committed());
    Ok(record)
}

/// All record transitions are fenced by the parked claim and compare the entire v1 record.
pub(crate) async fn transition(
    ctx: &SpawnCtx,
    op: &Operation,
    old: &Record,
    new: &Record,
    task: Option<&str>,
) -> Result<()> {
    let pool = ctx.operation_repo.sqlite_db();
    let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
    super::repo_sqlite::require_live_claim_tx(&mut tx, op).await?;
    if let Some(task) = task {
        super::task_launch::TaskLaunch::authorize_tx(&mut tx, task).await?;
    }
    sql::compare_exchange(
        &mut tx,
        &op.id,
        &serde_json::to_string(old)?,
        &serde_json::to_string(new)?,
    )
    .await?;
    if matches!(new.decision, Decision::Stopping { .. }) {
        calm_truth::db::sqlite::workspace_execution::request(
            &mut tx,
            new.execution_id.as_str(),
            "stop",
        )
        .await?;
    }
    tx.commit().await?;
    ctx.execution.drive([new.execution_id.clone()]);
    Ok(())
}

pub(crate) async fn stop(ctx: &SpawnCtx, op: &Operation, reason: StopReason) -> Result<()> {
    let old = record(op)?;
    if matches!(old.decision, Decision::Stopping { .. }) {
        return Ok(());
    }
    let mut new = old.clone();
    new.decision = Decision::Stopping { reason };
    transition(ctx, op, &old, &new, None).await
}

/// Called only by the post-park observer under a parked claim.
pub(crate) async fn release(
    ctx: &SpawnCtx,
    op: &Operation,
    task: &str,
    #[cfg(any(test, feature = "fixtures"))] response: Option<&super::task_gate_run::FaultHook>,
) -> Result<()> {
    let old = record(op)?;
    if old.release != Release::Held || old.decision != Decision::Observing {
        return Ok(());
    }
    let mut new = old.clone();
    new.release = Release::Released {
        authorized_at_ms: now_ms(),
    };
    transition(ctx, op, &old, &new, Some(task)).await?;
    let engine = engine(ctx)?;
    let send = async {
        loop {
            if let Ok(ExecutionReply::Observation {
                receipt: Some(_),
                state: ExecutionState::Spawned,
                ..
            }) = engine.output(&old.execution_id, None).await
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let token = b"go\n";
        let mut offset = 0;
        while offset < token.len() {
            match engine
                .interact(
                    &old.execution_id,
                    ProcessAction::WriteStdin {
                        bytes: token[offset..].to_vec(),
                    },
                )
                .await
                .map_err(conflict)?
            {
                ExecutionReply::StdinProgress { accepted_bytes }
                    if accepted_bytes <= token.len() - offset =>
                {
                    offset += accepted_bytes
                }
                _ => return Err(conflict("gate release returned invalid progress")),
            }
            if offset < token.len() {
                tokio::task::yield_now().await;
            }
        }
        engine
            .close_stdin(&old.execution_id)
            .await
            .map_err(conflict)?;
        // Fault the initial response only after the real token and close reached the executor.
        #[cfg(any(test, feature = "fixtures"))]
        if let Some(response) = response {
            response().await?;
        }
        Ok(())
    };
    tokio::time::timeout(super::gate_lifecycle::RELEASE_TIMEOUT, send)
        .await
        .map_err(conflict)?
}

pub(crate) async fn settlement(ctx: &SpawnCtx, record: &Record) -> Result<Option<Settlement>> {
    let pool = ctx.operation_repo.sqlite_db();
    let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
    let value = sql::settlement(&mut tx, record.execution_id.as_str()).await?;
    tx.commit().await?;
    value
        .map(|s| serde_json::from_str(&s).map_err(Into::into))
        .transpose()
}

/// Completion rechecks the decision in the same transaction as phase write and Retire.
pub(crate) async fn retire(tx: &mut Tx<'_>, op: &Operation) -> Result<()> {
    let snapshot = record(op)?;
    sql::require_record(tx, &op.id, &serde_json::to_string(&snapshot)?).await?;
    calm_truth::db::sqlite::workspace_execution::request(
        tx,
        snapshot.execution_id.as_str(),
        "retire",
    )
    .await?;
    Ok(())
}

pub struct Projection;
#[async_trait::async_trait]
impl crate::execution_store::ExecutionProjection for Projection {
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
