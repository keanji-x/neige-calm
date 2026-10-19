//! A persistent format fact, independent of the Card's current session pointer.
//! This query grants no execution and never reconstructs a current carrier.
use crate::error::Result;
use sqlx::SqliteConnection;

/// One canonical-format predicate for a caller-owned card alias `c`.
/// Bind the declared harness mode at ?2 and canonical input-key prefix at ?3.
/// Sharing it keeps batched metadata and single-card admission on the same fact.
pub(in crate::db::sqlite) const CANONICAL_STORAGE_PREDICATE: &str = r#"EXISTS(SELECT 1 FROM conversation_messages WHERE card_id=c.id)
         OR EXISTS(SELECT 1 FROM conversation_turns WHERE card_id=c.id)
         OR EXISTS(SELECT 1 FROM conversation_presentations WHERE card_id=c.id)
         OR EXISTS(SELECT 1 FROM planner_input_idempotency
             WHERE card_id=c.id AND payload_hash LIKE ?3)
         OR EXISTS(SELECT 1 FROM conversation_human_track_creations a
             JOIN track_create_idempotency b ON b.area_id=a.area_id AND b.idempotency_key=a.idempotency_key
             WHERE b.planner_card_id=c.id)
         OR EXISTS(SELECT 1 FROM worker_sessions WHERE card_id=c.id AND
             CASE WHEN json_valid(handle_state_json) THEN
                 json_extract(handle_state_json,'$.mode')=?2 AND
                 (json_extract(handle_state_json,'$.schema_version')>=2
                  OR json_type(handle_state_json,'$.canonical_inputs') IS NOT NULL)
             ELSE 0 END)"#;

/// Canonical history is monotone for a surviving conversation. A failed or
/// retired frame, withdrawn message, or permanent creation ACK remains evidence
/// even after the current runtime becomes unavailable. The current-carrier
/// loader separately requires an unambiguous actual binding.
pub async fn card_has_canonical_storage(
    connection: &mut SqliteConnection,
    card_id: &str,
) -> Result<bool> {
    let sql = format!("SELECT {CANONICAL_STORAGE_PREDICATE} FROM (SELECT ?1 AS id) c");
    let canonical: bool = sqlx::query_scalar(&sql)
        .bind(card_id)
        .bind(calm_types::harness::HARNESS_MODE)
        .bind(format!(
            "{}%",
            crate::db::sqlite::CANONICAL_PLANNER_INPUT_BINDING_PREFIX
        ))
        .fetch_one(connection)
        .await?;
    Ok(canonical)
}
