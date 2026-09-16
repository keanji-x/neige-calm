//! Explicit human recovery of a current, uncompleted conversation after a
//! provider system error. This exception does not widen worker transitions.
use serde_json::Value;
use sqlx::{Sqlite, Transaction};

use crate::error::Result;
use crate::model::now_ms;

/// Validate the carrier AND its complete snapshot under the caller's write
/// transaction. A queue harvest, replacement, or completed execution cannot be
/// revived even if an earlier read made it look recoverable.
pub async fn session_system_error_recovery_matches_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    runtime_id: &str,
    thread_id: &str,
    snapshot: &Value,
) -> Result<bool> {
    let raw: Option<String> = sqlx::query_scalar(
        r#"SELECT ws.handle_state_json FROM worker_sessions ws
           JOIN cards c ON c.session_id = ws.id AND c.id = ws.card_id
           JOIN tracks t ON t.id = c.track_id AND t.id = ws.track_id
           WHERE c.id = ?1 AND ws.id = ?2 AND ws.provider = 'codex'
             AND ws.state = 'failed' AND ws.completed_at_ms IS NULL
             AND ws.queue_harvested_at_ms IS NULL AND ws.terminal_run_id IS NULL
             AND ws.active_turn_id IS NULL
             AND t.lifecycle NOT IN ('done', 'canceled', 'failed')
             AND c.kind = 'codex'
             AND ((c.role = 'planner' AND ws.contract = 'planner' AND COALESCE(t.purpose, '') != 'area-chat')
               OR (c.role = 'assistant' AND ws.contract = 'executor' AND json_extract(c.payload, '$.harness_profile') = 'assistant')
               OR (c.role = 'worker' AND ws.contract = 'executor' AND json_extract(c.payload, '$.harness_profile') = 'plain_chat'))
             AND json_extract(ws.handle_state_json, '$.mode') = 'harness'
             AND json_extract(ws.handle_state_json, '$.schema_version') = 1
             AND json_extract(ws.handle_state_json, '$.phase') = 'wedged'
             AND json_extract(ws.handle_state_json, '$.wedged_reason') = 'system_error'
             AND COALESCE(NULLIF(trim(ws.thread_id), ''), json_extract(ws.handle_state_json, '$.last_thread_id')) = ?3
             AND (json_extract(ws.handle_state_json, '$.last_thread_id') IS NULL
                  OR json_extract(ws.handle_state_json, '$.last_thread_id') = ?3)"#,
    ).bind(card_id).bind(runtime_id).bind(thread_id).fetch_optional(&mut **tx).await?;
    Ok(raw
        .map(|raw| serde_json::from_str::<Value>(&raw))
        .transpose()?
        .as_ref()
        == Some(snapshot))
}

/// Restore the existing carrier without reserializing its queue, clearing its
/// transcript, changing its thread, or reviving any other terminal state.
pub async fn session_resume_system_error_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    runtime_id: &str,
    thread_id: &str,
    snapshot: &Value,
) -> Result<bool> {
    if !session_system_error_recovery_matches_tx(tx, card_id, runtime_id, thread_id, snapshot)
        .await?
    {
        return Ok(false);
    }
    let result = sqlx::query(
        r#"UPDATE worker_sessions SET state = 'idle', thread_id = ?2,
             handle_state_json = json_set(handle_state_json, '$.phase', 'idle', '$.wedged_reason', NULL),
             updated_at_ms = ?3 WHERE id = ?1 AND state = 'failed'"#,
    ).bind(runtime_id).bind(thread_id).bind(now_ms()).execute(&mut **tx).await?;
    Ok(result.rows_affected() == 1)
}

/// Settle the pending input of a failed, current harness before recovery.
/// A retired or harvested carrier must never acquire another copy of its queue.
pub async fn session_set_failed_harness_snapshot_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    runtime_id: &str,
    snapshot: &Value,
) -> Result<bool> {
    let result = sqlx::query(
        r#"UPDATE worker_sessions SET handle_state_json=?3, updated_at_ms=?4
        WHERE id=?2 AND card_id=?1 AND state='failed' AND completed_at_ms IS NULL
          AND queue_harvested_at_ms IS NULL
          AND EXISTS(SELECT 1 FROM cards WHERE id=?1 AND session_id=?2)
          AND json_extract(handle_state_json,'$.mode')='harness'
          AND json_extract(handle_state_json,'$.phase')='wedged'
          AND json_extract(handle_state_json,'$.wedged_reason')='system_error'"#,
    )
    .bind(card_id)
    .bind(runtime_id)
    .bind(serde_json::to_string(snapshot)?)
    .bind(now_ms())
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected() == 1)
}
