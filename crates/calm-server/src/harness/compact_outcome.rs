//! Maintenance facts in the runtime snapshot, never transcript turns or rewind anchors.
pub use calm_types::harness::snapshot::{CompactOutcome, CompactStatus};

use serde::{Deserialize, Serialize};

/// Session-owned intent. The owner commits Submitted with the maintenance Start,
/// and commits wake_enqueued with the existing kernel wake event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlannerCompaction {
    pub request_id: String,
    pub requested_turn_id: String,
    pub requested_at_ms: i64,
    pub phase: PlannerCompactionPhase,
    pub wake_enqueued: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PlannerCompactionPhase {
    Requested,
    Submitted {
        operation_id: String,
    },
    Settled {
        operation_id: String,
        status: CompactStatus,
    },
}

impl PlannerCompaction {
    /// A request remains pending until both maintenance and its wake commit.
    pub fn pending(&self) -> bool {
        !matches!(self.phase, PlannerCompactionPhase::Settled { .. }) || !self.wake_enqueued
    }

    pub fn restore(&mut self) {
        if let PlannerCompactionPhase::Submitted { operation_id } = &self.phase {
            self.phase = PlannerCompactionPhase::Settled {
                operation_id: operation_id.clone(),
                status: CompactStatus::Unconfirmed,
            };
        }
    }
}

/// Admission result for a session-owned maintenance request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompactionAnswer {
    /// Recorded, including an idempotent retry of the same retained request ID.
    Scheduled,
    /// Another request still owns maintenance or its uncommitted wake.
    AlreadyPending {
        request_id: String,
    },
    Refused(CompactionRefusal),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionRefusal {
    UnsupportedProvider,
    ConversationUnavailable,
    CompactionInProgress,
    NotCurrentSession,
}
