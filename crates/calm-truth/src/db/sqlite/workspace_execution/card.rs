//! Card ownership comes from the accepted operation target, never editable card payload.
use super::*;
use crate::error::{CalmError, Result};
use workspace::execution::ExecutionId;

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
    let rows = sqlx::query(
        "SELECT e.execution_id,e.track_id,e.alias,e.intent_json,
                e.settled_at_ms IS NOT NULL AS settled,
                o.id AS operation_id,o.tx_output_json
         FROM workspace_executions e
         LEFT JOIN operations o ON json_extract(e.intent_json,'$.owner')=o.id
         WHERE (e.settled_at_ms IS NULL OR e.desired<>'retire' OR e.secrets_json IS NOT NULL)
           AND ((o.target_type='card' AND o.target_id=?1)
                OR e.alias IN (SELECT 'term:' || id FROM terminals WHERE card_id=?1))
         ORDER BY e.execution_id",
    )
    .bind(card)
    .fetch_all(conn)
    .await?;
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
                return Err(CalmError::Conflict("execution metadata mismatch".into()));
            }
            // A terminal alias can have a non-operation owner. An operation-owned row must
            // retain exactly one registered record naming this execution, even after settlement.
            if let Some(owner) = row.try_get::<Option<String>, _>("operation_id")? {
                let output: serde_json::Value = serde_json::from_str(
                    row.try_get::<Option<&str>, _>("tx_output_json")?
                        .ok_or_else(|| CalmError::Conflict("owner record missing".into()))?,
                )?;
                let matches = super::super::gate_execution::OWNER_STATE_KEYS
                    .iter()
                    .filter(|key| output["data"][**key]["execution_id"] == id)
                    .count();
                if metadata.owner != owner || matches != 1 {
                    return Err(CalmError::Conflict("execution owner record mismatch".into()));
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
