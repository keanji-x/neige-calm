//! The protected Human send transaction. The existing key table is its only
//! receipt; the existing canonical carrier is its only pending queue.
use super::*;
use crate::auth::HumanInputSource;
use crate::db::sqlite::conversation::{current_input_content_carrier_tx, message_tx};
use crate::db::sqlite::{PlannerInputBinding, planner_input_bind_tx};
use calm_types::conversation::{
    AcceptedInputRef, ConversationMessageId, HumanPayload, MessageAuthor,
};

pub(super) async fn accept(
    state: &RouteState,
    source: HumanInputSource,
    actor: Actor,
    card_id: String,
    request: SendPlannerInputRequest,
    raw_key: String,
) -> Result<SendPlannerInputResponse> {
    let char_count = validate_planner_input(&request.text, !request.attachments.is_empty())?;
    let source_for_wake = source.clone();
    let result = crate::db::write_in_tx_typed(state.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let principal = source.current_principal()?;
            let area = crate::conversation_inputs::authorize_human_tx(
                tx,
                &principal,
                &card_id,
            )
            .await?;
            // V1 is immutable historical evidence. A new source-bound key is
            // deliberately disjoint, even for identical words and Actor labels.
            let mut original = json!({
                "actor":actor.as_str(),
                "text":request.text,
                "attachments":request.attachments,
            });
            if let Some(turn) = &request.replaces_turn {
                original["replaces_turn"] = json!(turn);
            }
            let payload_hash = format!(
                "v2:{}",
                stable_payload_hash(&json!({
                    "author_user_id":principal.user_id,
                    "request":original,
                }))?,
            );
            if let Some(binding) =
                planner_input_binding_get(&mut **tx, &card_id, &raw_key).await?
            {
                if binding.payload_hash != payload_hash {
                    return Err(CalmError::IdempotencyKeyReused(
                        "this key names another original input or an unproven historical send"
                            .into(),
                    ));
                }
                let entry = binding.entry_id.as_deref().ok_or_else(|| {
                    CalmError::Internal("canonical send has no original message identity".into())
                })?;
                let id = ConversationMessageId::parse(entry)
                    .map_err(|error| CalmError::Internal(error.to_string()))?;
                let message = message_tx(tx, &id).await?;
                if message.card_id != card_id
                    || message.author
                        != (MessageAuthor::AuthenticatedHuman {
                            registered_user_id: principal.user_id,
                        })
                {
                    return Err(CalmError::Internal(
                        "canonical send receipt disagrees with its original author".into(),
                    ));
                }
                return Ok((response(card_id, binding), None));
            }
            if request.replaces_turn.is_some() || !request.attachments.is_empty() {
                return Err(CalmError::Conflict(
                    "canonical input currently accepts ordinary text; attachment and history operations require their owning admission"
                        .into(),
                ));
            }
            let mut carrier = current_input_content_carrier_tx(tx, &card_id).await?;
            let current_session = crate::db::sqlite::session_get_tx(
                tx,
                &calm_types::worker::WorkerSessionId(carrier.worker_session_id().into()),
            )
            .await?
            .ok_or_else(|| CalmError::Conflict("conversation carrier disappeared".into()))?;
            let current = source.current_principal()?;
            let accepted = crate::conversation_inputs::accept_human_tx(
                tx,
                &current,
                &mut carrier,
                HumanPayload {
                    text: request.text,
                    bound_attachments: vec![],
                },
            )
            .await?;
            let AcceptedInputRef::Human { message } = accepted.input_ref else {
                return Err(CalmError::Internal("Human send returned another input class".into()));
            };
            let binding = PlannerInputBinding {
                payload_hash,
                worker_session_id: carrier.worker_session_id().to_owned(),
                entry_id: Some(message.message_id.as_str().to_owned()),
            };
            planner_input_bind_tx(tx, &card_id, &raw_key, &binding).await?;
            let event = Event::HarnessUserMessageEnqueued {
                worker_session_id: binding.worker_session_id.clone(),
                card_id: CardId::from(card_id.clone()),
                track_id: current_session.track_id.clone(),
                char_count: char_count as u32,
            };
            let scope = EventScope::Card {
                card: CardId::from(card_id.clone()),
                track: current_session.track_id.clone(),
                area: area.id,
            };
            let event_id = crate::db::sqlite::append_decision_event_in_tx(
                tx, &ActorId::User, &scope, None, &event,
            ).await?;
            let envelope = crate::event::BroadcastEnvelope {
                id: event_id,
                event_version: crate::event::SYNC_EVENT_VERSION,
                actor: ActorId::User,
                scope,
                event,
            };
            Ok((response(card_id, binding), Some(envelope)))
        })
    })
    .await?;
    let (ack, fresh) = result;
    if let Some(envelope) = fresh {
        state.events.emit_envelope(envelope);
    }
    crate::operation::planner_start_fence::wake_canonical_human(
        state.clone(),
        ack.card_id.clone(),
        source_for_wake,
    );
    Ok(ack)
}

fn response(card_id: String, binding: PlannerInputBinding) -> SendPlannerInputResponse {
    SendPlannerInputResponse {
        card_id: CardId::from(card_id),
        worker_session_id: binding.worker_session_id,
        entry_id: binding.entry_id,
    }
}
