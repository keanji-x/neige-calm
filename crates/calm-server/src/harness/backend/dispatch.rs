//! The input owner checkpoints its attempt before entering these provider effects.
use super::*;
use crate::shared_codex_appserver::{CodexPromptDelivery, CodexPromptReport};
use calm_types::conversation::{InputAttempt, LogicalTurnId, SubmissionDispatchProof};

impl PlannerBackend {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn dispatch_input(
        &self,
        logical_turn_id: &LogicalTurnId,
        attempt: &InputAttempt,
        thread_id: &str,
        proof: &SubmissionDispatchProof,
        items: Vec<InputItem>,
        selection: &TurnModelSelection,
        permission: PlannerPermissionMode,
        rewind: Option<&BackendRewind>,
    ) -> InputAttemptReport {
        let correlation = AttemptCorrelation::new(attempt, self.provider(), thread_id);
        let rewind_observation = rewind_observation(self, thread_id, rewind);
        if proof.attempt() != attempt {
            return not_dispatched(
                correlation,
                NonDispatchBasis::LocalPreflight,
                CalmError::Conflict("input proof names another attempt".into()).into(),
                rewind_observation,
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
                rewind_observation,
            );
        }
        // The canonical owner must checkpoint an independent successful rewind first.
        if rewind.is_some_and(|r| !matches!((&self.0, &r.0), (Arm::Codex(..), RewindArm::CodexV2 { thread_id: bound, reverted: true, .. }) if bound == thread_id)) {
            return not_dispatched(
                correlation,
                NonDispatchBasis::LocalPreflight,
                CalmError::Conflict(
                    "history replacement is not confirmed; input was not dispatched".into(),
                )
                .into(),
                rewind_observation,
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
                    )
                    .await;
                codex_report(
                    correlation,
                    native,
                    NativeAcceptanceBasis::CodexTurnStart,
                    rewind_observation,
                )
            }
            Arm::Claude(session) => {
                if rewind.is_some() {
                    return not_dispatched(
                        correlation,
                        NonDispatchBasis::Unsupported,
                        mismatched_rewind(),
                        rewind_observation,
                    );
                }
                let correlation = correlation.with_session(Some(thread_id.into()));
                match session
                    .dispatch_input(
                        logical_turn_id,
                        thread_id,
                        items,
                        selection,
                        permission,
                        proof,
                    )
                    .await
                {
                    Ok(turn) => InputAttemptReport {
                        delivery: InputDelivery::Unknown(correlation.with_local_turn(turn.clone())),
                        outcome: Ok(turn),
                        rewind: rewind_observation,
                        rpc_failure: None,
                    },
                    Err(failure) if !failure.write_attempted() => not_dispatched(
                        correlation,
                        NonDispatchBasis::LocalPreflight,
                        failure.into_failure(),
                        rewind_observation,
                    ),
                    Err(failure) => InputAttemptReport {
                        delivery: InputDelivery::Unknown(correlation),
                        outcome: Err(failure.into_failure()),
                        rewind: rewind_observation,
                        rpc_failure: None,
                    },
                }
            }
            Arm::Acp(session) => {
                if rewind.is_some() {
                    return not_dispatched(
                        correlation,
                        NonDispatchBasis::Unsupported,
                        mismatched_rewind(),
                        rewind_observation,
                    );
                }
                let native = session
                    .dispatch_input(
                        logical_turn_id,
                        thread_id,
                        items,
                        selection,
                        permission,
                        proof,
                    )
                    .await;
                let correlation =
                    correlation.with_session(native.native_session_id().map(str::to_owned));
                let owned = native.locally_owned();
                match native.into_outcome() {
                    Ok(turn) => InputAttemptReport {
                        delivery: InputDelivery::Unknown(correlation.with_local_turn(turn.clone())),
                        outcome: Ok(turn),
                        rewind: rewind_observation,
                        rpc_failure: None,
                    },
                    Err(failure) if !owned => not_dispatched(
                        correlation,
                        NonDispatchBasis::LocalPreflight,
                        failure,
                        rewind_observation,
                    ),
                    Err(failure) => InputAttemptReport {
                        delivery: InputDelivery::Unknown(correlation),
                        outcome: Err(failure),
                        rewind: rewind_observation,
                        rpc_failure: None,
                    },
                }
            }
        }
    }

    pub(crate) async fn dispatch_supplemental(
        &self,
        logical_turn_id: &LogicalTurnId,
        attempt: &InputAttempt,
        thread_id: &str,
        expected_turn_id: &str,
        proof: &SubmissionDispatchProof,
        items: Vec<InputItem>,
    ) -> InputAttemptReport {
        let correlation = AttemptCorrelation::new(attempt, self.provider(), thread_id);
        if proof.attempt() != attempt || expected_turn_id.is_empty() {
            return not_dispatched(
                correlation,
                NonDispatchBasis::LocalPreflight,
                CalmError::Conflict("supplemental input proof or native target is invalid".into())
                    .into(),
                RewindObservation::NotRequested,
            );
        }
        match &self.0 {
            Arm::Codex(daemon, owner) => {
                let native = daemon
                    .turn_steer_report(
                        thread_id,
                        expected_turn_id,
                        items,
                        Some(attempt.submission_id.as_str()),
                        crate::shared_codex_appserver::NativeSteer::Canonical {
                            binding: owner.binding.clone(),
                            logical_turn_id: logical_turn_id.clone(),
                        },
                    )
                    .await;
                codex_report(
                    correlation.with_turn(expected_turn_id.into()),
                    native,
                    NativeAcceptanceBasis::CodexTurnSteer,
                    RewindObservation::NotRequested,
                )
            }
            Arm::Claude(_) | Arm::Acp(_) => not_dispatched(
                correlation,
                NonDispatchBasis::Unsupported,
                CalmError::Conflict("this provider does not support supplemental input".into())
                    .into(),
                RewindObservation::NotRequested,
            ),
        }
    }
}

fn rewind_observation(
    backend: &PlannerBackend,
    thread_id: &str,
    rewind: Option<&BackendRewind>,
) -> RewindObservation {
    match rewind {
        None => RewindObservation::NotRequested,
        Some(
            plan @ BackendRewind(RewindArm::CodexV2 {
                thread_id: bound,
                reverted: true,
                ..
            }),
        ) if bound == thread_id && matches!(backend.0, Arm::Codex(..)) => {
            RewindObservation::Applied(plan.clone())
        }
        Some(plan) => RewindObservation::Unknown(plan.clone()),
    }
}

fn not_dispatched(
    correlation: AttemptCorrelation,
    basis: NonDispatchBasis,
    error: TurnStartFailure,
    rewind: RewindObservation,
) -> InputAttemptReport {
    InputAttemptReport {
        delivery: InputDelivery::NotDispatched(NonDispatchEvidence::new(correlation, basis)),
        outcome: Err(error),
        rewind,
        rpc_failure: None,
    }
}

fn codex_report(
    correlation: AttemptCorrelation,
    native: CodexPromptReport,
    basis: NativeAcceptanceBasis,
    rewind: RewindObservation,
) -> InputAttemptReport {
    let rpc_failure = native.rpc_failure_ref().map(NativeRpcFailure::from_rpc);
    let correlation = correlation.with_request(rpc_failure.as_ref().and_then(|f| f.request_id));
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
        rewind,
        rpc_failure,
        outcome: native.into_outcome().map_err(codex_turn_start_failure),
    }
}
