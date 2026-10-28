//! Indexed terminal reads, without transcript pagination or arbitrary history cutoffs.
//! The owner-assigned row id orders durable outcomes; UTC samples are facts, not causality.
use crate::db::terminal_outcome::HarnessTerminalOutcome;
use crate::error::Result;
use futures::TryStreamExt;
use sqlx::SqlitePool;

const SELECT: &str = "SELECT id, worker_session_id, card_id, track_id, thread_id, turn_id, params, created_at_ms \
     FROM harness_items";

pub async fn latest(
    pool: &SqlitePool,
    card: &str,
    track: &str,
) -> Result<Option<HarnessTerminalOutcome>> {
    Ok(sqlx::query_as(&format!(
        "{SELECT} WHERE card_id=?1 AND method='turn/completed' AND track_id=?2 \
         AND turn_id IS NOT NULL ORDER BY id DESC LIMIT 1"
    ))
    .bind(card)
    .bind(track)
    .fetch_optional(pool)
    .await?)
}

/// Every confirmed timeout stays visible until a later successful completion.
/// Stream all candidates after that success; malformed evidence cannot hide an older valid row.
pub async fn pending_timeouts(
    pool: &SqlitePool,
    card: &str,
    track: &str,
) -> Result<Vec<HarnessTerminalOutcome>> {
    let mut rows = sqlx::query_as::<_, HarnessTerminalOutcome>(
        "SELECT h.id, h.worker_session_id, h.card_id, h.track_id, h.thread_id, h.turn_id, h.params, h.created_at_ms \
         FROM harness_items h WHERE h.card_id=?1 AND h.method='turn/completed' AND h.track_id=?2 \
         AND h.turn_id IS NOT NULL AND json_extract(h.params, '$.status')='interrupted' \
         AND json_type(h.params, '$.harness_interruption_reason')='object' \
         AND NOT EXISTS (SELECT 1 FROM harness_items s WHERE s.card_id=?1 AND s.method='turn/completed' \
             AND s.track_id=?2 AND json_extract(s.params,'$.status')='completed' \
             AND s.id>h.id) \
         ORDER BY h.id DESC"
    ).bind(card).bind(track).fetch(pool);
    let mut pending = Vec::new();
    while let Some(row) = rows.try_next().await? {
        if row.timeout().is_some() {
            pending.push(row);
        }
    }
    Ok(pending)
}

/// Read the full execution key under the caller's existing current-frame observation.
pub(super) async fn exact(
    connection: &mut sqlx::SqliteConnection,
    worker: &str,
    card: &str,
    track: &str,
    thread: &str,
    turn: &str,
) -> Result<Option<HarnessTerminalOutcome>> {
    Ok(sqlx::query_as(&format!(
        "{SELECT} WHERE worker_session_id=?1 AND card_id=?2 AND track_id=?3
         AND thread_id=?4 AND turn_id=?5 AND method='turn/completed' ORDER BY id DESC LIMIT 1",
    ))
    .bind(worker)
    .bind(card)
    .bind(track)
    .bind(thread)
    .bind(turn)
    .fetch_optional(connection)
    .await?)
}
