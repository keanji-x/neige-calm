//! Internal user-audited identity cutover. No provider allocation or external ingress.
#[cfg(test)]
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(test)]
use super::{Operation, OperationKey, OperationOutcome, OperationRuntime, TxOnlyAdapter};
use super::{Tx, TxOutput};
use crate::error::{CalmError, Result};
use crate::harness::handoff_pause::PauseReceipt;
use crate::ids::{CardId, TrackId};

pub(crate) const HANDOFF_CUTOVER: &str = "handoff-cutover";

/// Exact identities and user authorization audit; constructors remain inside the kernel.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CutoverScope {
    pub track_id: TrackId,
    pub source_card_id: CardId,
    pub source_session_id: String,
    pub target_card_id: CardId,
    pub target_session_id: String,
    pub source_pause: PauseReceipt,
    pub target_pause: PauseReceipt,
    pub plan_id: String,
    pub new_session_id: String,
    pub user_audit_id: String,
}

/// Captured by precheck, never supplied as free JSON by a user or model.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CutoverConfiguration {
    pub conversation: crate::harness::profile::PlannerHandoffConfiguration,
    pub source_payload_hash: String,
    pub target_payload_hash: String,
    pub template_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ValidatedCutoverPlan {
    pub scope: CutoverScope,
    pub configuration: CutoverConfiguration,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CutoverResult {
    CutoverCommittedPendingActivation {
        operation_id: String,
        plan_id: String,
        track_id: TrackId,
        history_card_id: CardId,
        retired_source_session_id: String,
        retired_target_session_id: String,
        planner_card_id: CardId,
        planner_session_id: String,
        configuration: CutoverConfiguration,
        snapshot_session_id: String,
        push_watermark: i64,
    },
}

pub(crate) fn conflict() -> CalmError {
    CalmError::Conflict("handoff cutover facts or exact ownership changed".into())
}

impl CutoverScope {
    fn validate(&self) -> Result<()> {
        self.source_pause
            .request()
            .validate()
            .map_err(|_| conflict())?;
        self.target_pause
            .request()
            .validate()
            .map_err(|_| conflict())?;
        if !self.source_pause.matches_binding(
            &self.source_card_id,
            &self.source_session_id,
            &self.track_id,
        ) || !self.target_pause.matches_binding(
            &self.target_card_id,
            &self.target_session_id,
            &self.track_id,
        ) || self.source_card_id == self.target_card_id
            || self.source_session_id == self.target_session_id
            || self.new_session_id == self.source_session_id
            || self.new_session_id == self.target_session_id
            || [&self.plan_id, &self.new_session_id, &self.user_audit_id]
                .iter()
                .any(|s| s.is_empty())
        {
            return Err(conflict());
        }
        Ok(())
    }
}

/// Configuration precheck has no resource effects. Its read transaction is rolled back.
pub(crate) async fn precheck(
    pool: &sqlx::SqlitePool,
    scope: CutoverScope,
) -> Result<ValidatedCutoverPlan> {
    scope.validate()?;
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    let facts = super::handoff_cutover_sql::facts(&mut tx, &scope).await?;
    tx.rollback().await?;
    Ok(ValidatedCutoverPlan {
        scope,
        configuration: facts.configuration,
    })
}

/// Only future user-authorized internal owners may call this service. There is no MCP route.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "internal service for the later user-authorized entry point"
    )
)]
#[cfg(test)]
pub(crate) async fn commit(
    runtime: &OperationRuntime,
    cache: &crate::card_role_cache::CardRoleCache,
    plan: ValidatedCutoverPlan,
) -> Result<CutoverResult> {
    let payload = serde_json::to_value(&plan)?;
    let key = OperationKey {
        operation_key: plan.scope.plan_id.clone(),
        idempotency_key: Some(plan.scope.plan_id.clone()),
        payload_hash: crate::routes::idempotency_key::stable_payload_hash(&payload)?,
    };
    let result = super::tx_only::commit_tx_only(
        &runtime.repo.sqlite_pool(),
        &HandoffCutoverAdapter,
        key,
        payload,
    )
    .await?;
    let id = match result {
        super::tx_only::KeyedCommit::Replay(id) => id,
        super::tx_only::KeyedCommit::Committed { result, .. } => result.op_id,
    };
    let result = runtime
        .wait_for_receipt(&id, std::time::Duration::ZERO)
        .await?;
    let super::OperationReceipt::Settled(result) = result else {
        return Err(conflict());
    };
    let OperationOutcome::Succeeded { result } = result.outcome else {
        return Err(conflict());
    };
    let result: CutoverResult = serde_json::from_value(result)?;
    publish_cache(&runtime.repo.sqlite_pool(), cache, &plan).await?;
    Ok(result)
}

// Never publish a replay's old grant over a later SQL owner.
pub(super) async fn publish_cache(
    pool: &sqlx::SqlitePool,
    cache: &crate::card_role_cache::CardRoleCache,
    plan: &ValidatedCutoverPlan,
) -> Result<()> {
    for id in [&plan.scope.source_card_id, &plan.scope.target_card_id] {
        let role: String = sqlx::query_scalar("SELECT role FROM cards WHERE id=?1")
            .bind(id.as_str())
            .fetch_one(pool)
            .await?;
        cache.insert(
            id.clone(),
            crate::model::CardRole::try_from(role).map_err(CalmError::Internal)?,
            plan.scope.track_id.clone(),
        );
    }
    Ok(())
}

/// IMMEDIATE serializes with an in-flight SQLite commit, including a missing commit answer.
/// The exact original payload is checked before consuming a succeeded operation.
pub(super) async fn query_recorded(
    pool: &sqlx::SqlitePool,
    plan: &ValidatedCutoverPlan,
) -> Result<Option<CutoverResult>> {
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    let result = query_recorded_tx(&mut tx, plan).await?;
    tx.rollback().await?;
    Ok(result)
}

pub(super) async fn query_recorded_tx(
    tx: &mut Tx<'_>,
    plan: &ValidatedCutoverPlan,
) -> Result<Option<CutoverResult>> {
    let row:Option<(String,String,String,Option<String>)>=sqlx::query_as("SELECT id,payload_hash,payload_json,tx_output_json FROM operations WHERE kind=?1 AND idempotency_key=?2")
        .bind(HANDOFF_CUTOVER).bind(&plan.scope.plan_id).fetch_optional(&mut **tx).await?;
    let Some((id, hash, payload, output)) = row else {
        return Ok(None);
    };
    let expected = serde_json::to_value(plan)?;
    if serde_json::from_str::<Value>(&payload)? != expected
        || hash != crate::routes::idempotency_key::stable_payload_hash(&expected)?
    {
        return Err(conflict());
    }
    let output: TxOutput = serde_json::from_str(&output.ok_or_else(conflict)?)?;
    let result: CutoverResult = serde_json::from_value(output.result)?;
    let CutoverResult::CutoverCommittedPendingActivation { operation_id, .. } = &result;
    if *operation_id != id {
        return Err(conflict());
    }
    Ok(Some(result))
}

#[cfg(test)]
pub(crate) struct HandoffCutoverAdapter;

#[cfg(test)]
#[async_trait]
impl TxOnlyAdapter for HandoffCutoverAdapter {
    fn kind(&self) -> &'static str {
        HANDOFF_CUTOVER
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let plan: ValidatedCutoverPlan = serde_json::from_value(input.clone())?;
        plan.scope.validate()
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> Result<TxOutput> {
        let plan: ValidatedCutoverPlan = serde_json::from_value(input.clone())?;
        let result = super::handoff_cutover_sql::cutover(tx, &plan, &op.id).await?;
        Ok(TxOutput::new(
            "track",
            Some(plan.scope.track_id.to_string()),
            serde_json::to_value(result)?,
        ))
    }
}
