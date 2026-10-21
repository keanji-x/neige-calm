use super::*;
use crate::runtime::AgentProvider;
use serde_json::json;
use std::num::NonZeroU32;

fn attempt(id: &str) -> InputAttempt {
    InputAttempt {
        submission_id: InputSubmissionId::parse("11111111111111111111111111111111").unwrap(),
        attempt_id: InputAttemptId::parse(id).unwrap(),
    }
}
fn correlation() -> InputCorrelationFact {
    InputCorrelationFact {
        provider: AgentProvider::Codex,
        thread_id: "thread".into(),
        native_session_id: None,
        local_turn_id: None,
        native_turn_id: None,
        rpc_request_id: None,
    }
}
fn human(revision: u32) -> PendingInput {
    PendingInput::Human {
        queue_position: u64::from(revision - 1),
        message: MessageRevisionRef {
            message_id: ConversationMessageId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
            revision: NonZeroU32::new(revision).unwrap(),
        },
        queued_at_ms: 10,
        envelope_id: None,
    }
}

#[test]
fn pending_v2_has_references_and_rejects_two_revisions_of_one_message() {
    let first = human(1);
    assert!(validate_pending_inputs(&[first.clone(), human(2)]).is_err());
    let mut hidden_body = serde_json::to_value(first).unwrap();
    hidden_body["text"] = json!("not a snapshot body");
    assert!(serde_json::from_value::<PendingInput>(hidden_body).is_err());
    assert!(
        serde_json::from_value::<ConversationInputCheckpoint>(json!({"pending":[],"active":null}))
            .is_err()
    );
}

#[test]
fn durable_unknown_cannot_be_replaced_by_a_new_refusal_or_crossed_native_report() {
    let original = attempt("22222222222222222222222222222222");
    let later = attempt("33333333333333333333333333333333");
    let mut checkpoint = ConversationInputCheckpoint::empty();
    checkpoint
        .begin_attempt(original.clone(), correlation(), invocation())
        .unwrap();
    assert!(
        checkpoint
            .begin_attempt(later.clone(), correlation(), invocation())
            .is_err()
    );
    let refusal = InputAttemptFact {
        attempt: later,
        correlation: correlation(),
        delivery: InputDeliveryFact::NotDispatched {
            basis: "transport_before_send".into(),
        },
    };
    assert!(checkpoint.record_attempt_fact(refusal).is_err());
    let mut other_thread = correlation();
    other_thread.thread_id = "other-thread".into();
    assert!(
        checkpoint
            .record_attempt_fact(InputAttemptFact {
                attempt: original.clone(),
                correlation: other_thread,
                delivery: InputDeliveryFact::Unknown
            })
            .is_err()
    );
    assert_eq!(
        checkpoint
            .attempt(&original.submission_id)
            .unwrap()
            .delivery,
        InputDeliveryFact::CrossingUnknown
    );
}

#[test]
fn exact_non_dispatch_permits_a_new_attempt_but_stale_and_accepted_facts_remain_fenced() {
    let original = attempt("22222222222222222222222222222222");
    let later = attempt("33333333333333333333333333333333");
    let mut checkpoint = ConversationInputCheckpoint::empty();
    checkpoint
        .begin_attempt(original.clone(), correlation(), invocation())
        .unwrap();
    let refusal = InputAttemptFact {
        attempt: original.clone(),
        correlation: correlation(),
        delivery: InputDeliveryFact::NotDispatched {
            basis: "transport_before_send".into(),
        },
    };
    checkpoint.record_attempt_fact(refusal.clone()).unwrap();
    checkpoint
        .begin_attempt(later.clone(), correlation(), invocation())
        .unwrap();
    assert!(checkpoint.record_attempt_fact(refusal).is_err());
    let mut accepted_correlation = correlation();
    accepted_correlation.native_turn_id = Some("native-turn".into());
    checkpoint
        .record_attempt_fact(InputAttemptFact {
            attempt: later.clone(),
            correlation: accepted_correlation,
            delivery: InputDeliveryFact::Accepted {
                basis: "codex_turn_start".into(),
                evidence: NativeInputAcceptance::NativeTurn {
                    native_turn_id: "native-turn".into(),
                },
            },
        })
        .unwrap();
    assert!(
        checkpoint
            .begin_attempt(
                attempt("44444444444444444444444444444444"),
                correlation(),
                invocation()
            )
            .is_err()
    );
    assert!(matches!(
        checkpoint.attempt(&later.submission_id).unwrap().delivery,
        InputDeliveryFact::Accepted { .. }
    ));
}

#[test]
fn local_turn_ownership_is_not_positive_native_delivery() {
    let original = attempt("22222222222222222222222222222222");
    let mut checkpoint = ConversationInputCheckpoint::empty();
    checkpoint
        .begin_attempt(original.clone(), correlation(), invocation())
        .unwrap();
    let mut local = correlation();
    local.local_turn_id = Some("owned-local-turn".into());
    checkpoint
        .record_attempt_fact(InputAttemptFact {
            attempt: original.clone(),
            correlation: local.clone(),
            delivery: InputDeliveryFact::Unknown,
        })
        .unwrap();
    assert!(
        checkpoint
            .record_attempt_fact(InputAttemptFact {
                attempt: original.clone(),
                correlation: local.clone(),
                delivery: InputDeliveryFact::NotDispatched {
                    basis: "later_local_refusal".into()
                }
            })
            .is_err(),
        "reported Unknown cannot be cleared by a same-attempt local refusal"
    );
    assert!(
        checkpoint
            .record_attempt_fact(InputAttemptFact {
                attempt: original,
                correlation: local,
                delivery: InputDeliveryFact::Accepted {
                    basis: "local_start".into(),
                    evidence: NativeInputAcceptance::NativeTurn {
                        native_turn_id: "owned-local-turn".into()
                    }
                }
            })
            .is_err()
    );
}

#[test]
fn canonical_held_projection_survives_without_active_execution() {
    let mut checkpoint = ConversationInputCheckpoint::empty();
    checkpoint
        .begin_attempt(
            attempt("22222222222222222222222222222222"),
            correlation(),
            invocation(),
        )
        .unwrap();
    let projected = crate::conversation_display::ConversationActiveInput::from_checkpoint(
        &checkpoint,
        "carrier",
        &[(
            attempt("22222222222222222222222222222222").submission_id,
            LogicalTurnId::parse("33333333333333333333333333333333").unwrap(),
        )],
        |_| Ok(crate::conversation_display::ConversationActionAvailability::Unavailable {
            reason: crate::conversation_display::ConversationActionUnavailableReason::UnsupportedSource,
        }),
    )
    .unwrap();
    let projected = serde_json::to_value(projected).unwrap();
    assert_eq!(
        projected["submissions"][0]["submission_id"],
        "11111111111111111111111111111111"
    );
}

#[test]
fn canonical_attempt_has_checked_revision_and_replay_does_not_advance_it() {
    let original = attempt("22222222222222222222222222222222");
    let mut checkpoint = ConversationInputCheckpoint::empty();
    checkpoint
        .begin_attempt(original.clone(), correlation(), invocation())
        .unwrap();
    let report = InputAttemptFact {
        attempt: original.clone(),
        correlation: correlation(),
        delivery: InputDeliveryFact::Unknown,
    };
    checkpoint.record_attempt_fact(report.clone()).unwrap();
    checkpoint.record_attempt_fact(report).unwrap();
    assert_eq!(
        serde_json::to_value(checkpoint.attempt(&original.submission_id).unwrap()).unwrap()["revision"],
        1
    );
}

fn invocation() -> FrozenInputInvocation {
    FrozenInputInvocation {
        model: None,
        effort: None,
        permission: crate::harness::PlannerPermissionMode::Never,
    }
}

#[test]
fn canonical_attempt_revision_overflow_is_atomic() {
    let original = attempt("22222222222222222222222222222222");
    let mut checkpoint = ConversationInputCheckpoint::empty();
    checkpoint
        .begin_attempt(original.clone(), correlation(), invocation())
        .unwrap();
    checkpoint.attempts[0].revision = u64::MAX;
    let before = checkpoint.clone();
    assert!(
        checkpoint
            .record_attempt_fact(InputAttemptFact {
                attempt: original,
                correlation: correlation(),
                delivery: InputDeliveryFact::Unknown
            })
            .is_err()
    );
    assert_eq!(checkpoint, before);
}

fn projection(delivery: InputDeliveryFact) -> crate::conversation_display::ConversationActiveInput {
    let original = attempt("22222222222222222222222222222222");
    let mut checkpoint = ConversationInputCheckpoint::empty();
    checkpoint
        .begin_attempt(original.clone(), correlation(), invocation())
        .unwrap();
    checkpoint.attempts[0].revision = 7;
    if let InputDeliveryFact::Accepted {
        evidence: NativeInputAcceptance::NativeTurn { native_turn_id },
        ..
    } = &delivery
    {
        checkpoint.attempts[0].correlation.native_turn_id = Some(native_turn_id.clone());
    }
    if matches!(
        &delivery,
        InputDeliveryFact::NotDispatched { .. } | InputDeliveryFact::Accepted { .. }
    ) {
        checkpoint.active = Some(ActiveInputSubmissions {
            logical_turn_id: LogicalTurnId::parse("33333333333333333333333333333333").unwrap(),
            submission_ids: vec![original.submission_id.clone()],
        });
    }
    checkpoint.attempts[0].delivery = delivery;
    crate::conversation_display::ConversationActiveInput::from_checkpoint(
        &checkpoint,
        "carrier",
        &[(
            original.submission_id,
            LogicalTurnId::parse("33333333333333333333333333333333").unwrap(),
        )],
        |_| Ok(crate::conversation_display::ConversationActionAvailability::Unavailable {
            reason: crate::conversation_display::ConversationActionUnavailableReason::UnsupportedSource,
        }),
    )
    .unwrap()
    .unwrap()
}

#[test]
fn canonical_held_projection_carries_exact_attempt_revision() {
    let view = projection(InputDeliveryFact::Unknown);
    let submission = &view.submissions[0];
    assert_eq!(submission.carrier_id, "carrier");
    assert_eq!(
        submission.logical_turn_id,
        "33333333333333333333333333333333"
    );
    let attempt = submission.attempt.as_ref().unwrap();
    assert_eq!(attempt.revision, "7");
    assert_eq!(attempt.attempt_id, "22222222222222222222222222222222");
    assert_eq!(attempt.target.thread_id, "thread");
    assert_eq!(attempt.target.provider, AgentProvider::Codex);
    assert_eq!(
        attempt.invocation,
        InputInvocation::Recorded {
            configuration: invocation()
        }
    );
}

#[test]
fn canonical_projection_each_supported_status_and_recorded_echo() {
    use crate::conversation_display::ConversationDelivery;
    for (fact, expected) in [
        (
            InputDeliveryFact::CrossingUnknown,
            ConversationDelivery::CrossingUnknown,
        ),
        (InputDeliveryFact::Unknown, ConversationDelivery::Unknown),
        (
            InputDeliveryFact::NotDispatched {
                basis: "local_preflight".into(),
            },
            ConversationDelivery::NotDispatched,
        ),
        (
            InputDeliveryFact::Accepted {
                basis: "codex_turn_start".into(),
                evidence: NativeInputAcceptance::NativeTurn {
                    native_turn_id: "native".into(),
                },
            },
            ConversationDelivery::Accepted,
        ),
    ] {
        let view = projection(fact);
        let attempt = view.submissions[0].attempt.as_ref().unwrap();
        assert_eq!(attempt.revision, "7");
        assert_eq!(attempt.delivery, expected);
        assert_eq!(
            view.current_execution_id.is_none(),
            matches!(
                expected,
                ConversationDelivery::CrossingUnknown | ConversationDelivery::Unknown
            )
        );
    }
}

#[test]
fn canonical_projection_no_host_paths_or_diagnostic_leakage_and_typed_actions() {
    let encoded = serde_json::to_value(projection(InputDeliveryFact::NotDispatched {
        basis: "/private/secret token=password".into(),
    }))
    .unwrap();
    for secret in [
        "/private",
        "password",
        "basis",
        "diagnostic",
        "text",
        "path",
    ] {
        assert!(!encoded.to_string().contains(secret), "{secret}");
    }
    assert_eq!(
        encoded["submissions"][0]["attempt"]["actions"],
        json!({
            "keep":{"kind":"unavailable","reason":"not_held"},
            "retry":{"kind":"unavailable","reason":"unsupported_source"}
        })
    );
}

#[test]
fn canonical_projection_keeps_ended_accepted_evidence_out_of_held_display() {
    let original = attempt("22222222222222222222222222222222");
    let mut checkpoint = ConversationInputCheckpoint::empty();
    checkpoint
        .begin_attempt(original.clone(), correlation(), invocation())
        .unwrap();
    let mut native = correlation();
    native.native_turn_id = Some("native-turn".into());
    checkpoint
        .record_attempt_fact(InputAttemptFact {
            attempt: original.clone(),
            correlation: native,
            delivery: InputDeliveryFact::Accepted {
                basis: "codex_turn_start".into(),
                evidence: NativeInputAcceptance::NativeTurn {
                    native_turn_id: "native-turn".into(),
                },
            },
        })
        .unwrap();
    let retained = checkpoint.clone();
    let view = crate::conversation_display::ConversationActiveInput::from_checkpoint(
        &checkpoint,
        "carrier",
        &[(
            original.submission_id,
            LogicalTurnId::parse("33333333333333333333333333333333").unwrap(),
        )],
        |_| Ok(crate::conversation_display::ConversationActionAvailability::Unavailable {
            reason: crate::conversation_display::ConversationActionUnavailableReason::UnsupportedSource,
        }),
    )
    .unwrap();
    assert!(
        view.is_none(),
        "an ended positive receipt is not a held input: {view:?}"
    );
    assert_eq!(
        checkpoint, retained,
        "display must retain the original Accepted evidence"
    );
}

#[test]
fn canonical_resolution_receipt_fences_runtime_and_pending_identity() {
    let original = attempt("22222222222222222222222222222222");
    let mut checkpoint = ConversationInputCheckpoint::empty();
    checkpoint
        .begin_attempt(original.clone(), correlation(), invocation())
        .unwrap();
    checkpoint.attempts[0].revision = 1;
    let request = json!({"carrier_id":"carrier", "submission_id":original.submission_id,
        "attempt_id":original.attempt_id, "revision":"0"});
    let result = json!({"carrier_id":"carrier", "submission_id":original.submission_id,
        "attempt_id":original.attempt_id, "revision":"1", "waiting_for_settlement":true});
    let mut encoded = serde_json::to_value(&checkpoint).unwrap();
    encoded["receipts"] = json!([{"key":"keep", "principal":"human", "fingerprint":"a".repeat(64),
        "decision":{"kind":"keep","request":request,"result":result}}]);
    let checkpoint: ConversationInputCheckpoint = serde_json::from_value(encoded.clone()).unwrap();
    checkpoint.validate().unwrap();
    assert!(checkpoint.referenced_submissions().is_empty());
    assert!(checkpoint.requires_settled_activity("thread").unwrap());
    assert!(
        checkpoint
            .requires_settled_activity("different-thread")
            .is_err()
    );
    let mut invalid = encoded.clone();
    invalid["receipts"][0]["decision"]["result"]["revision"] = json!("2");
    assert!(
        serde_json::from_value::<ConversationInputCheckpoint>(invalid)
            .unwrap()
            .validate()
            .is_err()
    );
    let mut duplicate = encoded;
    let receipt = duplicate["receipts"][0].clone();
    duplicate["receipts"].as_array_mut().unwrap().push(receipt);
    assert!(
        serde_json::from_value::<ConversationInputCheckpoint>(duplicate)
            .unwrap()
            .validate()
            .is_err()
    );
    let pending = human(1);
    assert_eq!(
        pending.identity(),
        CanonicalPendingIdentity::Accepted {
            input: pending.accepted_input_ref().unwrap(),
        }
    );
    assert_ne!(
        pending.identity(),
        human(2).identity(),
        "sources bind exact revisions"
    );
    assert!(validate_pending_inputs(&[pending, human(2)]).is_err());
}

#[test]
fn canonical_resolution_late_ack_cannot_remove_the_settlement_gate() {
    let original = attempt("22222222222222222222222222222222");
    let request = KeepConversationInputRequest {
        carrier_id: "carrier".into(),
        submission_id: original.submission_id.to_string(),
        attempt_id: original.attempt_id.to_string(),
        revision: "0".into(),
    };
    let decisions = [
        HumanInputResolutionDecision::Keep {
            request: request.clone(),
            result: KeepConversationInputResult {
                carrier_id: request.carrier_id.clone(),
                submission_id: request.submission_id.clone(),
                attempt_id: request.attempt_id.clone(),
                revision: "1".into(),
                waiting_for_settlement: true,
            },
        },
        HumanInputResolutionDecision::Retry {
            predecessor: request,
            retry_intent_id: RetryIntentId::parse(&"a".repeat(32)).unwrap(),
            revision: "1".into(),
            waiting_for_settlement: true,
        },
    ];
    for decision in decisions {
        let mut checkpoint = ConversationInputCheckpoint::empty();
        checkpoint
            .begin_attempt(original.clone(), correlation(), invocation())
            .unwrap();
        checkpoint.attempts[0].revision = 1;
        checkpoint.receipts.push(HumanInputResolutionReceipt {
            key: "resolve".into(),
            principal: "human".into(),
            fingerprint: "a".repeat(64),
            decision,
        });
        let mut late = correlation();
        late.native_turn_id = Some("late-original-native".into());
        checkpoint
            .record_attempt_fact(InputAttemptFact {
                attempt: original.clone(),
                correlation: late,
                delivery: InputDeliveryFact::Accepted {
                    basis: "codex_turn_start".into(),
                    evidence: NativeInputAcceptance::NativeTurn {
                        native_turn_id: "late-original-native".into(),
                    },
                },
            })
            .unwrap();
        assert!(checkpoint.requires_settled_activity("thread").unwrap());
        assert!(
            checkpoint
                .requires_settled_activity("another-thread")
                .is_err()
        );
    }
}

#[test]
fn canonical_resolution_known_local_execution_needs_no_native_activity_probe() {
    let original = attempt("22222222222222222222222222222222");
    let mut local = correlation();
    local.provider = AgentProvider::Claude;
    local.local_turn_id = Some("local-turn".into());
    let mut checkpoint = ConversationInputCheckpoint::empty();
    checkpoint
        .begin_attempt(original.clone(), local, invocation())
        .unwrap();
    checkpoint.attempts[0].revision = 1;
    let request = KeepConversationInputRequest {
        carrier_id: "carrier".into(),
        submission_id: original.submission_id.to_string(),
        attempt_id: original.attempt_id.to_string(),
        revision: "0".into(),
    };
    let result = KeepConversationInputResult {
        carrier_id: request.carrier_id.clone(),
        submission_id: request.submission_id.clone(),
        attempt_id: request.attempt_id.clone(),
        revision: "1".into(),
        waiting_for_settlement: true,
    };
    checkpoint.receipts.push(HumanInputResolutionReceipt {
        key: "keep-local".into(),
        principal: "human".into(),
        fingerprint: "a".repeat(64),
        decision: HumanInputResolutionDecision::Keep { request, result },
    });
    checkpoint.validate().unwrap();
    assert!(!checkpoint.requires_settled_activity("thread").unwrap());
}
