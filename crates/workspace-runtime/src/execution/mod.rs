//! Transactional intent, monotonic desire, and a single execution driver.
mod engine;
mod ports;
#[cfg(feature = "session-executor")]
pub mod session_executor;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[cfg(test)]
mod tests;

pub use calm_session::control::execution::*;
pub use engine::*;
pub use ports::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use workspace::identity::WorkspaceCwd;

pub const QUIESCE_DEADLINE: std::time::Duration = STOP_GRACE.saturating_add(DRAIN_BUDGET);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationRef(pub String);
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackId(pub String);
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessAlias(pub String);
/// Consumer-owned registry key, persisted with the immutable execution intent.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExecutionConsumer(pub String);
impl From<&str> for ExecutionConsumer {
    fn from(value: &str) -> Self {
        Self(value.into())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrivateSource {
    pub root: PathBuf,
    pub cwd: WorkspaceCwd,
    pub registration: ObjectRegistrationId,
    pub nonce: TransferNonce,
    pub directory: ExternalDirectoryId,
    pub incarnation: ExternalDirectoryIncarnationId,
    pub authority: ExecutionAuthority,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionMetadata {
    pub consumer: ExecutionConsumer,
    pub id: ExecutionId,
    pub alias: ProcessAlias,
    pub owner: OperationRef,
    pub track: TrackId,
    pub source: PrivateSource,
    pub program: ExecutionSpecification,
    pub specification: SpecificationId,
    pub ticket_id: LaunchTicketId,
}
/// Capabilities retained only while an execution is active.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionSecrets {
    pub ticket: CapabilityToken,
    pub tokens: ExecutionControlTokens,
}
#[derive(Clone, Debug)]
pub struct ExecutionIntent {
    pub metadata: ExecutionMetadata,
    pub secrets: ExecutionSecrets,
}
impl ExecutionIntent {
    pub fn consume(&self) -> ConsumeExecution {
        ConsumeExecution {
            execution_id: self.metadata.id.clone(),
            ticket_id: self.metadata.ticket_id.clone(),
            ticket: self.secrets.ticket.clone(),
        }
    }
}

/// Store implementations apply `max(current, requested)` in the caller's transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Desired {
    Run,
    Stop,
    Retire,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fact {
    Absent,
    Journal {
        state: ExecutionState,
        receipt: Option<Box<ExecutionReceipt>>,
        output: OutputEnd,
    },
    Retired(ExecutionTombstone),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Prepare,
    Launch,
    Revoke,
    Stop,
    Wait,
    Settle,
    Finish,
}
/// Action selection only. The executor alone establishes durable settlement.
pub fn next_step(desired: Desired, fact: &Fact) -> Step {
    match fact {
        Fact::Absent if desired == Desired::Run => Step::Prepare,
        Fact::Absent => Step::Revoke,
        Fact::Retired(_) => Step::Finish,
        Fact::Journal {
            state: ExecutionState::OriginalGroupEmpty { .. },
            output,
            ..
        } if !matches!(output, OutputEnd::Complete { .. }) => Step::Wait,
        Fact::Journal { state, .. } => match state {
            ExecutionState::Prepared if desired == Desired::Run => Step::Launch,
            ExecutionState::Prepared => Step::Revoke,
            ExecutionState::Forking | ExecutionState::Spawned | ExecutionState::Exited { .. }
                if desired != Desired::Run =>
            {
                Step::Stop
            }
            ExecutionState::Forking
            | ExecutionState::Spawned
            | ExecutionState::Exited { .. }
            | ExecutionState::Unknown { .. } => Step::Wait,
            ExecutionState::Revoked
            | ExecutionState::NoChild { .. }
            | ExecutionState::Drained { .. }
            | ExecutionState::OriginalGroupEmpty { .. }
            | ExecutionState::InterruptedQuiescent { .. } => Step::Settle,
        },
    }
}
#[derive(Clone, Debug)]
pub enum Stored {
    Active {
        intent: ExecutionMetadata,
        secrets: ExecutionSecrets,
        desired: Desired,
        settlement: Option<Settlement>,
    },
    Finished {
        intent: ExecutionMetadata,
        settlement: Settlement,
    },
}
impl Stored {
    pub fn is_settled(&self) -> bool {
        match self {
            Self::Active { settlement, .. } => settlement.is_some(),
            Self::Finished { .. } => true,
        }
    }
}
/// Secret-free settlement evidence, copied directly from Inspect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Settlement {
    Journal(ExecutionState),
    Retired(ExecutionTombstone),
}
impl Settlement {
    /// Live-helper drain proves all descendants gone (subreaper + ECHILD).
    /// Restart/group-only observations and tombstones do not preserve that proof.
    pub fn descendants_complete(&self) -> bool {
        matches!(self, Self::Journal(ExecutionState::Drained { .. }))
    }

    pub(super) fn from_fact(fact: &Fact) -> Self {
        match fact {
            Fact::Journal { state, .. } => Self::Journal(state.clone()),
            Fact::Retired(t) => Self::Retired(t.clone()),
            Fact::Absent => unreachable!("absent is not settlement evidence"),
        }
    }
}

/// Only construct after the surrounding transaction has committed. A transaction-local
/// result is deliberately a different type, so it cannot be passed directly to `drive`.
#[must_use]
pub struct DriveTicket(ExecutionId);
impl DriveTicket {
    pub fn committed(id: ExecutionId) -> Self {
        Self(id)
    }
}
#[must_use]
pub struct PendingDriveTicket(pub ExecutionId);
impl PendingDriveTicket {
    /// The caller must first commit its transaction; a rolled-back ticket must be discarded.
    pub fn committed(self) -> DriveTicket {
        DriveTicket::committed(self.0)
    }
}
