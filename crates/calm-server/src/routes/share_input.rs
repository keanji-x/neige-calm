//! Text-only anonymous acceptance into the original canonical conversation.
use super::planner_input_send::SendPlannerInputResponse;
use crate::conversation_inputs::InputSource;
use crate::db::{sqlite, write_in_tx_typed};
use crate::error::{CalmError, Result};
use crate::event::{Event, EventScope};
use crate::extract::Json;
use crate::ids::{ActorId, CardId};
use crate::state::RouteState;
use axum::http::HeaderMap;
use calm_types::conversation::{AcceptedInputRef, HumanPayload, MessageAuthor};
pub(crate) async fn accept(
    s: &RouteState,
    access: crate::share_access::scope::ShareAccessScope,
    track: String,
    card: String,
    headers: HeaderMap,
    text: String,
) -> Result<Json<SendPlannerInputResponse>> {
    let key = super::idempotency_key::parse_idempotency_key_header(&headers)?
        .ok_or_else(|| CalmError::IdempotencyKeyInvalid("Idempotency-Key is required".into()))?;
    let chars = super::planner_cards::validate_planner_input(&text, false)?;
    let source = access.input_source()?;
    let _key_guard =
        crate::per_card_lock::lock_key(&s.planner_input_key_locks, &format!("{card}:{key}")).await;
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("share-input-before-accept", &card).await;
    let registry = s.harness.clone();
    let wake = source.clone();
    let (ack, event, handoff) = write_in_tx_typed(s.repo.as_ref(), move |tx| {
        Box::pin(async move {
            access.authorize_ingress_tx(tx, access.area(), Some(&track)).await?;
            source.authorize_tx(tx, &card).await?;
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
                    &serde_json::json!({"share_link_id":source.delegation().id,"text":text})
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
                #[cfg(feature = "fixtures")]
                crate::test_seams::pause_point("share-input-before-commit", &card).await;
                source.check_current()?;
                access.check_current()?;
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
                    text,
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
            #[cfg(feature = "fixtures")]
            crate::test_seams::pause_point("share-input-before-commit", &card).await;
            source.check_current()?;
            access.check_current()?;
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
            s.clone(),
            ack.card_id.clone(),
            InputSource::Share(wake),
            handoff,
        )
        .await;
    }
    Ok(Json(ack))
}
