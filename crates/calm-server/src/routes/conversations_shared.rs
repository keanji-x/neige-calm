//! Shared mechanics for lazily minting a Track assistant conversation on its
//! first message.

use calm_truth::session_projection_row::ACTIVE_CARD_RUNTIME_SELECT;

use crate::error::{CalmError, Result};
use crate::operation::{Operation, Phase};
use crate::routes::planner_cards::MAX_PLANNER_INPUT_CHARS;
use crate::state::{RouteState, WorkerState};

pub(crate) const PLANNER_HARNESS_START: &str = "planner-harness-start";

/// Ceiling on the `#N` operation-key suffix search; reaching it answers 409.
pub(crate) const MAX_OPERATION_KEY_ATTEMPTS: u32 = 64;

/// Byte-identical rules to `POST /api/cards/{id}/planner/input`: a message accepted
/// here must not be rejected when it is delivered through that handler.
pub(crate) fn validate_first_message(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Err(CalmError::BadRequest("text must not be empty".into()));
    }
    if text.chars().count() > MAX_PLANNER_INPUT_CHARS {
        return Err(CalmError::BadRequest(format!(
            "text must be at most {MAX_PLANNER_INPUT_CHARS} characters",
        )));
    }
    Ok(())
}

/// The existing bounded start-retry namespace, shared with pure acceptance's
/// refusal to adopt any historical start attempt. No suffix is stripped.
pub(crate) fn operation_key_attempts(base: &str) -> impl Iterator<Item = String> + '_ {
    (1..=MAX_OPERATION_KEY_ATTEMPTS).map(move |attempt| {
        if attempt == 1 {
            base.to_string()
        } else {
            format!("{base}#{attempt}")
        }
    })
}

/// A terminally `Failed` predecessor is stepped over with a `#N` suffix; `Stuck` is
/// not, because its compensation may not have finished and the card may still exist.
pub(crate) async fn retryable_operation_key(s: &RouteState, base: &str) -> Result<String> {
    Ok(retryable_operation(s, base).await?.0)
}

/// The same authoritative key walk, retaining its read for receipt lookup.
pub(crate) async fn retryable_operation(
    s: &RouteState,
    base: &str,
) -> Result<(String, Option<Operation>)> {
    // Reads absence as "nothing has happened under this key yet", so this is only
    // correct while keyed `operations` rows are permanent.
    for key in operation_key_attempts(base) {
        let existing = lookup_start_operation(s, &key).await?;
        match existing {
            None => return Ok((key, None)),
            Some(op) if op.phase != Phase::Failed => return Ok((key, Some(op))),
            Some(_) => continue,
        }
    }
    Err(CalmError::IdempotencyKeyExhausted(format!(
        "this Idempotency-Key exhausted its {MAX_OPERATION_KEY_ATTEMPTS} retry slots ({MAX_OPERATION_KEY_ATTEMPTS} failed attempts); retry under a new Idempotency-Key",
    )))
}

/// Factual lookup in the permanent start journal; this never submits or wakes.
pub(crate) async fn lookup_start_operation(s: &RouteState, key: &str) -> Result<Option<Operation>> {
    s.operation_runtime
        .find_by_kind_and_idempotency(PLANNER_HARNESS_START, key)
        .await
}

/// Has a user message been enqueued onto this card's currently ACTIVE runtime?
/// No active runtime ⇒ `false` (the re-send direction). Relies on
/// `harness.user_message.enqueued` never being prunable.
pub(crate) async fn user_message_enqueued_on_active_runtime(
    w: &WorkerState,
    track_id: &str,
    card_id: &str,
) -> Result<bool> {
    let pool = w
        .repo
        .sqlite_db()
        .ok_or_else(|| CalmError::Internal("conversations require a sqlite-backed repo".into()))?;
    // One autocommit statement: as two reads, `/planner/reset` can supersede the
    // runtime between them and the bootstrap is skipped forever. The `json_extract`
    // is CASE-gated because SQLite raises on malformed JSON and does not guarantee
    // AND-term evaluation order.
    let found: Option<i64> = sqlx::query_scalar(&user_message_enqueued_on_active_runtime_sql())
        .bind(card_id)
        .bind(track_id)
        .fetch_optional(pool.read_pool())
        .await?;
    Ok(found.is_some())
}

/// `?1` is the card id (bound twice: outer filter and the active-runtime subquery),
/// `?2` the track id. A function so tests can run the production text.
pub fn user_message_enqueued_on_active_runtime_sql() -> String {
    format!(
        r#"SELECT 1
             FROM events e
            WHERE e.kind = 'harness.user_message.enqueued'
              AND e.scope_card = ?1
              AND e.scope_track = ?2
              AND (CASE WHEN json_valid(e.payload)
                        THEN json_extract(e.payload, '$.worker_session_id') END)
                  = ({ACTIVE_CARD_RUNTIME_SELECT})
            LIMIT 1"#,
    )
}
