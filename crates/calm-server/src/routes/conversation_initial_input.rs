//! Bind acceptance to the original creation operation; observe presence on the queue owner.
use crate::conversation_keys::{derive_track_conversation_keys, first_message_digest};
use crate::error::{CalmError, ErrorBody, Result};
use crate::extract::{Json, Path};
use crate::harness::{QueueEntryId, run_loop::input_inspection::InputInspection};
use crate::routes::{
    conversations_shared::retryable_operation, idempotency_key::parse_idempotency_key_header,
};
use crate::state::{RouteState, WorkerState};
use axum::{extract::State, http::HeaderMap};
use calm_types::conversation_receipt::{
    ConversationInitialInputObservation as Observation,
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
    let canonical_card = card_id.to_owned();
    let canonical = crate::db::write_in_tx_typed(s.repo.as_ref(), move |tx| {
        Box::pin(async move {
            Ok(
                crate::db::sqlite::conversation::assistant_creation_get_tx(tx, &canonical_card)
                    .await?,
            )
        })
    })
    .await?;
    if let Some(original) = canonical {
        if original.track_id != track_id {
            return Ok(Err(Reason::Integrity));
        }
        return Ok(Ok(ReceiptResult::Known {
            receipt: original.receipt,
        }));
    }
    receipt_from_historical_operation(s, track_id, card_id, key, &derived.operation_key).await
}

/// Read-only journal interpretation. The caller already qualified the derived
/// scope; this reader can be reused inside the creation owner's transaction.
pub(crate) async fn receipt_from_historical_operation(
    s: &RouteState,
    track_id: &str,
    card_id: &str,
    key: &str,
    operation_key: &str,
) -> Result<std::result::Result<ReceiptResult, Reason>> {
    let (_, op) = retryable_operation(s, operation_key).await?;
    Ok(
        crate::operation::offline_facts::historical_initial_input_receipt(
            op.as_ref().map(Into::into),
            track_id,
            card_id,
            key,
        ),
    )
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
    if crate::conversation_inputs::has_canonical_storage(s.repo.as_ref(), &card_id).await? {
        return Ok(Json(Observation::Unavailable {
            reason: Reason::RuntimeUnavailable,
            receipt: Some(receipt),
        }));
    }
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
    let observed_rev = match &observed {
        InputInspection::Queued { rev, .. } => Some(*rev),
        InputInspection::Projected { entry_rev, .. } => Some(*entry_rev),
        InputInspection::Unavailable(_) => None,
    };
    if observed_rev.is_some_and(|rev| rev < receipt.entry_rev) {
        return Ok(Json(Observation::Unavailable {
            reason: Reason::RuntimeUnavailable,
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
            entry_rev,
        } => Observation::Projected {
            receipt,
            runtime_id: runtime.id,
            item_id,
            segment_index,
            entry_rev,
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
