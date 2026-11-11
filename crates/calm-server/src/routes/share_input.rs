//! Text-only anonymous acceptance into the original canonical conversation.
use super::planner_input_send::SendPlannerInputResponse;
use crate::auth::{AuthState, ValidatedApplicationAuth};
use crate::conversation_inputs::{InputSource, ShareInputSource};
use crate::db::{sqlite, write_in_tx_typed};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::extract::{Json, JsonBody, Path};
use crate::ids::{ActorId, CardId};
use crate::state::RouteState;
use axum::{Extension, extract::State, http::HeaderMap};
use calm_types::conversation::{AcceptedInputRef, HumanPayload, MessageAuthor};
use serde::Deserialize;
use utoipa::ToSchema;

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ShareInputRequest {
    pub text: String,
}

#[utoipa::path(post, path="/api/share/{token}/tracks/{track_id}/conversations/{card_id}/input", tag="share_links", security(()),
    params(("token"=String,Path),("track_id"=String,Path),("card_id"=String,Path),
        ("Idempotency-Key"=String,Header,description="Required source-bound receipt key")),
    request_body=ShareInputRequest,
    responses(
        (status=200,description="Accepted or active source receipt replayed",body=SendPlannerInputResponse),
        (status=400,description="Invalid text or key",body=ErrorBody),
        (status=401,description="Deployment issuer unavailable",body=ErrorBody),
        (status=403,description="Share authority lost",body=ErrorBody),
        (status=404,description="Active shared conversation unavailable",body=ErrorBody),
        (status=409,description="Historical conversation or key conflict",body=ErrorBody),
        (status=500,description="Internal error",body=ErrorBody)
    ))]
pub(crate) async fn send(
    State(s): State<RouteState>,
    Extension(auth): Extension<AuthState>,
    Path((token, track, card)): Path<(String, String, String)>,
    headers: HeaderMap,
    JsonBody(request): JsonBody<ShareInputRequest>,
) -> Result<Json<SendPlannerInputResponse>> {
    let key = super::idempotency_key::parse_idempotency_key_header(&headers)?
        .ok_or_else(|| CalmError::IdempotencyKeyInvalid("Idempotency-Key is required".into()))?;
    let chars = super::planner_cards::validate_planner_input(&request.text, false)?;
    let read =
        crate::share_access::resolve(s.repo.as_ref(), &auth.config, &token, Some(&track)).await?;
    let validated =
        ValidatedApplicationAuth::from_state(auth).map_err(|_| CalmError::Unauthorized)?;
    let source = ShareInputSource::new(validated.execution, read.link.delegation);
    // The raw token is never captured by storage, events, receipts or the wake.
    drop(token);
    let _key_guard =
        crate::per_card_lock::lock_key(&s.planner_input_key_locks, &format!("{card}:{key}")).await;
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("share-input-before-accept", &card).await;
    let registry = s.harness.clone();
    let wake = source.clone();
    let (ack, event, handoff) = write_in_tx_typed(s.repo.as_ref(), move |tx| {
        Box::pin(async move {
            source.authorize_tx(tx, &card).await?;
            let link = sqlite::share_links::get_tx(tx, &source.delegation().id).await?;
            if link.revoked_at.is_some() {
                return Err(CalmError::NotFound("share link".into()));
            }
            let actual: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
                .bind(&card)
                .fetch_one(&mut **tx)
                .await?;
            if actual != track {
                return Err(CalmError::NotFound("shared conversation".into()));
            }
            let shape = sqlite::card_execution_shape_tx(tx, &card).await?;
            crate::harness::profile::PlannerBinding::from_shape(
                &shape.kind,
                shape.role,
                &shape.payload,
            )
            .filter(|b| {
                matches!(
                    b.profile,
                    crate::harness::profile::HarnessProfile::Planner
                        | crate::harness::profile::HarnessProfile::Assistant
                )
            })
            .filter(crate::harness::backend::factory::PlannerFactory::canonical_inputs)
            .ok_or_else(|| CalmError::Conflict("canonical Planner or Assistant required".into()))?;
            if !sqlite::conversation::card_has_canonical_storage(tx, &card).await? {
                return Err(CalmError::Conflict("canonical conversation storage required".into()));
            }
            let hash = format!(
                "{}{}",
                sqlite::CANONICAL_PLANNER_INPUT_BINDING_PREFIX,
                super::idempotency_key::stable_payload_hash(
                    &serde_json::json!({"share_link_id":source.delegation().id,"text":request.text})
                )?
            );
            let author = MessageAuthor::LinkGuest {
                share_link_id: source.delegation().id.clone(),
            };
            if let Some(receipt) = sqlite::planner_input_binding_get(&mut **tx, &card, &key).await?
            {
                if receipt.payload_hash != hash {
                    return Err(CalmError::IdempotencyKeyReused(
                        "key names another source or input".into(),
                    ));
                }
                let id = calm_types::conversation::ConversationMessageId::parse(
                    receipt
                        .entry_id
                        .as_deref()
                        .ok_or_else(|| CalmError::Internal("missing message receipt".into()))?,
                )
                .map_err(|e| CalmError::Internal(e.to_string()))?;
                let message = sqlite::conversation::message_tx(tx, &id).await?;
                if message.card_id != card || message.author != author {
                    return Err(CalmError::Conflict("receipt author changed".into()));
                }
                // Receipt evidence survives loss of the current carrier. This
                // optional pointer only scopes a transient handoff.
                let target: Option<(String, String)> = sqlx::query_as(
                    "SELECT ws.id,ws.track_id FROM cards c JOIN worker_sessions ws ON ws.id=c.session_id AND ws.card_id=c.id WHERE c.id=?1",
                ).bind(&card).fetch_optional(&mut **tx).await?;
                let handoff = target.map(|(worker, track)| {
                    registry.canonical_sources(&worker, &track.into()).begin_handoff()
                });
                source.check_current()?;
                return Ok((
                    SendPlannerInputResponse {
                        card_id: card.into(),
                        worker_session_id: receipt.worker_session_id,
                        entry_id: receipt.entry_id,
                    },
                    None,
                    handoff,
                ));
            }
            let mut carrier =
                sqlite::conversation::current_input_content_carrier_tx(tx, &card).await?;
            let handoff = registry
                .canonical_sources(carrier.worker_session_id(), &track.clone().into())
                .begin_handoff();
            let accepted = sqlite::conversation::accept_pending_message_tx(
                tx,
                &mut carrier,
                &author,
                HumanPayload {
                    text: request.text,
                    bound_attachments: vec![],
                },
            )
            .await?;
            let AcceptedInputRef::Human { message } = accepted.input_ref else {
                return Err(CalmError::Internal("guest acceptance lost message".into()));
            };
            let receipt = sqlite::PlannerInputBinding {
                payload_hash: hash,
                worker_session_id: carrier.worker_session_id().into(),
                entry_id: Some(message.message_id.to_string()),
            };
            sqlite::planner_input_bind_tx(tx, &card, &key, &receipt).await?;
            let area = sqlite::conversation::conversation_area_tx(tx, &card).await?;
            let scope = EventScope::Card {
                card: card.clone().into(),
                track: track.clone().into(),
                area: area.id,
            };
            let event = Event::HarnessUserMessageEnqueued {
                worker_session_id: receipt.worker_session_id.clone(),
                card_id: card.clone().into(),
                track_id: track.into(),
                char_count: chars as u32,
            };
            let id = sqlite::append_decision_event_in_tx(tx, &ActorId::User, &scope, None, &event)
                .await?;
            source.check_current()?;
            Ok((
                SendPlannerInputResponse {
                    card_id: CardId::from(card),
                    worker_session_id: receipt.worker_session_id,
                    entry_id: receipt.entry_id,
                },
                Some(crate::event::BroadcastEnvelope {
                    id,
                    event_version: crate::event::SYNC_EVENT_VERSION,
                    actor: ActorId::User,
                    scope,
                    event,
                }),
                Some(handoff),
            ))
        })
    })
    .await?;
    if let Some(event) = event {
        s.events.emit_envelope(event);
    }
    if let Some(handoff) = handoff {
        crate::operation::planner_start_fence::finish_canonical_human_handoff(
            s,
            ack.card_id.clone(),
            InputSource::Share(wake),
            handoff,
        )
        .await;
    }
    Ok(Json(ack))
}
