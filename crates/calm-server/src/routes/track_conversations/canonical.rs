//! Assistant creation composes the existing acceptance/carrier owners before wake.
use super::*;
use crate::db::sqlite::conversation::{self, AssistantCreationRecord};
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use calm_types::conversation::{AcceptedInputRef, HumanPayload};
use calm_types::conversation_receipt::{
    ConversationInitialInputReceipt, ConversationInitialInputReceiptResult,
    InitialInputReceiptVersion,
};

#[allow(deprecated)]
pub(super) async fn accept(
    state: &RouteState,
    worker: &WorkerState,
    source: crate::auth::HumanInputSource,
    binding: crate::harness::profile::PlannerBinding,
    payload: &PlannerHarnessStartOperationPayload,
    request_sha256: String,
) -> Result<ConversationInitialInputReceiptResult> {
    let for_wake = source.clone();
    let payload = payload.clone();
    let card_id = payload.planner_card_id.clone();
    let repo = worker.repo.clone();
    let roles = state.write.role_cache().clone();
    let legacy_state = state.clone();
    // Like dormant Track creation, replay is a checked read-only rollback:
    // the event owner deliberately refuses an empty event batch.
    let (replay_tx, mut replay_rx) = tokio::sync::oneshot::channel();
    let (legacy_tx, mut legacy_rx) = tokio::sync::oneshot::channel();
    let result = crate::db::write_with_actor_events_typed(
        state.repo.as_ref(), None, &state.events, &state.write, move |tx| {
            Box::pin(async move {
                let track = crate::db::sqlite::track_get_tx(tx, &payload.track_id.clone().into()).await?;
                let principal = source.current_principal()?;
                let area = crate::organization_access::require_area_access_tx(
                    tx, track.area_id.as_str(), &principal,
                    calm_types::organization::AreaAccess::Collaborate,
                ).await?;
                if let Some(original) = conversation::assistant_creation_get_tx(tx, payload.planner_card_id.as_str()).await? {
                    if original.request_sha256 != request_sha256 || original.source_user_id != principal.user_id {
                        return Err(CalmError::IdempotencyKeyReused("this key names another original Assistant request".into()));
                    }
                    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cards WHERE id=?1 AND track_id=?2)")
                        .bind(&original.card_id).bind(&original.track_id).fetch_one(&mut **tx).await?;
                    if !exists {
                        return Err(CalmError::IdempotencyKeyExhausted("the original Assistant no longer exists".into()));
                    }
                    let _ = replay_tx.send(original.receipt);
                    return Err(CalmError::Conflict("Assistant acceptance already committed".into()));
                }
                let seed = payload.create_card.as_ref().ok_or_else(|| CalmError::Internal("Assistant seed missing".into()))?;
                let key = seed.idempotency_key.as_deref().ok_or_else(|| CalmError::Internal("Assistant key missing".into()))?;
                let derived = derive_track_conversation_keys(&payload.track_id, key);
                // Permanent historical journals are factual replay evidence, never
                // authority to adopt the key as a new canonical acceptance or retry it.
                if super::super::conversations_shared::lookup_start_operation(&legacy_state, &derived.operation_key).await?.is_some() {
                    let (_, original) = super::super::conversations_shared::retryable_operation(&legacy_state, &derived.operation_key).await?;
                    let original = original.ok_or_else(|| CalmError::Conflict("historical_result_unprovable: Assistant creation incomplete".into()))?;
                    if original.payload_hash != request_sha256 {
                        return Err(CalmError::IdempotencyKeyReused("this key names another original request".into()));
                    }
                    let receipt = super::super::conversation_initial_input::receipt_from_historical_operation(
                        &legacy_state, &payload.track_id, payload.planner_card_id.as_str(), key, &derived.operation_key,
                    ).await?.map_err(|reason| CalmError::Conflict(format!("historical_result_unprovable: Assistant create {reason:?}")))?;
                    let _ = legacy_tx.send(receipt);
                    return Err(CalmError::Conflict("historical Assistant result replayed".into()));
                }
                let context = crate::side_conversation::opening_material(
                    repo.as_ref(), &payload.track_id, seed.side.as_ref(), payload.opening_briefing,
                ).await?;
                if let Some(side) = &seed.side { side.validate_tx(tx, &payload.track_id).await?; }
                let mut card_payload = serde_json::json!({"schemaVersion":1,"harness_profile":binding.profile,"assistant_provider":binding.provider});
                if let Some(context) = context {
                    card_payload[crate::validation::ASSISTANT_OPENING_CONTEXT_PAYLOAD_KEY] = serde_json::json!(context);
                }
                if let Some(side) = &seed.side {
                    card_payload[crate::validation::SIDE_SOURCE_CARD_PAYLOAD_KEY] = serde_json::json!(side.source_card_id);
                }
                crate::planner_model::CardModelSelection::apply_to_payload(
                    card_payload.as_object_mut().expect("object"), seed.model.as_deref(), seed.reasoning_effort.as_deref(),
                );
                let card = crate::db::sqlite::card_create_with_id_tx(
                    tx, payload.planner_card_id.to_string(), crate::model::NewCard {
                        track_id: track.id.clone(), kind:"codex".into(), sort:seed.sort,
                        payload:card_payload,title:seed.title.clone(),
                    }, CardRole::Assistant, false, &roles,
                ).await?;
                let watermark: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id),0) FROM events").fetch_one(&mut **tx).await?;
                let worker = crate::model::new_id();
                crate::db::sqlite::session_start_runtime_tx(tx,
                    crate::session_projection_repo::WorkerSessionInit::managed_assistant(
                        worker.clone(), card.id.to_string(), binding.provider,
                        WorkerSessionState::Idle, None,
                        serde_json::to_value(crate::harness::snapshot::CanonicalHarnessSnapshot::dormant(watermark))?, crate::model::now_ms(),
                    ),
                ).await?;
                let mut carrier = conversation::current_input_carrier_tx(tx, card.id.as_str(), &worker).await?;
                let accepted = crate::conversation_inputs::accept_human_tx(tx, &source.current_principal()?, &mut carrier,
                    HumanPayload { text:payload.first_message.clone().ok_or_else(|| CalmError::Internal("first input missing".into()))?,bound_attachments:vec![] },
                ).await?;
                let AcceptedInputRef::Human { message } = accepted.input_ref else { return Err(CalmError::Internal("unexpected acceptance class".into())); };
                let receipt = ConversationInitialInputReceipt {
                    version: InitialInputReceiptVersion::V1, entry_id:message.message_id.as_str().into(),
                    entry_rev:message.revision.get(), queued_at_ms:carrier.checkpoint().pending[0].queued_at_ms(),
                    text_sha256:crate::conversation_keys::first_message_digest(payload.first_message.as_deref().expect("checked")),
                    initial_runtime_id:worker,
                };
                conversation::assistant_creation_insert_tx(tx, &AssistantCreationRecord {
                    card_id:card.id.to_string(),track_id:track.id.to_string(),request_sha256,
                    source_user_id:principal.user_id,presentation_sequence:accepted.presentation_sequence,
                    receipt:receipt.clone(),
                }).await?;
                source.current_principal()?;
                let scope = EventScope::Card {card:card.id.clone(),track:track.id.clone(),area:area.id};
                Ok(((receipt, true), vec![(ActorId::User,scope.clone(),Event::CardAdded(card.clone())),
                    (ActorId::User,scope,Event::HarnessPresentationChanged {
                        card_id:card.id.clone(),track_id:track.id,
                        cursor:calm_types::conversation_display::ConversationCursor::new(card.id.as_str(),calm_types::conversation_display::ConversationCursorSource::Presentation,accepted.presentation_sequence),
                    })]))
            })
        },
    ).await;
    let (receipt, fresh) = match result {
        Ok((accepted, _)) => accepted,
        Err(error @ CalmError::Conflict(_)) => {
            if let Ok(original) = legacy_rx.try_recv() {
                return Ok(original);
            }
            (replay_rx.try_recv().map_err(|_| error)?, false)
        }
        Err(error) => return Err(error),
    };
    // Only fresh acceptance schedules activation. Replaying a receipt has no execution effect.
    if fresh {
        crate::operation::planner_start_fence::wake_canonical_human(
            state.clone(),
            card_id,
            for_wake,
        );
    }
    Ok(ConversationInitialInputReceiptResult::Known { receipt })
}

/// A canonical create key cannot be reused by a still-historical provider entry.
/// Check the permanent fact before mutable provider admission; no format fallback.
pub(super) async fn require_unused_historical_key(
    state: &RouteState,
    source: crate::auth::HumanInputSource,
    track_id: String,
    card_id: String,
) -> Result<()> {
    crate::db::write_in_tx_typed(state.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let track = crate::db::sqlite::track_get_tx(tx, &track_id.into()).await?;
            crate::organization_access::require_area_access_tx(
                tx,
                track.area_id.as_str(),
                &source.current_principal()?,
                calm_types::organization::AreaAccess::Collaborate,
            )
            .await?;
            if conversation::assistant_creation_get_tx(tx, &card_id)
                .await?
                .is_some()
            {
                return Err(CalmError::IdempotencyKeyReused(
                    "this key names an original canonical Assistant request".into(),
                ));
            }
            Ok(())
        })
    })
    .await
}
