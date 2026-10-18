//! Explicit forward-only activation of one accepted cutover. No external ingress.
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::handoff_cutover::conflict;
use super::planner_harness_start_adapter::{HarnessProfile, PlannerHarnessStartAdapter};
use super::{
    AppServerInteractKind, AppServerInteractOutcome, CompensationStateVersioned, CompensationStep,
    Operation, OperationKey, OperationRuntime, PhaseTag, ProviderAdapter, SpawnCtx, SpawnHandle,
    SpawnOutcome, Tx, TxOutput,
};
use crate::error::{CalmError, Result};

pub(crate) const HANDOFF_ACTIVATION: &str = "handoff-activation";
const PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::AppServerInteract,
    PhaseTag::AwaitingRetry,
    PhaseTag::SpawnStarted,
    PhaseTag::SpawnSucceeded,
    PhaseTag::Succeeded,
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivationInput {
    pub cutover_operation_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ActivationResult {
    AcceptedPending,
    Preparing,
    Blocked {
        reason: String,
        resources_checkpointed: bool,
    },
    ResourceRequestRecorded,
    Unknown {
        reason: String,
    },
    ResourcesCheckpointed {
        thread_id: String,
    },
    NeedsRecovery {
        reason: String,
    },
    Activated {
        session_id: String,
    },
}

/// This internal API takes only the persisted cutover identity, never a caller's configuration.
/// Replays observe the original action. Only retry explicitly rearms an awaiting interaction.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "internal service; no external activation ingress")
)]
pub(crate) async fn activate(
    runtime: &OperationRuntime,
    cutover_operation_id: String,
) -> Result<super::OperationReceipt> {
    let (key, payload) = activation_key(cutover_operation_id)?;
    let id = runtime.submit(HANDOFF_ACTIVATION, key, payload).await?;
    runtime.observe_operation(&id).await
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "internal explicit retry; never background regrant"
    )
)]
pub(crate) async fn retry(
    runtime: &OperationRuntime,
    cutover_operation_id: String,
    attempt: i32,
) -> Result<super::OperationReceipt> {
    let (key, _) = activation_key(cutover_operation_id)?;
    runtime
        .retry_interaction(HANDOFF_ACTIVATION, &key, attempt)
        .await
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "read-only original internal receipt")
)]
pub(crate) async fn query(
    runtime: &OperationRuntime,
    cutover_operation_id: &str,
) -> Result<ActivationResult> {
    let op = runtime
        .find_by_kind_and_idempotency(HANDOFF_ACTIVATION, cutover_operation_id)
        .await?
        .ok_or_else(|| CalmError::NotFound("activation intent is not recorded".into()))?;
    if matches!(op.phase, super::Phase::Failed | super::Phase::Stuck { .. }) {
        return Ok(ActivationResult::NeedsRecovery {
            reason: op
                .last_error
                .ok_or_else(|| CalmError::Conflict("activation recovery reason missing".into()))?,
        });
    }
    if let Some(output) = op.tx_output {
        return Ok(serde_json::from_value(output.result)?);
    }
    if matches!(op.phase, super::Phase::Pending) {
        return Ok(ActivationResult::AcceptedPending);
    }
    Err(CalmError::Conflict(
        "activation has no prepared result; inspect original operation recovery receipt".into(),
    ))
}

fn activation_key(cutover_operation_id: String) -> Result<(OperationKey, Value)> {
    let payload = serde_json::to_value(ActivationInput {
        cutover_operation_id: cutover_operation_id.clone(),
    })?;
    Ok((
        OperationKey {
            operation_key: cutover_operation_id.clone(),
            idempotency_key: Some(cutover_operation_id),
            payload_hash: crate::routes::idempotency_key::stable_payload_hash(&payload)?,
        },
        payload,
    ))
}

pub(crate) struct HandoffActivationAdapter {
    pub start: Arc<PlannerHarnessStartAdapter>,
}

impl HandoffActivationAdapter {
    async fn checkpoint(
        &self,
        output: &mut TxOutput,
        op: &Operation,
        state: ActivationResult,
    ) -> Result<()> {
        let mut next = output.clone();
        next.result = serde_json::to_value(&state)?;
        let pool = self.start_repo_pool()?;
        let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
        if matches!(state, ActivationResult::ResourceRequestRecorded) {
            super::handoff_activation_sql::facts(&mut tx, &op.payload, Some(&op.id))
                .await?
                .validate_output(output)?;
        }
        super::checkpoint_app_server_interact_tx(
            &mut tx,
            op,
            AppServerInteractKind::MintAndAwait { thread_id: None },
            output,
            &next,
        )
        .await?;
        tx.commit().await?;
        *output = next;
        Ok(())
    }

    fn start_repo_pool(&self) -> Result<sqlx::SqlitePool> {
        self.start
            .activation_repo()
            .sqlite_pool()
            .ok_or_else(|| CalmError::Internal("activation requires SQLite".into()))
    }

    async fn interact(
        &self,
        output: &mut TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        let state: ActivationResult = serde_json::from_value(output.result.clone())?;
        // A persisted sending intent with no returned resource is not an idempotency receipt.
        if matches!(
            state,
            ActivationResult::ResourceRequestRecorded | ActivationResult::Unknown { .. }
        ) {
            self.checkpoint(output, op, ActivationResult::Unknown {
                reason: "Native resource result unconfirmed; explicit evidence-based recovery required".into(),
            }).await?;
            return Ok(AppServerInteractOutcome::AwaitingRetry);
        }
        let pool = self.start_repo_pool()?;
        let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
        let facts =
            super::handoff_activation_sql::facts(&mut tx, &op.payload, Some(&op.id)).await?;
        facts.validate_output(output)?;
        tx.rollback().await?;
        let provider = &facts.plan.configuration.conversation.provider;
        let resource = output.output_optional_string("codex_thread_id", "activation")?;
        if let Err(error) = self.start.activation_readiness(provider).await {
            self.checkpoint(
                output,
                op,
                ActivationResult::Blocked {
                    reason: error.to_string(),
                    resources_checkpointed: resource.is_some(),
                },
            )
            .await?;
            return Ok(AppServerInteractOutcome::AwaitingRetry);
        }

        if resource.is_none() {
            // Briefing and entire queue were committed in prepare_tx, before this intent or RPC.
            self.checkpoint(output, op, ActivationResult::ResourceRequestRecorded)
                .await?;
            let minted = self
                .start
                .mint_harness_resources(
                    HarnessProfile::Planner,
                    provider,
                    facts.plan.scope.target_card_id.as_str(),
                    facts.plan.scope.track_id.as_str(),
                    output.output_string("cwd", "activation")?,
                    true,
                )
                .await;
            let (thread_id, token_hash) = match minted {
                Ok(minted) => minted,
                Err(error) => {
                    self.checkpoint(
                        output,
                        op,
                        ActivationResult::Unknown {
                            reason: error.to_string(),
                        },
                    )
                    .await?;
                    return Ok(AppServerInteractOutcome::AwaitingRetry);
                }
            };
            // Even if this commit fails, the durable pre-RPC intent prevents a second mint.
            super::handoff_activation_sql::bind_resources(
                &pool,
                op,
                output,
                &thread_id,
                token_hash.as_deref(),
                &self.start,
            )
            .await?;
        }
        // Catch-up and complete source CAS stay in the existing recovery owner. No operation
        // snapshot may overwrite queue ACKs from an installed/recovered owner.
        if let Err(error) = self
            .start
            .install_harness(output, HarnessProfile::Planner, ctx, true)
            .await
        {
            self.checkpoint(
                output,
                op,
                ActivationResult::NeedsRecovery {
                    reason: error.to_string(),
                },
            )
            .await?;
            return Ok(AppServerInteractOutcome::AwaitingRetry);
        }
        let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
        let facts =
            super::handoff_activation_sql::facts(&mut tx, &op.payload, Some(&op.id)).await?;
        facts.validate_output(output)?;
        super::handoff_activation_sql::validate_resources(&mut tx, &facts, &op.id).await?;
        let mut next = output.clone();
        next.result = serde_json::to_value(ActivationResult::Activated {
            session_id: facts.plan.scope.new_session_id,
        })?;
        super::checkpoint_app_server_interact_tx(
            &mut tx,
            op,
            AppServerInteractKind::MintAndAwait {
                thread_id: output.output_optional_string("codex_thread_id", "activation")?,
            },
            output,
            &next,
        )
        .await?;
        tx.commit().await?;
        *output = next;
        Ok(AppServerInteractOutcome::NotApplicable)
    }
}

#[async_trait]
impl ProviderAdapter for HandoffActivationAdapter {
    fn kind(&self) -> &'static str {
        HANDOFF_ACTIVATION
    }
    fn phases(&self) -> &'static [PhaseTag] {
        PHASES
    }
    fn app_server_interact_kind(
        &self,
        _: &TxOutput,
        _: &Operation,
    ) -> Result<AppServerInteractKind> {
        Ok(AppServerInteractKind::MintAndAwait { thread_id: None })
    }
    async fn validate(&self, input: &Value) -> Result<()> {
        let pool = self.start_repo_pool()?;
        let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
        super::handoff_activation_sql::facts(&mut tx, input, None).await?;
        tx.rollback().await?;
        Ok(())
    }
    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> Result<TxOutput> {
        let facts = super::handoff_activation_sql::facts(tx, input, None).await?;
        let briefing =
            super::handoff_activation_briefing::render(self.start.activation_repo(), &facts.plan)
                .await?;
        super::handoff_activation_sql::prepare(tx, &facts, op, briefing).await
    }
    async fn app_server_interact(
        &self,
        output: &mut TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        match self.interact(output, op, ctx).await {
            Ok(result) => Ok(result),
            Err(error) => {
                // Never compensate a committed identity transition. Preserve any SQL checkpoint
                // that succeeded even when its answer was lost before returning to this caller.
                let pool = self.start_repo_pool()?;
                let stored: String =
                    sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id=?1")
                        .bind(&op.id)
                        .fetch_one(&pool)
                        .await?;
                *output = serde_json::from_str(&stored)?;
                let state: ActivationResult = serde_json::from_value(output.result.clone())?;
                let pending = match state {
                    ActivationResult::ResourceRequestRecorded
                    | ActivationResult::Unknown { .. } => ActivationResult::Unknown {
                        reason: error.to_string(),
                    },
                    _ => ActivationResult::NeedsRecovery {
                        reason: error.to_string(),
                    },
                };
                self.checkpoint(output, op, pending).await?;
                Ok(AppServerInteractOutcome::AwaitingRetry)
            }
        }
    }
    async fn spawn_side_effect(
        &self,
        _: &TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
    }
    async fn plan_compensation(
        &self,
        _: PhaseTag,
        _: &str,
        _: &TxOutput,
        _: &Operation,
    ) -> Result<CompensationStateVersioned> {
        Err(CalmError::Conflict(
            "Activation has no backward compensation; forward recovery required".into(),
        ))
    }
    async fn compensate_step(
        &self,
        _: &CompensationStep,
        _: &TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<()> {
        Err(conflict())
    }
}
