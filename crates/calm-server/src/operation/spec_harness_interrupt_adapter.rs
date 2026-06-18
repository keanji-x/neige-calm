use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::db::sqlite::session_projection_by_id_tx;
use crate::error::{CalmError, Result};
use crate::harness::HarnessRegistry;
use crate::session_projection_repo::RuntimeId;

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
pub struct SpecHarnessInterruptAdapter {
    harness_registry: HarnessRegistry,
}

impl SpecHarnessInterruptAdapter {
    pub fn new(harness_registry: HarnessRegistry) -> Self {
        Self { harness_registry }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpecHarnessInterruptOperationPayload {
    pub runtime_id: RuntimeId,
    pub reason: String,
}

#[async_trait]
impl ProviderAdapter for SpecHarnessInterruptAdapter {
    fn kind(&self) -> &'static str {
        "spec-harness-interrupt"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        INTERRUPT_PHASES
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: SpecHarnessInterruptOperationPayload = serde_json::from_value(input.clone())?;
        if payload.runtime_id.trim().is_empty() {
            return Err(CalmError::BadRequest("runtime_id is required".into()));
        }
        if payload.reason.len() > MAX_INTERRUPT_REASON_LEN {
            return Err(CalmError::BadRequest(format!(
                "interrupt reason must be at most {MAX_INTERRUPT_REASON_LEN} bytes"
            )));
        }
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        let payload: SpecHarnessInterruptOperationPayload = serde_json::from_value(input.clone())?;
        let runtime = session_projection_by_id_tx(tx, &payload.runtime_id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("runtime {}", payload.runtime_id)))?;
        if self.harness_registry.get(&payload.runtime_id).is_none() {
            return Err(CalmError::NotFound(format!(
                "harness {}",
                payload.runtime_id
            )));
        }
        let mut output = TxOutput::new(
            "runtime",
            Some(runtime.id.clone()),
            serde_json::to_value(&runtime)?,
        );
        output.data = json!({
            "runtime_id": runtime.id,
            "reason": payload.reason,
        });
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

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        let runtime_id = output.output_string("runtime_id", "spec harness")?;
        let reason = output.output_string("reason", "spec harness")?;
        let harness = self
            .harness_registry
            .get(&runtime_id)
            .ok_or_else(|| CalmError::NotFound(format!("harness {runtime_id}")))?;
        harness.interrupt(reason).await?;
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
