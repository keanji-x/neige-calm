//! Workspace-owned reclaim invocation and receipt recovery on the existing parked lifecycle.
use super::*;
use crate::operation::{
    AppServerInteractOutcome, CompensationStateVersioned, CompensationStep, Operation,
    ParkedOutcome, ParkedRecovery, PhaseTag, ProviderAdapter, RecoveryMode, SpawnCtx, SpawnOutcome,
    Tx, TxOutput, owned_execution, owned_parked,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use workspace_runtime::{execution::*, track_worktree::ObservePoint};

pub(crate) const KIND: &str = "workspace-reclaim";
pub(crate) const KEY: &str = "workspace_reclaim";
pub(crate) const CONSUMER: &str = "workspace-reclaim";

#[derive(Serialize, Deserialize)]
struct Frozen {
    target: ExtraWorktree,
    delete_authorized: bool,
}
#[derive(Serialize, Deserialize)]
struct Record {
    // Explicit null means admission has not happened, never evidence of an effect.
    #[serde(deserialize_with = "required_execution")]
    execution_id: Option<ExecutionId>,
}
fn required_execution<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<ExecutionId>, D::Error> {
    Option::deserialize(d)
}
fn record(op: &Operation) -> Result<Record> {
    Ok(serde_json::from_value(
        op.tx_output
            .as_ref()
            .ok_or_else(|| conflict("reclaim output missing"))?
            .data[KEY]
            .clone(),
    )?)
}
fn frozen(op: &Operation) -> Result<Frozen> {
    Ok(serde_json::from_value(op.payload.clone())?)
}
fn conflict(e: impl std::fmt::Debug) -> CalmError {
    CalmError::Conflict(format!("{e:?}"))
}

/// Freeze completion authority before the deleting transaction loses its live owner rows.
/// A close and a deletion have distinct authorization; neither can rewrite an admitted effect.
pub(super) async fn register(
    tx: &mut Tx<'_>,
    target: &ExtraWorktree,
    delete_authorized: bool,
) -> Result<()> {
    let frozen = Frozen {
        target: target.clone(),
        delete_authorized,
    };
    let payload = serde_json::to_value(frozen)?;
    let mut output = TxOutput::new("track", Some(target.track_id.clone()), Value::Null);
    output.data = json!({KEY: Record { execution_id: None }});
    // No reclaim timeout may turn an unknown filesystem effect into completion. The existing
    // parked lifecycle requires a deadline; its open endpoint keeps this resource owned.
    sqlx::query("INSERT INTO operations (id,operation_key,kind,idempotency_key,payload_hash,target_type,target_id,target_json,payload_json,phase,tx_output_json,parked_at_ms,parked_deadline_ms,created_at_ms,updated_at_ms) VALUES (?,?,?, ?,?,'track',?,'{}',?,'parked',?,?,?, ?,?) ON CONFLICT(id) DO NOTHING")
        .bind(&target.operation_id).bind(&target.operation_id).bind(KIND).bind(&target.operation_id)
        .bind(crate::routes::idempotency_key::stable_payload_hash(&payload)?)
        .bind(&target.track_id).bind(serde_json::to_string(&payload)?)
        .bind(serde_json::to_string(&output)?).bind(crate::model::now_ms()).bind(i64::MAX)
        .bind(crate::model::now_ms()).bind(crate::model::now_ms()).execute(&mut **tx).await?;
    Ok(())
}

/// Initial invocation already owns close/delete's track fence. Recovery acquires its own.
pub(super) async fn run(
    ctx: &SpawnCtx,
    target: &ExtraWorktree,
    observe: ObservePoint,
) -> Result<()> {
    if let Some(op) = ctx
        .operation_repo
        .claim_parked(&target.operation_id)
        .await?
    {
        owned_parked::reconcile(
            &Adapter {
                fenced: true,
                observe,
            },
            &op,
            RecoveryMode::PreDeadlineProbe,
            ctx,
        )
        .await?;
    }
    Ok(())
}

pub(crate) struct Adapter {
    fenced: bool,
    observe: ObservePoint,
}
impl Adapter {
    pub(crate) fn new() -> Self {
        Self {
            fenced: false,
            observe: crate::workspace_service::ports::observe,
        }
    }
}

#[async_trait::async_trait]
impl ProviderAdapter for Adapter {
    fn kind(&self) -> &'static str {
        KIND
    }
    fn phases(&self) -> &'static [PhaseTag] {
        &[PhaseTag::Parked, PhaseTag::Succeeded]
    }
    async fn validate(&self, _: &Value) -> Result<()> {
        Err(conflict("reclaim requires workspace-owned admission"))
    }
    async fn prepare_tx<'tx>(&self, _: &mut Tx<'tx>, _: &Value, _: &Operation) -> Result<TxOutput> {
        Err(conflict("reclaim requires frozen authorization"))
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
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        Err(conflict("reclaim is admitted by its workspace owner"))
    }
    fn owns_parked_resource(&self) -> Option<&'static str> {
        Some(KEY)
    }
    async fn recover_owned_parked(
        &self,
        op: &Operation,
        _: RecoveryMode,
        ctx: &SpawnCtx,
    ) -> Result<ParkedRecovery> {
        let frozen = frozen(op)?;
        let mut execution = record(op)?.execution_id;
        if execution.is_none() {
            let _track = if self.fenced {
                None
            } else {
                let Some(guard) = crate::per_card_lock::try_lock_key(
                    &ctx.track_delete_locks,
                    &frozen.target.track_id,
                ) else {
                    return Ok(ParkedRecovery::LeaveParked);
                };
                Some(guard)
            };
            let _reservation = ctx.reclaim_admission.clone().write_owned().await;
            let Some(request) = refresh(&ctx.operation_repo.sqlite_db(), &frozen.target).await?
            else {
                return Ok(ParkedRecovery::Complete(Box::new((
                    ParkedOutcome::Succeeded {
                        result: json!({"removed":false}),
                    },
                    None,
                ))));
            };
            if !frozen.delete_authorized
                && !crate::workspace_service::ports::closed_idle_reclaim_safe(
                    &ctx.operation_repo.sqlite_db(),
                    &frozen.target.track_id,
                    None,
                )
                .await?
            {
                return Ok(ParkedRecovery::LeaveParked);
            }
            let prepared = tokio::task::spawn_blocking({
                let observe = self.observe;
                move || workspace::reclaim::prepare(&request, observe)
            })
            .await
            .map_err(conflict)??;
            let workspace::reclaim::Preparation::Ready(prepared) = prepared else {
                return Ok(ParkedRecovery::LeaveParked);
            };
            let id = admit(ctx, op, &frozen.target, &prepared.command).await?;
            // Both guards outlive the command and its durable settlement. The store remains a
            // terminal admission fence if transport loss prevents this wait from completing.
            wait(ctx, op, &id).await?;
            drop(prepared);
            execution = Some(id);
        }
        let id = execution.expect("admitted execution");
        ctx.execution.drive([id.clone()]);
        owned_execution::drain_exited(ctx, op, &id).await?;
        if successful(ctx, &id).await? {
            Ok(ParkedRecovery::Complete(Box::new((
                ParkedOutcome::Succeeded {
                    result: json!({"removed":true}),
                },
                None,
            ))))
        } else {
            // Missing path, absent journal, nonzero exit and unknown effect never authorize a receipt.
            tracing::warn!(op_id=%op.id,"reclaim effect has no durable successful settlement; claim retained");
            Ok(ParkedRecovery::LeaveParked)
        }
    }
    async fn complete_owned_parked_tx(
        &self,
        tx: &mut Tx<'_>,
        op: &Operation,
    ) -> Result<Vec<BroadcastEnvelope>> {
        let frozen = frozen(op)?;
        let output: String = sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id=?")
            .bind(&op.id)
            .fetch_one(&mut **tx)
            .await?;
        let output: TxOutput = serde_json::from_str(&output)?;
        let Some(id) = serde_json::from_value::<Record>(output.data[KEY].clone())?.execution_id
        else {
            // No effect was admitted and the original provision has already changed.
            return Ok(Vec::new());
        };
        let settlement =
            calm_truth::db::sqlite::gate_execution::settlement(tx, id.as_str()).await?;
        if !settlement
            .as_deref()
            .map(serde_json::from_str::<Settlement>)
            .transpose()?
            .as_ref()
            .is_some_and(is_success)
        {
            return Err(conflict("reclaim settlement changed"));
        }
        calm_truth::db::sqlite::workspace_execution::request(tx, id.as_str(), "retire").await?;
        removed_tx(tx, &frozen.target).await
    }
    fn owned_parked_completed(&self, op: &Operation, ctx: &SpawnCtx) {
        if let Ok(Record {
            execution_id: Some(id),
        }) = record(op)
        {
            ctx.execution.drive([id]);
        }
    }
    async fn plan_compensation(
        &self,
        _: PhaseTag,
        _: &str,
        _: &TxOutput,
        _: &Operation,
    ) -> Result<CompensationStateVersioned> {
        Err(conflict("reclaim retains unknown effects"))
    }
    async fn compensate_step(
        &self,
        _: &CompensationStep,
        _: &TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<()> {
        Err(conflict("reclaim retains unknown effects"))
    }
}

async fn admit(
    ctx: &SpawnCtx,
    op: &Operation,
    target: &ExtraWorktree,
    command: &std::process::Command,
) -> Result<ExecutionId> {
    let engine = owned_execution::engine(ctx)?;
    let id = ExecutionId::fresh()?;
    let utf8 = |s: &std::ffi::OsStr| {
        s.to_str()
            .map(str::to_owned)
            .ok_or_else(|| conflict("reclaim command is not UTF-8"))
    };
    let intent = ExecutionIntent {
        metadata: ExecutionMetadata {
            consumer: CONSUMER.into(),
            id: id.clone(),
            alias: ProcessAlias(format!("reclaim:{}", op.id)),
            owner: OperationRef(op.id.clone()),
            track: TrackId(target.track_id.clone()),
            source: PrivateSource {
                root: target.request.repo.clone(),
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
                version: calm_session::control::execution::SpecificationVersion::AmbientV2,
                ambient_environment: calm_session::control::execution::AmbientEnvironment::None,
                // The executor requires an absolute program. Preserve Command's PATH lookup
                // through an explicit shell exec; every argument remains positional data.
                program: "/bin/sh".into(),
                args: [
                    "-c".to_owned(),
                    "exec \"$@\"".to_owned(),
                    "reclaim".to_owned(),
                ]
                .into_iter()
                .chain(std::iter::once(utf8(command.get_program())?))
                .chain(command.get_args().map(utf8).collect::<Result<Vec<_>>>()?)
                .collect(),
                environment: command
                    .get_envs()
                    .filter_map(|(k, v)| v.map(|v| (k, v)))
                    .map(|(k, v)| Ok((utf8(k)?, utf8(v)?)))
                    .collect::<Result<_>>()?,
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
    let db = ctx.operation_repo.sqlite_db();
    let mut tx = calm_truth::db::sqlite::begin_immediate_tx(&db).await?;
    crate::operation::require_live_claim_tx(&mut tx, op).await?;
    let ticket = engine.admit(&mut tx, intent).await.map_err(conflict)?;
    let updated = sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.workspace_reclaim',json(?)) WHERE id=? AND json_extract(tx_output_json,'$.data.workspace_reclaim.execution_id') IS NULL")
        .bind(serde_json::to_string(&Record { execution_id: Some(id.clone()) })?).bind(&op.id).execute(&mut **tx).await?;
    if updated.rows_affected() != 1 {
        return Err(conflict("reclaim already admitted"));
    }
    tx.commit().await?;
    engine.drive(ticket.committed());
    Ok(id)
}
fn is_success(s: &Settlement) -> bool {
    matches!(
        s,
        Settlement::Journal(
            ExecutionState::Drained { status: Some(0) }
                | ExecutionState::OriginalGroupEmpty { status: Some(0) }
        )
    )
}
async fn successful(ctx: &SpawnCtx, id: &ExecutionId) -> Result<bool> {
    Ok(owned_execution::settlement(ctx, id)
        .await?
        .as_ref()
        .is_some_and(is_success))
}
async fn wait(ctx: &SpawnCtx, op: &Operation, id: &ExecutionId) -> Result<()> {
    let result = tokio::time::timeout(QUIESCE_DEADLINE, async {
        loop {
            if owned_execution::settlement(ctx, id).await?.is_some() {
                return Ok::<_, CalmError>(());
            }
            owned_execution::drain_exited(ctx, op, id).await?;
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(conflict)
    .and_then(|result| result);
    if result.is_err() {
        let db = ctx.operation_repo.sqlite_db();
        let mut tx = calm_truth::db::sqlite::begin_immediate_tx(&db).await?;
        crate::operation::require_live_claim_tx(&mut tx, op).await?;
        calm_truth::db::sqlite::workspace_execution::request(&mut tx, id.as_str(), "stop").await?;
        tx.commit().await?;
        ctx.execution.drive([id.clone()]);
    }
    result
}

async fn removed_tx(tx: &mut Tx<'_>, target: &ExtraWorktree) -> Result<Vec<BroadcastEnvelope>> {
    // Conditional and idempotent: a newer provision owns its own lifecycle.
    if !unmatched_provisioned_worktrees(tx)
        .await?
        .iter()
        .any(|r| r.event_id == target.provision_id)
    {
        return Ok(Vec::new());
    }
    let actor = ActorId::KernelDispatcher;
    let scope = EventScope::Card {
        card: target.card_id.clone().into(),
        track: target.track_id.clone().into(),
        area: target.area_id.clone().into(),
    };
    let event = Event::WorktreeRemoved {
        track_id: target.track_id.clone().into(),
        card_id: target.card_id.clone().into(),
        path: target.request.path.to_string_lossy().into_owned(),
    };
    let id = append_decision_event_in_tx(tx, &actor, &scope, None, &event).await?;
    Ok(vec![BroadcastEnvelope {
        id,
        actor,
        scope,
        event,
        event_version: SYNC_EVENT_VERSION,
    }])
}

/// A lost live wait cannot release durable admission while the executor may still be removing.
pub(crate) async fn terminal_admission_tx(tx: &mut Tx<'_>, track: &str) -> Result<()> {
    let busy: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workspace_executions WHERE json_extract(intent_json,'$.consumer')=? AND settled_at_ms IS NULL)")
        .bind(CONSUMER).fetch_one(&mut **tx).await?;
    if busy {
        return Err(CalmError::WorkspaceBusy(track.to_owned()));
    }
    Ok(())
}

pub(crate) struct Projection;
#[async_trait::async_trait]
impl crate::execution_store::ExecutionProjection for Projection {
    async fn spawned(
        &self,
        _: &mut Tx<'_>,
        _: &str,
        _: i64,
    ) -> std::result::Result<(), sqlx::Error> {
        Ok(())
    }
    async fn settled(
        &self,
        _: &mut Tx<'_>,
        _: &str,
        _: &Settlement,
        _: i64,
    ) -> std::result::Result<(), sqlx::Error> {
        Ok(())
    }
}
