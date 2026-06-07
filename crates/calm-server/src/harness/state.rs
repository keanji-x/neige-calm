use std::time::Instant;

use crate::runtime_repo::RunStatus;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HarnessState {
    PendingThreadStart,
    Idle,
    Issuing {
        since: Instant,
        kind: IssuingKind,
    },
    TurnRunning {
        turn_id: String,
        started_at: Instant,
    },
    TurnCompleted {
        last_turn_id: String,
    },
    Resumed {
        resumed_at: Instant,
    },
    Wedged {
        since: Instant,
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssuingKind {
    TurnStart,
    Interrupt {
        target_turn_id: String,
        reason: String,
    },
}

pub fn run_status_for(state: &HarnessState) -> RunStatus {
    match state {
        HarnessState::PendingThreadStart => RunStatus::Starting,
        HarnessState::Idle | HarnessState::TurnCompleted { .. } | HarnessState::Resumed { .. } => {
            RunStatus::Idle
        }
        HarnessState::Issuing { .. } | HarnessState::TurnRunning { .. } => RunStatus::TurnPending,
        HarnessState::Wedged { .. } => RunStatus::Failed,
    }
}

impl HarnessState {
    pub fn can_issue_turn(&self) -> bool {
        matches!(self, Self::Idle | Self::TurnCompleted { .. })
    }

    pub fn active_turn_id(&self) -> Option<String> {
        match self {
            Self::TurnRunning { turn_id, .. } => Some(turn_id.clone()),
            Self::Issuing {
                kind: IssuingKind::Interrupt { target_turn_id, .. },
                ..
            } => Some(target_turn_id.clone()),
            _ => None,
        }
    }
}
