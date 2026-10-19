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
fn m1_mapping_revert_facts_keep_diagnostic_and_policy() {
    assert!(revert_result(Ok(())).unwrap().is_none());
    for refused in [false, true] {
        let error = CalmError::CodexAppServer("exact diagnostic".into());
        let original = error.to_string();
        let result = revert_result(Err(if refused {
            StartError::Refused(error)
        } else {
            StartError::Unanswered(error)
        }))
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
