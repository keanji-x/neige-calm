//! Secret-free executor facts shared by protocol, persistence and lifecycle owners.
use super::{
    ExecutionId, ExecutionInterruption, ExternalDirectoryId, InterruptedQuiescenceProof,
    LaunchTicketId, ObjectRegistrationId,
};
use serde::{Deserialize, Serialize};

/// Immutable execution history. Unknown-id revocation has no source or slot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionTombstone {
    pub execution_id: ExecutionId,
    pub ticket_id: LaunchTicketId,
    pub registration: Option<ObjectRegistrationId>,
    pub directory: Option<ExternalDirectoryId>,
    pub slot: Option<(String, u64)>,
    pub outcome: TombstoneOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TombstoneOutcome {
    Revoked,
    Retired,
}

// Preserve released JSON shapes and bincode variant order when relocating these facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionState {
    Prepared,
    Revoked,
    Forking,
    Spawned,
    Exited {
        status: Option<i32>,
    },
    Drained {
        status: Option<i32>,
    },
    NoChild {
        reason: String,
    },
    Unknown {
        reason: String,
    },
    /// Original identity is gone and the platform scope in the proof is empty after restart.
    /// No child exit status or descendant-wide drain proof is implied.
    InterruptedQuiescent {
        reason: ExecutionInterruption,
        proof: InterruptedQuiescenceProof,
    },
    /// Owned root exited and its pinned original process group was observed empty.
    /// Does not prove absence of descendants which deliberately left that group.
    OriginalGroupEmpty {
        status: Option<i32>,
    },
}

impl ExecutionState {
    pub fn permits_handoff(&self) -> bool {
        matches!(
            self,
            Self::Revoked
                | Self::Drained { .. }
                | Self::OriginalGroupEmpty { .. }
                | Self::InterruptedQuiescent { .. }
                | Self::NoChild { .. }
        )
    }
}

/// Secret-free settlement evidence, copied directly from executor Inspect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Settlement {
    Journal(ExecutionState),
    Retired(ExecutionTombstone),
}

impl Settlement {
    /// A timestamp alone cannot discharge an execution's resource protection.
    pub fn permits_handoff(&self, execution_id: &ExecutionId) -> bool {
        match self {
            Self::Journal(state) => state.permits_handoff(),
            Self::Retired(tombstone) => &tombstone.execution_id == execution_id,
        }
    }

    /// Live-helper drain proves all descendants gone (subreaper + ECHILD).
    /// Restart/group-only observations and tombstones do not preserve that proof.
    pub fn descendants_complete(&self) -> bool {
        matches!(self, Self::Journal(ExecutionState::Drained { .. }))
    }
}
