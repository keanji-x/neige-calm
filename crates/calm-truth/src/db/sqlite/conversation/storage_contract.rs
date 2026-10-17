//! A persistent format fact, independent of the Card's current session pointer.
//! This query grants no execution and never reconstructs a current carrier.
use crate::error::Result;
use sqlx::SqliteConnection;

/// Canonical history is monotone for a surviving conversation. A failed or
/// retired frame, withdrawn message, or permanent creation ACK remains evidence
/// even after the current runtime becomes unavailable. The current-carrier
/// loader separately requires an unambiguous actual binding.
pub async fn card_has_canonical_storage(
    connection: &mut SqliteConnection,
    card_id: &str,
) -> Result<bool> {
    let canonical: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM conversation_messages WHERE card_id=?1)
         OR EXISTS(SELECT 1 FROM conversation_turns WHERE card_id=?1)
         OR EXISTS(SELECT 1 FROM conversation_presentations WHERE card_id=?1)
         OR EXISTS(SELECT 1 FROM conversation_human_track_creations a
             JOIN track_create_idempotency b ON b.area_id=a.area_id AND b.idempotency_key=a.idempotency_key
             WHERE b.planner_card_id=?1)
         OR EXISTS(SELECT 1 FROM worker_sessions WHERE card_id=?1 AND
             CASE WHEN json_valid(handle_state_json) THEN
                 json_extract(handle_state_json,'$.mode')=?2 AND
                 (json_extract(handle_state_json,'$.schema_version')>=2
                  OR json_type(handle_state_json,'$.canonical_inputs') IS NOT NULL)
             ELSE 0 END)",
    )
    .bind(card_id)
    .bind(calm_types::harness::HARNESS_MODE)
    .fetch_one(connection)
    .await?;
    Ok(canonical)
}
