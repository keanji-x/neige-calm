//! The V2 snapshot's only ordered pending queue. No human bodies live here.
use super::{
    AcceptedInputRef, ConversationMessageId, InputSubmissionId, MessageRevisionRef,
    SystemObservation, SystemObservationId,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Queue/source identity is independent of the accepted provenance in a render plan.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CanonicalPendingIdentity {
    Accepted {
        input: AcceptedInputRef,
    },
    ExplicitSuccessor {
        retry_intent_id: super::RetryIntentId,
    },
}

impl From<AcceptedInputRef> for CanonicalPendingIdentity {
    fn from(input: AcceptedInputRef) -> Self {
        Self::Accepted { input }
    }
}

impl From<MessageRevisionRef> for CanonicalPendingIdentity {
    fn from(message: MessageRevisionRef) -> Self {
        Self::Accepted {
            input: AcceptedInputRef::Human { message },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PendingInput {
    ExplicitSuccessor {
        retry_intent_id: super::RetryIntentId,
        queue_position: u64,
        queued_at_ms: i64,
    },
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
        /// Keyed acceptance is addressable and can never be coalesced/evicted.
        /// Ordinary observations retain their existing feature-owned coalescing.
        addressable: bool,
    },
}

impl PendingInput {
    pub fn queue_position(&self) -> u64 {
        match self {
            Self::Human { queue_position, .. }
            | Self::System { queue_position, .. }
            | Self::ExplicitSuccessor { queue_position, .. } => *queue_position,
        }
    }

    pub fn queued_at_ms(&self) -> i64 {
        match self {
            Self::Human { queued_at_ms, .. }
            | Self::System { queued_at_ms, .. }
            | Self::ExplicitSuccessor { queued_at_ms, .. } => *queued_at_ms,
        }
    }
    pub fn identity(&self) -> CanonicalPendingIdentity {
        match self {
            Self::ExplicitSuccessor {
                retry_intent_id, ..
            } => CanonicalPendingIdentity::ExplicitSuccessor {
                retry_intent_id: retry_intent_id.clone(),
            },
            Self::Human { message, .. } => message.clone().into(),
            Self::System { observation_id, .. } => AcceptedInputRef::System {
                observation_id: observation_id.clone(),
            }
            .into(),
        }
    }

    pub fn accepted_input_ref(&self) -> Option<AcceptedInputRef> {
        match self.identity() {
            CanonicalPendingIdentity::Accepted { input } => Some(input),
            CanonicalPendingIdentity::ExplicitSuccessor { .. } => None,
        }
    }

    pub fn message_id(&self) -> Option<&ConversationMessageId> {
        match self {
            Self::Human { message, .. } => Some(&message.message_id),
            Self::System { .. } | Self::ExplicitSuccessor { .. } => None,
        }
    }

    pub fn envelope_id(&self) -> Option<i64> {
        match self {
            Self::Human { envelope_id, .. } | Self::System { envelope_id, .. } => *envelope_id,
            Self::ExplicitSuccessor { .. } => None,
        }
    }
}

/// Validates identities, not admission. The transaction owner separately checks
/// current author/source, revision, membership and exact current carrier.
pub fn validate_pending_inputs(pending: &[PendingInput]) -> Result<(), &'static str> {
    let mut seen = HashSet::new();
    let mut intents = HashSet::new();
    let mut last_position = None;
    for entry in pending {
        if last_position.is_some_and(|last| last >= entry.queue_position()) {
            return Err("pending queue positions must be strictly increasing");
        }
        last_position = Some(entry.queue_position());
        let unique = match entry.identity() {
            CanonicalPendingIdentity::Accepted { input } => seen.insert(input.identity()),
            CanonicalPendingIdentity::ExplicitSuccessor { retry_intent_id } => {
                intents.insert(retry_intent_id)
            }
        };
        if !unique {
            return Err("pending queue repeats an input identity");
        }
    }
    Ok(())
}

/// Snapshot active batches reference immutable submissions, never another list
/// of input bodies. These IDs confer no execution permission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveInputSubmissions {
    pub logical_turn_id: super::LogicalTurnId,
    pub submission_ids: Vec<InputSubmissionId>,
}

impl From<MessageRevisionRef> for AcceptedInputRef {
    fn from(message: MessageRevisionRef) -> Self {
        Self::Human { message }
    }
}
