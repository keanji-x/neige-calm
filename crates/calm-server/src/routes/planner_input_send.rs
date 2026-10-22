//! `POST /api/cards/{id}/planner/input` — one message into a harness card's queue.
//! Every send carries an `Idempotency-Key` (#2043). Its binding commits in the harness
//! transaction that stores the message, so a retry after a lost answer replays that answer
//! instead of queueing the message a second time. A send that names `replaces_turn` (Edit,
//! #1923) also removes that turn, in the same transaction.

use crate::actor::Actor;
use crate::db::sqlite::planner_input_binding_get;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::extract::{Json, JsonBody, Path};
use crate::harness::SendKey;
use crate::ids::{ActorId, CardId};
use crate::operation::planner_start_fence::CardStartFence;
use crate::per_card_lock::lock_key;
use crate::routes::idempotency_key::{parse_idempotency_key_header, stable_payload_hash};
use crate::routes::planner_cards::{card_runs_headless_harness, validate_planner_input};
use crate::routes::planner_session::dormant;
use crate::routes::track_report_blocks::require_rest_user_actor_for;
use crate::session_projection_repo::WorkerSessionProjection;
use crate::state::{CodexShellState, RouteState, WorkerState};

use axum::{extract::State, http::HeaderMap};
use calm_types::planner_attachment::AttachmentId;
use calm_types::worker::WorkerSessionId;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

mod human;

#[derive(Debug, Deserialize, ToSchema)]
pub struct SendPlannerInputRequest {
    pub text: String,
    /// Uploaded image IDs. Assistant canonical input binds them before acceptance;
    /// canonical Planner attachment input retains its existing refusal.
    #[serde(default)]
    pub attachments: Vec<AttachmentId>,
    /// Historical replacement intent. Canonical history replacement requires
    /// its owning operation and is currently refused.
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
    /// Canonical message identity or a real addressable new historical entry.
    /// Stored historical acknowledgements retain their original optional ID.
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

/// Select the persisted conversation contract before acceptance or replay.
/// Canonical errors never enter the historical queue. Historical public work
/// requires the current deployment owner and its existing Private resources.
#[utoipa::path(
    post,
    path = "/api/cards/{id}/planner/input",
    tag = "cards",
    params(
        ("id" = String, Path, description = "Harness conversation card id"),
        ("Idempotency-Key" = String, Header, description = "Required immutable key (1–128 visible ASCII bytes); canonical bindings include the verified author."),
    ),
    request_body = SendPlannerInputRequest,
    responses(
        (status = 200, description = "Input saved or its original receipt replayed after current authorization; execution may remain pending.", body = SendPlannerInputResponse),
        (status = 400, description = "Invalid text/request or missing/invalid Idempotency-Key (idempotency_key_invalid)", body = ErrorBody),
        (status = 401, description = "A verified current Human session source is required; invalid or revoked sessions are refused", body = ErrorBody),
        (status = 403, description = "Area collaboration required; historical input also requires deployment-owner Private-resource access.", body = ErrorBody),
        (status = 404, description = "Conversation or its owning Area was not found", body = ErrorBody),
        (status = 409, description = "Key conflict or unsupported input; no new message accepted. Reconcile uncertain outcomes with the original key.", body = ErrorBody),
        (status = 500, description = "Internal storage or response error; reconcile an uncertain outcome with the original key", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn send_planner_input(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    State(cs): State<CodexShellState>,
    source: crate::auth::HumanInputSource,
    (actor, headers): (Actor, HeaderMap),
    Path(id): Path<String>,
    JsonBody(body): JsonBody<SendPlannerInputRequest>,
) -> Result<Json<SendPlannerInputResponse>> {
    let idempotency_key = parse_idempotency_key_header(&headers)?.ok_or_else(|| {
        CalmError::BadRequest(
            "Idempotency-Key header is required so a retried send cannot queue the message twice"
                .into(),
        )
    })?;
    let contract =
        crate::conversation_inputs::human_input_storage_contract(s.repo.as_ref(), &source, &id)
            .await?;
    match contract {
        crate::conversation_inputs::InputStorageContract::Canonical => {
            human::accept(&s, source, actor, id, body, idempotency_key).await
        }
        crate::conversation_inputs::InputStorageContract::Historical => {
            send_keyed(
                &s,
                &w,
                &cs,
                actor,
                id,
                body,
                idempotency_key,
                Some(crate::conversation_inputs::HistoricalHumanSource::new(
                    source,
                )),
            )
            .await
        }
    }
    .map(Json)
}

/// Historical internal feature sends. They have no authenticated Human source
/// and cannot enter a canonical runtime through this body-queue path.
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
    send_keyed(s, w, cs, actor, id, body, idempotency_key, None).await
}

#[allow(clippy::too_many_arguments, deprecated)]
async fn send_keyed(
    s: &RouteState,
    w: &WorkerState,
    cs: &CodexShellState,
    actor: Actor,
    id: String,
    body: SendPlannerInputRequest,
    idempotency_key: String,
    source: Option<crate::conversation_inputs::HistoricalHumanSource>,
) -> Result<SendPlannerInputResponse> {
    if let Some(source) = &source {
        source.require_card(s.repo.as_ref(), &id).await?;
    } else {
        crate::conversation_inputs::require_historical_storage(s.repo.as_ref(), &id).await?;
    }
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
    if let Some(answer) = replay(w, &card.id, &key, source.as_ref()).await? {
        return Ok(answer);
    }
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point(
        crate::test_seams::PLANNER_INPUT_REPLAY_MISSED,
        card.id.as_str(),
    )
    .await;

    let track = s
        .repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {} for card {id}", card.track_id)))?;
    if let Some(runtime) = s.repo.session_projection_active_for_card(&id).await?
        && runtime.handle_state_json.as_ref().is_some_and(|value| {
            value
                .get("schema_version")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|version| version >= 2)
        })
    {
        return Err(CalmError::Conflict(
            "internal feature input requires its canonical source admission".into(),
        ));
    }
    let image_root = if attachments.is_empty() {
        None
    } else {
        let binding =
            crate::harness::profile::checked_card_binding(s.repo.as_ref(), &card, role).await?;
        Some(crate::planner_attachments::image_input_root(
            &binding.provider,
            &track.workspace,
            &s.workspace_root,
        )?)
    };
    // `_recovery_guard` holds the card's start fence (its recovery lock) until end of scope, so a concurrent `/planner/reset` can't supersede the just-recovered runtime before the observe/audit below.
    let (runtime, harness, _recovery_guard) = match replaces_turn {
        None => {
            super::planner_session::ensure_planner_session(
                s,
                w,
                cs,
                &card.id,
                &actor,
                source.clone(),
            )
            .await?
        }
        Some(_) => live_planner_harness(s, &card.id).await?,
    };
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
        let root = image_root
            .as_ref()
            .expect("nonempty images were admitted before recovery");
        crate::planner_attachments::bind::bind_attachments_admitted(
            root,
            &card.id,
            &attachments,
            &s.planner_attachment_locks,
            || async {
                if let Some(source) = &source {
                    source
                        .require_card(s.repo.as_ref(), card.id.as_str())
                        .await?;
                }
                Ok(())
            },
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
                .observe_user_message_admitted(text, attachments, key, source)
                .await?
        }
        Some(turn_id) => {
            harness
                .replace_turn_admitted(turn_id, text, attachments, key, source)
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
    source: Option<&crate::conversation_inputs::HistoricalHumanSource>,
) -> Result<Option<SendPlannerInputResponse>> {
    let source = source.cloned();
    let id = card_id.to_string();
    let raw_key = key.idempotency_key.clone();
    let binding = crate::db::write_in_tx_typed(w.repo.as_ref(), move |tx| {
        Box::pin(async move {
            if let Some(source) = source {
                source.require_card_tx(tx, &id).await?;
            } else {
                crate::conversation_inputs::require_historical_storage_tx(tx, &id).await?;
            }
            Ok(planner_input_binding_get(&mut **tx, &id, &raw_key).await?)
        })
    })
    .await?;
    let Some(binding) = binding else {
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
    let fence = CardStartFence::lock(
        &s.planner_recovery_locks,
        &s.repo,
        &s.operation_runtime,
        card_id,
    )
    .await;
    crate::conversation_archive::require_unarchived(s.repo.as_ref(), card_id.as_str()).await?;
    let runtime = s
        .repo
        .session_projection_active_for_card(&card_id.to_string())
        .await?
        .ok_or_else(dormant)?;
    let harness = s.harness.get(&runtime.id).ok_or_else(dormant)?;
    Ok((runtime, harness, Some(fence)))
}
