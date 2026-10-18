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
use std::path::PathBuf;
use workspace::identity::WorkspaceCwd;

pub const QUIESCE_DEADLINE: std::time::Duration = STOP_GRACE.saturating_add(DRAIN_BUDGET);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationRef(pub String);
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackId(pub String);
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessAlias(pub String);
#[derive(Clone, Debug)]
pub struct LaunchTicket {
    pub id: LaunchTicketId,
    pub secret: CapabilityToken,
}
#[derive(Clone, Debug)]
pub struct PrivateSource {
    pub root: PathBuf,
    pub cwd: WorkspaceCwd,
    pub registration: ObjectRegistrationId,
    pub nonce: TransferNonce,
    pub directory: ExternalDirectoryId,
    pub incarnation: ExternalDirectoryIncarnationId,
    pub authority: ExecutionAuthority,
}
#[derive(Clone, Debug)]
pub struct ExecutionIntent {
    pub id: ExecutionId,
    pub alias: ProcessAlias,
    pub owner: OperationRef,
    pub track: TrackId,
    pub source: PrivateSource,
    pub program: ExecutionSpecification,
    pub specification: SpecificationId,
    pub ticket: LaunchTicket,
    pub tokens: ExecutionControlTokens,
}
impl ExecutionIntent {
    pub fn consume(&self) -> ConsumeExecution {
        ConsumeExecution {
            execution_id: self.id.clone(),
            ticket_id: self.ticket.id.clone(),
            ticket: self.ticket.secret.clone(),
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
            | ExecutionState::Drained { .. } => Step::Settle,
        },
    }
}
#[derive(Clone, Debug)]
pub struct Stored {
    pub intent: ExecutionIntent,
    pub desired: Desired,
    pub settled: bool,
    pub finished: bool,
}
/// Secret-free settlement evidence, copied directly from Inspect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Settlement {
    Journal(ExecutionState),
    Retired(ExecutionTombstone),
}
impl Settlement {
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
