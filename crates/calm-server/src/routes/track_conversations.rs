//! `/api/tracks/{track_id}/conversations` — a track's assistant conversations and
//! its "mint on first message" creation endpoint (#1189 slice 3).
//!
//! A conversation here is a headless codex card carrying the persisted
//! `harness_profile: "assistant"` marker and `CardRole::Assistant`, parked on
//! an ordinary, user-visible track. Pressing `+` in the UI creates nothing at
//! all; the card, its session and its codex thread are all minted by the first
//! message, which is what this module's POST does in one operation.
//!
//! The list predicate is intentionally exact: a Track also carries a planner card,
//! a report card and dispatched worker cards, none of which are conversations.
//!
//! # One track is treated differently, and it is named (#1343)
//!
//! A conversation created on **Today's launchpad track** is opened with the
//! day's activity window ahead of the user's first message; see
//! [`activity_window::launchpad_opening_briefing`]. Every other track gets
//! exactly the behaviour
//! it always had. That is the only track-dependent branch in this module, and
//! it exists because the launchpad is where the user asks "what happened
//! today?" — the projection that answers it is server-side by design
//! (`activity_window`, D4), so nothing but the server can put it in front of
//! the agent. Since #1314 this module only *rules* on it — see
//! [`OpeningBriefing`] — and the rendering happens inside the mint
//! transaction.
//!
//! [`activity_window::launchpad_opening_briefing`]: crate::activity_window::launchpad_opening_briefing

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::actor::Actor;
use crate::conversation_keys::derive_track_conversation_keys;
use crate::error::{CalmError, ErrorBody, Result};
use crate::model::{CardRole, TrackConversationSummary};
use crate::operation::planner_harness_start_adapter::{
    ASSISTANT_HARNESS_PROFILE_MARKER, HarnessProfile, LazyMintCardSeed, OpeningBriefing,
    PlannerHarnessStartOperationPayload,
};
use crate::operation::{OperationKey, OperationOutcome};
use crate::routes::conversations_shared::{
    PLANNER_HARNESS_START, retryable_operation_key, validate_first_message,
};
use crate::routes::terminal_cards::{
    calm_error_from_operation_failure, parse_idempotency_key_header, stable_payload_hash,
};
use crate::session_projection_repo::WorkerSessionState;
use crate::state::{AppState, RouteState, WorkerState};

/// The `kind` every row of this list carries.
const TRACK_CONVERSATION_KIND: &str = "track-assistant";

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/tracks/{track_id}/conversations",
        get(list_track_conversations).post(create_track_conversation),
    )
}

/// Body of `POST /api/tracks/{track_id}/conversations`: the first message.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct NewTrackConversationBody {
    /// The first message. Validated exactly like `POST /api/cards/{id}/planner/input`
    /// (non-blank after trim, at most 32768 chars) and validated *before*
    /// anything is minted, so a rejected message leaves no card behind.
    pub text: String,
}

#[utoipa::path(
    get,
    path = "/api/tracks/{track_id}/conversations",
    tag = "tracks",
    params(("track_id" = String, Path, description = "Track id")),
    responses(
        (status = 200, description = "Assistant conversations on this track, newest activity first. The track's planner card, report card and dispatched worker cards are never listed here.", body = Vec<TrackConversationSummary>),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_track_conversations(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    Path(track_id): Path<String>,
) -> Result<Json<Vec<TrackConversationSummary>>> {
    if s.repo.track_get(&track_id).await?.is_none() {
        return Err(CalmError::NotFound(format!("track {track_id}")));
    }
    let rows = load_track_conversation_summaries(&w, &track_id, None).await?;
    Ok(Json(rows))
}

#[utoipa::path(
    post,
    path = "/api/tracks/{track_id}/conversations",
    tag = "tracks",
    params(
        ("track_id" = String, Path, description = "Track id"),
        ("Idempotency-Key" = String, Header, description = "**Required.** Scopes the derived card id and the operation dedup key, so retrying the same request can never mint a second conversation. A missing or blank header is 400.\n\n**This is NOT standard HTTP idempotency — it is \"same key = the same retryable draft\"**: a success replays without re-sending; a terminal failure retries under a `#N` operation key; a stuck attempt stays failed closed; 64 failed attempts exhaust the key; and the same key with different text is a conflict. The derived card id never carries the retry suffix."),
    ),
    request_body = NewTrackConversationBody,
    responses(
        (status = 201, description = "Conversation card minted, harness started, first message delivered — all three in the mint operation's own transaction, so a 201 means the message is on the assistant's queue and not merely that a card exists. Also returned when a retry under the same `Idempotency-Key` replays an earlier success (same conversation, no second message).", body = TrackConversationSummary),
        (status = 400, description = "Missing/blank `Idempotency-Key`, or empty/over-long text. A `BadRequest` raised by `PlannerHarnessStartAdapter::validate` also lands here — the operation-failure mapping keeps `bad_request` a 400.", body = ErrorBody),
        (status = 403, description = "The track is retired hidden Area-chat scaffolding and cannot accept Track conversations.", body = ErrorBody),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 409, description = "Distinguished by the body's `code`:\n* `conflict` — the derived card already exists, or this `Idempotency-Key` was already used for a request whose first-message text differed (the text is bound into the operation payload, and its hash is what `submit` compares).\n* `idempotency_key_exhausted` — the key used up its 64 retry slots; retry under a NEW `Idempotency-Key`.", body = ErrorBody),
        (status = 500, description = "Internal error. The message rides inside the mint operation, so the card and the delivery fail together: a terminally failed attempt is compensated (no card, no session) and the same `Idempotency-Key` retries under a `#N` operation key, re-deriving the same card id and delivering the message again. The `harness.user_message.enqueued` row a failed attempt already committed is NOT rolled back — `events` is append-only — and records only that a delivery was attempted, never that one happened. A previous attempt left `Stuck` also answers 500 under the same key until an operator clears it; there the card survives with the message still queued on a runtime that never started.", body = ErrorBody),
        (status = 503, description = "Shared codex app-server not running — retry shortly", body = ErrorBody),
    ),
)]
/// Mint a track assistant conversation and deliver its first message.
///
/// #1314 — the message is folded INTO the mint operation. `first_message`
/// travels in the `planner-harness-start` payload and
/// `PlannerHarnessStartAdapter::prepare_tx` seeds the
/// `Observation::UserMessage` and writes `harness.user_message.enqueued` in the
/// same transaction that mints the card and its session. There is no
/// post-operation send here, and consequently no first-message claim: the two
/// #1098 gaps this handler used to document — a claim that asked "has this CARD
/// ever had a user message enqueued?" instead of "has THIS request's message
/// landed?", and evidence written outside the transaction that carried the
/// message — are gone with the code that had them.
///
/// **Nothing on this path may read that evidence row back.** A failed attempt
/// is compensated by deleting the card, but its `harness.user_message.enqueued`
/// row survives (`events` is append-only and compensation only marks the
/// runtime failed), while the retry re-derives the very same card id from the
/// same `Idempotency-Key`. So the row means "a delivery was attempted", never
/// "a delivery happened", and treating it as a delivered-marker would turn
/// every retry-after-failure into a silently dropped message.
/// `a_retry_after_a_failed_attempt_still_delivers_the_message` pins that, and a
/// persisted marker that could answer the question honestly is #1384.
pub(crate) async fn create_track_conversation(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    actor: Actor,
    headers: HeaderMap,
    Path(track_id): Path<String>,
    Json(body): Json<NewTrackConversationBody>,
) -> Result<(StatusCode, Json<TrackConversationSummary>)> {
    create_track_conversation_inner(
        s,
        w,
        actor,
        headers,
        track_id,
        body,
        OpeningBriefing::TodaysActivityOnTheLaunchpad,
    )
    .await
}

/// `create_track_conversation`, plus the caller's ruling on opening material.
///
/// Server-internal callers go through here rather than through the route
/// handler so that the mint, the derived-id guard, the retry arms and the
/// in-transaction first-message delivery are still the ones production uses —
/// the only thing that varies is [`OpeningBriefing`].
pub(crate) async fn create_track_conversation_inner(
    s: RouteState,
    w: WorkerState,
    actor: Actor,
    headers: HeaderMap,
    track_id: String,
    body: NewTrackConversationBody,
    briefing: OpeningBriefing,
) -> Result<(StatusCode, Json<TrackConversationSummary>)> {
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

    let track = s
        .repo
        .track_get(&track_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {track_id}")))?;
    // Retired Area-chat tracks are hidden legacy scaffolding. Narrowing, not
    // the guard `validate` relies on:
    // the mint's actual wall is the derived-id recomputation, which does not
    // care what kind of track this is. This keeps new Track conversations off
    // rows that no user-visible Track list can reach.
    if track.purpose.as_deref() == Some(crate::AREA_CHAT_PURPOSE) {
        return Err(CalmError::Forbidden(format!(
            "track {} is retired area-chat scaffolding and cannot accept conversations",
            track.id
        )));
    }

    let derived = derive_track_conversation_keys(track.id.as_str(), &idempotency_key);

    let payload = PlannerHarnessStartOperationPayload {
        actor: actor.to_actor_id(),
        track_id: track.id.to_string(),
        planner_card_id: derived.card_id.clone().into(),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.path.clone(),
        // No goal. A seeded `Observation::TrackGoal` would make the assistant
        // open the conversation by talking about the track title before the
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
        // #1343's ruling, carried into the transaction that acts on it. The
        // caller decides; `prepare_tx` renders. `None` is not spelled out for
        // any caller here — both arms are explicit — but it is what every
        // payload written before #1343 deserializes to, and it means "no
        // briefing", which is what those payloads meant.
        opening_briefing: Some(briefing),
        // #1314 — the text itself, so the adapter can enqueue the actual bytes
        // inside the mint transaction. This is the whole change: before it, the
        // message was sent by a second, non-transactional call after the
        // operation had already committed.
        //
        // It also binds the body into `payload_hash`, which is what makes "same
        // key, different text" a 409 instead of a silent replay: `submit`
        // compares that hash before anything else runs.
        first_message: Some(text),
        // #1384 — bound only by `POST /api/tracks`; this route mints no track,
        // so it has no create request to hash.
        create_request_sha256: None,
    };
    let payload = serde_json::to_value(payload)?;
    let operation_key = retryable_operation_key(&s, &derived.operation_key).await?;
    let op_id = s
        .operation_runtime
        .submit(
            PLANNER_HARNESS_START,
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

    // No send, no per-card first-message claim, and no briefing call out here;
    // none of the three is an omission. The message was enqueued by
    // `prepare_tx` inside the operation above, and the operation is what
    // serializes concurrent POSTs under one key: two of them share ONE
    // operation row, so the second is a collision that replays the first's
    // success rather than a second mint with a second delivery. The claim used
    // to exist only because the send happened out here, after that
    // serialization point.
    //
    // #1343's opening briefing moved with it. What travels in the payload is
    // the caller's RULING (`opening_briefing`), never the briefing TEXT; the
    // adapter renders the text inside the transaction. See
    // `PlannerHarnessStartOperationPayload::opening_briefing` for why the text
    // must not enter `payload_hash`.

    let summary = load_track_conversation_summaries(&w, track.id.as_str(), Some(&derived.card_id))
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

/// Read the assistant conversation rows of one track.
///
/// An ordinary track is populated with a planner card, a report card, and every codex
/// worker card the dispatcher has spawned for the plan (#1149). Widen this
/// predicate to "a codex card" and the conversation list fills up with the
/// track's workers.
///
/// `role = 'assistant'` is the primary discriminator: the role
/// column is what the authorization gate reads, so a row that is listed here is
/// by construction a row that holds the assistant tool surface.
///
/// The marker conjunct is kept as the second half because nothing stops a future card from being created with the
/// assistant role by some other path, and a conversation the user can open must
/// be one this endpoint knows how to mint.
///
/// `cards` is the driving table and the session is LEFT JOINed: a conversation
/// card with no live session row is still one the user owns and must see (that
/// is the whole reason `state` is nullable). Driving from `worker_sessions`
/// instead would silently hide every card between its mint and its first live
/// session, plus every card whose harness has since been shut down.
async fn load_track_conversation_summaries(
    w: &WorkerState,
    track_id: &str,
    card_id: Option<&str>,
) -> Result<Vec<TrackConversationSummary>> {
    let pool = w.repo.sqlite_pool().ok_or_else(|| {
        CalmError::Internal("track conversations require a sqlite-backed repo".into())
    })?;
    let rows = sqlx::query_as::<_, TrackConversationRow>(
        r#"SELECT c.id                                   AS id,
                  c.track_id                              AS track_id,
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
            WHERE c.track_id = ?1
              AND c.role = ?2
              AND c.kind = 'codex'
              AND json_extract(c.payload, '$.harness_profile') = ?3
              AND (?4 IS NULL OR c.id = ?4)
            ORDER BY updated_at DESC, c.id"#,
    )
    .bind(track_id)
    .bind(CardRole::Assistant.as_db_str())
    .bind(ASSISTANT_HARNESS_PROFILE_MARKER)
    .bind(card_id)
    .fetch_all(&pool)
    .await?;
    rows.into_iter()
        .map(TrackConversationSummary::try_from)
        .collect()
}

#[derive(sqlx::FromRow)]
struct TrackConversationRow {
    id: String,
    track_id: String,
    title: Option<String>,
    state: Option<String>,
    updated_at: i64,
}

impl TryFrom<TrackConversationRow> for TrackConversationSummary {
    type Error = CalmError;

    fn try_from(row: TrackConversationRow) -> Result<Self> {
        let state = row
            .state
            .map(WorkerSessionState::try_from)
            .transpose()
            .map_err(CalmError::Internal)?;
        Ok(TrackConversationSummary {
            id: row.id,
            track_id: row.track_id,
            title: row.title,
            kind: TRACK_CONVERSATION_KIND.to_string(),
            state,
            updated_at: row.updated_at,
        })
    }
}
