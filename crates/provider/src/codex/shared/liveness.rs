//! Derive worker liveness facts from authoritative Codex read responses.
pub fn liveness_facts_from_read(
    read: crate::codex::ThreadReadResponse,
    loaded: bool,
) -> crate::worker::CodexLivenessFacts {
    use crate::codex::{ThreadActiveFlag, ThreadStatus, TurnStatus};
    use crate::worker::{CodexLivenessFacts, LastTurnFacts, ThreadStatusLite, TurnStatusLite};

    let status = match read.thread.status {
        ThreadStatus::NotLoaded => ThreadStatusLite::NotLoaded,
        ThreadStatus::Idle => ThreadStatusLite::Idle,
        ThreadStatus::SystemError => ThreadStatusLite::SystemError,
        ThreadStatus::Active { active_flags } => ThreadStatusLite::Active {
            waiting_on_user_input: active_flags.contains(&ThreadActiveFlag::WaitingOnUserInput),
            waiting_on_approval: active_flags.contains(&ThreadActiveFlag::WaitingOnApproval),
        },
    };
    // `last_turn`: None = no turns present (None or empty list).
    let last_turn = read
        .thread
        .turns
        .as_deref()
        .and_then(|turns| turns.last())
        .map(|turn| LastTurnFacts {
            completed_at: turn.completed_at,
            status: match turn.status {
                TurnStatus::Completed => TurnStatusLite::Completed,
                TurnStatus::Interrupted => TurnStatusLite::Interrupted,
                TurnStatus::Failed => TurnStatusLite::Failed,
                TurnStatus::InProgress => TurnStatusLite::InProgress,
                TurnStatus::Unknown => TurnStatusLite::Unknown,
            },
        });
    CodexLivenessFacts {
        loaded,
        status,
        last_turn,
    }
}
