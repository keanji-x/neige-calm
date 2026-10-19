//! Gate owner record CAS and execution settlement reads within the caller's transaction.
use sqlx::{Sqlite, Transaction};
pub async fn insert(
    tx: &mut Transaction<'_, Sqlite>,
    op: &str,
    record: &str,
) -> Result<(), sqlx::Error> {
    let result = sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.gate_execution',json(?2)) WHERE id=?1 AND phase='spawn_started' AND json_type(tx_output_json,'$.data.gate_execution') IS NULL")
        .bind(op).bind(record).execute(&mut **tx).await?;
    if result.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}
pub async fn compare_exchange(
    tx: &mut Transaction<'_, Sqlite>,
    op: &str,
    old: &str,
    new: &str,
) -> Result<(), sqlx::Error> {
    let result = sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.gate_execution',json(?3)) WHERE id=?1 AND json_extract(tx_output_json,'$.data.gate_execution.execution_id')=json_extract(?2,'$.execution_id') AND json_extract(tx_output_json,'$.data.gate_execution.release')=json_extract(?2,'$.release') AND json_extract(tx_output_json,'$.data.gate_execution.decision')=json_extract(?2,'$.decision') AND json_extract(tx_output_json,'$.data.gate_execution.version')=json_extract(?2,'$.version')")
        .bind(op).bind(old).bind(new).execute(&mut **tx).await?;
    if result.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}
pub async fn require_record(
    tx: &mut Transaction<'_, Sqlite>,
    op: &str,
    record: &str,
) -> Result<(), sqlx::Error> {
    let matches: bool = sqlx::query_scalar("SELECT json_extract(tx_output_json,'$.data.gate_execution.execution_id')=json_extract(?2,'$.execution_id') AND json_extract(tx_output_json,'$.data.gate_execution.release')=json_extract(?2,'$.release') AND json_extract(tx_output_json,'$.data.gate_execution.decision')=json_extract(?2,'$.decision') AND json_extract(tx_output_json,'$.data.gate_execution.version')=json_extract(?2,'$.version') FROM operations WHERE id=?1")
        .bind(op).bind(record).fetch_one(&mut **tx).await?;
    if !matches {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}
pub async fn settlement(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT settlement_json FROM workspace_executions WHERE execution_id=?1")
        .bind(id)
        .fetch_one(&mut **tx)
        .await
}
