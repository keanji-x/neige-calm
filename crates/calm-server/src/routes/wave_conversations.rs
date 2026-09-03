//! `/api/waves/{wave_id}/conversations` — a wave's assistant conversations and
//! its "mint on first message" creation endpoint (#1189 slice 3).
//!
//! A conversation here is a headless codex card carrying the persisted
//! `harness_profile: "assistant"` marker and `CardRole::Assistant`, parked on
//! an ordinary, user-visible wave. Pressing `+` in the UI creates nothing at
//! all; the card, its session and its codex thread are all minted by the first
//! message, which is what this module's POST does in one operation.
//!
//! # How this differs from `area_conversations`
//!
//! The retry contract is shared verbatim (`conversations_shared`), the derived
//! ids live in separate namespaces (`conversation_keys`), and the two things
//! that are genuinely different are:
//!
//! * **the card that gets minted** — `CardRole::Assistant` with an MCP token and
//!   the block-channel tool surface, versus a `CardRole::Worker` plain chat with
//!   no MCP at all;
//! * **the list predicate** — this wave carries a spec card, a report card and
//!   however many dispatched worker cards (#1149), so the predicate has to be
//!   exact about the role rather than merely "a codex card with a marker".

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::actor::Actor;
use crate::conversation_keys::derive_wave_conversation_keys;
use crate::error::{CalmError, ErrorBody, Result};
use crate::model::{CardRole, WaveConversationSummary};
use crate::operation::spec_harness_start_adapter::{
    ASSISTANT_HARNESS_PROFILE_MARKER, HarnessProfile, LazyMintCardSeed,
    SpecHarnessStartOperationPayload,
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
use crate::session_projection_repo::WorkerSessionState;
use crate::state::{AppState, CodexShellState, RouteState, WorkerState};

/// The `kind` every row of this list carries. Distinct from the area list's
/// `"shared-chat"`: see [`WaveConversationSummary::kind`].
const WAVE_CONVERSATION_KIND: &str = "wave-assistant";

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/waves/{wave_id}/conversations",
        get(list_wave_conversations).post(create_wave_conversation),
    )
}

/// Body of `POST /api/waves/{wave_id}/conversations`: the first message.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct NewWaveConversationBody {
    /// The first message. Validated exactly like `POST /api/cards/{id}/spec/input`
    /// (non-blank after trim, at most 32768 chars) and validated *before*
    /// anything is minted, so a rejected message leaves no card behind.
    pub text: String,
}

#[utoipa::path(
    get,
    path = "/api/waves/{wave_id}/conversations",
    tag = "waves",
    params(("wave_id" = String, Path, description = "Wave id")),
    responses(
        (status = 200, description = "Assistant conversations on this wave, newest activity first. The wave's spec card, report card and dispatched worker cards are never listed here.", body = Vec<WaveConversationSummary>),
        (status = 404, description = "Wave not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_wave_conversations(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    Path(wave_id): Path<String>,
) -> Result<Json<Vec<WaveConversationSummary>>> {
    if s.repo.wave_get(&wave_id).await?.is_none() {
        return Err(CalmError::NotFound(format!("wave {wave_id}")));
    }
    let rows = load_wave_conversation_summaries(&w, &wave_id, None).await?;
    Ok(Json(rows))
}

#[utoipa::path(
    post,
    path = "/api/waves/{wave_id}/conversations",
    tag = "waves",
    params(
        ("wave_id" = String, Path, description = "Wave id"),
        ("Idempotency-Key" = String, Header, description = "**Required.** Scopes the derived card id and the operation dedup key, so retrying the same request can never mint a second conversation. A missing or blank header is 400.\n\n**This is NOT standard HTTP idempotency — it is \"same key = the same retryable draft\"**, with the same four arms as `POST /api/areas/{area_id}/conversations`: (a) same key after a **success** replays the same conversation and does not re-send the first message; (b) same key after a **terminally failed** attempt genuinely RETRIES under a fresh `#N` operation key and may therefore return 201 where the first call gave 500; (c) same key after a **stuck** attempt keeps returning 500 on purpose (fail-closed); (d) after 64 failed attempts the key is exhausted and answers 409 `idempotency_key_exhausted`; (e) same key with a **different `text`** is 409 `conflict`, because the message body is bound into the operation payload as a SHA-256 — except after arm (b), whose fresh operation key no earlier payload hash is bound to. The derived card id never carries the retry suffix, so none of this can mint a second conversation."),
    ),
    request_body = NewWaveConversationBody,
    responses(
        (status = 201, description = "Conversation card minted, harness started, first message sent. Also returned when a retry under the same `Idempotency-Key` replays an earlier success (same conversation, no second message).", body = WaveConversationSummary),
        (status = 400, description = "Missing/blank `Idempotency-Key`, empty/over-long text, or the wave carries the kernel view/template overlay — `SpecHarnessStartAdapter::validate` refuses template waves with a `BadRequest`, and the operation-failure mapping keeps `bad_request` a 400.", body = ErrorBody),
        (status = 403, description = "The wave is an area chat wave; its conversations are created through the area endpoint.", body = ErrorBody),
        (status = 404, description = "Wave not found", body = ErrorBody),
        (status = 409, description = "Distinguished by the body's `code`:\n* `conflict` — the derived card already exists, or this `Idempotency-Key` was already used for a request whose first-message text differed (the text is bound into the operation payload as a SHA-256).\n* `idempotency_key_exhausted` — the key used up its 64 retry slots; retry under a NEW `Idempotency-Key`.", body = ErrorBody),
        (status = 500, description = "Internal error. A failed harness *start* is compensated: no card, no session, and the same key can be retried. A failed first *send* after a successful start leaves the created conversation in place on purpose — that is what makes the same key retry the send instead of answering a silent 201. A previous attempt left `Stuck` also answers 500 under the same key until an operator clears it.", body = ErrorBody),
        (status = 503, description = "Shared codex app-server not running — retry shortly", body = ErrorBody),
    ),
)]
/// Mint a wave assistant conversation and deliver its first message.
///
/// The `Idempotency-Key` contract, the first-message claim and both of its
/// known gaps are identical to `create_area_conversation`, whose doc comment is
/// the long-form statement of all of them; this handler differs only in the
/// profile it mints under and the namespace its ids come from. The gaps are
/// restated in brief where they bite:
///
/// * the first-message claim asks "has this CARD ever had a user message
///   enqueued?", not "has THIS request's message landed?", so a foreign
///   `POST /api/cards/{id}/spec/input` between a failed send and its retry
///   satisfies the claim;
/// * the evidence is written non-transactionally, so a send whose audit write
///   fails is re-sent on retry.
///
/// Both are tracked on #1098 and deliberately unchanged here: fixing them means
/// folding the first message into the mint operation, which would change both
/// endpoints at once and belongs in one dedicated change rather than being
/// half-done on the newer of the two.
pub(crate) async fn create_wave_conversation(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    State(cs): State<CodexShellState>,
    actor: Actor,
    headers: HeaderMap,
    Path(wave_id): Path<String>,
    Json(body): Json<NewWaveConversationBody>,
) -> Result<(StatusCode, Json<WaveConversationSummary>)> {
    // Required, not optional. The deterministic card id and the operation
    // idempotency key are both derived from this header; without it a retried
    // POST would mint a second conversation, and `validate`'s derived-id guard
    // has nothing to recompute from.
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

    let wave = s
        .repo
        .wave_get(&wave_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {wave_id}")))?;
    // An area chat wave is hidden scaffolding whose conversations are the area
    // endpoint's plain chats. Narrowing, not the guard `validate` relies on:
    // the mint's actual wall is the derived-id recomputation, which does not
    // care what kind of wave this is. This exists so the two endpoints cannot
    // both plant conversations on one wave and produce a list the UI has no
    // place to show.
    if wave.purpose.as_deref() == Some(crate::AREA_CHAT_PURPOSE) {
        return Err(CalmError::Forbidden(format!(
            "wave {} is an area chat wave; create its conversations through the area endpoint",
            wave.id
        )));
    }

    let derived = derive_wave_conversation_keys(wave.id.as_str(), &idempotency_key);

    let payload = SpecHarnessStartOperationPayload {
        actor: actor.to_actor_id(),
        wave_id: wave.id.to_string(),
        spec_card_id: derived.card_id.clone().into(),
        report_card_id: None,
        sort: None,
        cwd: wave.workspace.path.clone(),
        // No goal. A seeded `Observation::WaveGoal` would make the assistant
        // open the conversation by talking about the wave title before the
        // user has said anything.
        goal: None,
        reset_harness_items: false,
        force_new_thread: true,
        profile: HarnessProfile::Assistant,
        create_card: Some(LazyMintCardSeed {
            title: None,
            sort: None,
            // The adapter re-derives the card id from this and refuses any id
            // it did not compute itself (§4.3). Passing the raw header rather
            // than the derived id is the whole point: a derived id sent along
            // with itself would prove nothing.
            idempotency_key: Some(idempotency_key.clone()),
        }),
        // Binds the body into `payload_hash` so "same key, different text" is
        // a 409 instead of a silent replay. Hash, not text.
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
    // observes an empty transcript and sends. See this handler's doc comment
    // for what the claim does NOT promise.
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

    let summary = load_wave_conversation_summaries(&w, wave.id.as_str(), Some(&derived.card_id))
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

/// Read the assistant conversation rows of one wave.
///
/// **Not** a reuse of `area_conversations::load_conversation_summaries`, and
/// the difference is the whole point of G3. That query selects
/// `role = 'worker' AND kind = 'codex' AND harness_profile = 'plain_chat'`,
/// which on an area chat wave is exact because nothing else lives there. An
/// ordinary wave is populated: a spec card, a report card, and every codex
/// worker card the dispatcher has spawned for the plan (#1149). Widen this
/// predicate to "a codex card" and the conversation list fills up with the
/// wave's workers.
///
/// `role = 'assistant'` is the primary discriminator and, unlike the area
/// query's `role` conjunct, it is exact rather than defence in depth: the role
/// column is what the authorization gate reads, so a row that is listed here is
/// by construction a row that holds the assistant tool surface.
///
/// The marker conjunct is kept as the second half for the same reason the area
/// query keeps `kind`: nothing stops a future card from being created with the
/// assistant role by some other path, and a conversation the user can open must
/// be one this endpoint knows how to mint.
///
/// `cards` is the driving table and the session is LEFT JOINed: a conversation
/// card with no live session row is still one the user owns and must see (that
/// is the whole reason `state` is nullable). Driving from `worker_sessions`
/// instead would silently hide every card between its mint and its first live
/// session, plus every card whose harness has since been shut down.
async fn load_wave_conversation_summaries(
    w: &WorkerState,
    wave_id: &str,
    card_id: Option<&str>,
) -> Result<Vec<WaveConversationSummary>> {
    let pool = w.repo.sqlite_pool().ok_or_else(|| {
        CalmError::Internal("wave conversations require a sqlite-backed repo".into())
    })?;
    let rows = sqlx::query_as::<_, WaveConversationRow>(
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
              AND c.role = ?2
              AND c.kind = 'codex'
              AND json_extract(c.payload, '$.harness_profile') = ?3
              AND (?4 IS NULL OR c.id = ?4)
            ORDER BY updated_at DESC, c.id"#,
    )
    .bind(wave_id)
    .bind(CardRole::Assistant.as_db_str())
    .bind(ASSISTANT_HARNESS_PROFILE_MARKER)
    .bind(card_id)
    .fetch_all(&pool)
    .await?;
    rows.into_iter()
        .map(WaveConversationSummary::try_from)
        .collect()
}

#[derive(sqlx::FromRow)]
struct WaveConversationRow {
    id: String,
    wave_id: String,
    title: Option<String>,
    state: Option<String>,
    updated_at: i64,
}

impl TryFrom<WaveConversationRow> for WaveConversationSummary {
    type Error = CalmError;

    fn try_from(row: WaveConversationRow) -> Result<Self> {
        let state = row
            .state
            .map(WorkerSessionState::try_from)
            .transpose()
            .map_err(CalmError::Internal)?;
        Ok(WaveConversationSummary {
            id: row.id,
            wave_id: row.wave_id,
            title: row.title,
            kind: WAVE_CONVERSATION_KIND.to_string(),
            state,
            updated_at: row.updated_at,
        })
    }
}
