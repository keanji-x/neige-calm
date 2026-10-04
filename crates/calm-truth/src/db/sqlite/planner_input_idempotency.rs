//! #2043 — the `Idempotency-Key` bindings of `POST /api/cards/{id}/planner/input`.
//! A binding is written only by the harness transaction that durably accepts the message, so
//! a binding exists exactly when its message was stored.

use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::error::Result;
use crate::model::now_ms;

/// How many bindings a card keeps; the writer drops the oldest beyond it. A key older than
/// that is unknown again, and its request is a new send.
pub const PLANNER_INPUT_BINDINGS_PER_CARD: i64 = 64;

/// What an earlier send under one key was answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannerInputBinding {
    pub payload_hash: String,
    pub worker_session_id: String,
    /// `None` exactly when the message folded into a queue entry that has no id.
    pub entry_id: Option<String>,
}

pub async fn planner_input_binding_get(
    pool: &SqlitePool,
    card_id: &str,
    idempotency_key: &str,
) -> Result<Option<PlannerInputBinding>> {
    let row: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT payload_hash, worker_session_id, entry_id FROM planner_input_idempotency \
         WHERE card_id = ?1 AND idempotency_key = ?2",
    )
    .bind(card_id)
    .bind(idempotency_key)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(
        |(payload_hash, worker_session_id, entry_id)| PlannerInputBinding {
            payload_hash,
            worker_session_id,
            entry_id,
        },
    ))
}

/// Must run in the transaction that writes the snapshot holding the message. A plain INSERT on
/// purpose: a second binding for one key fails that transaction instead of storing the message twice.
pub async fn planner_input_bind_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    idempotency_key: &str,
    binding: &PlannerInputBinding,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO planner_input_idempotency \
         (card_id, idempotency_key, payload_hash, worker_session_id, entry_id, created_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(card_id)
    .bind(idempotency_key)
    .bind(&binding.payload_hash)
    .bind(&binding.worker_session_id)
    .bind(&binding.entry_id)
    .bind(now_ms())
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "DELETE FROM planner_input_idempotency WHERE card_id = ?1 AND id NOT IN \
         (SELECT id FROM planner_input_idempotency WHERE card_id = ?1 ORDER BY id DESC LIMIT ?2)",
    )
    .bind(card_id)
    .bind(PLANNER_INPUT_BINDINGS_PER_CARD)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
