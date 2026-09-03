//! The parts of "mint a conversation on its first message" that are identical
//! for an area chat (`area_conversations`) and a track assistant
//! (`track_conversations`).
//!
//! Shared rather than copied because these are the four-arm retry contract's
//! moving parts. Two divergent copies of `retryable_operation_key` would mean
//! two different answers to "what does the same `Idempotency-Key` mean after a
//! failure", which is exactly the sort of drift the documented contract exists
//! to prevent.
//!
//! What is deliberately NOT here: the derived ids (`crate::conversation_keys`,
//! one namespace per flavour) and the list predicates (an area chat is a
//! `worker`/`plain_chat` card, a track assistant is an `assistant` card, and
//! collapsing those would be the bug G3 is about).

use sha2::{Digest, Sha256};

use crate::error::{CalmError, Result};
use crate::operation::Phase;
use crate::routes::cards::MAX_SPEC_INPUT_CHARS;
use crate::state::{RouteState, WorkerState};

pub(crate) const SPEC_HARNESS_START: &str = "spec-harness-start";

/// Ceiling on the `#N` operation-key suffix search of
/// [`retryable_operation_key`]. Reaching it means one `(scope, key)` pair
/// already failed 64 times; answering 409 "this key is used up, pick another"
/// beats looping.
pub(crate) const MAX_OPERATION_KEY_ATTEMPTS: u32 = 64;

/// Validate a conversation's first message *before* anything is minted, so a
/// rejected message leaves no card behind.
///
/// Byte-identical rules to `POST /api/cards/{id}/spec/input`, because that is
/// the handler the message is ultimately delivered through: a message this
/// function accepts must not be rejected two steps later, after the card,
/// session and thread already exist.
pub(crate) fn validate_first_message(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Err(CalmError::BadRequest("text must not be empty".into()));
    }
    if text.chars().count() > MAX_SPEC_INPUT_CHARS {
        return Err(CalmError::BadRequest(format!(
            "text must be at most {MAX_SPEC_INPUT_CHARS} characters",
        )));
    }
    Ok(())
}

/// SHA-256 of the first message, verbatim — no trim, no normalisation.
///
/// Verbatim on purpose: this is the value that decides whether "same key,
/// different body" is a 409, so it has to change whenever the bytes the agent
/// would receive change. `send_spec_input` also forwards the text untrimmed,
/// so hashing the untrimmed string is what actually mirrors what is sent.
pub(crate) fn first_message_digest(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

/// Pick the operation key to submit under.
///
/// The **card id** is untouched by this: it stays a pure function of
/// `(scope, Idempotency-Key)`, which is the wall INV-CHAT-013(b) leans on — no
/// suffix can ever produce a second card, because every attempt aims at the
/// same id and `validate` refuses to re-mint an existing card.
///
/// The *operation* key is a different question. A terminally `Failed` op was
/// fully compensated (the card is gone), so replaying its recorded failure
/// forever would make the user's key a dead end — and the key is bound to the
/// draft the user keeps pressing send on. So a `Failed` predecessor is stepped
/// over with a `#N` suffix and the retry genuinely retries.
///
/// `Stuck` is deliberately NOT stepped over, and this is fail-closed on
/// purpose, not an oversight: `Stuck` means compensation did **not** finish
/// (`driver.rs` marks it on any compensation-step error and never re-drives
/// it), so the derived card may still exist. Stepping over it would hand the
/// user a 409 "card already exists" from `validate` instead — a worse answer,
/// and one that hides an operation an operator has to look at. What the user
/// sees under this key is the recorded `500 operation stuck, see DB`, until
/// someone clears the row.
pub(crate) async fn retryable_operation_key(s: &RouteState, base: &str) -> Result<String> {
    for attempt in 1..=MAX_OPERATION_KEY_ATTEMPTS {
        let key = if attempt == 1 {
            base.to_string()
        } else {
            format!("{base}#{attempt}")
        };
        let existing = s
            .operation_runtime
            .find_by_kind_and_idempotency(SPEC_HARNESS_START, &key)
            .await?;
        match existing {
            None => return Ok(key),
            Some(op) if op.phase != Phase::Failed => return Ok(key),
            Some(_) => continue,
        }
    }
    // 409, not 500: nothing is broken server-side. This key has simply been
    // used up by that many terminally failed attempts, and the actionable
    // answer is "use a different Idempotency-Key", which a generic `internal`
    // never conveys.
    // Its own code, not the generic `conflict`: these routes' other 409s mean
    // "already exists, ignorable" or "your body disagrees with the key, fix
    // the request", and a client cannot tell them apart from a status alone.
    // `idempotency_key_exhausted` says the one thing that is actionable here —
    // mint a new key.
    Err(CalmError::IdempotencyKeyExhausted(format!(
        "this Idempotency-Key exhausted its {MAX_OPERATION_KEY_ATTEMPTS} retry slots ({MAX_OPERATION_KEY_ATTEMPTS} failed attempts); retry under a new Idempotency-Key",
    )))
}

/// Has this conversation card ever had a user message enqueued?
///
/// The truth read here is the same row the transcript, the tests and the audit
/// log read — `harness.user_message.enqueued`, written by `send_spec_input`
/// *after* the observation reached the harness queue. There is deliberately no
/// separate "first message sent" flag: a write-only marker would have to be
/// set before or after the send and would be wrong in one direction either way
/// (double send, or a silently swallowed message).
///
/// Both scope columns are bound: `scope_track` is indexed (`0007`), so the scan
/// is bounded by one track rather than by every conversation in the DB.
///
/// Durability premise, and it is a premise not a nice-to-have:
/// `harness.user_message.enqueued` is **not** in `EVENTS_PRUNE_KINDS`
/// (`calm-truth/src/events_prune.rs`). That allowlist is exact-kind and
/// fails safe — a kind absent from it is permanent by construction — so this
/// row outlives every retention pass and the dedup answer never decays.
/// Adding this kind to the allowlist would silently re-open first-message
/// double-send after the horizon; `first_message_dedup_kind_is_never_prunable`
/// (in `events_prune.rs`) fails closed if anyone tries. If that ever has to
/// change, this read must move to a marker that cannot be pruned.
pub(crate) async fn user_message_already_enqueued(
    w: &WorkerState,
    track_id: &str,
    card_id: &str,
) -> Result<bool> {
    let pool = w
        .repo
        .sqlite_pool()
        .ok_or_else(|| CalmError::Internal("conversations require a sqlite-backed repo".into()))?;
    let found: Option<i64> = sqlx::query_scalar(
        r#"SELECT 1
             FROM events
            WHERE kind = 'harness.user_message.enqueued'
              AND scope_track = ?1
              AND scope_card = ?2
            LIMIT 1"#,
    )
    .bind(track_id)
    .bind(card_id)
    .fetch_optional(&pool)
    .await?;
    Ok(found.is_some())
}
