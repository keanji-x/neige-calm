//! Canonical-only semantic upgrade, before repository owners and recovery.
//! Immutable operation outputs remain evidence and use explicit read adaptation.
use crate::error::{CalmError, Result};
use calm_types::harness::snapshot::CanonicalHarnessSnapshot;

pub(super) async fn migrate(db: &super::SqliteDb) -> Result<()> {
    let mut tx = super::begin_immediate_tx(db).await?;
    // No V1 queue is loaded or adopted. Every retained canonical carrier state,
    // including superseded and failed, is converted in the same transaction.
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, handle_state_json FROM worker_sessions
         WHERE CASE WHEN json_valid(handle_state_json) THEN
            (json_extract(handle_state_json,'$.mode')=?1
             AND json_extract(handle_state_json,'$.schema_version')>=2)
            OR json_type(handle_state_json,'$.canonical_inputs') IS NOT NULL
         ELSE 0 END ORDER BY id",
    )
    .bind(calm_types::harness::HARNESS_MODE)
    .fetch_all(&mut **tx)
    .await?;
    for (id, raw) in rows {
        let value = serde_json::from_str(&raw)?;
        let frame = CanonicalHarnessSnapshot::from_released(value).map_err(|error| {
            CalmError::Internal(format!("canonical revision migration {id}: {error}"))
        })?;
        let encoded = serde_json::to_string(&frame)?;
        // V5 is validated on every open but not rewritten. JSON key order is not
        // a reason to replace a protected carrier or alter its snapshot fence.
        if serde_json::from_str::<serde_json::Value>(&raw)?["schema_version"]
            .as_u64()
            .is_some_and(|version| version < 5)
        {
            sqlx::query("UPDATE worker_sessions SET handle_state_json=?1 WHERE id=?2 AND handle_state_json=?3")
                .bind(encoded).bind(id).bind(raw).execute(&mut **tx).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}
