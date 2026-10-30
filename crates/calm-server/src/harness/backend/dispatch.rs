//! The input owner checkpoints its attempt before entering these provider effects.
use super::*;
use crate::shared_codex_appserver::{CodexPromptDelivery, CodexPromptReport};
use calm_types::conversation::{InputAttempt, LogicalTurnId, SubmissionDispatchProof};

impl PlannerBackend {
    pub(crate) async fn dispatch_supplemental(
        &self,
        target: &ExecutionEndTarget,
        proof: &SubmissionDispatchProof,
        items: Vec<InputItem>,
        admission: &crate::harness::native_admission::NativePromptAdmission<'_>,
    ) -> InputAttemptReport {
        let (thread, turn) = target.scope.thread_and_turn();
        let correlation = AttemptCorrelation::new(proof.attempt(), self.provider(), &thread)
            .with_turn(turn.clone());
        let Arm::Codex(daemon, owner) = &self.0 else {
            return not_dispatched(
                correlation,
                NonDispatchBasis::LocalPreflight,
                CalmError::Conflict("provider does not support supplemental input".into()).into(),
            );
        };
        if !owner.binding.matches(target) || proof.attempt() == target.proof.attempt() {
            return not_dispatched(
                correlation,
                NonDispatchBasis::LocalPreflight,
                CalmError::Conflict("supplemental target differs from its original owner".into())
                    .into(),
            );
        }
        let native = daemon
            .turn_steer_report(
                &thread,
                &turn,
                items,
                Some(proof.attempt().submission_id.as_str()),
                crate::shared_codex_appserver::NativeSteer::CanonicalSupplemental {
                    target: Box::new(target.clone()),
                    proof: proof.clone(),
                },
                admission,
            )
            .await;
        codex_report(correlation, native, NativeAcceptanceBasis::CodexTurnSteer)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn dispatch_input<'a>(
        &self,
        settlement_card: Option<&str>,
        logical_turn_id: &LogicalTurnId,
        attempt: &InputAttempt,
        thread_id: &str,
        proof: &SubmissionDispatchProof,
        items: Vec<InputItem>,
        selection: &TurnModelSelection,
        permission: PlannerPermissionMode,
        admission: &crate::harness::native_admission::NativePromptAdmission<'a>,
        track_fence: &crate::provider_execution::TrackFenceHandoff,
    ) -> InputAttemptReport {
        if matches!(&self.0, Arm::Codex(..)) {
            track_fence.release();
        }
        let correlation = AttemptCorrelation::new(attempt, self.provider(), thread_id);
        if let Some(card) = settlement_card {
            match self.settled_activity(card, thread_id, admission).await {
                Ok(SettledActivity::Settled) => {}
                Ok(SettledActivity::Held) => {
                    return not_dispatched(
                        correlation,
                        NonDispatchBasis::LocalPreflight,
                        CalmError::Conflict("kept original thread has not settled".into()).into(),
                    );
                }
                Err(error) => {
                    return not_dispatched(
                        correlation,
                        NonDispatchBasis::LocalPreflight,
                        error.into(),
                    );
                }
            }
        }
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
                            settlement_card.is_some(),
                        ),
                        admission,
                    )
                    .await;
                codex_report(correlation, native, NativeAcceptanceBasis::CodexTurnStart)
            }
            Arm::Claude(session) => {
                use crate::claude_planner::session::{BeforeWriteFailure, SpawnFailure};
                match session
                    .start_canonical(
                        thread_id,
                        items,
                        attempt.submission_id.as_str(),
                        selection,
                        permission,
                        ExecutionInput::canonical(logical_turn_id, proof),
                        admission,
                        track_fence,
                    )
                    .await
                {
                    Ok(turn) => InputAttemptReport::unknown(
                        correlation.with_local_turn(turn),
                        Ok(ExecutionIdentity::Local(turn)),
                    ),
                    Err(SpawnFailure::BeforeWrite(failure)) => not_dispatched(
                        correlation,
                        NonDispatchBasis::TransportBeforeSend,
                        match failure {
                            BeforeWriteFailure::Host(failure) => failure,
                            BeforeWriteFailure::Failed(error) => error.into(),
                        },
                    ),
                    Err(SpawnFailure::CrossedWrite { error, local_turn }) => {
                        InputAttemptReport::unknown(
                            correlation.with_local_turn(local_turn),
                            Err(error.into()),
                        )
                    }
                }
            }
            Arm::Acp(session) => {
                use crate::acp_planner::session::CanonicalStart;
                match session
                    .start_canonical(
                        thread_id,
                        items,
                        attempt.submission_id.as_str(),
                        selection,
                        permission,
                        ExecutionInput::canonical(logical_turn_id, proof),
                        admission,
                        track_fence,
                    )
                    .await
                {
                    CanonicalStart::UncertainReceipt(error) => {
                        InputAttemptReport::unknown(correlation, Err(error.into()))
                    }
                    CanonicalStart::BeforeReceipt(error) => {
                        not_dispatched(correlation, NonDispatchBasis::LocalPreflight, error)
                    }
                    CanonicalStart::Receipted { receipt, outcome } => {
                        let correlation =
                            correlation.with_native_session(receipt.native_session_id);
                        match uuid::Uuid::parse_str(&receipt.turn) {
                            Ok(turn) => InputAttemptReport::unknown(
                                correlation.with_local_turn(turn),
                                outcome
                                    .map(|()| ExecutionIdentity::Local(turn))
                                    .map_err(Into::into),
                            ),
                            Err(error) => InputAttemptReport::unknown(
                                correlation,
                                Err(CalmError::Conflict(format!(
                                    "ACP local receipt identity is invalid: {error}"
                                ))
                                .into()),
                            ),
                        }
                    }
                }
            }
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
            }) => Ok(ExecutionIdentity::Native(turn)),
            Ok(TurnStartOutcome::Started {
                turn: TurnIdentity::Local(_),
            }) => {
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
