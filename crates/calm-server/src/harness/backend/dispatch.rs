//! The input owner checkpoints its attempt before entering these provider effects.
use super::*;
use crate::shared_codex_appserver::{CodexPromptDelivery, CodexPromptReport};
use calm_types::conversation::{InputAttempt, LogicalTurnId, SubmissionDispatchProof};

impl PlannerBackend {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn dispatch_input<'a>(
        &self,
        logical_turn_id: &LogicalTurnId,
        attempt: &InputAttempt,
        thread_id: &str,
        proof: &SubmissionDispatchProof,
        items: Vec<InputItem>,
        selection: &TurnModelSelection,
        permission: PlannerPermissionMode,
        admission: &crate::shared_codex_appserver::NativePromptAdmission<'a>,
    ) -> InputAttemptReport {
        let correlation = AttemptCorrelation::new(attempt, self.provider(), thread_id);
        if proof.attempt() != attempt {
            return not_dispatched(
                correlation,
                NonDispatchBasis::LocalPreflight,
                CalmError::Conflict("input proof names another attempt".into()).into(),
            );
        }
        if let Some(reader) = self.issuance_hold() {
            return not_dispatched(
                correlation,
                NonDispatchBasis::LocalPreflight,
                TurnStartFailure::AwaitingRecovery {
                    error: CalmError::ServiceUnavailable(reader.clone()),
                    reader,
                },
            );
        }
        match &self.0 {
            Arm::Codex(daemon, approvals) => {
                approvals.route(daemon, thread_id);
                let native = daemon
                    .turn_start_report(
                        thread_id,
                        items,
                        selection,
                        TurnApprovals::Explicit(permission),
                        Some(attempt.submission_id.as_str()),
                        crate::shared_codex_appserver::NativeInput::canonical(
                            approvals.binding.clone(),
                            logical_turn_id,
                            proof,
                        ),
                        admission,
                    )
                    .await;
                codex_report(correlation, native, NativeAcceptanceBasis::CodexTurnStart)
            }
            Arm::Claude(_) | Arm::Acp(_) => not_dispatched(
                correlation,
                NonDispatchBasis::LocalPreflight,
                TurnStartFailure::Refused {
                    error: CalmError::BadRequest(
                        "canonical input execution is unsupported for Claude/ACP".into(),
                    ),
                    reader: "Canonical input execution is unsupported for Claude/ACP".into(),
                },
            ),
        }
    }
}

fn not_dispatched(
    correlation: AttemptCorrelation,
    basis: NonDispatchBasis,
    error: TurnStartFailure,
) -> InputAttemptReport {
    InputAttemptReport {
        delivery: InputDelivery::NotDispatched(NonDispatchEvidence::new(correlation, basis)),
        outcome: Err(error),
    }
}

fn codex_report(
    correlation: AttemptCorrelation,
    native: CodexPromptReport,
    basis: NativeAcceptanceBasis,
) -> InputAttemptReport {
    let rpc_failure = native.rpc_failure_ref();
    let correlation =
        correlation.with_request(rpc_failure.and_then(|failure| failure.request_id()));
    // A mismatched report can never settle the caller's input claim.
    let delivery = if native.thread_id() != correlation.thread_id()
        || native.client_id() != Some(correlation.client_id())
    {
        InputDelivery::Unknown(correlation)
    } else {
        match native.delivery() {
            CodexPromptDelivery::NotDispatched => {
                InputDelivery::NotDispatched(NonDispatchEvidence::new(
                    correlation,
                    if rpc_failure.is_some() {
                        NonDispatchBasis::TransportBeforeSend
                    } else {
                        NonDispatchBasis::LocalPreflight
                    },
                ))
            }
            CodexPromptDelivery::Unknown => InputDelivery::Unknown(correlation),
            CodexPromptDelivery::Accepted { turn_id } => InputDelivery::Accepted(
                NativeAcceptanceReceipt::codex(correlation, turn_id.clone(), basis),
            ),
        }
    };
    InputAttemptReport {
        delivery,
        outcome: match start::codex_start_result(native.into_start_outcome()) {
            Ok(TurnStartOutcome::Started {
                turn: TurnIdentity::Native(turn),
            }) => Ok(turn),
            Ok(TurnStartOutcome::Started { .. }) => {
                Err(CalmError::Conflict("Codex returned a non-native turn identity".into()).into())
            }
            Ok(TurnStartOutcome::Rejected { error }) => Err(TurnStartFailure::Refused {
                error: error.error,
                reader: error.reader,
            }),
            Ok(TurnStartOutcome::RecoveryRequired { error }) => {
                Err(TurnStartFailure::AwaitingRecovery {
                    error: error.error,
                    reader: error.reader,
                })
            }
            Ok(TurnStartOutcome::Failed { error }) => Err(error.into()),
            Err(error) => Err(error),
        },
    }
}
