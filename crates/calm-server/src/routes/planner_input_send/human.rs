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
    let (bound_attachments, bound_target) = if request.attachments.is_empty() {
        (vec![], None)
    } else {
        let (card, admission, key) = (card_id.clone(), source.clone(), raw_key.clone());
        let target = crate::db::write_in_tx_typed(state.repo.as_ref(), move |tx| {
            Box::pin(async move {
                crate::conversation_inputs::authorize_human_tx(
                    tx,
                    &admission.current_principal()?,
                    &card,
                )
                .await?;
                if planner_input_binding_get(&mut **tx, &card, &key)
                    .await?
                    .is_some()
                {
                    return Ok(None);
                }
                let shape = crate::db::sqlite::card_execution_shape_tx(tx, &card).await?;
                let binding = crate::harness::profile::PlannerBinding::from_shape(
                    &shape.kind,
                    shape.role,
                    &shape.payload,
                )
                .filter(|b| b.profile == crate::harness::profile::HarnessProfile::Assistant)
                .ok_or_else(|| {
                    CalmError::Conflict("canonical attachments require the Assistant entry".into())
                })?;
                let track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
                    .bind(&card)
                    .fetch_one(&mut **tx)
                    .await?;
                Ok(Some((
                    binding,
                    crate::db::sqlite::track_get_tx(tx, &track.into()).await?,
                )))
            })
        })
        .await?;
        if let Some((binding, track)) = target {
            let root = crate::planner_attachments::image_input_root(
                &binding.provider,
                &track.workspace,
                &state.workspace_root,
            )?;
            let attachments = crate::planner_attachments::bind::bind_attachments_admitted(
                &root,
                &CardId::from(card_id.clone()),
                &request.attachments,
                &state.planner_attachment_locks,
                || async {
                    let (source, card) = (source.clone(), card_id.clone());
                    crate::db::write_in_tx_typed(state.repo.as_ref(), move |tx| {
                        Box::pin(async move {
                            crate::conversation_inputs::authorize_human_tx(
                                tx,
                                &source.current_principal()?,
                                &card,
                            )
                            .await?;
                            Ok(())
                        })
                    })
                    .await
                },
            )
            .await?;
            (attachments, Some((binding, track.workspace)))
        } else {
            (vec![], None)
        }
    };
    let registry = state.harness.clone();
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
                "{}{}",
                crate::db::sqlite::CANONICAL_PLANNER_INPUT_BINDING_PREFIX,
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
                // A receipt is immutable evidence, independent of whether a
                // current execution owner remains. This optional pointer lookup
                // only scopes a transient handoff; it grants no execution.
                let target: Option<(String, String)> = sqlx::query_as(
                    "SELECT ws.id,ws.track_id FROM cards c JOIN worker_sessions ws ON ws.id=c.session_id AND ws.card_id=c.id WHERE c.id=?1",
                ).bind(&card_id).fetch_optional(&mut **tx).await?;
                let handoff = target.map(|(worker, track)| registry.canonical_sources(&worker, &track.into()).begin_handoff());
                return Ok((response(card_id, binding), None, handoff));
            }
            let mut carrier = current_input_content_carrier_tx(tx, &card_id).await?;
            let current_session = crate::db::sqlite::session_get_tx(
                tx, &calm_types::worker::WorkerSessionId(carrier.worker_session_id().into()),
            ).await?.ok_or_else(|| CalmError::Conflict("conversation carrier disappeared".into()))?;
            let handoff = registry.canonical_sources(carrier.worker_session_id(), &current_session.track_id).begin_handoff();
            // A replay above retains its original acknowledgement. Fresh input
            // requires a statically valid Planner, independently of native readiness.
            let shape = crate::db::sqlite::card_execution_shape_tx(tx, &card_id).await?;
            let binding = crate::harness::profile::PlannerBinding::from_shape(
                &shape.kind, shape.role, &shape.payload,
            ).filter(|binding| matches!(binding.profile, crate::harness::profile::HarnessProfile::Planner | crate::harness::profile::HarnessProfile::Assistant))
                .ok_or_else(|| CalmError::Conflict("Planner provider binding is invalid".into()))?;
            if request.replaces_turn.is_some() {
                return Err(CalmError::Conflict(
                    "canonical input currently accepts ordinary text; attachment and history operations require their owning admission"
                        .into(),
                ));
            }
            if let Some((expected, workspace)) = bound_target {
                let track = crate::db::sqlite::track_get_tx(tx, &current_session.track_id).await?;
                if binding != expected || track.workspace != workspace {
                    return Err(CalmError::Conflict("attachment target changed before acceptance".into()));
                }
            }
            let current = source.current_principal()?;
            let accepted = crate::conversation_inputs::accept_human_tx(
                tx,
                &current,
                &mut carrier,
                HumanPayload {
                    text: request.text,
                    bound_attachments,
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
            Ok((response(card_id, binding), Some(envelope), Some(handoff)))
        })
    })
    .await?;
    let (ack, fresh, handoff) = result;
    if let Some(envelope) = fresh {
        state.events.emit_envelope(envelope);
    }
    if let Some(handoff) = handoff {
        crate::operation::planner_start_fence::finish_canonical_human_handoff(
            state.clone(),
            ack.card_id.clone(),
            source_for_wake,
            handoff,
        )
        .await;
    }
    Ok(ack)
}

fn response(card_id: String, binding: PlannerInputBinding) -> SendPlannerInputResponse {
    SendPlannerInputResponse {
        card_id: CardId::from(card_id),
        worker_session_id: binding.worker_session_id,
        entry_id: binding.entry_id,
    }
}
