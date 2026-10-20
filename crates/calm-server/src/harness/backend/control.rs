//! Native-result translation for the existing managed facade.
use super::*;
use crate::shared_codex_appserver::control::ControlError;

pub(super) fn steer_delivery(
    delivery: provider::codex::delivery::SteerDelivery<ControlError>,
) -> SteerOutcome<CalmError> {
    use provider::codex::delivery::SteerDelivery;
    match delivery {
        SteerDelivery::NativeAck { turn } => SteerOutcome::Taken {
            turn: TurnIdentity::Native(turn),
        },
        // Historical host decisions still use diagnostics; invalid receipts never become Taken.
        SteerDelivery::NotWritten { error } | SteerDelivery::Unknown { error } => match error {
            ControlError::Refused(error) => SteerOutcome::NotTaken { error },
            ControlError::Unanswered(error) => SteerOutcome::Unknown {
                error,
                evidence: SubmissionEvidence::Unobserved,
            },
        },
    }
}

pub(super) fn compact_delivery(
    delivery: provider::codex::delivery::CompactDelivery<ControlError>,
) -> CompactSubmission<CalmError> {
    use provider::codex::delivery::CompactDelivery;
    match delivery {
        CompactDelivery::NativeAck => CompactSubmission::Submitted {
            basis: SubmissionBasis::NativeAck,
        },
        CompactDelivery::NotWritten { error } => CompactSubmission::NotSubmitted {
            error: error.into(),
            reason: CompactNotSubmitted::PreparationFailed,
        },
        CompactDelivery::Unknown { error } => CompactSubmission::Unconfirmed {
            error: error.into(),
            evidence: SubmissionEvidence::Unobserved,
        },
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

    fn sealed() -> ControlError {
        ControlError::Unanswered(CalmError::Conflict("sealed before RPC".into()))
    }

    #[test]
    fn h2_steer_delivery_preserves_taken_not_taken_unknown_host_policy() {
        for input in [
            SteerDelivery::NotWritten {
                error: unanswered(),
            },
            SteerDelivery::Unknown {
                error: unanswered(),
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
            steer_delivery(SteerDelivery::Unknown { error: remote() }),
            SteerOutcome::NotTaken { .. }
        ));
        assert!(
            matches!(steer_delivery(SteerDelivery::NativeAck { turn: "turn".into() }), SteerOutcome::Taken { turn: TurnIdentity::Native(turn) } if turn == "turn")
        );
    }

    #[test]
    fn h2_compact_delivery_uses_only_ack_and_crossing_facts() {
        assert!(matches!(
            compact_delivery(CompactDelivery::NativeAck),
            CompactSubmission::Submitted {
                basis: SubmissionBasis::NativeAck
            }
        ));
        for make_error in [unanswered, remote, sealed] {
            assert!(matches!(
                compact_delivery(CompactDelivery::NotWritten {
                    error: make_error()
                }),
                CompactSubmission::NotSubmitted {
                    reason: CompactNotSubmitted::PreparationFailed,
                    ..
                }
            ));
            assert!(matches!(
                compact_delivery(CompactDelivery::Unknown {
                    error: make_error()
                }),
                CompactSubmission::Unconfirmed {
                    evidence: SubmissionEvidence::Unobserved,
                    ..
                }
            ));
        }
    }

    #[test]
    fn h2_control_delivery_keeps_original_diagnostics() {
        for make_error in [unanswered, remote, sealed] {
            for written in [false, true] {
                let error = make_error();
                let original = match &error {
                    ControlError::Refused(error) | ControlError::Unanswered(error) => {
                        (std::mem::discriminant(error), error.to_string())
                    }
                };
                let error = match compact_delivery(if written {
                    CompactDelivery::Unknown { error }
                } else {
                    CompactDelivery::NotWritten { error }
                }) {
                    CompactSubmission::NotSubmitted { error, .. }
                    | CompactSubmission::Unconfirmed { error, .. } => error,
                    CompactSubmission::Submitted { .. } => panic!("error became compact Ack"),
                };
                assert_eq!(
                    (std::mem::discriminant(&error), error.to_string()),
                    original
                );
                let error = make_error();
                let error = match steer_delivery(if written {
                    SteerDelivery::Unknown { error }
                } else {
                    SteerDelivery::NotWritten { error }
                }) {
                    SteerOutcome::NotTaken { error } | SteerOutcome::Unknown { error, .. } => error,
                    SteerOutcome::Taken { .. } => panic!("error became steer Ack"),
                };
                assert_eq!(
                    (std::mem::discriminant(&error), error.to_string()),
                    original
                );
            }
        }
    }
}
