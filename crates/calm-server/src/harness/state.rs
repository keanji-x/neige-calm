use std::time::{Duration, Instant};

use crate::session_projection_repo::WorkerSessionState;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HarnessState {
    PendingThreadStart,
    Idle,
    Issuing {
        since: Instant,
        kind: IssuingKind,
    },
    Compacting {
        since: Instant,
    },
    CompactionRunning {
        turn_id: String,
        started_at: Instant,
    },
    TurnRunning {
        turn_id: String,
        /// When the harness accepted the turn's `TurnStarted`. Never moved: the running turn's
        /// elapsed time, waits on the user included, is measured from it.
        started_at: Instant,
        /// What the turn watchdog measures `max_turn_duration` from: `started_at`, moved forward
        /// by every wait on a held request, so waiting on the user is not turn time.
        watchdog_from: Instant,
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

/// The turn a `TurnRunning` state is running, and how long ago the harness accepted its
/// `TurnStarted`, on the monotonic clock: no wall-clock skew enters it, and a duplicate start for
/// the same turn never resets it (the run loop keeps the state it already has).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunningTurn {
    pub turn_id: String,
    pub elapsed: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssuingKind {
    TurnStart,
    Interrupt {
        target_turn_id: String,
        reason: String,
    },
}

pub fn run_status_for(state: &HarnessState) -> WorkerSessionState {
    match state {
        HarnessState::PendingThreadStart => WorkerSessionState::Starting,
        HarnessState::Idle | HarnessState::TurnCompleted { .. } | HarnessState::Resumed { .. } => {
            WorkerSessionState::Idle
        }
        HarnessState::CompactionRunning { .. }
        | HarnessState::Compacting { .. }
        | HarnessState::Issuing { .. }
        | HarnessState::TurnRunning { .. } => WorkerSessionState::TurnPending,
        HarnessState::Wedged { .. } => WorkerSessionState::Failed,
    }
}

impl HarnessState {
    /// `turn_id` running since `started_at`, with nothing waited on yet: the watchdog counts
    /// from the same instant.
    pub fn turn_running(turn_id: String, started_at: Instant) -> Self {
        Self::TurnRunning {
            turn_id,
            started_at,
            watchdog_from: started_at,
        }
    }

    pub fn can_issue_turn(&self) -> bool {
        matches!(self, Self::Idle | Self::TurnCompleted { .. })
    }

    /// `Some` exactly in `TurnRunning`, measured at `now`.
    pub fn running_turn(&self, now: Instant) -> Option<RunningTurn> {
        match self {
            Self::CompactionRunning {
                turn_id,
                started_at,
            }
            | Self::TurnRunning {
                turn_id,
                started_at,
                ..
            } => Some(RunningTurn {
                turn_id: turn_id.clone(),
                elapsed: now.saturating_duration_since(*started_at),
            }),
            _ => None,
        }
    }

    pub fn active_turn_id(&self) -> Option<String> {
        match self {
            Self::CompactionRunning { turn_id, .. } | Self::TurnRunning { turn_id, .. } => {
                Some(turn_id.clone())
            }
            Self::Issuing {
                kind: IssuingKind::Interrupt { target_turn_id, .. },
                ..
            } => Some(target_turn_id.clone()),
            _ => None,
        }
    }
}
