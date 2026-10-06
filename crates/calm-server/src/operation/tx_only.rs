//! Operations whose every effect ends at their transaction's commit: a card or a recipe create
//! (#2175 K1a). [`OperationRuntime::commit_keyed`](super::OperationRuntime::commit_keyed) runs one
//! in a single `BEGIN IMMEDIATE` transaction that checks the request's key, writes its `operations`
//! row and its effects, and settles the row as `succeeded`. It takes no drive lock and no lease and
//! drives no other operation; an error rolls everything back, so nothing is bound to the key. The
//! blanket [`ProviderAdapter`] impl still drives a row an older server left before its end.

use async_trait::async_trait;
use serde_json::Value;
use sqlx::SqlitePool;

use crate::db::sqlite::begin_immediate_tx;
use crate::error::Result;
use crate::event::BroadcastEnvelope;
use crate::model::{new_id, now_ms};

use super::repo_sqlite::{insert_pending_row, operation_from_row};
use super::{
    AppServerInteractOutcome, CompensationStateVersioned, CompensationStep, Operation, OperationId,
    OperationKey, OperationResult, PhaseTag, ProviderAdapter, SpawnCtx, SpawnHandle, SpawnOutcome,
    Tx, TxOutput, idempotency_payload_conflict, operation_result_from,
};

/// An adapter whose effects all end at the commit of [`Self::prepare_tx`]: no app-server step, no
/// spawn, nothing to compensate. Only these kinds may use `commit_keyed`. `phases()` cannot carry
/// this: the planner-harness interrupt and shutdown adapters declare the same phases and do have
/// side effects.
#[async_trait]
pub trait TxOnlyAdapter: Send + Sync {
    fn kind(&self) -> &'static str;

    async fn validate(&self, input: &Value) -> Result<()>;

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> Result<TxOutput>;
}

const TX_ONLY_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::Succeeded,
];

#[async_trait]
impl<T: TxOnlyAdapter> ProviderAdapter for T {
    fn kind(&self) -> &'static str {
        TxOnlyAdapter::kind(self)
    }

    fn phases(&self) -> &'static [PhaseTag] {
        TX_ONLY_PHASES
    }

    fn as_tx_only(&self) -> Option<&dyn TxOnlyAdapter> {
        Some(self)
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        TxOnlyAdapter::validate(self, input).await
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> Result<TxOutput> {
        TxOnlyAdapter::prepare_tx(self, tx, input, op).await
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
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
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
        _step: &CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<()> {
        Ok(())
    }
}

/// What one keyed commit found or did.
pub(super) enum KeyedCommit {
    /// The key already names this request's operation; nothing was written.
    Replay(OperationId),
    /// The operation committed as `succeeded`; its events go out after the commit.
    Committed {
        result: OperationResult,
        events: Vec<BroadcastEnvelope>,
    },
}

/// The transaction of `commit_keyed`. Any error rolls it back whole.
pub(super) async fn commit_tx_only(
    pool: &SqlitePool,
    adapter: &dyn TxOnlyAdapter,
    key: OperationKey,
    payload: Value,
) -> Result<KeyedCommit> {
    // Before BEGIN: a request paused inside the IMMEDIATE transaction would hold the write lock the
    // concurrent request it waits for needs.
    #[cfg(feature = "fixtures")]
    if let Some(idempotency_key) = key.idempotency_key.as_deref() {
        crate::test_seams::pause_point(crate::test_seams::OPERATION_DEDUP_MISSED, idempotency_key)
            .await;
    }
    let mut tx = begin_immediate_tx(pool).await?;
    match commit_in_tx(&mut tx, adapter, key, payload).await {
        Ok(commit @ KeyedCommit::Committed { .. }) => {
            tx.commit().await?;
            Ok(commit)
        }
        Ok(replay @ KeyedCommit::Replay(_)) => {
            tx.rollback().await?;
            Ok(replay)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

async fn commit_in_tx(
    tx: &mut Tx<'_>,
    adapter: &dyn TxOnlyAdapter,
    key: OperationKey,
    payload: Value,
) -> Result<KeyedCommit> {
    let kind = adapter.kind();
    // Under the write lock this read is final: no other writer can bind the key before the commit.
    if let Some(idempotency_key) = key.idempotency_key.as_deref() {
        let existing: Option<(String, String)> = sqlx::query_as(
            "SELECT id, payload_hash FROM operations WHERE kind = ?1 AND idempotency_key = ?2",
        )
        .bind(kind)
        .bind(idempotency_key)
        .fetch_optional(&mut **tx)
        .await?;
        if let Some((op_id, payload_hash)) = existing {
            if payload_hash != key.payload_hash {
                return Err(idempotency_payload_conflict(Some(idempotency_key)));
            }
            return Ok(KeyedCommit::Replay(op_id));
        }
    }
    let op_id = new_id();
    insert_pending_row(&op_id, kind, &key, &payload)?
        .execute(&mut **tx)
        .await?;
    let op = read_row(tx, &op_id).await?;
    let mut output = adapter.prepare_tx(tx, &payload, &op).await?;
    let events = std::mem::take(&mut output.post_commit_events);
    let now = now_ms();
    sqlx::query(
        r#"UPDATE operations
           SET tx_output_json = ?1,
               target_type = ?2,
               target_id = ?3,
               target_json = ?4,
               phase = 'succeeded',
               phase_detail_json = NULL,
               completed_at_ms = ?5,
               updated_at_ms = ?5
           WHERE id = ?6"#,
    )
    .bind(serde_json::to_string(&output)?)
    .bind(&output.target_type)
    .bind(&output.target_id)
    .bind(serde_json::to_string(&serde_json::json!({
        "type": output.target_type,
        "id": output.target_id,
    }))?)
    .bind(now)
    .bind(&op_id)
    .execute(&mut **tx)
    .await?;
    // The answer is read back from the row, the same way a replay reads it.
    let result = operation_result_from(&read_row(tx, &op_id).await?)?.ok_or_else(|| {
        crate::error::CalmError::Internal(format!("operation {op_id} did not settle"))
    })?;
    Ok(KeyedCommit::Committed { result, events })
}

async fn read_row(tx: &mut Tx<'_>, op_id: &str) -> Result<Operation> {
    let row = sqlx::query("SELECT * FROM operations WHERE id = ?1")
        .bind(op_id)
        .fetch_one(&mut **tx)
        .await?;
    operation_from_row(&row)
}
