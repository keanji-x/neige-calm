use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{CalmError, Result};
use crate::harness::HarnessRegistry;

use super::{
    AppServerInteractOutcome, CompensationStateVersioned, Operation, PhaseTag, ProviderAdapter,
    SpawnCtx, SpawnHandle, SpawnOutcome, Tx, TxOutput,
};

const INTERRUPT_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::Succeeded,
];
const MAX_INTERRUPT_REASON_LEN: usize = 512;

#[derive(Clone)]
pub struct PlannerHarnessInterruptAdapter {
    harness_registry: HarnessRegistry,
}

impl PlannerHarnessInterruptAdapter {
    pub fn new(harness_registry: HarnessRegistry) -> Self {
        Self { harness_registry }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlannerHarnessInterruptOperationPayload {
    /// Wire key frozen as `runtime_id`: stored `operations.payload_json` rows keep that key and this field has
    /// no default, so without the rename a parked operation would fail to deserialize on resume.
    #[serde(rename = "runtime_id")]
    pub worker_session_id: String,
    pub reason: String,
}

#[async_trait]
impl ProviderAdapter for PlannerHarnessInterruptAdapter {
    fn kind(&self) -> &'static str {
        "planner-harness-interrupt"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        INTERRUPT_PHASES
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: PlannerHarnessInterruptOperationPayload =
            serde_json::from_value(input.clone())?;
        if payload.worker_session_id.trim().is_empty() {
            return Err(CalmError::BadRequest("runtime_id is required".into()));
        }
        if payload.reason.len() > MAX_INTERRUPT_REASON_LEN {
            return Err(CalmError::BadRequest(format!(
                "interrupt reason must be at most {MAX_INTERRUPT_REASON_LEN} bytes"
            )));
        }
        Err(CalmError::Conflict(
            "new unbound interruption operations are unavailable; use the current control owner"
                .into(),
        ))
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        let _: PlannerHarnessInterruptOperationPayload = serde_json::from_value(input.clone())?;
        let _ = tx;
        Err(CalmError::Conflict(
            "old pending interruption has no original target; refusing to select a later turn"
                .into(),
        ))
    }

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        let (original, reason) = original_receipt(op, output)?;
        let harness = self
            .harness_registry
            .get(&original.id)
            .ok_or_else(|| CalmError::NotFound(format!("harness {}", original.id)))?;
        let requested_at_ms = super::repo_sqlite::original_request_created_at(
            &ctx.operation_repo.sqlite_pool(),
            &op.id,
        )
        .await?;
        harness
            .interrupt_original_receipt(&original, reason, requested_at_ms)
            .await?;
        Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        _output: &TxOutput,
        _op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.to_string(),
            steps: vec![],
        })
    }

    async fn compensate_step(
        &self,
        _step: &super::CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<()> {
        Ok(())
    }
}

/// Lifetime decoder for the released operation's mutually bound original facts.
/// New callers cannot create this receipt; missing or contradictory old values
/// are held rather than interpreted as a new current-target request.
fn original_receipt(
    op: &Operation,
    output: &TxOutput,
) -> Result<(
    crate::session_projection_repo::WorkerSessionProjection,
    String,
)> {
    let payload: PlannerHarnessInterruptOperationPayload =
        serde_json::from_value(op.payload.clone())?;
    let runtime = output.output_string("runtime_id", "planner harness")?;
    let reason = output.output_string("reason", "planner harness")?;
    let original: crate::session_projection_repo::WorkerSessionProjection =
        serde_json::from_value(output.result.clone()).map_err(|_| {
            CalmError::Conflict("old interruption has no valid original runtime receipt".into())
        })?;
    if runtime.is_empty()
        || payload.worker_session_id != runtime
        || payload.reason != reason
        || op.target_type != "runtime"
        || op.target_id.as_deref() != Some(runtime.as_str())
        || op.target.get("type").and_then(Value::as_str) != Some("runtime")
        || op.target.get("id").and_then(Value::as_str) != Some(runtime.as_str())
        || output.target_type != "runtime"
        || output.target_id.as_deref() != Some(runtime.as_str())
        || original.id != runtime
    {
        return Err(CalmError::Conflict(
            "old interruption receipt contradicts its original request or target".into(),
        ));
    }
    Ok((original, reason))
}
