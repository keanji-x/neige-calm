//! `POST /api/cards/{id}/planner/input` — one message into a harness card's queue.
//! Every send carries an `Idempotency-Key` (#2043). Its binding commits in the harness
//! transaction that stores the message, so a retry after a lost answer replays that answer
//! instead of queueing the message a second time. A send that names `replaces_turn` (Edit,
//! #1923) also removes that turn, in the same transaction.

use crate::actor::Actor;
use crate::db::sqlite::planner_input_binding_get;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::extract::{JsonBody, Path};
use crate::harness::SendKey;
use crate::ids::{ActorId, CardId};
use crate::per_card_lock::lock_key;
use crate::routes::idempotency_key::{parse_idempotency_key_header, stable_payload_hash};
use crate::routes::planner_cards::{card_runs_headless_harness, validate_planner_input};
use crate::routes::planner_start_fence::CardStartFence;
use crate::routes::track_report_blocks::require_rest_user_actor_for;
use crate::session_projection_repo::WorkerSessionProjection;
use crate::state::{CodexShellState, RouteState, WorkerState};

use axum::{Json, extract::State, http::HeaderMap};
use calm_types::planner_attachment::AttachmentId;
use calm_types::worker::WorkerSessionId;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema)]
pub struct SendPlannerInputRequest {
    pub text: String,
    /// Ids returned by `POST /api/cards/{id}/planner/attachments`. Naming an attachment here is what BINDS it: the bytes move out of the sweepable staging area before this request writes anything to the queue.
    /// An id belonging to another card is a 400, as is naming the same one twice or naming more than eight.
    #[serde(default)]
    pub attachments: Vec<AttachmentId>,
    /// The `turnId` of the conversation's latest response, when this message replaces that turn
    /// (Edit). The turn's rows leave the conversation and this message is queued in one commit; a
    /// refusal is 409 `planner_turn_not_replaceable` and changes nothing. Person only.
    #[serde(default)]
    pub replaces_turn: Option<String>,
}

const REPLACE_SUBJECT: &str = "planner input that replaces a turn";
const REPLACE_REDIRECT: &str =
    "Replacing a turn deletes the person's own message and its replies; agents have no path to it.";

#[derive(Debug, Serialize, ToSchema)]
pub struct SendPlannerInputResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    pub worker_session_id: String,
    /// Stable id of the queue entry this text landed in, so the client can match its optimistic echo. Null only when the text folded into a pre-id queue entry.
    pub entry_id: Option<String>,
}

fn planner_input_audit_actor(actor: &Actor, card_id: &CardId) -> ActorId {
    match actor.to_actor_id() {
        ActorId::AiCodex(c) if c.as_str().is_empty() => ActorId::AiCodex(card_id.clone()),
        // Middleware currently only admits `ai:codex`; the other branches are ready for more AI kinds.
        ActorId::AiClaude(c) if c.as_str().is_empty() => ActorId::AiClaude(card_id.clone()),
        ActorId::AiPlanner(c) if c.as_str().is_empty() => ActorId::AiPlanner(card_id.clone()),
        other => other,
    }
}

/// Queue one message for the card's harness, under a required `Idempotency-Key`.
///
/// The first request under a key that the server stores binds the key to that message, in the
/// transaction that stores it. A retry with the same key and the same body (text, attachments,
/// `replaces_turn`, actor) answers 200 with the first request's body and queues nothing, whatever
/// happened to the message since. The same key with a different body is 409 `idempotency_key_reused`. A
/// refusal stores and binds nothing, so its key can be sent again. A binding lasts as long as its card.
///
/// With `replaces_turn`, the named turn must be the conversation's latest, finished, with nothing
/// queued: it is removed and this message queued in the transaction that binds the key, and the
/// provider drops the turn when the next one starts. There is no lazy recovery on this path.
///
/// A person's plain send to a card that nothing else can start any more and that has no thread or
/// transcript to preserve (a kernel-managed Track's card, whose creation starts no model, or a
/// conversation whose own minting start failed) starts its conversation first, then queues it.
#[utoipa::path(
    post,
    path = "/api/cards/{id}/planner/input",
    tag = "cards",
    params(
        ("id" = String, Path, description = "Planner card id"),
        ("Idempotency-Key" = String, Header, description = "**Required.** One key per message, at most 128 ASCII bytes; a retry under it replays the first answer."),
    ),
    request_body = SendPlannerInputRequest,
    responses(
        (status = 200, description = "User text queued for next harness turn, or the answer of the earlier request under this Idempotency-Key", body = SendPlannerInputResponse),
        (status = 400, description = "Empty text, a blank `replaces_turn`, or a missing or invalid Idempotency-Key (`idempotency_key_invalid`)", body = ErrorBody),
        (status = 403, description = "Card is not a planner codex card, or `replaces_turn` from an actor other than `X-Calm-Actor: user`", body = ErrorBody),
        (status = 404, description = "Card or track not found", body = ErrorBody),
        (status = 409, description = "Distinguished by `code`:\n* `idempotency_key_reused` — this Idempotency-Key was already used for a different message on this card (text, attachments, `replaces_turn` or actor); final, send the new message under a new key.\n* `idempotency_key_concurrent` — another request under this key was stored at the same moment and nothing of this one was; send it again under the same key to receive that answer.\n* `planner_harness_dormant` — the planner harness session is dormant and not recoverable; reset to start a session.\n* `planner_harness_runtime_superseded` — on a plain send, the runtime is no longer this card's and the text was NOT stored, so re-sending it reaches the successor.\n* `planner_turn_not_replaceable` — on a send with `replaces_turn`, the turn cannot be replaced now (not the latest, still running, messages waiting, the conversation shutting down or no longer the card's, or the provider refusing the cut) and nothing changed; the body carries the reason.\n* `conflict` — the conversation's run loop is shutting down, already closed, or stopped before answering, on a plain send and on a send with `replaces_turn` alike. This answer does not say whether the message was stored or the turn replaced: a loop that stopped before answering may have committed it. Send it again under the same key.", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
        (status = 503, description = "Observation queue saturated, shared codex app-server not running, a planner-harness start is still in flight, or the provider did not check a replace in time — retry shortly", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn send_planner_input(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    State(cs): State<CodexShellState>,
    actor: Actor,
    headers: HeaderMap,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<SendPlannerInputRequest>,
) -> Result<Json<SendPlannerInputResponse>> {
    let idempotency_key = parse_idempotency_key_header(&headers)?.ok_or_else(|| {
        CalmError::BadRequest(
            "Idempotency-Key header is required so a retried send cannot queue the message twice"
                .into(),
        )
    })?;
    send_planner_input_keyed(&s, &w, &cs, actor, id, body, idempotency_key)
        .await
        .map(Json)
}

/// The send behind the route, for callers inside the server that mint their own key.
#[allow(deprecated)]
pub(crate) async fn send_planner_input_keyed(
    s: &RouteState,
    w: &WorkerState,
    cs: &CodexShellState,
    actor: Actor,
    id: String,
    body: SendPlannerInputRequest,
    idempotency_key: String,
) -> Result<SendPlannerInputResponse> {
    let SendPlannerInputRequest {
        text,
        attachments,
        replaces_turn,
    } = body;
    if let Some(turn_id) = &replaces_turn {
        require_rest_user_actor_for(&actor, REPLACE_SUBJECT, REPLACE_REDIRECT)?;
        if turn_id.trim().is_empty() {
            return Err(CalmError::BadRequest(
                "replaces_turn names no turn; omit it to send a new message".into(),
            ));
        }
    }
    let char_count = validate_planner_input(&text, !attachments.is_empty())?;

    let card = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    let role = s
        .write
        .verify_role(&card.id)
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    if !card_runs_headless_harness(&card, role) {
        return Err(CalmError::Forbidden(format!(
            "card {id} is not a planner codex card",
        )));
    }

    // Decided before anything with an effect: a retry's attachments are already bound, and a lazy
    // restart would recover a runtime this request no longer needs. A plain send hashes exactly as
    // before `replaces_turn` existed, so its stored keys still match.
    let mut hashed = json!({
        "actor": actor.as_str(),
        "text": &text,
        "attachments": &attachments,
    });
    if let Some(turn_id) = &replaces_turn {
        hashed["replaces_turn"] = json!(turn_id);
    }
    let key = SendKey {
        payload_hash: format!("v1:{}", stable_payload_hash(&hashed)?),
        idempotency_key,
    };
    let _key_guard = lock_key(
        &s.planner_input_key_locks,
        &format!("{}\u{0}{}", card.id, key.idempotency_key),
    )
    .await;
    if let Some(answer) = replay(w, &card.id, &key).await? {
        return Ok(answer);
    }
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point(
        crate::test_seams::PLANNER_INPUT_REPLAY_MISSED,
        card.id.as_str(),
    )
    .await;

    // `_recovery_guard` holds the card's start fence (its recovery lock) until end of scope, so a concurrent `/planner/reset` can't supersede the just-recovered runtime before the observe/audit below.
    let (runtime, harness, _recovery_guard) = match replaces_turn {
        None => super::planner_session::ensure_planner_session(s, w, cs, &card.id, &actor).await?,
        Some(_) => live_planner_harness(s, &card.id).await?,
    };
    let track = s
        .repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {} for card {id}", card.track_id)))?;
    let scope = EventScope::Card {
        card: card.id.clone(),
        track: track.id.clone(),
        area: track.area_id.clone(),
    };
    // Bind BEFORE the entry exists: the bytes leave the sweepable staging directory first, so a queued message never names a file the orphan sweep may remove (codex answers an unreadable image with placeholder text and no error). A bind followed by a failed enqueue leaks bytes in `bound/`, which is the better failure direction.
    // `attachment_root` refuses an attached workspace, so such a card gets a 400 here only when it actually names an attachment.
    let attachments = if attachments.is_empty() {
        Vec::new()
    } else {
        let root =
            crate::planner_attachments::attachment_root(&track.workspace, &s.workspace_root)?;
        crate::planner_attachments::bind::bind_attachments(
            &root,
            &card.id,
            &attachments,
            &s.planner_attachment_locks,
        )
        .await?
    };
    // Migrate ONLY the AI-header path (empty placeholder card) to the live planner session actor; the human path MUST stay unchanged so the audit log keeps distinguishing human input from agent actions.
    let audit_actor = match actor.to_actor_id() {
        ActorId::AiCodex(c) | ActorId::AiClaude(c) | ActorId::AiPlanner(c)
            if c.as_str().is_empty() && runtime.status.is_active_authority() =>
        {
            ActorId::AiPlannerSession(WorkerSessionId::from(runtime.id.clone()))
        }
        _ => planner_input_audit_actor(&actor, &card.id),
    };

    let attachment_count = attachments.len();
    let replaced = replaces_turn.clone();
    let ack = match replaces_turn {
        None => {
            harness
                .observe_user_message_durable(text, attachments, key)
                .await?
        }
        Some(turn_id) => {
            harness
                .replace_turn_durable(turn_id, text, attachments, key)
                .await?
        }
    };

    tracing::info!(
        actor = %actor.as_str(),
        card_id = %card.id,
        runtime_id = %runtime.id,
        char_count,
        attachment_count,
        replaced_turn = replaced.as_deref(),
        "planner harness user message enqueued"
    );

    if let Err(error) = s
        .repo
        .log_pure_event(
            audit_actor,
            scope,
            None,
            &s.events,
            s.write.role_cache(),
            s.write.area_cache(),
            Event::HarnessUserMessageEnqueued {
                worker_session_id: runtime.id.clone(),
                card_id: card.id.clone(),
                track_id: card.track_id.clone(),
                char_count: char_count as u32,
            },
        )
        .await
    {
        // The user message is already durably accepted; a 500 here would invite a retry that executes the same intent twice.
        tracing::error!(
            card_id = %card.id,
            runtime_id = %runtime.id,
            error = %error,
            "planner input was accepted but its audit event failed"
        );
    }

    Ok(SendPlannerInputResponse {
        card_id: card.id,
        worker_session_id: runtime.id.clone(),
        entry_id: ack.entry_id.map(|id| id.as_str().to_string()),
    })
}

/// The answer an earlier request under this key was given, if one was stored. The answer is a
/// fact about that request and is replayed as it was, wherever its entry has gone since.
async fn replay(
    w: &WorkerState,
    card_id: &CardId,
    key: &SendKey,
) -> Result<Option<SendPlannerInputResponse>> {
    let pool = w.repo.sqlite_pool().ok_or_else(|| {
        CalmError::Internal("planner input keys require a sqlite-backed repo".into())
    })?;
    let Some(binding) =
        planner_input_binding_get(&pool, card_id.as_str(), &key.idempotency_key).await?
    else {
        return Ok(None);
    };
    if binding.payload_hash != key.payload_hash {
        return Err(CalmError::IdempotencyKeyReused(
            "This Idempotency-Key was already used for a different message on this card; send \
             the new message under a new key."
                .into(),
        ));
    }
    Ok(Some(SendPlannerInputResponse {
        card_id: card_id.clone(),
        worker_session_id: binding.worker_session_id,
        entry_id: binding.entry_id,
    }))
}

/// The live harness of a card, for a send that replaces a turn: no lazy recovery, since a harness
/// that needs recovering holds no turn that could be removed safely, so a miss is 409
/// `planner_harness_dormant`. The per-card recovery lock is held and returned, so no send, reset
/// or recovery moves the runtime underneath.
#[allow(deprecated)]
async fn live_planner_harness(
    s: &RouteState,
    card_id: &CardId,
) -> Result<(
    WorkerSessionProjection,
    crate::harness::PlannerHarness,
    Option<CardStartFence>,
)> {
    let fence = CardStartFence::lock(s, card_id).await;
    let dormant = || {
        CalmError::PlannerHarnessDormant(format!(
            "no live planner harness session for card {card_id}; reset to start a session",
        ))
    };
    let runtime = s
        .repo
        .session_projection_active_for_card(&card_id.to_string())
        .await?
        .ok_or_else(dormant)?;
    let harness = s.harness.get(&runtime.id).ok_or_else(dormant)?;
    Ok((runtime, harness, Some(fence)))
}
