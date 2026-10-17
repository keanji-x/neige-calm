//! Original operation insertion and domain-owned acceptance share one writer
//! transaction. Read-only original results take a separate callback.

use async_trait::async_trait;
use serde_json::Value;
use sqlx::SqlitePool;

use super::{
    Operation, OperationId, OperationKey, Tx, idempotency_payload_conflict,
    repo_sqlite::{insert_pending_row, operation_from_row},
    required_lease_owner,
};
use crate::{
    db::sqlite::begin_immediate_tx,
    error::{CalmError, Result},
    model::{new_id, now_ms},
};

#[async_trait]
pub(crate) trait OperationAdmission: Send + Sync {
    async fn accept_tx(&self, tx: &mut Tx<'_>, original: &Operation) -> Result<()>;
    async fn authorize_original_tx(&self, tx: &mut Tx<'_>, original: &Operation) -> Result<()>;
}

pub(super) async fn insert_admitted(
    pool: &SqlitePool,
    kind: &str,
    key: OperationKey,
    payload: Value,
    admission: &dyn OperationAdmission,
) -> Result<OperationId> {
    let mut tx = begin_immediate_tx(pool).await?;
    if let Some(key_value) = key.idempotency_key.as_deref() {
        let row = sqlx::query("SELECT * FROM operations WHERE kind=?1 AND idempotency_key=?2")
            .bind(kind)
            .bind(key_value)
            .fetch_optional(&mut *tx)
            .await?;
        if let Some(row) = row {
            let original = operation_from_row(&row)?;
            if original.payload_hash != key.payload_hash {
                return Err(idempotency_payload_conflict(Some(key_value)));
            }
            admission.authorize_original_tx(&mut tx, &original).await?;
            tx.commit().await?;
            return Ok(original.id);
        }
    }
    let id = new_id();
    insert_pending_row(&id, kind, &key, &payload)?
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE operations SET execution_admitted=1 WHERE id=?1")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    let row = sqlx::query("SELECT * FROM operations WHERE id=?1")
        .bind(&id)
        .fetch_one(&mut *tx)
        .await?;
    let original = operation_from_row(&row)?;
    admission.accept_tx(&mut tx, &original).await?;
    tx.commit().await?;
    Ok(original.id)
}

/// One generic live-claim predicate for a new operation effect and its owning
/// preparation transaction. Domain owners do not copy lease/phase SQL.
pub(crate) async fn require_live_claim_tx(tx: &mut Tx<'_>, original: &Operation) -> Result<()> {
    let phase = original.phase.tag();
    let detail = original
        .phase_detail
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let current: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM operations WHERE id=?1 AND operation_key=?2 AND kind=?3 \
         AND idempotency_key IS ?4 AND payload_hash=?5 AND payload_json=?6 \
         AND phase=?7 AND phase_detail_json IS ?8 AND lease_owner=?9 \
         AND lease_until_ms>=?10 AND completed_at_ms IS NULL",
    )
    .bind(&original.id)
    .bind(&original.operation_key)
    .bind(&original.kind)
    .bind(&original.idempotency_key)
    .bind(&original.payload_hash)
    .bind(serde_json::to_string(&original.payload)?)
    .bind(phase.as_str())
    .bind(detail)
    .bind(required_lease_owner(original)?)
    .bind(now_ms())
    .fetch_optional(&mut **tx)
    .await?;
    if current.is_none() {
        return Err(CalmError::Conflict(
            "operation no longer owns its original live claim".into(),
        ));
    }
    Ok(())
}
