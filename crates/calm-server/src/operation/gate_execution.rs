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
pub(crate) use super::owned_execution::{Decision, Release, StopReason, engine, refresh_claim};
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
    let engine = engine(ctx)?;
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
    let id = ExecutionId::fresh()?;
    let intent = ExecutionIntent {
        metadata: ExecutionMetadata {
            consumer: CONSUMER.into(),
            id: id.clone(),
            alias: ProcessAlias(format!("gate:{}", op.id)),
            owner: OperationRef(op.id.clone()),
            track: TrackId(track.into()),
            source: PrivateSource {
                root,
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
    sql::insert(&mut tx, &op.id, KEY, &serde_json::to_string(&record)?).await?;
    tx.commit().await?;
    engine.drive(ticket.committed());
    Ok(record)
}

pub(crate) enum ReleaseAuthority<'a> {
    Task(&'a str),
    ObjectiveIntent(&'a calm_truth::objective_host::IntentId),
}

impl ReleaseAuthority<'_> {
    async fn authorize_tx(self, tx: &mut Tx<'_>) -> Result<()> {
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

/// All record transitions are fenced by the parked claim and compare the entire v1 record.
pub(crate) async fn transition(
    ctx: &SpawnCtx,
    op: &Operation,
    old: &Record,
    new: &Record,
    authority: Option<ReleaseAuthority<'_>>,
) -> Result<()> {
    let pool = ctx.operation_repo.sqlite_db();
    let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
    super::repo_sqlite::require_live_claim_tx(&mut tx, op).await?;
    if let Some(authority) = authority {
        authority.authorize_tx(&mut tx).await?;
    }
    sql::compare_exchange(
        &mut tx,
        &op.id,
        KEY,
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
    authority: ReleaseAuthority<'_>,
    #[cfg(any(test, feature = "fixtures"))] response: Option<&super::task_gate_run::FaultHook>,
) -> Result<()> {
    let old = record(op)?;
    if old.release != Release::Held || old.decision != Decision::Observing {
        return Ok(());
    }
    let mut new = old.clone();
    new.release = Release::Releasing {
        authorized_at_ms: now_ms(),
    };
    transition(ctx, op, &old, &new, Some(authority)).await?;
    let send = async {
        super::owned_execution::send_release(ctx, &old.execution_id, b"go\n").await?;
        // Fault only after the real token and close reached the executor.
        #[cfg(any(test, feature = "fixtures"))]
        if let Some(response) = response {
            response().await?;
        }
        Ok::<(), CalmError>(())
    };
    tokio::time::timeout(super::gate_lifecycle::RELEASE_TIMEOUT, send)
        .await
        .map_err(conflict)??;
    // Only a CloseStdin acknowledgement makes settlement status eligible for a verdict.
    // Persist it under the original claim; an expired/replaced claimant cannot ack.
    let mut acked = new.clone();
    acked.release = Release::Released {
        authorized_at_ms: match new.release {
            Release::Releasing { authorized_at_ms } => authorized_at_ms,
            _ => unreachable!(),
        },
    };
    transition(ctx, op, &new, &acked, None).await
}

/// Completion rechecks the decision in the same transaction as phase write and Retire.
pub(crate) async fn retire(tx: &mut Tx<'_>, op: &Operation) -> Result<()> {
    let snapshot = record(op)?;
    sql::require_record(tx, &op.id, KEY, &serde_json::to_string(&snapshot)?).await?;
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
