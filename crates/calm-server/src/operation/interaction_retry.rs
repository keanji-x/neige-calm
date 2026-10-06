//! Read-only receipts and explicit, generation-fenced interaction retry.
use std::time::Duration;

use sqlx::SqlitePool;

use super::{
    OPERATION_LEASE_MS, Operation, OperationKey, OperationResult, OperationRuntime, Phase,
    TxOutput, operation_result_from, required_output,
};
use crate::error::{CalmError, Result};
use crate::model::{new_id, now_ms};

#[derive(Clone, Debug)]
pub enum OperationReceipt {
    Running {
        operation_id: String,
        attempt: i32,
    },
    AwaitingRetry {
        operation_id: String,
        attempt: i32,
        output: TxOutput,
    },
    Settled(OperationResult),
}

impl OperationRuntime {
    pub(super) async fn refuse_deferred_wait(&self, operation_id: &str) -> Result<()> {
        if matches!(
            self.observe_operation(operation_id).await?,
            OperationReceipt::AwaitingRetry { .. }
        ) {
            return Err(CalmError::ServiceUnavailable(
                "Operation outcome is unconfirmed; an explicit retry is required.".into(),
            ));
        }
        Ok(())
    }

    /// Observing a receipt never drives or rearms an external effect.
    pub async fn observe_operation(&self, operation_id: &str) -> Result<OperationReceipt> {
        let op = self
            .repo
            .get_operation(operation_id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("operation {operation_id}")))?;
        if let Some(result) = operation_result_from(&op)? {
            return Ok(OperationReceipt::Settled(result));
        }
        if matches!(op.phase, Phase::AwaitingRetry { .. }) {
            return Ok(OperationReceipt::AwaitingRetry {
                operation_id: op.id.clone(),
                attempt: op.attempt,
                output: required_output(&op)?.clone(),
            });
        }
        Ok(OperationReceipt::Running {
            operation_id: op.id,
            attempt: op.attempt,
        })
    }

    /// A bounded read-back; a running receipt at the deadline is unconfirmed.
    pub async fn wait_for_receipt(
        &self,
        operation_id: &str,
        duration: Duration,
    ) -> Result<OperationReceipt> {
        let wait = async {
            loop {
                let receipt = self.observe_operation(operation_id).await?;
                if !matches!(receipt, OperationReceipt::Running { .. }) {
                    return Ok(receipt);
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        };
        match tokio::time::timeout(duration, wait).await {
            Ok(result) => result,
            Err(_) => self.observe_operation(operation_id).await,
        }
    }

    /// Called only for a new explicit press, with the generation from its receipt.
    /// A replay/read of the original intent does not call this method.
    pub async fn retry_interaction(
        &self,
        kind: &str,
        key: &OperationKey,
        expected_attempt: i32,
    ) -> Result<OperationReceipt> {
        if key.idempotency_key.is_none() || expected_attempt < 0 {
            return Err(CalmError::BadRequest(
                "key and nonnegative retry attempt are required".into(),
            ));
        }
        let id = self
            .keyed_replay(kind, key)
            .await?
            .ok_or_else(|| CalmError::NotFound("operation intent is not recorded".into()))?;
        let op = self
            .repo
            .get_operation(&id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("operation {id}")))?;
        let adapter = self.adapter(kind)?;
        if !adapter.phases().contains(&super::PhaseTag::AwaitingRetry) {
            return Err(CalmError::BadRequest(
                "operation does not support explicit interaction retry".into(),
            ));
        }
        let _guard = self.drive_mutex.lock().await;
        let claimed = self
            .repo
            .claim_interaction_retry(&op, expected_attempt)
            .await?;
        let won = claimed.is_some();
        if let Some(claimed) = claimed {
            self.drive_claimed(adapter, claimed).await?;
        }
        drop(_guard);
        if won {
            self.drive().await?;
        }
        self.observe_operation(&id).await
    }
}

pub(super) async fn claim_retry(
    pool: &SqlitePool,
    op: &Operation,
    expected_attempt: i32,
) -> Result<Option<Operation>> {
    let Phase::AwaitingRetry { kind } = &op.phase else {
        return Ok(None);
    };
    let Some(key) = op.idempotency_key.as_deref() else {
        return Err(CalmError::Internal(
            "unkeyed interaction cannot await explicit retry".into(),
        ));
    };
    let (_, detail) = Phase::AppServerInteract { kind: kind.clone() }.serialize_split();
    let now = now_ms();
    let row = sqlx::query(
        "UPDATE operations SET phase='app_server_interact', phase_detail_json=?1,
         attempt=attempt+1, lease_owner=?2, lease_until_ms=?3, updated_at_ms=?4
         WHERE id=?5 AND kind=?6 AND idempotency_key=?7 AND payload_hash=?8
         AND phase='awaiting_retry' AND attempt=?9 AND phase_detail_json=?1
         AND lease_owner IS NULL AND completed_at_ms IS NULL RETURNING *",
    )
    .bind(detail.map(|value| value.to_string()))
    .bind(new_id())
    .bind(now + OPERATION_LEASE_MS)
    .bind(now)
    .bind(&op.id)
    .bind(&op.kind)
    .bind(key)
    .bind(&op.payload_hash)
    .bind(expected_attempt)
    .fetch_optional(pool)
    .await?;
    row.as_ref()
        .map(super::repo_sqlite::operation_from_row)
        .transpose()
}
