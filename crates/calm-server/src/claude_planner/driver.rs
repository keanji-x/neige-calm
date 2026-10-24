//! Terminal outcome policy shared by turn completion.
use super::turn_slot::TerminalCause;
use provider::claude::translate::TurnOutcome;

/// The event that ends a turn (§6.2 columns).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TerminalEvent {
    ResultSuccess {
        is_error: bool,
        result: String,
    },
    ResultError {
        errors: Vec<String>,
    },
    /// Stdout ended or the child exited before a result.
    Exit {
        detail: String,
    },
    /// The stop timer, a shutdown, or a failed check or line: the recorded cause decides.
    Stopped,
}

/// §6.2: a recorded cause wins over the event that follows it; otherwise the event decides.
pub(crate) fn decide(cause: Option<&TerminalCause>, event: &TerminalEvent) -> TurnOutcome {
    match (cause, event) {
        (Some(TerminalCause::Failed(check)), _) => TurnOutcome::Failed {
            message: check.clone(),
        },
        (
            Some(TerminalCause::Interrupted),
            TerminalEvent::ResultSuccess {
                is_error: false, ..
            },
        ) => TurnOutcome::Completed,
        (Some(TerminalCause::Interrupted), _) => TurnOutcome::Interrupted,
        (
            None,
            TerminalEvent::ResultSuccess {
                is_error: false, ..
            },
        ) => TurnOutcome::Completed,
        (
            None,
            TerminalEvent::ResultSuccess {
                is_error: true,
                result,
            },
        ) => TurnOutcome::Failed {
            message: result.clone(),
        },
        (None, TerminalEvent::ResultError { errors }) if errors.is_empty() => TurnOutcome::Failed {
            message: "claude ended the turn with an error result".into(),
        },
        (None, TerminalEvent::ResultError { errors }) => TurnOutcome::Failed {
            message: errors.join("; "),
        },
        (None, TerminalEvent::Exit { detail }) => TurnOutcome::Failed {
            message: detail.clone(),
        },
        // Nothing stops a turn without recording why first; keep the reading honest if it did.
        (None, TerminalEvent::Stopped) => TurnOutcome::Failed {
            message: "claude planner turn stopped without a recorded cause".into(),
        },
    }
}
