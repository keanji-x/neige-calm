//! Turn issuance's host context and exact projection to the existing run-loop decisions.
use super::*;
use crate::shared_codex_appserver::control::StartError;

/// The reader is required for a refusal/recovery diagnostic, never defaulted for other facts.
#[derive(Debug)]
pub struct StartDiagnostic {
    pub error: CalmError,
    pub reader: String,
}

pub(super) fn codex_start_delivery(
    delivery: provider::codex::delivery::StartDelivery<StartError>,
) -> std::result::Result<TurnStartOutcome<CalmError, StartDiagnostic>, TurnStartFailure> {
    use provider::codex::delivery::StartDelivery;
    // C0 intentionally keeps the existing retry/rebuffer policy, including cleanup failure.
    let result = match delivery {
        StartDelivery::NativeAck {
            turn,
            cleanup: None,
        }
        | StartDelivery::Unknown {
            response_turn: Some(turn),
            ..
        } => Ok(turn),
        StartDelivery::NativeAck {
            cleanup: Some(error),
            ..
        }
        | StartDelivery::NotWritten { error }
        | StartDelivery::Unknown {
            error,
            response_turn: None,
        } => Err(error),
    };
    codex_start_result(result)
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
    revert_delivery(
        daemon
            .thread_revert_delivery(thread_id, before_turn_id)
            .await,
    )
}

fn revert_delivery(
    delivery: provider::codex::delivery::RevertDelivery<StartError>,
) -> std::result::Result<Option<TurnStartOutcome<CalmError, StartDiagnostic>>, TurnStartFailure> {
    use provider::codex::delivery::RevertDelivery;
    let error = match delivery {
        RevertDelivery::NativeAck => return Ok(None),
        RevertDelivery::NotWritten { error } | RevertDelivery::Unknown { error } => error,
    };
    Ok(match error {
        StartError::Host(error) => return Err(error.into()),
        StartError::Refused(error) | StartError::RecoveryRequired(error) => {
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
        StartError::Unanswered(error) => Some(TurnStartOutcome::Failed { error }),
    })
}

impl BackendRewind {
    /// The stored applied marker controls whether a native edit is still needed; it is never
    /// placed in the ephemeral protocol plan.
    pub(crate) fn turn_edit(&self) -> TurnEdit {
        let target = match &self.0 {
            RewindArm::Codex { reverted: true, .. } | RewindArm::CodexV2 { reverted: true, .. } => {
                return TurnEdit::NoEdit;
            }
            RewindArm::Codex {
                before_turn_id,
                reverted: false,
            }
            | RewindArm::CodexV2 {
                before_turn_id,
                reverted: false,
                ..
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
