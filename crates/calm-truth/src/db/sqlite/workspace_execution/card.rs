//! Card ownership comes from the accepted operation target, never editable card payload.
use super::*;
use crate::error::{CalmError, Result};
use workspace::execution::ExecutionId;

const UNFINISHED_SQL: &str =
    "e.settled_at_ms IS NULL OR e.desired <> 'retire' OR e.secrets_json IS NOT NULL";

pub struct CardExecution {
    pub id: ExecutionId,
    pub settled: bool,
}

/// Include settled owners until their completion/retirement obligations have finished.
/// Legacy typed terminal aliases are a union with operation ownership, with no duplicates.
pub async fn executions(
    conn: &mut sqlx::SqliteConnection,
    card: &str,
) -> Result<Vec<CardExecution>> {
    // Negative proof only: a scoped owner's intact record must not silently disappear
    // from the canonical owner join when metadata.owner is damaged. This never grants
    // ownership or authorizes stopping an execution through a reverse lookup.
    let mismatch_query = format!(
        "WITH scoped AS (
            SELECT id,tx_output_json FROM operations
            WHERE target_type = 'card' AND target_id = ?1
         ), record_refs AS (
            SELECT id AS owner,json_extract(tx_output_json,'$.data.execution_id') AS execution_id
            FROM scoped
            UNION ALL
            SELECT s.id,json_extract(s.tx_output_json,'$.data.' || k.value || '.execution_id')
            FROM scoped s CROSS JOIN json_each(?2) k
         )
         SELECT e.execution_id FROM record_refs r
         JOIN workspace_executions e ON e.execution_id=r.execution_id
         WHERE ({UNFINISHED_SQL})
           AND json_extract(e.intent_json,'$.owner') IS NOT r.owner
         LIMIT 1"
    );
    let mismatch: Option<String> = sqlx::query_scalar(&mismatch_query)
        .bind(card)
        .bind(serde_json::to_string(
            super::super::gate_execution::OWNER_STATE_KEYS,
        )?)
        .fetch_optional(&mut *conn)
        .await?;
    if let Some(id) = mismatch {
        return Err(CalmError::Conflict(format!(
            "card execution {id} has a damaged owner binding"
        )));
    }
    let query = format!(
        "SELECT e.execution_id,e.track_id,e.alias,e.intent_json,
                e.settled_at_ms IS NOT NULL AS settled,
                o.id AS operation_id,o.tx_output_json
         FROM workspace_executions e
         LEFT JOIN operations o ON json_extract(e.intent_json,'$.owner')=o.id
         WHERE ({UNFINISHED_SQL})
           AND ((o.target_type='card' AND o.target_id=?1)
                OR e.alias IN (SELECT {TERMINAL_ALIAS_SQL} FROM terminals WHERE card_id=?1))
         ORDER BY e.execution_id",
    );
    let rows = sqlx::query(&query).bind(card).fetch_all(conn).await?;
    let mut executions = Vec::new();
    for row in rows {
        let id: String = row.try_get("execution_id")?;
        let validate = || -> Result<ExecutionId> {
            // Required metadata facts; the owner's registered port checks its exact record schema.
            #[derive(serde::Deserialize)]
            struct Metadata {
                id: ExecutionId,
                consumer: String,
                owner: String,
                alias: String,
                track: String,
            }
            let metadata: Metadata = serde_json::from_str(row.try_get("intent_json")?)?;
            if metadata.id.as_str() != id
                || metadata.alias != row.try_get::<String, _>("alias")?
                || metadata.track != row.try_get::<String, _>("track_id")?
                || metadata.consumer.is_empty()
            {
                return Err(CalmError::Conflict("execution metadata mismatch"));
            }
            // A terminal alias can have a non-operation owner. An operation-owned row must
            // retain exactly one registered record naming this execution, even after settlement.
            if let Some(owner) = row.try_get::<Option<String>, _>("operation_id")? {
                let output: serde_json::Value = serde_json::from_str(
                    row.try_get::<Option<&str>, _>("tx_output_json")?
                        .ok_or_else(|| CalmError::Conflict("owner record missing"))?,
                )?;
                // Typed terminal preparation owns a direct checkpoint; Pipe/forge owners
                // use the registered nested records. These are distinct persisted contracts.
                let matches = usize::from(output["data"]["execution_id"] == id)
                    + super::super::gate_execution::OWNER_STATE_KEYS
                        .iter()
                        .filter(|key| output["data"][**key]["execution_id"] == id)
                        .count();
                if metadata.owner != owner || matches != 1 {
                    return Err(CalmError::Conflict("execution owner record mismatch"));
                }
            }
            Ok(metadata.id)
        };
        executions.push(CardExecution {
            id: validate().map_err(|error| {
                CalmError::Conflict(format!("card execution {id} is unreadable: {error}"))
            })?,
            settled: row.try_get("settled")?,
        });
    }
    Ok(executions)
}

#[cfg(test)]
mod tests;
