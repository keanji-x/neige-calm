//! Workspace-owned reclaim invocation and receipt recovery on the existing parked lifecycle.
use crate::operation::{
    AppServerInteractOutcome, CompensationStateVersioned, CompensationStep, Operation,
    ParkedOutcome, ParkedRecovery, PhaseTag, ProviderAdapter, RecoveryMode, SpawnCtx, SpawnOutcome,
    Tx, TxOutput, owned_execution,
};
use crate::{
    error::{CalmError, Result},
    event::{BroadcastEnvelope, Event, EventScope, SYNC_EVENT_VERSION},
    ids::ActorId,
};
use calm_truth::db::sqlite::{
    append_decision_event_in_tx, worktree_events::unmatched_provisioned_worktrees,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use workspace_runtime::execution::*;
use workspace_runtime::reclaim::ExtraWorktree;

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

pub(crate) struct Adapter;
impl Adapter {
    pub(crate) fn new() -> Self {
        Self
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
        let Some(id) = record(op)?.execution_id else {
            let db = ctx.operation_repo.sqlite_db();
            let current = db.writer().with_writer(|write_session| Box::pin(async move {
                let mut tx = write_session.begin_immediate().await?;
                crate::operation::require_live_claim_tx(&mut tx, op).await?;
                let current = calm_truth::db::sqlite::workspace_retirement::retire_extra_worktree_tx(
                    &mut tx, frozen.target.provision_id, &frozen.target.track_id,
                    &frozen.target.request.path,
                ).await?;
                sqlx::query("UPDATE operations SET last_error='cleanup deferred: isolation_unavailable' WHERE id=?")
                    .bind(&op.id).execute(&mut **tx).await?;
                tx.commit().await?;
                Ok::<_, CalmError>(current)
            })).await??;
            return Ok(if current {
                ParkedRecovery::LeaveParked
            } else {
                ParkedRecovery::Complete(Box::new((
                    ParkedOutcome::Succeeded {
                        result: json!({"removed":false}),
                    },
                    None,
                )))
            });
        };
        ctx.execution.drive([id.clone()]);
        owned_execution::drain_exited(ctx, op, &id).await?;
        if owned_execution::settlement(ctx, &id)
            .await?
            .as_ref()
            .is_some_and(is_success)
        {
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

fn is_success(s: &Settlement) -> bool {
    matches!(
        s,
        Settlement::Journal(
            ExecutionState::Drained { status: Some(0) }
                | ExecutionState::OriginalGroupEmpty { status: Some(0) }
        )
    )
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
pub(crate) async fn terminal_admission_tx(tx: &mut Tx<'_>, _track: &str) -> Result<()> {
    // Implicit-cwd Terminals can cd anywhere. Preserve their global reservation,
    // using the same durable targets as explicit-cwd and attached admission.
    if let Some(location) = calm_truth::db::sqlite::execution_workspace_locations(tx)
        .await?
        .iter()
        .find(|location| location.removing)
    {
        return Err(crate::operation::location_admission::removal_blocker(
            location,
        )?);
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
