//! `/api/coves/{cove_id}/conversations` — the cove home page's chat list and
//! its "mint on first message" creation endpoint (#1098 slice 3).
//!
//! A conversation is a headless codex card carrying the persisted
//! `harness_profile: "plain_chat"` marker, parked on the cove's single hidden
//! chat wave. Pressing `+` in the UI creates nothing at all; the card, its
//! session and its codex thread are all minted by the first message, which is
//! what this module's POST does in one operation.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::actor::Actor;
use crate::conversation_keys::derive_cove_conversation_keys;
use crate::error::{CalmError, ErrorBody, Result};
use crate::model::{CoveConversationSummary, Wave};
use crate::operation::spec_harness_start_adapter::{
    HarnessProfile, LazyMintCardSeed, SpecHarnessStartOperationPayload,
};
use crate::operation::{OperationKey, OperationOutcome};
use crate::per_card_lock::lock_card;
use crate::routes::cards::{SendSpecInputRequest, send_spec_input};
use crate::routes::conversations_shared::{
    SPEC_HARNESS_START, first_message_digest, retryable_operation_key,
    user_message_already_enqueued, validate_first_message,
};
use crate::routes::terminal_cards::{
    calm_error_from_operation_failure, parse_idempotency_key_header, stable_payload_hash,
};
use crate::routes::waves::ensure_cove_chat_wave_inner;
use crate::session_projection_repo::WorkerSessionState;
use crate::state::{AppState, CodexShellState, RouteState, WorkerState};

/// The `kind` every row of this list carries. Derived from the card's
/// persisted marker, never from the session kind: the session is an ordinary
/// `codex-card` session and says nothing about the conversation being a shared
/// cove chat.
const COVE_CONVERSATION_KIND: &str = "shared-chat";

/// Test-only rendezvous inside `create_cove_conversation`, placed exactly at
/// the window F1 is about: after the mint operation settled, before the
/// first-message claim is taken. Mirrors
/// `waves::install_chat_wave_ensure_barrier_for_test`.
#[cfg(feature = "fixtures")]
fn first_message_barriers() -> &'static std::sync::Mutex<
    std::collections::HashMap<String, std::sync::Arc<tokio::sync::Barrier>>,
> {
    static BARRIERS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::Barrier>>>,
    > = std::sync::OnceLock::new();
    BARRIERS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn install_first_message_barrier_for_test(
    cove_id: &str,
    barrier: std::sync::Arc<tokio::sync::Barrier>,
) {
    first_message_barriers()
        .lock()
        .expect("first-message barrier lock poisoned")
        .insert(cove_id.to_string(), barrier);
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn remove_first_message_barrier_for_test(cove_id: &str) {
    first_message_barriers()
        .lock()
        .expect("first-message barrier lock poisoned")
        .remove(cove_id);
}

async fn wait_at_first_message_barrier(cove_id: &str) {
    #[cfg(feature = "fixtures")]
    {
        let barrier = first_message_barriers()
            .lock()
            .expect("first-message barrier lock poisoned")
            .get(cove_id)
            .cloned();
        if let Some(barrier) = barrier {
            barrier.wait().await;
        }
    }
    #[cfg(not(feature = "fixtures"))]
    {
        let _ = cove_id;
    }
}

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/coves/{cove_id}/conversations",
        get(list_cove_conversations).post(create_cove_conversation),
    )
}

/// Body of `POST /api/coves/{cove_id}/conversations`: the first message.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct NewCoveConversationBody {
    /// The first message. Validated exactly like `POST /api/cards/{id}/spec/input`
    /// (non-blank after trim, at most 32768 chars) and validated *before*
    /// anything is minted, so a rejected message leaves no card behind.
    pub text: String,
}

#[utoipa::path(
    get,
    path = "/api/coves/{cove_id}/conversations",
    tag = "coves",
    params(("cove_id" = String, Path, description = "Cove id")),
    responses(
        (status = 200, description = "Conversations on this cove's chat wave, newest activity first. Empty when the cove has no chat wave yet — this endpoint never creates one.", body = Vec<CoveConversationSummary>),
        (status = 404, description = "Cove not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_cove_conversations(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    Path(cove_id): Path<String>,
) -> Result<Json<Vec<CoveConversationSummary>>> {
    if s.repo.cove_get(&cove_id).await?.is_none() {
        return Err(CalmError::NotFound(format!("cove {cove_id}")));
    }
    let Some(wave) = cove_chat_wave(&s, &cove_id).await? else {
        // No chat wave yet means no conversations. Ensuring one here would
        // make a read create a wave (and, without a claimed folder, turn a
        // list into a 409).
        return Ok(Json(Vec::new()));
    };
    let rows = load_conversation_summaries(&w, wave.id.as_str(), None).await?;
    Ok(Json(rows))
}

#[utoipa::path(
    post,
    path = "/api/coves/{cove_id}/conversations",
    tag = "coves",
    params(
        ("cove_id" = String, Path, description = "Cove id"),
        ("Idempotency-Key" = String, Header, description = "**Required.** Scopes the derived card id and the operation dedup key, so retrying the same request can never mint a second conversation. A missing or blank header is 400: INV-CHAT-013(b) has to hold unconditionally, not only for callers who remember to opt in.\n\n**This is NOT standard HTTP idempotency — it is \"same key = the same retryable draft\".** Precisely: (a) same key after a **success** replays the same conversation and does not re-send the first message; (b) same key after a **terminally failed** attempt genuinely RETRIES (the failed attempt was fully compensated, so replaying its failure would make the key a dead end) and may therefore return a different result, e.g. 201 where the first call gave 500 — a standard idempotency key would replay the failure instead; (c) same key after a **stuck** attempt keeps returning 500 on purpose (fail-closed: compensation did not finish, so the card may still exist); (d) after 64 failed attempts under one key the key is exhausted and answers 409 `idempotency_key_exhausted` forever — use a new key; (e) same key with a **different `text`** is 409 `conflict`, because the message body is bound into the operation payload as a SHA-256 — except after arm (b), where the retry runs under a fresh `#N` operation key that no earlier payload hash is bound to, so an edited draft is no longer rejected *for the old hash* — the attempt genuinely re-executes and its final status is whatever that execution produces (201 on success). The derived card id never carries the retry suffix, so none of this can mint a second conversation."),
    ),
    request_body = NewCoveConversationBody,
    responses(
        (status = 201, description = "Conversation card minted, harness started, first message sent. Also returned when a retry under the same `Idempotency-Key` replays an earlier success (same conversation, no second message).", body = CoveConversationSummary),
        (status = 400, description = "Missing/blank `Idempotency-Key`, or empty/over-long text", body = ErrorBody),
        (status = 404, description = "Cove not found", body = ErrorBody),
        (status = 409, description = "Distinguished by the body's `code`:\n* `conflict` — the cove has no claimed folder (no cwd for the chat wave), or the derived card already exists.\n* `conflict` (`operation idempotency key … already used with different payload`) — this `Idempotency-Key` was already used for a request whose first-message text differed. The text is bound into the operation payload as a SHA-256, so a changed body really does change the payload hash; the request is rejected instead of silently replaying the earlier conversation. Exception, and it is deliberate: if the previous attempt under this key ended terminally `Failed` it was compensated away and the retry runs under a fresh `#N` operation key, which no earlier payload hash is bound to — an edited draft resent after that failure is therefore **not** rejected for the old payload hash. It is not a promise of 201 either: the retry really re-executes, and its outcome is decided by that execution (201 on success; it can still fail, e.g. 409 `conflict` if the derived card already exists).\n* `idempotency_key_exhausted` — the key used up its 64 retry slots after that many failed attempts; retry under a NEW `Idempotency-Key` (previously a generic 500, which read as \"server broke\" rather than \"this key is used up\").", body = ErrorBody),
        (status = 500, description = "Internal error. A failed harness *start* is compensated: no card, no session, and the same key can be retried. A failed first *send* after a successful start leaves the created conversation in place on purpose — that is what makes the same key retry the send instead of answering a silent 201. Note the conversation is not necessarily empty: if `harness.observe` succeeded and only the audit write failed, the message is already in the harness queue even though no audit event records it. A previous attempt left `Stuck` also answers 500 under the same key until an operator clears it.", body = ErrorBody),
        (status = 503, description = "Shared codex app-server not running — retry shortly", body = ErrorBody),
    ),
)]
/// Mint a conversation and deliver its first message.
///
/// # What `Idempotency-Key` means here (it is NOT standard HTTP idempotency)
///
/// A standard idempotency key promises "same key ⇒ same recorded result,
/// forever". This endpoint deliberately promises something weaker and more
/// useful: **same key = the same retryable draft**. Slice 4 binds the key to
/// the draft the user keeps pressing send on, so replaying a failure forever
/// would turn one bad attempt into a permanently dead compose box.
///
/// The full contract, all four arms:
///
/// | same key, previous attempt … | answer |
/// |---|---|
/// | succeeded | replays the same conversation, **does not re-send** the first message (201) |
/// | terminally `Failed` | **genuinely retries** under a `#N` operation key, so it may return 201 where the first call returned 500 — different result, same key |
/// | `Stuck` | keeps returning 500, on purpose (fail-closed: compensation did not finish, the card may still exist, an operator has to look) |
/// | failed 64 times | 409 `idempotency_key_exhausted` — the key is exhausted, use a new one |
///
/// # Same key, different `text`
///
/// The first message's SHA-256 travels in the operation payload
/// (`SpecHarnessStartOperationPayload::first_message_sha256`), so it is part
/// of `stable_payload_hash` and therefore part of what
/// `OperationRuntime::submit` compares before it does anything else. Reusing a
/// key with a different body is consequently a real 409 `conflict` — no card,
/// no message — rather than a silent 201 replaying whatever the key sent the
/// first time. Only the hash travels: the payload is persisted in
/// `operations.payload_json`, and the body has no business being copied there.
///
/// The one arm where a changed body is not rejected for the old hash is the
/// `Failed` row above: the retry submits under a fresh `#N` operation key,
/// which no prior payload hash is bound to. That is the intended behaviour, not
/// an oversight — arm (b) exists so an edited draft can be resent after a
/// failure. What it buys is a genuine re-execution, not a guaranteed 201: the
/// retry's final status is decided by that execution and can still be a failure
/// (a 409 `conflict` if the derived card already exists, say).
///
/// A concurrent laggard can also observe the 500 arm: if the predecessor it
/// shares an operation with ended terminally failed, the replayed failure is
/// what it gets while the leader's fresh attempt succeeds.
///
/// None of this can produce a second conversation: the **card id** is a pure
/// function of `(cove_id, Idempotency-Key)` and the `#N` suffix touches only
/// the *operation* key (see `conversation_keys::derive_cove_conversation_keys` and its golden test).
pub(crate) async fn create_cove_conversation(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    State(cs): State<CodexShellState>,
    actor: Actor,
    headers: HeaderMap,
    Path(cove_id): Path<String>,
    Json(body): Json<NewCoveConversationBody>,
) -> Result<(StatusCode, Json<CoveConversationSummary>)> {
    // Required, not optional. The deterministic card id and the operation
    // idempotency key are both derived from this header; without it a retried
    // POST would mint a second conversation, so there is no sane `None`
    // behaviour to fall back to.
    let idempotency_key = parse_idempotency_key_header(&headers)?.ok_or_else(|| {
        CalmError::BadRequest(
            "Idempotency-Key header is required so a retried conversation create cannot mint a second card"
                .into(),
        )
    })?;
    // Validate the message before minting anything: an empty first message
    // must not leave a conversation behind.
    let text = body.text;
    validate_first_message(&text)?;
    if s.repo.cove_get(&cove_id).await?.is_none() {
        return Err(CalmError::NotFound(format!("cove {cove_id}")));
    }

    let (wave, _created) = ensure_cove_chat_wave_inner(&s, actor.clone(), &cove_id).await?;
    let derived = derive_cove_conversation_keys(&cove_id, &idempotency_key);

    let payload = SpecHarnessStartOperationPayload {
        actor: actor.to_actor_id(),
        wave_id: wave.id.to_string(),
        spec_card_id: derived.card_id.clone().into(),
        report_card_id: None,
        sort: None,
        cwd: wave.workspace.path.clone(),
        // A plain chat has no wave goal: seeding one would put an
        // `Observation::WaveGoal` in the queue and the harness would start
        // talking before the user did.
        goal: None,
        reset_harness_items: false,
        force_new_thread: true,
        profile: HarnessProfile::PlainChat,
        create_card: Some(LazyMintCardSeed {
            title: None,
            sort: None,
            // Deliberately absent. The cove flavour's mint guard is the chat-wave
            // check in `validate`, not the derived-id recomputation the wave
            // flavour uses, and setting this would change every cove create's
            // `payload_hash` for nothing.
            idempotency_key: None,
        }),
        // Binds the body into `payload_hash` so "same key, different text" is
        // a 409 instead of a silent replay. Hash, not text — see the field's
        // own doc comment.
        first_message_sha256: Some(first_message_digest(&text)),
    };
    let payload = serde_json::to_value(payload)?;
    let operation_key = retryable_operation_key(&s, &derived.operation_key).await?;
    let op_id = s
        .operation_runtime
        .submit(
            SPEC_HARNESS_START,
            OperationKey {
                operation_key: operation_key.clone(),
                idempotency_key: Some(operation_key),
                payload_hash: stable_payload_hash(&payload)?,
            },
            payload,
        )
        .await?;
    let result = s.operation_runtime.wait(&op_id).await?;
    match result.outcome {
        OperationOutcome::Succeeded { .. } | OperationOutcome::SucceededViaCollision { .. } => {}
        OperationOutcome::Failed {
            last_error,
            from_phase,
            last_error_class,
        } => {
            return Err(calm_error_from_operation_failure(
                last_error_class.as_deref(),
                last_error,
                from_phase,
            ));
        }
        OperationOutcome::Stuck { .. } => {
            return Err(CalmError::Internal("operation stuck, see DB".to_string()));
        }
    }

    // The first message is claimed against real, observable state — never
    // inferred from "was there already an operation row?". Two concurrent
    // POSTs under one key share ONE operation and both see it succeed, so a
    // pre-submit lookup answers "no prior attempt" to both and the agent gets
    // the same instruction twice. Under the per-card claim exactly one of them
    // observes an empty transcript and sends.
    //
    // What the claim actually asks is "has this CARD ever had a user message
    // enqueued?", not "has THIS request's first message been delivered?".
    // Those are two different questions, and this route asks the wrong one.
    //
    // === KNOWN GAP 1: the claim is not request-scoped ===
    //
    // Earlier rounds of this comment asserted the two questions were
    // equivalent "because no other entry point can reach the card". That is
    // FALSE, and round 3 of review is what corrected it. The reachable chain:
    //   1. A create under key K mints the card, then its first send fails
    //      after `harness.observe` (see gap 2) — the operation succeeded, so
    //      compensation does not run and the CARD SURVIVES. Client gets 500.
    //   2. `GET /api/coves/{cove}/conversations` lists that card, id and all.
    //      Nothing about the id is secret either: it is a public,
    //      deterministic SHA-256 of `(cove_id, Idempotency-Key)`, so anyone
    //      holding both can compute it (see `conversation_keys::derive_cove_conversation_keys`).
    //   3. `POST /api/cards/{id}/spec/input` is a public endpoint on exactly
    //      that card and writes exactly this `harness.user_message.enqueued`
    //      kind (`routes/cards.rs::send_spec_input`).
    //   4. The retry under key K now reads that FOREIGN message as evidence
    //      that its own first message already landed, and skips the send.
    // What that chain actually demonstrates — and it is what the test below
    // asserts — is that the claim ADOPTS FOREIGN EVIDENCE: somebody else's
    // message satisfied this request's claim, so the retry sends nothing. In
    // this chain the body's own first message is NOT lost: step 1 failed only
    // after `harness.observe`, so the original text is already in the harness
    // queue and one copy is the correct number. It is never a double-send
    // either, so nothing acts twice on one instruction.
    // The variant that WOULD silently drop the body's first message needs a
    // first attempt that fails BEFORE `harness.observe` (nothing enqueued, card
    // still surviving); the current suite has no injection point there, so that
    // variant is NOT COVERED — stated the same way in the test's doc comment
    // rather than asserted loosely.
    // Pinned as current behaviour (NOT as an invariant) by
    // `foreign_send_between_a_failed_first_send_and_its_retry_skips_the_first_message`.
    //
    // === KNOWN GAP 2: the evidence is written non-transactionally ===
    //
    //   * event write SUCCEEDED ⇒ a retry under the same key reads it and
    //     does not re-send. This is the guarantee.
    //   * `harness.observe` succeeded but `log_pure_event` FAILED (a DB error
    //     between the two steps of `send_spec_input`) ⇒ the message is
    //     already in the harness queue with nothing recorded, the client gets
    //     a 500, and a retry under the same key SENDS IT AGAIN — the agent can
    //     act twice on one instruction.
    // Pinned as current behaviour by
    // `first_send_whose_audit_event_fails_is_re_sent_on_retry`.
    //
    // === Shared root cause and the structural fix (tracked, not done here) ===
    //
    // Both gaps are the same mistake: the mint is transactional (operation
    // runtime) while the send is not (`harness.observe` is an in-memory queue
    // push), so a NON-TRANSACTIONAL, NON-REQUEST-SCOPED event is being used as
    // evidence for a non-transactional step. Gap 1 is the "not request-scoped"
    // half, gap 2 the "not transactional" half.
    // The structural fix is one change that closes both: fold the first
    // message into the same operation — seed `Observation::UserMessage` into
    // the harness snapshot's `pending_queue` inside `prepare_tx`, the way
    // `initial_snapshot_with_goal` already seeds a spec harness's goal. Then
    // the message ships inside the mint transaction, this read disappears, and
    // there is nothing left for a foreign send to satisfy. Tracked on #1098;
    // the user's ruling is that it is out of scope for this slice.
    wait_at_first_message_barrier(&cove_id).await;
    let _first_message_claim =
        lock_card(&s.conversation_first_message_locks, &derived.card_id).await;
    if !user_message_already_enqueued(&w, wave.id.as_str(), &derived.card_id).await? {
        // Call the real handler rather than reimplementing it: the first
        // message and every later message must go through byte-identical
        // validation, locking, harness recovery and audit.
        let _queued = send_spec_input(
            State(s.clone()),
            State(w.clone()),
            State(cs),
            actor,
            Path(derived.card_id.clone()),
            Json(SendSpecInputRequest { text }),
        )
        .await?;
    }

    let summary = load_conversation_summaries(&w, wave.id.as_str(), Some(&derived.card_id))
        .await?
        .pop()
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "conversation card {} missing right after a successful create",
                derived.card_id
            ))
        })?;
    Ok((StatusCode::CREATED, Json(summary)))
}

async fn cove_chat_wave(s: &RouteState, cove_id: &str) -> Result<Option<Wave>> {
    Ok(s.repo
        .waves_by_cove(cove_id)
        .await?
        .into_iter()
        .find(|wave| wave.purpose.as_deref() == Some(crate::COVE_CHAT_PURPOSE)))
}

/// Read the conversation rows of one chat wave.
///
/// `cards` is the driving table and the session is LEFT JOINed: a chat card
/// with no live session row is still a conversation the user owns and must see
/// (that is the whole reason `state` is nullable). Driving from
/// `worker_sessions` instead would silently hide every card between its mint
/// and its first live session, plus every card whose harness has since been
/// shut down.
///
/// The marker predicate mirrors `plain_chat::card_is_plain_chat(.., true)`
/// exactly — Worker role, `codex` kind, `harness_profile == "plain_chat"` — so
/// the chat wave's kernel-owned spec and report cards can never appear here
/// (INV-CHAT-007).
///
/// The three conjuncts are NOT three equal defences, and the tests say so:
/// - `harness_profile = 'plain_chat'` is the primary discriminator.
/// - `kind = 'codex'` is **load-bearing and production-reachable**: `POST
///   /api/cards` has no guard against `purpose = 'cove-chat'` waves (design v5
///   retired those guards) and this very list hands the chat `waveId` to the
///   client, so a user can park a marked `terminal` card on the chat wave.
///   This conjunct is the only thing that keeps it out.
/// - `role = 'worker'` has **no production-reachable counterexample**: a chat
///   wave's spec/report cards never receive the marker (their payload is
///   written by `create_wave_with_spec_harness` and the adapter's payload
///   rewrite only touches thread keys). It is defence in depth against a
///   tampered/corrupt row, and its test fixture is a tampering model, not a
///   reachable state.
async fn load_conversation_summaries(
    w: &WorkerState,
    wave_id: &str,
    card_id: Option<&str>,
) -> Result<Vec<CoveConversationSummary>> {
    let pool = w.repo.sqlite_pool().ok_or_else(|| {
        CalmError::Internal("cove conversations require a sqlite-backed repo".into())
    })?;
    let rows = sqlx::query_as::<_, ConversationRow>(
        r#"SELECT c.id                                   AS id,
                  c.wave_id                              AS wave_id,
                  c.title                                AS title,
                  ws.state                               AS state,
                  COALESCE(ws.updated_at_ms, c.updated_at) AS updated_at
             FROM cards c
             LEFT JOIN worker_sessions ws
                    ON ws.id = (SELECT inner_ws.id
                                  FROM worker_sessions inner_ws
                                 WHERE inner_ws.card_id = c.id
                                   AND inner_ws.state IN ('starting', 'running', 'idle', 'turn_pending')
                                 ORDER BY inner_ws.updated_at_ms DESC,
                                          inner_ws.created_at_ms DESC,
                                          inner_ws.id DESC
                                 LIMIT 1)
            WHERE c.wave_id = ?1
              AND c.role = 'worker'
              AND c.kind = 'codex'
              AND json_extract(c.payload, '$.harness_profile') = 'plain_chat'
              AND (?2 IS NULL OR c.id = ?2)
            ORDER BY updated_at DESC, c.id"#,
    )
    .bind(wave_id)
    .bind(card_id)
    .fetch_all(&pool)
    .await?;
    rows.into_iter()
        .map(CoveConversationSummary::try_from)
        .collect()
}

#[derive(sqlx::FromRow)]
struct ConversationRow {
    id: String,
    wave_id: String,
    title: Option<String>,
    state: Option<String>,
    updated_at: i64,
}

impl TryFrom<ConversationRow> for CoveConversationSummary {
    type Error = CalmError;

    fn try_from(row: ConversationRow) -> Result<Self> {
        let state = row
            .state
            .map(WorkerSessionState::try_from)
            .transpose()
            .map_err(CalmError::Internal)?;
        Ok(CoveConversationSummary {
            id: row.id,
            wave_id: row.wave_id,
            title: row.title,
            kind: COVE_CONVERSATION_KIND.to_string(),
            state,
            updated_at: row.updated_at,
        })
    }
}
