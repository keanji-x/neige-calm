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
//! [`launchpad_opening_briefing`]. Every other track gets exactly the behaviour
//! it always had. That is the only track-dependent branch in this module, and
//! it exists because the launchpad is where the user asks "what happened
//! today?" — the projection that answers it is server-side by design
//! (`activity_window`, D4), so nothing but the server can put it in front of
//! the agent.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::activity_window::{opening_activity_briefing, todays_workspace_activity};
use crate::actor::Actor;
use crate::conversation_keys::derive_track_conversation_keys;
use crate::error::{CalmError, ErrorBody, Result};
use crate::model::{CardRole, TrackConversationSummary};
use crate::operation::planner_harness_start_adapter::{
    ASSISTANT_HARNESS_PROFILE_MARKER, HarnessProfile, LazyMintCardSeed,
    PlannerHarnessStartOperationPayload,
};
use crate::operation::{OperationKey, OperationOutcome};
use crate::per_card_lock::lock_card;
use crate::routes::cards::send_planner_inputs;
use crate::routes::conversations_shared::{
    PLANNER_HARNESS_START, first_message_digest, retryable_operation_key,
    user_message_already_enqueued, validate_first_message,
};
use crate::routes::terminal_cards::{
    calm_error_from_operation_failure, parse_idempotency_key_header, stable_payload_hash,
};
use crate::session_projection_repo::WorkerSessionState;
use crate::state::{AppState, CodexShellState, RouteState, WorkerState};

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
        (status = 201, description = "Conversation card minted, harness started, first message sent. Also returned when a retry under the same `Idempotency-Key` replays an earlier success (same conversation, no second message).", body = TrackConversationSummary),
        (status = 400, description = "Missing/blank `Idempotency-Key`, or empty/over-long text. A `BadRequest` raised by `PlannerHarnessStartAdapter::validate` also lands here — the operation-failure mapping keeps `bad_request` a 400.", body = ErrorBody),
        (status = 403, description = "The track is retired hidden Area-chat scaffolding and cannot accept Track conversations.", body = ErrorBody),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 409, description = "Distinguished by the body's `code`:\n* `conflict` — the derived card already exists, or this `Idempotency-Key` was already used for a request whose first-message text differed (the text is bound into the operation payload as a SHA-256).\n* `idempotency_key_exhausted` — the key used up its 64 retry slots; retry under a NEW `Idempotency-Key`.", body = ErrorBody),
        (status = 500, description = "Internal error. A failed harness *start* is compensated: no card, no session, and the same key can be retried. A failed first *send* after a successful start leaves the created conversation in place on purpose — that is what makes the same key retry the send instead of answering a silent 201. A previous attempt left `Stuck` also answers 500 under the same key until an operator clears it.", body = ErrorBody),
        (status = 503, description = "Shared codex app-server not running — retry shortly", body = ErrorBody),
    ),
)]
/// Mint a track assistant conversation and deliver its first message.
///
/// The `Idempotency-Key` contract has two known gaps, restated where they bite:
///
/// * the first-message claim asks "has this CARD ever had a user message
///   enqueued?", not "has THIS request's message landed?", so a foreign
///   `POST /api/cards/{id}/planner/input` between a failed send and its retry
///   satisfies the claim;
/// * the evidence is written non-transactionally, so a send whose audit write
///   fails is re-sent on retry.
///
/// Both are tracked on #1098 and deliberately unchanged here: fixing them means
/// folding the first message into the mint operation, which would change both
/// sides of this endpoint and belongs in one dedicated change.
pub(crate) async fn create_track_conversation(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    State(cs): State<CodexShellState>,
    actor: Actor,
    headers: HeaderMap,
    Path(track_id): Path<String>,
    Json(body): Json<NewTrackConversationBody>,
) -> Result<(StatusCode, Json<TrackConversationSummary>)> {
    create_track_conversation_inner(
        s,
        w,
        cs,
        actor,
        headers,
        track_id,
        body,
        OpeningBriefing::TodaysActivityOnTheLaunchpad,
    )
    .await
}

/// Whether this create prepends #1343's activity briefing.
///
/// **An explicit parameter, because the answer is not a property of the track.**
/// `POST /api/today/summary` also creates its conversation on the launchpad, by
/// calling the very handler below, and it carries the day's counts itself in
/// `summary_prompt` — briefing it as well would put the same five numbers in
/// front of the agent twice and leave three
/// `harness.user_message.enqueued` rows where INV-TODAYDOC-010 requires two.
/// Deriving the answer from the track would make that outcome unavoidable; a
/// parameter makes each caller say what it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpeningBriefing {
    /// The user is starting this conversation. On the launchpad track it opens
    /// with today's activity window; on any other track it opens with nothing.
    TodaysActivityOnTheLaunchpad,
    /// The caller supplies its own material and must not be given a second
    /// copy of it. `POST /api/today/summary` is the only such caller.
    CallerSuppliesItsOwn,
}

/// `create_track_conversation`, plus the caller's ruling on opening material.
///
/// Server-internal callers go through here rather than through the route
/// handler so that the mint, the derived-id guard, the retry arms and the
/// first-message claim are still the ones production uses — the only thing that
/// varies is [`OpeningBriefing`].
#[allow(clippy::too_many_arguments)]
pub(crate) async fn create_track_conversation_inner(
    s: RouteState,
    w: WorkerState,
    cs: CodexShellState,
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
        // Binds the body into `payload_hash` so "same key, different text" is
        // a 409 instead of a silent replay. Hash, not text.
        first_message_sha256: Some(first_message_digest(&text)),
        first_message: None,
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

    // The first message is claimed against real, observable state — never
    // inferred from "was there already an operation row?". Two concurrent
    // POSTs under one key share ONE operation and both see it succeed, so a
    // pre-submit lookup answers "no prior attempt" to both and the agent gets
    // the same instruction twice. Under the per-card claim exactly one of them
    // observes an empty transcript and sends. See this handler's doc comment
    // for what the claim does NOT promise.
    let _first_message_claim =
        lock_card(&s.conversation_first_message_locks, &derived.card_id).await;
    if !user_message_already_enqueued(&w, track.id.as_str(), &derived.card_id).await? {
        // #1343 — on the launchpad track, and only there, the day's activity
        // window goes in **before** the user's first message.
        //
        // Before, because it is opening material: an agent whose first turn
        // holds the user's question and no context answers it from the
        // workspace, which is the state this injection exists to end. The
        // ordering is only as strong as the enqueue order — the harness may
        // fold both into one turn — but within that turn the briefing precedes
        // the question, which is the property that matters.
        //
        // Inside the claim and in one durable batch with the first message, not
        // as two independent sends. If the briefing committed and the user's
        // message failed, a retry would see "some user message" and permanently
        // skip the user's words. The batch persists both or restores both.
        //
        // **It is not part of the create operation's payload, and must not
        // become part of it.** `first_message_sha256` above binds the first
        // message into `payload_hash`; the counts change through the day, so a
        // briefing folded in there would make every retry under one
        // `Idempotency-Key` a permanent 409 (`operations` has no pruner). It
        // travels the ordinary message channel instead, which carries nothing
        // into the key.
        let opening = match briefing {
            OpeningBriefing::TodaysActivityOnTheLaunchpad => {
                launchpad_opening_briefing(&s, &w, track.id.as_str()).await?
            }
            OpeningBriefing::CallerSuppliesItsOwn => None,
        };
        let mut first_inputs = Vec::with_capacity(2);
        if let Some(briefing) = opening {
            first_inputs.push(briefing);
        }
        first_inputs.push(text);
        // The public one-message handler and this batch share validation,
        // harness recovery, durable persistence and audit in one implementation.
        let _queued = send_planner_inputs(
            s.clone(),
            w.clone(),
            cs,
            actor,
            derived.card_id.clone(),
            first_inputs,
        )
        .await?;
    }

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

/// Today's activity briefing, if this track is the launchpad (#1343).
///
/// `None` for every other track. The predicate is
/// [`routes::today::is_launchpad_track`], which is the one criterion in the
/// codebase — the agent's identity (`planner_harness_start_adapter`) forks on
/// the same call, and two spellings of "is this the launchpad?" would let the
/// briefing and the identity disagree about one track.
///
/// [`routes::today::is_launchpad_track`]: crate::routes::today::is_launchpad_track
///
/// The window itself comes from `activity_window::todays_workspace_activity`,
/// the same entry `POST /api/today/summary` uses, so the two surfaces cannot
/// report different numbers for one day. The launchpad excludes itself — that
/// is the reflexive exclusion documented on `workspace_activity_window`, so a
/// report the agent goes on to write does not turn up in the next briefing as
/// activity the workspace did.
///
/// **A workspace with no launchpad yet is `None`, not an empty briefing.**
/// Nothing is ensured from here: `ensure_today_launchpad` materialises a
/// workspace and waits on a `planner-harness-start` (INV-TODAYDOC-001), and a
/// conversation create on some other track has no business doing that.
async fn launchpad_opening_briefing(
    s: &RouteState,
    w: &WorkerState,
    track_id: &str,
) -> Result<Option<String>> {
    if !crate::routes::today::is_launchpad_track(s.repo.as_ref(), track_id).await? {
        return Ok(None);
    }
    let pool = w.repo.sqlite_pool().ok_or_else(|| {
        CalmError::Internal("today's activity window requires a sqlite-backed repo".into())
    })?;
    let activity = todays_workspace_activity(&pool, Some(track_id)).await?;
    Ok(Some(opening_activity_briefing(&activity)))
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
