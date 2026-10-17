//! Acceptance's canonical half. The public kernel owner binds its source-specific
//! receipt/creation intent and events in this same transaction; no route gets an
//! alternate unkeyed send endpoint from these storage primitives.
use super::carrier::save_input_carrier_tx;
use super::{CurrentInputCarrier, PresentationSource, allocate_presentation_tx, insert_message_tx};
use crate::error::{CalmError, Result};
use calm_types::conversation::{AcceptedInputRef, HumanPayload, MessageAuthor, PendingInput};
use sqlx::{Sqlite, Transaction};

#[derive(Clone, Debug, PartialEq)]
pub struct AcceptedPendingInput {
    pub input_ref: AcceptedInputRef,
    pub presentation_sequence: i64,
    pub snapshot_after_write: serde_json::Value,
}

pub async fn accept_pending_human_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &mut CurrentInputCarrier,
    authenticated_user_id: &str,
    payload: HumanPayload,
) -> Result<AcceptedPendingInput> {
    let position = carrier.checkpoint.next_queue_position;
    let next = position
        .checked_add(1)
        .filter(|next| *next <= i64::MAX as u64)
        .ok_or_else(|| CalmError::Conflict("conversation queue order exhausted"))?;
    let stored = insert_message_tx(
        tx,
        &carrier.card_id,
        &MessageAuthor::AuthenticatedHuman {
            registered_user_id: authenticated_user_id.into(),
        },
        payload,
    )
    .await?;
    let presentation_sequence = allocate_presentation_tx(
        tx,
        &carrier.card_id,
        PresentationSource::HumanMessage(&stored.reference.message_id),
    )
    .await?;
    let input_ref = AcceptedInputRef::Human {
        message: stored.reference.clone(),
    };
    carrier.checkpoint.pending.push(PendingInput::Human {
        message: stored.reference,
        queue_position: position,
        queued_at_ms: stored.created_at_ms,
        envelope_id: None,
    });
    carrier.checkpoint.next_queue_position = next;
    save_input_carrier_tx(tx, carrier).await?;
    Ok(AcceptedPendingInput {
        input_ref,
        presentation_sequence,
        snapshot_after_write: carrier.snapshot.clone(),
    })
}
