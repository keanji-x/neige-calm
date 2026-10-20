use super::*;

#[test]
fn m1_mapping_start_facts_keep_exact_error_reader_and_host_policy() {
    for case in [
        "native",
        "signin",
        "refused",
        "transport",
        "decode",
        "sealed",
    ] {
        let input = match case {
            "native" => Ok("turn".into()),
            "signin" => Err(StartError::RecoveryRequired(CalmError::CodexRefused(
                crate::codex_authentication::SIGN_IN_REFUSAL.into(),
            ))),
            "refused" => Err(StartError::Refused(CalmError::CodexRefused(
                "unknown model".into(),
            ))),
            "transport" => Err(StartError::Unanswered(CalmError::CodexAppServer(
                "closed".into(),
            ))),
            "decode" => Err(StartError::Unanswered(CalmError::Serde(
                serde_json::from_str::<bool>("bad").unwrap_err(),
            ))),
            "sealed" => Err(StartError::Host(CalmError::Conflict("sealed".into()))),
            _ => unreachable!(),
        };
        let original = input.as_ref().err().map(|error| match error {
            StartError::Host(error)
            | StartError::Refused(error)
            | StartError::RecoveryRequired(error)
            | StartError::Unanswered(error) => (std::mem::discriminant(error), error.to_string()),
        });
        let outcome = codex_start_result(input);
        let (error, reader) = match (case, outcome) {
            (
                "native",
                Ok(TurnStartOutcome::Started {
                    turn: TurnIdentity::Native(turn),
                }),
            ) => {
                assert_eq!(turn, "turn");
                continue;
            }
            (
                "signin",
                Ok(TurnStartOutcome::RecoveryRequired {
                    error: StartDiagnostic { error, reader },
                }),
            )
            | (
                "refused",
                Ok(TurnStartOutcome::Rejected {
                    error: StartDiagnostic { error, reader },
                }),
            ) => (error, reader),
            ("transport" | "decode", Ok(TurnStartOutcome::Failed { error }))
            | ("sealed", Err(TurnStartFailure::Transient(error))) => (error, String::new()),
            other => panic!("wrong operation fact {other:?}"),
        };
        assert_eq!(
            Some((std::mem::discriminant(&error), error.to_string())),
            original
        );
        if case == "signin" {
            assert_eq!(
                reader,
                format!(
                    "{} Your message is still queued.",
                    crate::codex_authentication::SIGN_IN_REQUIRED
                )
            );
        }
    }
}

#[test]
fn h2_revert_facts_keep_diagnostic_and_non_ack_host_policy() {
    use provider::codex::delivery::RevertDelivery;
    assert!(
        revert_delivery(RevertDelivery::NativeAck)
            .unwrap()
            .is_none()
    );
    for (written, refused) in [(false, false), (false, true), (true, false), (true, true)] {
        let error = CalmError::CodexAppServer("exact diagnostic".into());
        let original = error.to_string();
        let error = if refused {
            StartError::Refused(error)
        } else {
            StartError::Unanswered(error)
        };
        let result = revert_delivery(if written {
            RevertDelivery::Unknown { error }
        } else {
            RevertDelivery::NotWritten { error }
        })
        .unwrap()
        .unwrap();
        match result {
            TurnStartOutcome::Rejected {
                error: StartDiagnostic { error, reader },
            } => {
                assert!(refused);
                assert!(reader.contains("remove the edited turn"));
                assert_eq!(error.to_string(), original);
            }
            TurnStartOutcome::Failed { error } => {
                assert!(!refused);
                assert_eq!(error.to_string(), original);
            }
            other => panic!("wrong operation fact {other:?}"),
        }
    }
}

#[test]
fn m1_mapping_edit_plan_roundtrip_keeps_exact_storage_bytes() {
    for json in [
        r#"{"provider":"codex","before_turn_id":"turn"}"#,
        r#"{"provider":"claude","resume_at":"00000000-0000-0000-0000-000000000001","drops_turn":"00000000-0000-0000-0000-000000000002"}"#,
    ] {
        let rewind: BackendRewind = serde_json::from_str(json).unwrap();
        let TurnEdit::ApplyNativePlan(plan) = rewind.turn_edit() else {
            panic!("no plan")
        };
        assert_eq!(
            serde_json::to_string(&BackendRewind::from_plan(plan)).unwrap(),
            json
        );
    }
}

#[test]
fn h2_start_delivery_projects_to_unchanged_host_decisions() {
    use provider::codex::delivery::StartDelivery;
    for written in [false, true] {
        let input = if written {
            StartDelivery::Unknown {
                error: StartError::Unanswered(CalmError::CodexAppServer("timeout".into())),
                response_turn: None,
            }
        } else {
            StartDelivery::NotWritten {
                error: StartError::Unanswered(CalmError::CodexAppServer("not connected".into())),
            }
        };
        assert!(matches!(
            codex_start_delivery(input),
            Ok(TurnStartOutcome::Failed { .. })
        ));
    }
    assert!(matches!(
        codex_start_delivery(StartDelivery::Unknown {
            error: StartError::Refused(CalmError::CodexRefused("remote error".into())),
            response_turn: None
        }),
        Ok(TurnStartOutcome::Rejected { .. })
    ));
    assert!(matches!(
        codex_start_delivery(StartDelivery::NativeAck {
            turn: "turn".into(),
            cleanup: Some(StartError::Host(CalmError::Conflict("sealed".into())))
        }),
        Err(TurnStartFailure::Transient(CalmError::Conflict(_)))
    ));
    assert!(matches!(
        codex_start_delivery(StartDelivery::NativeAck {
            turn: "turn".into(),
            cleanup: Some(StartError::Unanswered(CalmError::CodexAppServer(
                "interrupt failed".into()
            )))
        }),
        Ok(TurnStartOutcome::Failed { .. })
    ));
    let empty_identity = StartDelivery::Unknown {
        error: StartError::Unanswered(CalmError::CodexAppServer("invalid id".into())),
        response_turn: Some(String::new()),
    };
    assert!(matches!(codex_start_delivery(empty_identity),
        Ok(TurnStartOutcome::Started { turn: TurnIdentity::Native(turn) }) if turn.is_empty()));
}
