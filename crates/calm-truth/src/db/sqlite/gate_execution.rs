//! Keyed owner-record CAS and execution settlement reads in the caller's transaction.
use sqlx::{Sqlite, Transaction};

/// Independently owned fields preserved across generic operation checkpoints.
pub const OWNER_STATE_KEYS: &[&str] = &[
    "terminal_launch",
    "gate_execution",
    "forge_execution",
    "workspace_reclaim",
];

fn record_path(key: &str) -> String {
    assert!(
        OWNER_STATE_KEYS.contains(&key),
        "unregistered owner-state key"
    );
    format!("$.data.{key}")
}

pub async fn insert(
    tx: &mut Transaction<'_, Sqlite>,
    op: &str,
    key: &str,
    record: &str,
) -> Result<(), sqlx::Error> {
    let path = record_path(key);
    let result = sqlx::query(
        "UPDATE operations SET tx_output_json=json_set(tx_output_json,?3,json(?2)) \
         WHERE id=?1 AND phase='spawn_started' AND json_type(tx_output_json,?3) IS NULL",
    )
    .bind(op)
    .bind(record)
    .bind(path)
    .execute(&mut **tx)
    .await?;
    if result.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}

// Compare every field, without depending on the top-level object's key order. Values retain
// SQLite's comparison semantics from the original gate CAS (including release/decision JSON).
const MATCHES_RECORD: &str = "json_type(tx_output_json,?2)='object' AND NOT EXISTS (
    SELECT key,value FROM json_each(json_extract(tx_output_json,?2))
    EXCEPT SELECT key,value FROM json_each(?3)
) AND NOT EXISTS (
    SELECT key,value FROM json_each(?3)
    EXCEPT SELECT key,value FROM json_each(json_extract(tx_output_json,?2))
)";

pub async fn compare_exchange(
    tx: &mut Transaction<'_, Sqlite>,
    op: &str,
    key: &str,
    old: &str,
    new: &str,
) -> Result<(), sqlx::Error> {
    let query = format!(
        "UPDATE operations SET tx_output_json=json_set(tx_output_json,?2,json(?4)) WHERE id=?1 AND {MATCHES_RECORD}"
    );
    let result = sqlx::query(&query)
        .bind(op)
        .bind(record_path(key))
        .bind(old)
        .bind(new)
        .execute(&mut **tx)
        .await?;
    if result.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}

pub async fn require_record(
    tx: &mut Transaction<'_, Sqlite>,
    op: &str,
    key: &str,
    record: &str,
) -> Result<(), sqlx::Error> {
    let query = format!("SELECT COALESCE({MATCHES_RECORD},false) FROM operations WHERE id=?1");
    let matches: bool = sqlx::query_scalar(&query)
        .bind(op)
        .bind(record_path(key))
        .bind(record)
        .fetch_one(&mut **tx)
        .await?;
    if !matches {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}
pub use super::workspace_execution::settlement;
