//! Bind acceptance to the original creation operation; observe presence on the queue owner.
use crate::conversation_keys::{derive_track_conversation_keys, first_message_digest};
use crate::error::{CalmError, ErrorBody, Result};
use crate::extract::{Json, Path};
use crate::harness::{QueueEntryId, run_loop::input_inspection::InputInspection};
use crate::operation::{
    Phase, assistant_start_wire::StartInput, planner_harness_start_adapter::HarnessProfile,
};
use crate::routes::{
    conversations_shared::retryable_operation, idempotency_key::parse_idempotency_key_header,
};
use crate::state::{RouteState, WorkerState};
use axum::{extract::State, http::HeaderMap};
use calm_types::conversation_receipt::{
    ConversationInitialInputObservation as Observation, ConversationInitialInputReceipt,
    ConversationInitialInputReceiptResult as ReceiptResult,
    InitialInputUnavailableReason as Reason,
};

/// A missing journal field is historical unavailability, not evidence manufactured from text,
/// queue position, a current snapshot, or the operation's completion time.
pub(crate) async fn receipt_for_create(
    s: &RouteState,
    track_id: &str,
    card_id: &str,
    key: &str,
) -> Result<std::result::Result<ReceiptResult, Reason>> {
    let derived = derive_track_conversation_keys(track_id, key);
    if derived.card_id != card_id {
        return Err(CalmError::NotFound("conversation creation".into()));
    }
    let (_, op) = retryable_operation(s, &derived.operation_key).await?;
    let Some(op) = op else {
        return Ok(Err(Reason::CreationIncomplete));
    };
    let payload = match StartInput::decode(&op.payload) {
        Ok(input) => input.payload,
        Err(_) => return Ok(Err(Reason::Integrity)),
    };
    if payload.profile != HarnessProfile::Assistant
        || payload.track_id != track_id
        || payload.planner_card_id.as_str() != card_id
        || payload
            .create_card
            .as_ref()
            .and_then(|seed| seed.idempotency_key.as_deref())
            != Some(key)
    {
        return Ok(Err(Reason::Integrity));
    }
    if op.target_type != "card" || op.target_id.as_deref() != Some(card_id) {
        return Ok(Err(Reason::Integrity));
    }
    if op.phase != Phase::Succeeded {
        return Ok(Err(Reason::CreationIncomplete));
    }
    let Some(output) = &op.tx_output else {
        return Ok(Err(Reason::Integrity));
    };
    if output.target_type != "card"
        || output.target_id.as_deref() != Some(card_id)
        || output.data.get("card_id").and_then(|v| v.as_str()) != Some(card_id)
        || output.data.get("track_id").and_then(|v| v.as_str()) != Some(track_id)
    {
        return Ok(Err(Reason::Integrity));
    }
    let Some(value) = output.data.get("initial_input_receipt") else {
        return Ok(Ok(ReceiptResult::LegacyUnavailable));
    };
    let receipt: ConversationInitialInputReceipt = match serde_json::from_value(value.clone()) {
        Ok(receipt) => receipt,
        Err(_) => return Ok(Err(Reason::Integrity)),
    };
    if receipt.entry_id.is_empty()
        || receipt.queued_at_ms < 0
        || receipt.initial_runtime_id.is_empty()
        || output.data.get("runtime_id").and_then(|v| v.as_str())
            != Some(receipt.initial_runtime_id.as_str())
        || payload
            .first_message
            .as_deref()
            .map(first_message_digest)
            .as_deref()
            != Some(receipt.text_sha256.as_str())
    {
        return Ok(Err(Reason::Integrity));
    }
    Ok(Ok(ReceiptResult::Known { receipt }))
}

#[utoipa::path(
    get,
    path = "/api/tracks/{track_id}/conversations/{card_id}/initial-input",
    tag = "tracks",
    params(
        ("track_id" = String, Path, description = "Track id"),
        ("card_id" = String, Path, description = "Conversation card id"),
        ("Idempotency-Key" = String, Header, description = "Required original conversation creation key"),
    ),
    responses(
        (status = 200, description = "Receipt-bound observation from the current queue owner; unavailable never means absent", body = Observation),
        (status = 400, description = "Invalid or missing creation key", body = ErrorBody),
        (status = 404, description = "Conversation does not belong to this track/key", body = ErrorBody),
        (status = 500, description = "Read failed; no presence conclusion", body = ErrorBody),
    ),
)]
pub(crate) async fn get_conversation_initial_input(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    Path((track_id, card_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Observation>> {
    let key = parse_idempotency_key_header(&headers)?
        .ok_or_else(|| CalmError::BadRequest("Idempotency-Key header is required".into()))?;
    let card = s
        .repo
        .card_get(&card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    if card.track_id.as_str() != track_id {
        return Err(CalmError::NotFound(format!("card {card_id}")));
    }
    let receipt = match receipt_for_create(&s, &track_id, &card_id, &key).await? {
        Ok(ReceiptResult::Known { receipt }) => receipt,
        Ok(ReceiptResult::LegacyUnavailable) => {
            return Ok(Json(Observation::Unavailable {
                reason: Reason::LegacyOperation,
                receipt: None,
            }));
        }
        Err(reason) => {
            return Ok(Json(Observation::Unavailable {
                reason,
                receipt: None,
            }));
        }
    };
    let Some(runtime) = w.repo.session_projection_active_for_card(&card_id).await? else {
        return Ok(Json(Observation::Unavailable {
            reason: Reason::RuntimeUnavailable,
            receipt: Some(receipt),
        }));
    };
    let Some(harness) = w.harness.get(&runtime.id) else {
        return Ok(Json(Observation::Unavailable {
            reason: Reason::RuntimeUnavailable,
            receipt: Some(receipt),
        }));
    };
    // Never hold the start fence while waiting for a loop that shutdown may itself be awaiting.
    let observed = harness
        .inspect_input(QueueEntryId::from_wire(receipt.entry_id.clone()))
        .await?;
    let current = w.repo.session_projection_active_for_card(&card_id).await?;
    if current.as_ref().map(|row| row.id.as_str()) != Some(runtime.id.as_str())
        || !w
            .harness
            .get(&runtime.id)
            .is_some_and(|current| harness.same_instance(&current))
    {
        return Ok(Json(Observation::Unavailable {
            reason: Reason::RuntimeChanged,
            receipt: Some(receipt),
        }));
    }
    if harness.is_shutting_down() {
        return Ok(Json(Observation::Unavailable {
            reason: Reason::ShuttingDown,
            receipt: Some(receipt),
        }));
    }
    Ok(Json(match observed {
        InputInspection::Queued {
            entry_id,
            rev,
            text,
        } => Observation::Queued {
            receipt,
            runtime_id: runtime.id,
            entry_id,
            rev,
            text_sha256: first_message_digest(&text),
            text,
        },
        InputInspection::Projected {
            item_id,
            segment_index,
        } => Observation::Projected {
            receipt,
            runtime_id: runtime.id,
            item_id,
            segment_index,
        },
        InputInspection::Unavailable(reason) => {
            let receipt = match reason {
                Reason::RuntimeUnavailable
                | Reason::RuntimeChanged
                | Reason::ShuttingDown
                | Reason::LegacyProjection => Some(receipt),
                Reason::LegacyOperation | Reason::CreationIncomplete | Reason::Integrity => None,
            };
            Observation::Unavailable { reason, receipt }
        }
    }))
}
