//! Canonical conversation identities and reference-only input rendering.
//!
//! None of these values authorizes an operation. Registration, live membership,
//! carrier ownership and provider receipt validation belong to their owning layers.
mod checkpoint;
mod ids;
mod pending;
mod proof;
mod render;

pub use checkpoint::*;
pub use ids::*;
pub use pending::*;
pub use proof::*;
pub use render::*;

use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;

/// Accepted human content. Only the server's binder supplies bound attachments;
/// untrusted ingress names upload IDs and never supplies these host paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanPayload {
    pub text: String,
    pub bound_attachments: Vec<crate::planner_attachment::BoundAttachment>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationHistoryContract {
    Private,
    SharedHistory,
}

/// Provenance records the existing execution authority; it does not grant it.
/// Construction is required at every new logical Turn, never defaulted on recovery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConversationExecutionProvenance {
    LegacySingleUserDeployment {
        deployment_owner_user_id: String,
        provider: crate::runtime::AgentProvider,
        profile: crate::harness::HarnessProfile,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MessageAuthor {
    AuthenticatedHuman {
        registered_user_id: String,
    },
    LegacyInput {
        source_locator: String,
        boundary: LegacyInputBoundary,
    },
}

/// A revision's identity stays valid after its private payload is erased.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageRevisionRef {
    pub message_id: ConversationMessageId,
    pub revision: NonZeroU32,
}

/// Migration preserves the boundary the old carrier actually recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyInputBoundary {
    SingleRecordedInput,
    OpaqueAggregate,
}

/// New producers construct only Human or System. Legacy is reserved for adoption;
/// it deliberately makes no claim about a registered human author.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AcceptedInputRef {
    Human { message: MessageRevisionRef },
    System { observation_id: SystemObservationId },
    Legacy { message: MessageRevisionRef },
}

/// Stable accepted identity, independent of revisions and migration aliases.
/// Equality is not source authorization: claim still validates exact provenance,
/// current revision, pending membership and effective ownership in its transaction.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AcceptedInputIdentity {
    Message(ConversationMessageId),
    System(SystemObservationId),
}

impl AcceptedInputRef {
    pub fn identity(&self) -> AcceptedInputIdentity {
        match self {
            Self::Human { message } | Self::Legacy { message } => {
                AcceptedInputIdentity::Message(message.message_id.clone())
            }
            Self::System { observation_id } => {
                AcceptedInputIdentity::System(observation_id.clone())
            }
        }
    }
}

/// The only permitted mutation of an accepted payload is explicit private erasure.
/// Digests and identities are held outside this value and survive the erasure.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum PrivateHistoryPayload<T> {
    Present {
        value: T,
    },
    ErasedByPrivateHistoryOperation {
        operation_id: PrivateHistoryOperationId,
    },
}

#[cfg(test)]
mod checkpoint_tests;
#[cfg(test)]
mod tests;
