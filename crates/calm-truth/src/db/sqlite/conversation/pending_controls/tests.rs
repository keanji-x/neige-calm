use super::super::tests::{body, fixture, plan};
use super::super::transaction_tests::{provenance, runtime};
use super::*;
use crate::db::sqlite::begin_immediate_tx;
use calm_types::conversation::*;
use calm_types::organization::LEGACY_OWNER_USER_ID as OWNER;

async fn accept(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &mut CurrentInputCarrier,
    text: &str,
) -> MessageRevisionRef {
    let accepted = accept_pending_human_tx(tx, carrier, OWNER, body(text))
        .await
        .unwrap();
    let AcceptedInputRef::Human { message } = accepted.input_ref else {
        panic!("Human input")
    };
    message
}

#[tokio::test]
async fn pending_controls_edit_preserves_old_revision_order_and_attachments() {
    let (repo, _, card) = fixture().await;
    let worker = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &worker)
        .await
        .unwrap();
    let first = accept(&mut tx, &mut carrier, "first").await;
    let attachment = calm_types::planner_attachment::BoundAttachment {
        id: serde_json::from_value(serde_json::json!(
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa.png"
        ))
        .unwrap(),
        size: 10,
        path: "/tmp/bound.png".into(),
    };
    let accepted = accept_pending_human_tx(
        &mut tx,
        &mut carrier,
        OWNER,
        HumanPayload {
            text: "old".into(),
            bound_attachments: vec![attachment.clone()],
        },
    )
    .await
    .unwrap();
    let AcceptedInputRef::Human { message: old } = accepted.input_ref else {
        panic!("Human input")
    };
    let positions = carrier
        .checkpoint()
        .pending
        .iter()
        .map(PendingInput::queue_position)
        .collect::<Vec<_>>();
    let result = mutate_pending_human_tx(
        &mut tx,
        &mut carrier,
        OWNER,
        &old,
        PendingHumanChange::Edit { text: "".into() },
    )
    .await
    .unwrap();
    let PendingHumanWrite::Applied {
        current: Some(current),
        payload,
        ..
    } = result
    else {
        panic!("edit must commit")
    };
    assert_eq!(current.revision.get(), 2);
    assert_eq!(payload.bound_attachments, vec![attachment]);
    assert_eq!(
        carrier
            .checkpoint()
            .pending
            .iter()
            .map(PendingInput::queue_position)
            .collect::<Vec<_>>(),
        positions
    );
    let PrivateHistoryPayload::Present { value } =
        revision_tx(&mut tx, &old).await.unwrap().payload
    else {
        panic!("old version remains readable")
    };
    assert_eq!(value.text, "old");
    assert!(matches!(
        mutate_pending_human_tx(
            &mut tx,
            &mut carrier,
            OWNER,
            &old,
            PendingHumanChange::Withdraw
        )
        .await
        .unwrap(),
        PendingHumanWrite::Stale(_)
    ));
    assert_eq!(
        carrier.checkpoint().pending[0].message_id(),
        Some(&first.message_id)
    );
    assert!(matches!(
        mutate_pending_human_tx(
            &mut tx,
            &mut carrier,
            OWNER,
            &current,
            PendingHumanChange::Withdraw
        )
        .await
        .unwrap(),
        PendingHumanWrite::Applied { current: None, .. }
    ));
    assert_eq!(carrier.checkpoint().pending.len(), 1);
    assert!(
        message_tx(&mut tx, &current.message_id)
            .await
            .unwrap()
            .withdrawn_at_ms
            .is_some()
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn pending_controls_refuse_another_author_and_do_not_change_claimed_input() {
    let (repo, _, card) = fixture().await;
    let worker = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &worker)
        .await
        .unwrap();
    let reference = accept(&mut tx, &mut carrier, "original").await;
    assert!(matches!(
        mutate_pending_human_tx(
            &mut tx,
            &mut carrier,
            "someone-else",
            &reference,
            PendingHumanChange::Withdraw
        )
        .await,
        Err(crate::error::TruthError::Forbidden(_))
    ));
    let claimed = claim_initial_submission_tx(
        &mut tx,
        &mut carrier,
        plan(reference.clone()),
        &provenance(),
    )
    .await
    .unwrap();
    assert!(matches!(
        mutate_pending_human_tx(
            &mut tx,
            &mut carrier,
            OWNER,
            &reference,
            PendingHumanChange::Edit {
                text: "changed".into()
            }
        )
        .await
        .unwrap(),
        PendingHumanWrite::Gone
    ));
    assert_eq!(
        submission_plan_tx(&mut tx, &claimed.submission_id)
            .await
            .unwrap()
            .plan,
        plan(reference)
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn pending_controls_supplemental_non_dispatch_releases_only_its_own_claim() {
    let (repo, _, card) = fixture().await;
    let worker = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &worker)
        .await
        .unwrap();
    let first = accept(&mut tx, &mut carrier, "initial").await;
    let initial = claim_initial_submission_tx(&mut tx, &mut carrier, plan(first), &provenance())
        .await
        .unwrap();
    let initial_attempt = InputAttempt {
        submission_id: initial.submission_id.clone(),
        attempt_id: InputAttemptId::parse(&crate::model::new_id()).unwrap(),
    };
    let correlation = InputCorrelationFact {
        provider: calm_types::runtime::AgentProvider::Codex,
        thread_id: "thread".into(),
        native_turn_id: Some("native-turn".into()),
        native_session_id: None,
        local_turn_id: None,
        rpc_request_id: None,
    };
    let invocation = FrozenInputInvocation {
        model: Some("model".into()),
        effort: Some("high".into()),
        permission: calm_types::harness::PlannerPermissionMode::Never,
    };
    begin_input_attempt_tx(
        &mut tx,
        &mut carrier,
        initial_attempt.clone(),
        correlation.clone(),
        invocation.clone(),
    )
    .await
    .unwrap();
    record_input_attempt_fact_tx(
        &mut tx,
        &mut carrier,
        InputAttemptFact {
            attempt: initial_attempt,
            correlation: correlation.clone(),
            delivery: InputDeliveryFact::Accepted {
                basis: "native_ack".into(),
                evidence: NativeInputAcceptance::NativeTurn {
                    native_turn_id: "native-turn".into(),
                },
            },
        },
    )
    .await
    .unwrap();
    carrier
        .set_phase(calm_types::harness::HarnessPhaseTag::TurnRunning)
        .unwrap();
    carrier.snapshot["last_turn_id"] = serde_json::json!("native-turn");
    super::super::carrier::save_input_carrier_tx(&mut tx, &mut carrier)
        .await
        .unwrap();
    let second = accept(&mut tx, &mut carrier, "supplemental").await;
    let third = accept(&mut tx, &mut carrier, "later").await;
    let supplemental = claim_supplemental_submission_tx(&mut tx, &mut carrier, &second)
        .await
        .unwrap();
    assert_eq!(supplemental.ordinal, 1);
    let attempt = InputAttempt {
        submission_id: supplemental.submission_id.clone(),
        attempt_id: InputAttemptId::parse(&crate::model::new_id()).unwrap(),
    };
    begin_input_attempt_tx(
        &mut tx,
        &mut carrier,
        attempt.clone(),
        correlation.clone(),
        invocation.clone(),
    )
    .await
    .unwrap();
    // A supplemental crossing cannot hide the original native target, but holds END eligibility.
    assert!(
        original_input_target_tx(&mut tx, &carrier)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        original_input_execution_tx(&mut tx, &carrier)
            .await
            .unwrap()
            .is_none()
    );
    record_input_attempt_fact_tx(
        &mut tx,
        &mut carrier,
        InputAttemptFact {
            attempt: attempt.clone(),
            correlation: correlation.clone(),
            delivery: InputDeliveryFact::NotDispatched {
                basis: "before_send".into(),
            },
        },
    )
    .await
    .unwrap();
    release_submission_tx(&mut tx, &mut carrier, &attempt)
        .await
        .unwrap();
    assert_eq!(
        carrier.phase().unwrap(),
        calm_types::harness::HarnessPhaseTag::TurnRunning
    );
    assert_eq!(
        carrier.checkpoint().active.as_ref().unwrap().submission_ids,
        vec![initial.submission_id]
    );
    assert_eq!(
        carrier
            .checkpoint()
            .pending
            .iter()
            .map(|p| p.message_id().unwrap().clone())
            .collect::<Vec<_>>(),
        vec![second.message_id.clone(), third.message_id]
    );
    let next = claim_supplemental_submission_tx(&mut tx, &mut carrier, &second)
        .await
        .unwrap();
    assert_eq!(
        next.ordinal, 2,
        "released ordinal slots are immutable facts"
    );
    let crossing = InputAttempt {
        submission_id: next.submission_id.clone(),
        attempt_id: InputAttemptId::parse(&crate::model::new_id()).unwrap(),
    };
    begin_input_attempt_tx(
        &mut tx,
        &mut carrier,
        crossing.clone(),
        correlation,
        invocation,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    // Reload persisted facts as after a cancelled HTTP waiter or server restart.
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &card, &worker)
        .await
        .unwrap();
    assert!(
        original_input_execution_tx(&mut tx, &carrier)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !carrier
            .checkpoint()
            .pending
            .iter()
            .any(|input| input.message_id() == Some(&second.message_id))
    );
    let request = KeepConversationInputRequest {
        carrier_id: worker.clone(),
        submission_id: crossing.submission_id.to_string(),
        attempt_id: crossing.attempt_id.to_string(),
        revision: "0".into(),
    };
    keep_conversation_input_tx(&mut tx, &card, OWNER, "keep-supplemental", &request)
        .await
        .unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &card, &worker)
        .await
        .unwrap();
    assert!(matches!(
        carrier
            .checkpoint()
            .attempt(&crossing.submission_id)
            .unwrap()
            .delivery,
        InputDeliveryFact::CrossingUnknown
    ));
    assert!(
        original_input_execution_tx(&mut tx, &carrier)
            .await
            .unwrap()
            .is_some(),
        "explicit Keep must unblock original END without inventing delivery"
    );
    tx.commit().await.unwrap();
}
