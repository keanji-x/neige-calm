//! `/api/tracks/{track_id}/conversations` — a track's assistant conversations and its
//! "mint on first message" creation endpoint. A conversation created on Today's
//! launchpad track is opened with the day's activity window ahead of the first message.

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
use calm_truth::session_projection_row::LAST_TURN_COMPLETED_MS_SUBQUERY;

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
    /// The first message. Validated exactly like `POST /api/cards/{id}/planner/input`, and
    /// before anything is minted, so a rejected message leaves no card behind.
    pub text: String,
    /// Explicit choice for the first turn; omitted/null follows the installation default.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
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
/// Mint a track assistant conversation and deliver its first message, folded into one
/// `planner-harness-start` operation. Nothing on this path may read the
/// `harness.user_message.enqueued` row back: a failed attempt's row survives compensation
/// while the retry re-derives the same card id, so the row means "attempted", never "delivered".
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

/// `create_track_conversation`, plus the caller's ruling on opening material; the only
/// thing that varies for server-internal callers is [`OpeningBriefing`].
pub(crate) async fn create_track_conversation_inner(
    s: RouteState,
    w: WorkerState,
    actor: Actor,
    headers: HeaderMap,
    track_id: String,
    body: NewTrackConversationBody,
    briefing: OpeningBriefing,
) -> Result<(StatusCode, Json<TrackConversationSummary>)> {
    // Required, not optional: the card id and the operation idempotency key are both
    // derived from this header; without it a retried POST would mint a second conversation.
    let idempotency_key = parse_idempotency_key_header(&headers)?.ok_or_else(|| {
        CalmError::BadRequest(
            "Idempotency-Key header is required so a retried conversation create cannot mint a second card"
                .into(),
        )
    })?;
    // Validate the message before minting anything, so an empty first message leaves no
    // conversation behind.
    if body.model.is_some() || body.reasoning_effort.is_some() {
        super::track_report_blocks::require_rest_user_actor_for(
            &actor,
            "conversation model selection",
            "The person starting the conversation chooses its model.",
        )?;
        for (field, value) in [
            ("model", &body.model),
            ("reasoning_effort", &body.reasoning_effort),
        ] {
            if value
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            {
                return Err(CalmError::BadRequest(format!(
                    "{field} must be non-blank or null"
                )));
            }
        }
    }
    let text = body.text;
    validate_first_message(&text)?;

    let track = s
        .repo
        .track_get(&track_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {track_id}")))?;
    // Retired Area-chat tracks are hidden legacy scaffolding; keep new conversations off
    // rows no user-visible Track list can reach.
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
        // No goal: a seeded `Observation::TrackGoal` would make the assistant open by talking
        // about the track title before the user has said anything.
        goal: None,
        reset_harness_items: false,
        force_new_thread: true,
        profile: HarnessProfile::Assistant,
        create_card: Some(LazyMintCardSeed {
            title: None,
            sort: None,
            // The adapter re-derives the card id from this and refuses any id it did not compute
            // itself; a derived id sent along with itself would prove nothing.
            idempotency_key: Some(idempotency_key.clone()),
            model: body.model,
            reasoning_effort: body.reasoning_effort,
        }),
        // The caller decides; `prepare_tx` renders. `None` (what older payloads deserialize
        // to) means "no briefing".
        opening_briefing: Some(briefing),
        // The text itself, so the adapter enqueues the actual bytes inside the mint
        // transaction. It also binds the body into `payload_hash`, which makes "same key,
        // different text" a 409 instead of a silent replay.
        first_message: Some(text),
        // This route mints no track, so it has no create request to hash.
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

    // No send, no per-card first-message claim, and no briefing call out here: the message
    // was enqueued by `prepare_tx` inside the operation, and the operation row is what
    // serializes concurrent POSTs under one key. The payload carries the caller's RULING,
    // never the briefing TEXT, which must not enter `payload_hash`.

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

/// Read the assistant conversation rows of one track. `role = 'assistant'` AND the
/// profile marker: widening to "a codex card" would list the track's workers. `cards`
/// drives and the session is LEFT JOINed, so a card with no live session row is still
/// listed (`state` is nullable for that reason).
async fn load_track_conversation_summaries(
    w: &WorkerState,
    track_id: &str,
    card_id: Option<&str>,
) -> Result<Vec<TrackConversationSummary>> {
    let pool = w.repo.sqlite_pool().ok_or_else(|| {
        CalmError::Internal("track conversations require a sqlite-backed repo".into())
    })?;
    // `last_turn_completed_at` is the same correlated subquery the card-runtime projection
    // evaluates, so planner and assistant rows read one definition of "the last turn ended".
    let sql = format!(
        r#"SELECT c.id                                   AS id,
                  c.track_id                              AS track_id,
                  c.title                                AS title,
                  ws.state                               AS state,
                  COALESCE(ws.updated_at_ms, c.updated_at) AS updated_at,
                  {LAST_TURN_COMPLETED_MS_SUBQUERY}       AS last_turn_completed_at
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
            ORDER BY updated_at DESC, c.id"#
    );
    let rows = sqlx::query_as::<_, TrackConversationRow>(&sql)
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
    last_turn_completed_at: Option<i64>,
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
            last_turn_completed_at: row.last_turn_completed_at,
        })
    }
}
