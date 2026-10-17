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
fn pending_v2_has_references_and_rejects_two_revisions_or_aliases_of_one_message() {
    let first = human(1);
    assert!(validate_pending_inputs(&[first.clone(), human(2)]).is_err());
    let PendingInput::Human { message, .. } = &first else {
        panic!()
    };
    let alias = PendingInput::Legacy {
        queue_position: 1,
        message: message.clone(),
        queued_at_ms: 10,
        envelope_id: None,
    };
    assert!(validate_pending_inputs(&[first.clone(), alias]).is_err());
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
        .begin_attempt(original.clone(), correlation())
        .unwrap();
    assert!(
        checkpoint
            .begin_attempt(later.clone(), correlation())
            .is_err()
    );
    let refusal = InputAttemptCheckpoint {
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
            .record_attempt_fact(InputAttemptCheckpoint {
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
        .begin_attempt(original.clone(), correlation())
        .unwrap();
    let refusal = InputAttemptCheckpoint {
        attempt: original.clone(),
        correlation: correlation(),
        delivery: InputDeliveryFact::NotDispatched {
            basis: "transport_before_send".into(),
        },
    };
    checkpoint.record_attempt_fact(refusal.clone()).unwrap();
    checkpoint
        .begin_attempt(later.clone(), correlation())
        .unwrap();
    assert!(checkpoint.record_attempt_fact(refusal).is_err());
    let mut accepted_correlation = correlation();
    accepted_correlation.native_turn_id = Some("native-turn".into());
    checkpoint
        .record_attempt_fact(InputAttemptCheckpoint {
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
            .begin_attempt(attempt("44444444444444444444444444444444"), correlation())
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
        .begin_attempt(original.clone(), correlation())
        .unwrap();
    let mut local = correlation();
    local.local_turn_id = Some("owned-local-turn".into());
    checkpoint
        .record_attempt_fact(InputAttemptCheckpoint {
            attempt: original.clone(),
            correlation: local.clone(),
            delivery: InputDeliveryFact::Unknown,
        })
        .unwrap();
    assert!(
        checkpoint
            .record_attempt_fact(InputAttemptCheckpoint {
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
            .record_attempt_fact(InputAttemptCheckpoint {
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
fn positive_native_input_uses_the_exact_session_submission_tuple_without_a_native_turn() {
    let attempt = attempt("22222222222222222222222222222222");
    let mut checkpoint = ConversationInputCheckpoint::empty();
    let mut native = correlation();
    native.provider = AgentProvider::Claude;
    // The generic value does not impose a provider's session/thread mapping.
    // That qualification belongs to its private adapter/event ingress.
    native.native_session_id = Some("reported-native-session".into());
    native.local_turn_id = Some("local-attempt-turn".into());
    checkpoint
        .begin_attempt(attempt.clone(), native.clone())
        .unwrap();
    let accepted = InputAttemptCheckpoint {
        attempt: attempt.clone(),
        correlation: native,
        delivery: InputDeliveryFact::Accepted {
            basis: "qualified_user_replay".into(),
            evidence: NativeInputAcceptance::NativeInput {
                native_session_id: "reported-native-session".into(),
                client_input_id: attempt.submission_id.clone(),
            },
        },
    };
    checkpoint.record_attempt_fact(accepted.clone()).unwrap();
    checkpoint.record_attempt_fact(accepted.clone()).unwrap();
    assert_eq!(checkpoint.attempt(&attempt.submission_id), Some(&accepted));
    assert!(accepted.correlation.native_turn_id.is_none());
    let encoded = serde_json::to_value(&accepted).unwrap();
    assert_eq!(encoded["delivery"]["evidence"]["kind"], "native_input");
    assert_eq!(
        serde_json::from_value::<InputAttemptCheckpoint>(encoded).unwrap(),
        accepted
    );
}

#[test]
fn native_input_receipt_cannot_cross_session_submission_or_owned_local_correlation() {
    let attempt = attempt("22222222222222222222222222222222");
    let mut native = correlation();
    native.provider = AgentProvider::Claude;
    native.native_session_id = Some("native-session".into());
    native.local_turn_id = Some("owned-local-turn".into());
    let accepted = InputAttemptCheckpoint {
        attempt: attempt.clone(),
        correlation: native.clone(),
        delivery: InputDeliveryFact::Accepted {
            basis: "qualified_user_replay".into(),
            evidence: NativeInputAcceptance::NativeInput {
                native_session_id: "native-session".into(),
                client_input_id: attempt.submission_id.clone(),
            },
        },
    };
    let mut reports = Vec::new();
    let mut other_session = accepted.clone();
    other_session.correlation.native_session_id = Some("foreign-session".into());
    reports.push(other_session);
    let mut missing_session = accepted.clone();
    missing_session.correlation.native_session_id = None;
    reports.push(missing_session);
    let mut other_submission = accepted.clone();
    other_submission.attempt.submission_id =
        InputSubmissionId::parse("33333333333333333333333333333333").unwrap();
    reports.push(other_submission);
    let mut other_local = accepted;
    other_local.correlation.local_turn_id = Some("foreign-local-turn".into());
    reports.push(other_local);
    for report in reports {
        let mut checkpoint = ConversationInputCheckpoint::empty();
        checkpoint
            .begin_attempt(attempt.clone(), native.clone())
            .unwrap();
        assert!(checkpoint.record_attempt_fact(report).is_err());
        assert!(matches!(
            checkpoint.attempt(&attempt.submission_id).unwrap().delivery,
            InputDeliveryFact::CrossingUnknown
        ));
    }
}
