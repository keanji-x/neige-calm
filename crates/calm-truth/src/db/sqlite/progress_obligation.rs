//! One-shot durable obligations. The caller owns enrollment, relevance and expiry policy.
//! Marking notified must share the transaction that persists the notification itself.
use crate::error::Result;
use sqlx::{Sqlite, Transaction};

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct ProgressObligation {
    pub execution_id: String,
    pub purpose: String,
    pub deadline_at_ms: i64,
}

/// A repeated enrollment never renews a deadline or re-arms a released obligation.
/// Owners must use a fresh execution identity for a new attempt.
pub async fn progress_obligation_require_tx(
    tx: &mut Transaction<'_, Sqlite>,
    obligation: &ProgressObligation,
) -> Result<u64> {
    Ok(sqlx::query(
        "INSERT INTO progress_obligations(execution_id,purpose,deadline_at_ms,state) \
         VALUES(?1,?2,?3,'pending') ON CONFLICT(execution_id,purpose) DO NOTHING",
    )
    .bind(&obligation.execution_id)
    .bind(&obligation.purpose)
    .bind(obligation.deadline_at_ms)
    .execute(&mut **tx)
    .await?
    .rows_affected())
}

pub async fn progress_obligation_release_tx(
    tx: &mut Transaction<'_, Sqlite>,
    execution_id: &str,
    purpose: &str,
) -> Result<()> {
    sqlx::query("UPDATE progress_obligations SET state='released' WHERE execution_id=?1 AND purpose=?2 AND state!='released'")
        .bind(execution_id).bind(purpose).execute(&mut **tx).await?;
    Ok(())
}

pub async fn progress_obligations_due_tx(
    tx: &mut Transaction<'_, Sqlite>,
    purpose: &str,
    now: i64,
) -> Result<Vec<ProgressObligation>> {
    Ok(sqlx::query_as(
        "SELECT execution_id,purpose,deadline_at_ms FROM progress_obligations WHERE purpose=?1 AND \
         state='pending' AND deadline_at_ms<=?2 ORDER BY deadline_at_ms,execution_id",
    )
    .bind(purpose)
    .bind(now)
    .fetch_all(&mut **tx)
    .await?)
}

/// Single winner. A rollback must also roll back the notification's event/queue write.
pub async fn progress_obligation_notify_tx(
    tx: &mut Transaction<'_, Sqlite>,
    obligation: &ProgressObligation,
    now: i64,
) -> Result<bool> {
    Ok(sqlx::query(
        "UPDATE progress_obligations SET state='notified' WHERE execution_id=?1 AND purpose=?2 AND \
         deadline_at_ms=?3 AND deadline_at_ms<=?4 AND state='pending'",
    )
    .bind(&obligation.execution_id)
    .bind(&obligation.purpose)
    .bind(obligation.deadline_at_ms)
    .bind(now)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        == 1)
}
