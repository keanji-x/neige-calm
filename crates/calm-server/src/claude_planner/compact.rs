//! A manual context compaction of a Claude Planner conversation (`POST /planner/compact`): one
//! `claude -p --resume` spawn through the turn path (seal, `--version`, `stop`, instructions file,
//! settlement), whose one user line is `/compact` (the Agent SDK sends it as an ordinary input).
//!
//! It emits `TurnStarted` and `TurnCompleted` under a minted turn id, so the harness's
//! `Compacting` → `CompactionRunning` → `TurnCompleted` handling applies as it does for Codex, and
//! nothing else: no item, delta or usage reading, and no stored outcome row. It is therefore no
//! turn of the transcript and no anchor a rewind can cut at; the harness keeps its last turn id.
//! It completes only when the CLI reported a `system/compact_boundary` before its result.
//!
//! After it, a turn resumes the compacted session as before. A rewind still cuts at the last record
//! of the person's turn before the one it removes; when a compaction lies between them, that
//! anchor precedes the boundary and the cut would bring the uncompacted history back. The rewind's
//! dry run lets the CLI judge that cut before anything changes, and a refusal is shown with nothing
//! changed.

use super::driver::TerminalEvent;
use super::protocol::{COMPACT_COMMAND, SystemInit};
use super::session::{ClaudePlannerSession, SpawnInput};
use crate::error::CalmError;
use crate::harness::backend::TurnStartFailure;
use crate::planner_model::TurnModelSelection;
use crate::planner_permission_mode::PlannerPermissionMode;

/// What a compaction's result is decided as when no boundary preceded it.
pub(crate) const NO_BOUNDARY: &str = "claude ended the compaction without a compact boundary";

impl ClaudePlannerSession {
    /// Start a compaction of `thread`'s session and return its turn id. `selection` is the card's
    /// model, so the summary runs on the conversation's own context window. It runs under `never`:
    /// it asks nothing of the person. A conversation whose session does not exist yet is refused.
    pub async fn compact_start(
        &self,
        thread: &str,
        selection: &TurnModelSelection,
    ) -> Result<String, TurnStartFailure> {
        self.start_spawn(
            thread,
            SpawnInput::Compaction,
            selection,
            PlannerPermissionMode::Never,
        )
        .await
    }
}

/// The init check a compaction spawn makes in place of the turn's no-skills one: the CLI must list
/// `compact` among the commands its user line may run. Skills load with the commands, but the
/// spawn's `--tools` leaves the Skill tool off, so none can run.
pub(crate) fn init_check(init: &SystemInit) -> Result<(), String> {
    if init
        .slash_commands
        .iter()
        .any(|command| command == COMPACT_COMMAND)
    {
        Ok(())
    } else {
        Err(format!("claude lists no /{COMPACT_COMMAND} command"))
    }
}

/// A compaction's successful result counts only after its boundary: otherwise `/compact` did not
/// compact, and the result is decided as an error result.
pub(crate) fn result_event(event: TerminalEvent, compacted: bool) -> TerminalEvent {
    match event {
        TerminalEvent::ResultSuccess {
            is_error: false, ..
        } if !compacted => TerminalEvent::ResultSuccess {
            is_error: true,
            result: NO_BOUNDARY.into(),
        },
        event => event,
    }
}

/// Refusals made before anything was spawned roll the harness back as Codex's refusal does; every
/// other failure leaves the submission unconfirmed.
pub(crate) fn submission_error(failure: TurnStartFailure) -> CalmError {
    match failure {
        TurnStartFailure::Refused { error, .. } => CalmError::BadRequest(format!(
            "Claude will not compact this conversation: {error}"
        )),
        TurnStartFailure::Transient(error) | TurnStartFailure::AwaitingRecovery { error, .. } => {
            error
        }
    }
}
