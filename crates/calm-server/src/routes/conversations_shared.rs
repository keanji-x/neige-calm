//! Shared mechanics for lazily minting a Track assistant conversation on its
//! first message: validation, retryable operation keys, the first-message
//! digest that the track-create binding row stores, and the read that says
//! whether a card's *currently active* runtime has already been sent a user
//! message.

use sha2::{Digest, Sha256};

use calm_truth::session_projection_row::ACTIVE_CARD_RUNTIME_SELECT;

use crate::error::{CalmError, Result};
use crate::operation::Phase;
use crate::routes::cards::MAX_PLANNER_INPUT_CHARS;
use crate::state::{RouteState, WorkerState};

pub(crate) const PLANNER_HARNESS_START: &str = "planner-harness-start";

/// Ceiling on the `#N` operation-key suffix search of
/// [`retryable_operation_key`]. Reaching it means one `(scope, key)` pair
/// already failed 64 times; answering 409 "this key is used up, pick another"
/// beats looping.
pub(crate) const MAX_OPERATION_KEY_ATTEMPTS: u32 = 64;

/// Validate a conversation's first message *before* anything is minted, so a
/// rejected message leaves no card behind.
///
/// Byte-identical rules to `POST /api/cards/{id}/planner/input`, because that is
/// the handler the message is ultimately delivered through: a message this
/// function accepts must not be rejected two steps later, after the card,
/// session and thread already exist.
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

/// SHA-256 of the first message, verbatim — no trim, no normalisation.
///
/// Verbatim on purpose: this is the value that decides whether "same key,
/// different body" is a 409, so it has to change whenever the bytes the agent
/// would receive change. `prepare_tx` enqueues the payload's `first_message`
/// untrimmed, so hashing the untrimmed string is what actually mirrors what is
/// sent.
///
/// The one caller is `routes/tracks/create.rs`, and what it hashes *for* is
/// the `track_create_idempotency` **binding row** (#1452): that row is written
/// inside the mint transaction and outlives the operation row, but it stores
/// no message text, so this digest is the message's only representation there
/// and is not redundant with anything.
///
/// It is deliberately NOT a field of `PlannerHarnessStartOperationPayload`
/// any more. #1314 deleted that field: the payload already carries
/// `first_message` verbatim, so a digest beside it hashed bytes the payload
/// held anyway, both for `stable_payload_hash` and for the replay comparison
/// in `ensure_replay_message_matches`.
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
    // #1428 REVERSE ANCHOR — this walk reads *absence* as "nothing has happened
    // under this key yet", so it is only correct while keyed `operations` rows
    // are permanent. Reap a succeeded row and the base key reads absent, this
    // returns it, `select_arm` calls the request a `GenuineRetry`, and the
    // first message is delivered a second time
    // (`docs/design-1428-idempotency-retention.md` §3.1). Migration 0093 is the
    // fence; this is the reader that depends on it.
    for attempt in 1..=MAX_OPERATION_KEY_ATTEMPTS {
        let key = if attempt == 1 {
            base.to_string()
        } else {
            format!("{base}#{attempt}")
        };
        let existing = s
            .operation_runtime
            .find_by_kind_and_idempotency(PLANNER_HARNESS_START, &key)
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

/// Has a user message been enqueued **onto this card's current ACTIVE
/// runtime**?
///
/// Not "has this card ever had one". The question is deliberately bound to the
/// runtime that is live *right now*, because "ever" is the wrong question for
/// every caller: a message enqueued onto a runtime that has since been replaced
/// is not on any queue a live harness will drain, so suppressing a re-send on
/// the strength of it strands the message forever (#1314; the measured shape is
/// in the ACTIVE paragraph below).
///
/// **One statement, not two.** "Which runtime is active" and "does that runtime
/// have evidence" are decided by a single SELECT over a single snapshot, because
/// as two reads they race: see the body comment for the interleaving that
/// strands the instruction permanently.
///
/// **What "ACTIVE" means is not restated here.** The runtime is picked by
/// `ACTIVE_CARD_RUNTIME_SELECT`, embedded as a subquery — the same statement
/// `Repo::session_projection_active_for_card` is built from, which owns the state
/// filter (`starting | running | idle | turn_pending`) and the newest-first
/// ordering. That is the pool-side twin of
/// `session_projection_active_for_card_tx` — the read
/// `session_supersede_and_start_tx` and every dormancy check go through — and the
/// two are pinned equal for every runtime kind by
/// `runtime_get_active_for_card_from_pool_matches_runtimes_backed_for_all_kinds`
/// (`calm-truth/src/db/sqlite/runtime_read_flip_parity_tests.rs`). So "this
/// predicate's notion of active" and "the runtime a send would actually reach"
/// cannot drift apart. A third copy of that state list, written out here, is
/// exactly how they would.
///
/// **No active runtime ⇒ `false`.** There is nothing a queued message could be
/// waiting on, so the next send is a re-send onto whatever runtime the caller's
/// send path restarts. That is the self-healing direction and it is the point of
/// the whole predicate.
///
/// The evidence row is `harness.user_message.enqueued`, the same row the
/// transcript, the tests and the audit log read. It has two writers, and both
/// stamp the runtime the message was enqueued onto:
///
/// * `send_planner_input` writes it *after* the observation reached a live
///   harness queue, carrying that harness's `runtime.id`, so the row trails a
///   delivery that already happened;
/// * `PlannerHarnessStartAdapter::prepare_tx` writes it *inside* the mint
///   transaction that seeds the observation onto a session that has not started
///   yet (#1299 S1, #1314), carrying that session's id, so the row commits
///   before anything could have been handed over — and survives a later
///   compensation that deletes the card, because `events` is append-only.
///
/// **Why the event row and not the queue itself.** The queue is the state a
/// caller actually cares about, and it is unusable as a predicate: the delivery
/// path hands the text to the harness over an mpsc channel and the run loop
/// persists it asynchronously, so a request cannot see its *own* just-sent
/// message in `pending_queue` — two concurrent triggers would both read "empty"
/// and both send. The event row is the only trace of a send that lands in the
/// database synchronously with the send, which is what makes a read-then-send
/// under a per-card lock actually exclusive.
///
/// **The residual, stated rather than papered over.** A runtime replacement that
/// *inherits* the old runtime's still-undelivered queue (`/planner/reset`, a
/// manual harness start against a live session) moves the message forward while
/// leaving its row pointing at the superseded runtime, so this answers `false`
/// and the caller re-sends a message that was still reachable. The cost is a
/// duplicated standing instruction on a path a human explicitly asked for — one
/// per such replacement, and nothing here caps how many times that can be
/// repeated before the inherited queue drains; the
/// alternative — the dormant restart *not* inheriting, which is the common case
/// — silently loses the message instead. A "still queued" conjunct would not fix
/// it either: see the synchronous-visibility paragraph above.
///
/// A caller that needs "did a delivery *reach* the agent?" must not use this:
/// see `routes/track_conversations.rs`, which deliberately reads nothing back,
/// and #1384. Queued is not delivered — the run loop can still drop an
/// observation on a full queue.
///
/// There is deliberately no separate "first message sent" flag: a write-only
/// marker would have to be set before or after the send and would be wrong in
/// one direction either way (double send, or a silently swallowed message).
///
/// Both scope columns are bound: `scope_track` is indexed (`0007`), so the scan
/// is bounded by one track rather than by every conversation in the DB.
/// The `json_extract` that pulls the enqueued-onto runtime out of `e.payload`
/// is **CASE-gated** on `json_valid`, not merely conjoined with it. SQLite *raises* on `json_extract` over malformed JSON, and
/// one historical bad row would turn this read into a 500 on every trigger;
/// `events_prune.rs` measured that a bare `AND json_valid(payload)` conjunct is
/// not a defence, because SQLite does not guarantee AND-term evaluation order —
/// `CASE` evaluation is the one that is guaranteed lazy. An unparseable row
/// therefore yields `NULL`, matches nothing, and reads as "no evidence": the
/// re-send direction, which is the safe one here.
///
/// Durability premise, and it is a premise not a nice-to-have:
/// `harness.user_message.enqueued` is **not** in `EVENTS_PRUNE_KINDS`
/// (`calm-truth/src/events_prune.rs`). That allowlist is exact-kind and
/// fails safe — a kind absent from it is permanent by construction — so the row
/// outlives every retention pass. What that buys is narrower than it was before
/// the ACTIVE binding, and it is still load-bearing: an evicted row would make a
/// trigger against a *live* runtime that has already been spoken to re-send the
/// standing bootstrap instruction. `first_message_dedup_kind_is_never_prunable`
/// (in `events_prune.rs`) fails closed if anyone tries to add the kind. If that
/// ever has to change, this read must move to a marker that cannot be pruned.
pub(crate) async fn user_message_enqueued_on_active_runtime(
    w: &WorkerState,
    track_id: &str,
    card_id: &str,
) -> Result<bool> {
    let pool = w
        .repo
        .sqlite_pool()
        .ok_or_else(|| CalmError::Internal("conversations require a sqlite-backed repo".into()))?;
    // ONE AUTOCOMMIT statement, and it has to be one.
    //
    // Two statements — pick the active runtime, then look for its evidence —
    // leave a window that a *different* endpoint walks through: `/planner/reset`
    // takes the recovery lock, not this caller's per-card lock, so it can
    // supersede R1 and start R2 between the two reads. The first read then
    // reports R1, the second finds R1's own (now unreachable) evidence row and
    // answers `true`, the bootstrap is skipped, and the unconditional summary
    // that follows writes R2's *own* enqueued row — so the next trigger skips it
    // again. The standing instruction never arrives, which is the exact failure
    // class this predicate exists to close.
    //
    // Nor is an explicit transaction the fix. #930/#1016: a deferred
    // `pool.begin()` holds every table lock it has taken (R locks included)
    // until commit, so a multi-table read like this one is the lock-holding
    // waiter that closes a deadlock cycle against a concurrent IMMEDIATE writer;
    // `deferred_write_tx_invariant` fails closed on it and the allowlist is empty
    // on purpose. That rule is about *multi-statement* read transactions. A
    // single statement is atomic in SQLite by construction — it reads one
    // snapshot — so it needs neither the transaction nor an exemption.
    //
    // The runtime choice is not restated here: `ACTIVE_CARD_RUNTIME_SELECT`
    // is the same statement `Repo::session_projection_active_for_card` is built
    // from (`calm-truth/src/session_projection_row.rs`), embedded as a subquery.
    // It owns the state list (`starting | running | idle | turn_pending`) and the
    // newest-first tie-break, and it is the pool-side twin pinned equal to
    // `session_projection_active_for_card_tx` — the read
    // `session_supersede_and_start_tx` and every dormancy check go through — by
    // `runtime_get_active_for_card_from_pool_matches_runtimes_backed_for_all_kinds`
    // (`calm-truth/src/db/sqlite/runtime_read_flip_parity_tests.rs`). So "this
    // predicate's notion of active" and "the runtime a send would actually
    // reach" cannot drift apart.
    //
    // No active runtime ⇒ the subquery yields no row ⇒ the comparison is NULL ⇒
    // nothing matches ⇒ `false`, the re-send direction, as documented above.
    let found: Option<i64> = sqlx::query_scalar(&user_message_enqueued_on_active_runtime_sql())
        .bind(card_id)
        .bind(track_id)
        .fetch_optional(&pool)
        .await?;
    Ok(found.is_some())
}

/// The statement [`user_message_enqueued_on_active_runtime`] runs: `?1` is the
/// card id (bound twice — the outer scope filter and the embedded active-runtime
/// subquery share it), `?2` the track id. A row means "already enqueued onto the
/// runtime that is active *at this statement's snapshot*"; no row means `false`.
///
/// It is a function rather than an inline literal so that a test can execute the
/// **production** text against a real database instead of restating it. A test
/// that re-typed this SQL would pass on its own copy and prove nothing about
/// this one.
pub fn user_message_enqueued_on_active_runtime_sql() -> String {
    format!(
        r#"SELECT 1
             FROM events e
            WHERE e.kind = 'harness.user_message.enqueued'
              AND e.scope_card = ?1
              AND e.scope_track = ?2
              AND (CASE WHEN json_valid(e.payload)
                        THEN json_extract(e.payload, '$.runtime_id') END)
                  = ({ACTIVE_CARD_RUNTIME_SELECT})
            LIMIT 1"#,
    )
}
