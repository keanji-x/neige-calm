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
    let (position, next) = pending_position(carrier)?;
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

/// Called only by the source-qualified acceptance owner, in the same transaction
/// as its strict ordered event prefix. No second dedup ledger is introduced.
pub async fn accept_pending_system_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &mut CurrentInputCarrier,
    observation: calm_types::conversation::SystemObservation,
    event: &super::super::event_inputs::EventInputFact,
) -> Result<AcceptedPendingInput> {
    let floor = carrier.snapshot["push_watermark"]
        .as_i64()
        .ok_or(CalmError::Conflict(
            "canonical carrier has no event watermark",
        ))?;
    let envelope_id = event.envelope.id;
    if envelope_id <= floor {
        return Err(CalmError::Conflict("event prefix was already accepted"));
    }
    let (position, next) = pending_position(carrier)?;
    let observation_id = event_system_observation_id_tx(tx, carrier, event, &observation).await?;
    let accepted_at_ms = crate::model::now_ms();
    let presentation_sequence = allocate_presentation_tx(
        tx,
        &carrier.card_id,
        PresentationSource::SystemObservation(&observation_id),
    )
    .await?;
    let input_ref = AcceptedInputRef::System {
        observation_id: observation_id.clone(),
    };
    carrier.checkpoint.pending.push(PendingInput::System {
        observation_id,
        observation,
        queue_position: position,
        queued_at_ms: accepted_at_ms,
        envelope_id: Some(envelope_id),
        addressable: false,
    });
    carrier.checkpoint.next_queue_position = next;
    carrier.snapshot["push_watermark"] = envelope_id.into();
    save_input_carrier_tx(tx, carrier).await?;
    Ok(AcceptedPendingInput {
        input_ref,
        presentation_sequence,
        snapshot_after_write: carrier.snapshot.clone(),
    })
}

/// This is a data binding, never execution authority. Cold admission verifies
/// the original immutable ID using current original-row facts and frozen content.
pub async fn event_system_observation_id_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &CurrentInputCarrier,
    event: &super::super::event_inputs::EventInputFact,
    observation: &calm_types::conversation::SystemObservation,
) -> Result<calm_types::conversation::SystemObservationId> {
    let actual = super::super::event_inputs::event_input_tx(tx, event.envelope.id).await?;
    if actual.fingerprint != event.fingerprint {
        return Err(CalmError::Conflict("input event changed before binding"));
    }
    let database: String = sqlx::query_scalar("SELECT id FROM database_identity WHERE singleton=1")
        .fetch_one(&mut **tx)
        .await?;
    let digest = super::digest(&(
        "event_system_input_v1",
        database,
        carrier.card_id(),
        carrier.worker_session_id(),
        actual.envelope.id,
        actual.fingerprint,
        observation,
    ))?;
    calm_types::conversation::SystemObservationId::parse(&digest[..32])
        .map_err(|error| CalmError::Internal(error.to_string()))
}

/// All pending kinds share one allocation counter and SQLite's ordering range.
pub(super) fn pending_position(carrier: &CurrentInputCarrier) -> Result<(u64, u64)> {
    let position = carrier.checkpoint.next_queue_position;
    let next = position
        .checked_add(1)
        .filter(|next| *next <= i64::MAX as u64)
        .ok_or(CalmError::Conflict("conversation queue order exhausted"))?;
    Ok((position, next))
}
