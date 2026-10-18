//! Turn issuance's host context and exact projection to the existing run-loop decisions.
use super::*;
use crate::shared_codex_appserver::control::StartError;

/// The reader is required for a refusal/recovery diagnostic, never defaulted for other facts.
#[derive(Debug)]
pub struct StartDiagnostic {
    pub error: CalmError,
    pub reader: String,
}

/// Project native-originated outcomes to ordinary issuance policy. The outer error is already
/// host policy (admission, durable receipts, seals); it bypasses provider outcomes entirely.
/// Thus matching decisions here does not duplicate authority in the provider contract.
pub(crate) fn ordinary_start_result(
    result: std::result::Result<TurnStartOutcome<CalmError, StartDiagnostic>, TurnStartFailure>,
) -> std::result::Result<TurnId, TurnStartFailure> {
    match result? {
        TurnStartOutcome::Started { turn } => Ok(turn.into_string()),
        TurnStartOutcome::Failed { error } => Err(TurnStartFailure::Transient(error)),
        TurnStartOutcome::Rejected { error } => {
            let StartDiagnostic { error, reader } = error;
            Err(TurnStartFailure::Refused { error, reader })
        }
        TurnStartOutcome::RecoveryRequired { error } => {
            let StartDiagnostic { error, reader } = error;
            Err(TurnStartFailure::AwaitingRecovery { error, reader })
        }
    }
}

pub(super) fn codex_start_result(
    result: std::result::Result<TurnId, StartError>,
) -> std::result::Result<TurnStartOutcome<CalmError, StartDiagnostic>, TurnStartFailure> {
    Ok(match result {
        Err(StartError::Host(error)) => return Err(error.into()),
        Ok(turn) => TurnStartOutcome::Started {
            turn: TurnIdentity::Native(turn),
        },
        Err(StartError::RecoveryRequired(error)) => TurnStartOutcome::RecoveryRequired {
            error: StartDiagnostic {
                error,
                reader: format!(
                    "{} Your message is still queued.",
                    crate::codex_authentication::SIGN_IN_REQUIRED
                ),
            },
        },
        Err(StartError::Refused(error)) => TurnStartOutcome::Rejected {
            error: StartDiagnostic {
                error,
                reader: "codex refused to start a turn for this conversation, so \
                             your message has not been sent. If you changed the model \
                             recently, it may not be one this account can run — try \
                             another."
                    .into(),
            },
        },
        Err(StartError::Unanswered(error)) => TurnStartOutcome::Failed { error },
    })
}

pub(super) async fn revert_codex_thread(
    daemon: &SharedCodexAppServer,
    thread_id: &str,
    before_turn_id: &str,
) -> std::result::Result<Option<TurnStartOutcome<CalmError, StartDiagnostic>>, TurnStartFailure> {
    revert_result(daemon.thread_revert(thread_id, before_turn_id).await)
}

fn revert_result(
    result: std::result::Result<(), StartError>,
) -> std::result::Result<Option<TurnStartOutcome<CalmError, StartDiagnostic>>, TurnStartFailure> {
    Ok(match result {
        Err(StartError::Host(error)) => return Err(error.into()),
        Ok(()) => None,
        Err(StartError::Refused(error) | StartError::RecoveryRequired(error)) => {
            Some(TurnStartOutcome::Rejected {
                error: StartDiagnostic {
                    error,
                    reader:
                        "codex refused to remove the edited turn from this conversation, so your \
                     message has not been sent. Reset the conversation to continue."
                            .into(),
                },
            })
        }
        Err(StartError::Unanswered(error)) => Some(TurnStartOutcome::Failed { error }),
    })
}

impl BackendRewind {
    /// The stored applied marker controls whether a native edit is still needed; it is never
    /// placed in the ephemeral protocol plan.
    pub(crate) fn turn_edit(&self) -> TurnEdit {
        let target = match &self.0 {
            RewindArm::Codex { reverted: true, .. } => return TurnEdit::NoEdit,
            RewindArm::Codex {
                before_turn_id,
                reverted: false,
            } => NativeEditTarget::BeforeTurn {
                before_turn_id: before_turn_id.clone(),
            },
            RewindArm::Claude {
                resume_at,
                drops_turn,
            } => NativeEditTarget::TranscriptCut {
                resume_at: *resume_at,
                drops_turn: *drops_turn,
            },
        };
        TurnEdit::ApplyNativePlan(NativeEditPlan { target })
    }
    pub(crate) fn from_plan(plan: NativeEditPlan) -> Self {
        Self(match plan.target {
            NativeEditTarget::BeforeTurn { before_turn_id } => RewindArm::Codex {
                before_turn_id,
                reverted: false,
            },
            NativeEditTarget::TranscriptCut {
                resume_at,
                drops_turn,
            } => RewindArm::Claude {
                resume_at,
                drops_turn,
            },
        })
    }
}

#[cfg(test)]
mod tests;
