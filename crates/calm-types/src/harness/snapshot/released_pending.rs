//! Frozen accepted-only queue shape shared by released canonical V2–V4.
use crate::conversation::{
    MessageRevisionRef, PendingInput, SystemObservation, SystemObservationId,
};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ReleasedPendingInput {
    Human {
        message: MessageRevisionRef,
        queue_position: u64,
        queued_at_ms: i64,
        envelope_id: Option<i64>,
    },
    System {
        observation_id: SystemObservationId,
        observation: SystemObservation,
        queue_position: u64,
        queued_at_ms: i64,
        envelope_id: Option<i64>,
        addressable: bool,
    },
}
impl From<ReleasedPendingInput> for PendingInput {
    fn from(value: ReleasedPendingInput) -> Self {
        match value {
            ReleasedPendingInput::Human {
                message,
                queue_position,
                queued_at_ms,
                envelope_id,
            } => Self::Human {
                message,
                queue_position,
                queued_at_ms,
                envelope_id,
            },
            ReleasedPendingInput::System {
                observation_id,
                observation,
                queue_position,
                queued_at_ms,
                envelope_id,
                addressable,
            } => Self::System {
                observation_id,
                observation,
                queue_position,
                queued_at_ms,
                envelope_id,
                addressable,
            },
        }
    }
}
