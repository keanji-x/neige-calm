//! Native-result translation for the existing managed facade.
use super::*;
use crate::shared_codex_appserver::control::ControlError;

pub(super) fn steer_delivery(
    delivery: provider::codex::delivery::SteerDelivery<ControlError>,
) -> SteerOutcome<CalmError> {
    use provider::codex::delivery::SteerDelivery;
    // C0 projects even invalid decoded identities to the historical Taken decision.
    let result = match delivery {
        SteerDelivery::NativeAck { turn } => Ok(turn),
        SteerDelivery::Unknown {
            response_turn: Some(turn),
            ..
        } => Ok(turn),
        SteerDelivery::NotWritten { error }
        | SteerDelivery::Unknown {
            error,
            response_turn: None,
        } => Err(error),
    };
    steer_outcome(result)
}

pub(super) fn compact_delivery(
    delivery: provider::codex::delivery::CompactDelivery<ControlError>,
) -> CompactSubmission<CalmError> {
    use provider::codex::delivery::CompactDelivery;
    let result = match delivery {
        CompactDelivery::NativeAck
        | CompactDelivery::Unknown {
            result_received: true,
            ..
        } => Ok(()),
        CompactDelivery::NotWritten { error }
        | CompactDelivery::Unknown {
            error,
            result_received: false,
        } => Err(error),
    };
    codex_compact_submission(result)
}

pub(super) fn steer_outcome(
    result: std::result::Result<TurnId, ControlError>,
) -> SteerOutcome<CalmError> {
    match result {
        Ok(turn) => SteerOutcome::Taken {
            turn: TurnIdentity::Native(turn),
        },
        Err(ControlError::Refused(error)) => SteerOutcome::NotTaken { error },
        Err(ControlError::Unanswered(error)) => SteerOutcome::Unknown {
            error,
            evidence: SubmissionEvidence::Unobserved,
        },
    }
}

pub(super) fn codex_compact_submission(
    result: std::result::Result<(), ControlError>,
) -> CompactSubmission<CalmError> {
    match result {
        Ok(()) => CompactSubmission::Submitted {
            basis: SubmissionBasis::NativeAck,
        },
        Err(ControlError::Refused(error)) => CompactSubmission::NotSubmitted {
            error,
            reason: CompactNotSubmitted::NativeRefusal,
        },
        Err(ControlError::Unanswered(error)) => CompactSubmission::Unconfirmed {
            error,
            evidence: SubmissionEvidence::Unobserved,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m1_mapping_codex_steer_keeps_refusal_and_unanswered_distinct() {
        let cases = [
            (Ok("native-turn".into()), "taken", "native-turn".into()),
            (
                Err(ControlError::Refused(CalmError::CodexRefused(
                    "native refusal".into(),
                ))),
                "refused",
                "native refusal".into(),
            ),
            (
                Err(ControlError::Unanswered(CalmError::CodexAppServer(
                    "unanswered".into(),
                ))),
                "unknown",
                CalmError::CodexAppServer("unanswered".into()).to_string(),
            ),
            (
                Err(ControlError::Unanswered(CalmError::Conflict(
                    "sealed".into(),
                ))),
                "unknown",
                CalmError::Conflict("sealed".into()).to_string(),
            ),
        ];
        for (input, expected, reader) in cases {
            let diagnostic = input.as_ref().err().map(|e| match e {
                ControlError::Refused(e) | ControlError::Unanswered(e) => {
                    (std::mem::discriminant(e), e.to_string())
                }
            });
            let (actual, error) = match steer_outcome(input) {
                SteerOutcome::Taken {
                    turn: TurnIdentity::Native(turn),
                } => {
                    assert_eq!(turn, reader);
                    ("taken", None)
                }
                SteerOutcome::NotTaken { error } => ("refused", Some(error)),
                SteerOutcome::Unknown { error, evidence } => {
                    assert_eq!(evidence, SubmissionEvidence::Unobserved);
                    ("unknown", Some(error))
                }
                other => panic!("wrong mapping: {other:?}"),
            };
            assert_eq!(actual, expected);
            if let Some(error) = error {
                assert_eq!(
                    if expected == "refused" {
                        error.reason()
                    } else {
                        error.to_string()
                    },
                    reader
                );
                assert_eq!(
                    Some((std::mem::discriminant(&error), error.to_string())),
                    diagnostic
                );
            }
        }
    }

    #[test]
    fn m1_mapping_codex_compact_preserves_coarse_uncertainty() {
        let cases = [
            (Ok(()), None),
            (
                Err(ControlError::Refused(CalmError::CodexRefused(
                    "refusal".into(),
                ))),
                Some(CompactNotSubmitted::NativeRefusal),
            ),
            (
                Err(ControlError::Unanswered(CalmError::Conflict(
                    "sealed before RPC".into(),
                ))),
                None,
            ),
            (
                Err(ControlError::Unanswered(CalmError::CodexAppServer(
                    "timeout".into(),
                ))),
                None,
            ),
        ];
        for (input, reason) in cases {
            let original = input.as_ref().err().map(|e| match e {
                ControlError::Refused(e) | ControlError::Unanswered(e) => {
                    (std::mem::discriminant(e), e.to_string())
                }
            });
            let error = match codex_compact_submission(input) {
                CompactSubmission::Submitted { basis } => {
                    assert_eq!(basis, SubmissionBasis::NativeAck);
                    assert!(original.is_none());
                    continue;
                }
                CompactSubmission::NotSubmitted {
                    error,
                    reason: actual,
                } => {
                    assert_eq!(Some(actual), reason);
                    error
                }
                CompactSubmission::Unconfirmed { error, evidence } => {
                    assert!(reason.is_none());
                    assert_eq!(evidence, SubmissionEvidence::Unobserved);
                    error
                }
            };
            assert_eq!(
                Some((std::mem::discriminant(&error), error.to_string())),
                original
            );
        }
    }
}

#[cfg(test)]
mod h2_tests {
    use super::*;
    use provider::codex::delivery::{CompactDelivery, SteerDelivery};

    fn unanswered() -> ControlError {
        ControlError::Unanswered(CalmError::CodexAppServer("unchanged diagnostic".into()))
    }
    fn remote() -> ControlError {
        ControlError::Refused(CalmError::CodexRefused("remote error".into()))
    }

    #[test]
    fn h2_steer_delivery_preserves_taken_not_taken_unknown_host_policy() {
        for turn in ["", "different-turn"] {
            assert!(
                matches!(steer_delivery(SteerDelivery::Unknown { error: unanswered(), response_turn: Some(turn.into()) }), SteerOutcome::Taken { turn: TurnIdentity::Native(actual) } if actual == turn)
            );
        }
        for input in [
            SteerDelivery::NotWritten {
                error: unanswered(),
            },
            SteerDelivery::Unknown {
                error: unanswered(),
                response_turn: None,
            },
        ] {
            assert!(matches!(
                steer_delivery(input),
                SteerOutcome::Unknown {
                    evidence: SubmissionEvidence::Unobserved,
                    ..
                }
            ));
        }
        assert!(matches!(
            steer_delivery(SteerDelivery::Unknown {
                error: remote(),
                response_turn: None
            }),
            SteerOutcome::NotTaken { .. }
        ));
        assert!(
            matches!(steer_delivery(SteerDelivery::NativeAck { turn: "turn".into() }), SteerOutcome::Taken { turn: TurnIdentity::Native(turn) } if turn == "turn")
        );
    }

    #[test]
    fn h2_compact_delivery_preserves_not_submitted_unconfirmed_host_policy() {
        for input in [
            CompactDelivery::NotWritten {
                error: unanswered(),
            },
            CompactDelivery::Unknown {
                error: unanswered(),
                result_received: false,
            },
        ] {
            assert!(matches!(
                compact_delivery(input),
                CompactSubmission::Unconfirmed {
                    evidence: SubmissionEvidence::Unobserved,
                    ..
                }
            ));
        }
        assert!(matches!(
            compact_delivery(CompactDelivery::Unknown {
                error: remote(),
                result_received: false
            }),
            CompactSubmission::NotSubmitted {
                reason: CompactNotSubmitted::NativeRefusal,
                ..
            }
        ));
        for input in [
            CompactDelivery::NativeAck,
            CompactDelivery::Unknown {
                error: unanswered(),
                result_received: true,
            },
        ] {
            assert!(matches!(
                compact_delivery(input),
                CompactSubmission::Submitted {
                    basis: SubmissionBasis::NativeAck
                }
            ));
        }
    }
}
