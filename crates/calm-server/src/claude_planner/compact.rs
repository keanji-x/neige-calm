//! A manual context compaction of a Claude Planner conversation (`POST /planner/compact`): one
//! `claude -p --resume` spawn through the turn path (seal, `--version`, `stop`, instructions file,
//! settlement), whose one user line is `/compact` (the Agent SDK sends it as an ordinary input).
//!
//! It emits `TurnStarted` and `TurnCompleted` under a minted turn id, so the harness's
//! `Compacting` → `CompactionRunning` → `TurnCompleted` handling applies as it does for Codex, and
//! forwards ContextReplaced at the declared boundary, but no item, delta, usage reading or stored outcome row. It is therefore no
//! turn of the transcript and no anchor a rewind can cut at; the harness keeps its last turn id.
//! It completes only when the CLI reported a `system/compact_boundary` before its result.
//!
//! After it, a turn resumes the compacted session as before. A rewind still cuts at the last record
//! of the person's turn before the one it removes; when a compaction lies between them, that
//! anchor precedes the boundary and the cut would bring the uncompacted history back. The rewind's
//! dry run lets the CLI judge that cut before anything changes, and a refusal is shown with nothing
//! changed.

use super::driver::TerminalEvent;
use super::session::{BeforeWriteFailure, ClaudePlannerSession, SpawnFailure, SpawnInput};
use crate::error::CalmError;
use crate::harness::backend::TurnStartFailure;
use crate::planner_permission_mode::PlannerPermissionMode;
use provider::TurnModelSelection;
use provider::claude::protocol::{COMPACT_COMMAND, SystemInit};
use provider::control::{
    CompactNotSubmitted, CompactSubmission, SubmissionBasis, SubmissionEvidence,
};

/// What a compaction's result is decided as when no boundary preceded it.
pub(crate) const NO_BOUNDARY: &str = "claude ended the compaction without a compact boundary";

impl ClaudePlannerSession {
    pub(crate) async fn compact_canonical(
        &self,
        target: crate::harness::backend::MaintenanceTarget,
        selection: &TurnModelSelection,
        admission: &crate::harness::native_admission::NativePromptAdmission<'_>,
        track_fence: &crate::provider_execution::TrackFenceHandoff,
    ) -> CompactSubmission<CalmError> {
        if !self.owns_maintenance_target(&target) {
            return submission_result(Err(CalmError::Conflict(
                "maintenance target names another Claude owner".into(),
            )
            .into()));
        }
        let thread = target.thread_id.clone();
        submission_result(
            self.start_spawn(
                &thread,
                SpawnInput::CanonicalCompaction(target),
                selection,
                PlannerPermissionMode::Never,
                admission,
                track_fence,
            )
            .await,
        )
    }

    #[cfg(feature = "fixtures")]
    pub async fn compact_start_for_test(
        &self,
        thread: &str,
        selection: &TurnModelSelection,
    ) -> CompactSubmission<CalmError> {
        let fence =
            crate::harness::run_loop::issuance::track_fence_for_test(self.track_for_test()).await;
        let handoff = crate::provider_execution::TrackFenceHandoff::new(fence);
        self.compact_start(thread, selection, &handoff).await
    }

    /// Start a compaction of `thread`'s session and report its submission. `selection` is the card's
    /// model, so the summary runs on the conversation's own context window. It runs under `never`:
    /// it asks nothing of the person, whatever the card's turns run under. A conversation whose
    /// session does not exist yet is refused.
    pub(crate) async fn compact_start(
        &self,
        thread: &str,
        selection: &TurnModelSelection,
        track_fence: &crate::provider_execution::TrackFenceHandoff,
    ) -> CompactSubmission<CalmError> {
        submission_result(
            self.start_spawn(
                thread,
                SpawnInput::Compaction,
                selection,
                PlannerPermissionMode::Never,
                &super::session::historical_admission(),
                track_fence,
            )
            .await,
        )
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

/// A refusal of the compaction before anything was submitted; the reader is told `reason`.
pub(crate) fn refused(reason: impl std::fmt::Display) -> CalmError {
    CalmError::BadRequest(format!(
        "Claude will not compact this conversation: {reason}"
    ))
}

/// Every failure before the `/compact` line was written (a refusal, an unanswered `--version`, a
/// setup `stop` or credential mint that failed, a turn still running, a spawn that did not start)
/// submitted nothing, so the harness goes back to its previous phase as after Codex's refusal.
/// Once the line may have reached the CLI, the submission is unconfirmed and the harness wedges.
fn submission_result(result: Result<uuid::Uuid, SpawnFailure>) -> CompactSubmission<CalmError> {
    match result {
        Ok(_) => CompactSubmission::Submitted {
            basis: SubmissionBasis::LocalWrite,
        },
        Err(SpawnFailure::BeforeWrite(BeforeWriteFailure::Host(failure))) => {
            let error = match failure {
                TurnStartFailure::Refused { error, .. } => refused(error),
                TurnStartFailure::Transient(error)
                | TurnStartFailure::AwaitingRecovery { error, .. } => error,
            };
            CompactSubmission::NotSubmitted {
                error,
                reason: CompactNotSubmitted::PreparationFailed,
            }
        }
        Err(SpawnFailure::BeforeWrite(BeforeWriteFailure::Failed(error))) => {
            CompactSubmission::NotSubmitted {
                error,
                reason: CompactNotSubmitted::PreparationFailed,
            }
        }
        Err(SpawnFailure::CrossedWrite { error, local_turn }) => {
            tracing::warn!(%local_turn, %error, "Claude compaction crossed stdin before failing");
            CompactSubmission::Unconfirmed {
                error,
                evidence: SubmissionEvidence::WriteMayHaveReachedPeer,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error() -> CalmError {
        CalmError::ServiceUnavailable("original diagnostic".into())
    }

    #[test]
    fn m1_mapping_compact_spawn_failure_keeps_write_evidence_and_error() {
        assert!(matches!(
            submission_result(Ok(uuid::Uuid::new_v4())),
            CompactSubmission::Submitted {
                basis: SubmissionBasis::LocalWrite
            }
        ));
        for failure in [
            SpawnFailure::BeforeWrite(BeforeWriteFailure::Failed(error())),
            TurnStartFailure::Transient(error()).into(),
            TurnStartFailure::AwaitingRecovery {
                error: error(),
                reader: "hold wording".into(),
            }
            .into(),
        ] {
            assert!(
                matches!(submission_result(Err(failure)), CompactSubmission::NotSubmitted { error: CalmError::ServiceUnavailable(message), reason: CompactNotSubmitted::PreparationFailed } if message == "original diagnostic")
            );
        }
        let original = error().to_string();
        let refusal = TurnStartFailure::Refused {
            error: error(),
            reader: "operator wording".into(),
        };
        assert!(
            matches!(submission_result(Err(refusal.into())), CompactSubmission::NotSubmitted { error: CalmError::BadRequest(message), reason: CompactNotSubmitted::PreparationFailed } if message == format!("Claude will not compact this conversation: {original}"))
        );
        assert!(
            matches!(submission_result(Err(SpawnFailure::CrossedWrite { error: error(), local_turn: uuid::Uuid::new_v4() })), CompactSubmission::Unconfirmed { error: CalmError::ServiceUnavailable(message), evidence: SubmissionEvidence::WriteMayHaveReachedPeer } if message == "original diagnostic")
        );
    }
}
