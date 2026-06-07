use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::db::sqlite::runtime_get_by_id_tx;
use crate::error::{CalmError, Result};
use crate::harness::HarnessRegistry;
use crate::model::now_ms;
use crate::runtime_repo::RuntimeId;

use super::{
    AppServerInteractOutcome, CompensationStateVersioned, Operation, PhaseTag, ProviderAdapter,
    SpawnCtx, SpawnHandle, Tx, TxOutput,
};

const SHUTDOWN_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::Succeeded,
];

#[derive(Clone)]
pub struct SpecHarnessShutdownAdapter {
    harness_registry: HarnessRegistry,
}

impl SpecHarnessShutdownAdapter {
    pub fn new(harness_registry: HarnessRegistry) -> Self {
        Self { harness_registry }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpecHarnessShutdownOperationPayload {
    pub runtime_id: RuntimeId,
}

#[async_trait]
impl ProviderAdapter for SpecHarnessShutdownAdapter {
    fn kind(&self) -> &'static str {
        "spec-harness-shutdown"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        SHUTDOWN_PHASES
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: SpecHarnessShutdownOperationPayload = serde_json::from_value(input.clone())?;
        if payload.runtime_id.trim().is_empty() {
            return Err(CalmError::BadRequest("runtime_id is required".into()));
        }
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        let payload: SpecHarnessShutdownOperationPayload = serde_json::from_value(input.clone())?;
        let runtime = runtime_get_by_id_tx(tx, &payload.runtime_id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("runtime {}", payload.runtime_id)))?;
        sqlx::query(
            r#"UPDATE runtimes
                  SET status = 'superseded',
                      updated_at_ms = ?1,
                      completed_at_ms = COALESCE(completed_at_ms, ?1)
                WHERE id = ?2"#,
        )
        .bind(now_ms())
        .bind(&payload.runtime_id)
        .execute(&mut **tx)
        .await?;
        let mut output = TxOutput::new(
            "runtime",
            Some(runtime.id.clone()),
            serde_json::to_value(&runtime)?,
        );
        output.data = json!({ "runtime_id": runtime.id });
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
    ) -> Result<SpawnHandle> {
        let runtime_id = output_string(output, "runtime_id")?;
        if let Some(harness) = self.harness_registry.remove(&runtime_id) {
            harness.shutdown().await?;
        }
        Ok(SpawnHandle::NoOp)
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

fn output_string(output: &TxOutput, key: &str) -> Result<String> {
    output
        .data
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| CalmError::Internal(format!("spec harness tx_output missing {key}")))
}
