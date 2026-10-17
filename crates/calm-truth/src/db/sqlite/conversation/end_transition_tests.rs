//! Facts/barriers only. Positive native END is exercised through real adapters
//! by the server; these tests never manufacture a qualified native observation.
use super::super::{SqlxRepo, begin_immediate_tx};
use super::tests::{body, fixture, plan};
use super::transaction_tests::{provenance, runtime};
use super::*;
use calm_types::conversation::*;
use calm_types::organization::LEGACY_OWNER_USER_ID;
use calm_types::runtime::AgentProvider;
use serde_json::json;

async fn original(repo: &SqlxRepo, card: &str, runtime: &str) -> InputAttempt {
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, card, runtime)
        .await
        .unwrap();
    let accepted =
        accept_pending_human_tx(&mut tx, &mut carrier, LEGACY_OWNER_USER_ID, body("initial"))
            .await
            .unwrap();
    let AcceptedInputRef::Human { message } = accepted.input_ref else {
        panic!("human")
    };
    let initial = claim_initial_submission_tx(&mut tx, &mut carrier, plan(message), &provenance())
        .await
        .unwrap();
    let attempt = InputAttempt {
        submission_id: initial.submission_id,
        attempt_id: InputAttemptId::parse(&crate::model::new_id()).unwrap(),
    };
    begin_input_attempt_tx(&mut tx, &mut carrier, attempt.clone(), correlation())
        .await
        .unwrap();
    record_input_attempt_fact_tx(
        &mut tx,
        &mut carrier,
        InputAttemptCheckpoint {
            attempt: attempt.clone(),
            correlation: correlation(),
            delivery: InputDeliveryFact::Unknown,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    attempt
}

fn correlation() -> InputCorrelationFact {
    InputCorrelationFact {
        provider: AgentProvider::Codex,
        thread_id: "thread".into(),
        native_session_id: None,
        native_turn_id: Some("native-original".into()),
        local_turn_id: None,
        rpc_request_id: None,
    }
}

#[tokio::test]
async fn original_end_waits_for_all_active_supplemental_reports_before_closing() {
    let (repo, _, card) = fixture().await;
    let runtime = runtime(&repo, &card).await;
    let initial = original(&repo, &card, &runtime).await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    let fence = RuntimeExecutionFence::from_carrier(&carrier);
    let mut header = fence.header().clone();
    header["phase"] = json!("turn_running");
    header["last_turn_id"] = json!("native-original");
    persist_runtime_frame_tx(&mut tx, &card, &runtime, &fence, header)
        .await
        .unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    let logical = carrier
        .checkpoint()
        .active
        .as_ref()
        .unwrap()
        .logical_turn_id
        .clone();
    let accepted =
        accept_pending_human_tx(&mut tx, &mut carrier, LEGACY_OWNER_USER_ID, body("steer"))
            .await
            .unwrap();
    let AcceptedInputRef::Human { message } = accepted.input_ref else {
        panic!("human")
    };
    let supplemental = claim_supplemental_submission_tx(
        &mut tx,
        &mut carrier,
        LEGACY_OWNER_USER_ID,
        &logical,
        "native-original",
        plan(message),
    )
    .await
    .unwrap();
    assert!(
        original_input_execution_tx(&mut tx, &carrier)
            .await
            .unwrap()
            .is_none(),
        "a claimed Supplemental still has its report owner ahead of END"
    );
    let attempt = InputAttempt {
        submission_id: supplemental.submission_id,
        attempt_id: InputAttemptId::parse(&crate::model::new_id()).unwrap(),
    };
    begin_input_attempt_tx(&mut tx, &mut carrier, attempt.clone(), correlation())
        .await
        .unwrap();
    assert!(
        original_input_execution_tx(&mut tx, &carrier)
            .await
            .unwrap()
            .is_none(),
        "a crossing can still return exact NotDispatched requiring atomic release"
    );
    record_input_attempt_fact_tx(
        &mut tx,
        &mut carrier,
        InputAttemptCheckpoint {
            attempt,
            correlation: correlation(),
            delivery: InputDeliveryFact::Unknown,
        },
    )
    .await
    .unwrap();
    let expected = carrier.checkpoint().clone();
    let actual = original_input_execution_tx(&mut tx, &carrier)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(actual.proof.attempt(), &initial);
    let fence = RuntimeExecutionFence::from_carrier(&carrier);
    assert!(
        complete_input_execution_tx(&mut tx, &card, &runtime, &fence, &actual)
            .await
            .unwrap()
            .is_some()
    );
    let current = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    let mut expected = expected;
    expected.active = None;
    assert_eq!(current.checkpoint(), &expected);
    let claims: i64 =
        sqlx::query_scalar("SELECT count(*) FROM conversation_submission_inputs WHERE effective=1")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(claims, 2);
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn original_end_refuses_missing_and_ambiguous_committed_execution_tuples() {
    for (native, session, local) in [
        (None, None, None),
        (None, None, Some("local")),
        (Some("native"), Some("session"), Some("local")),
    ] {
        let (repo, _, card) = fixture().await;
        let runtime = runtime(&repo, &card).await;
        let attempt = original(&repo, &card, &runtime).await;
        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        // A raw historical fact can be incomplete/ambiguous; it is never repaired
        // from current last_turn_id or another native slot by the END getter.
        let mut carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
            .await
            .unwrap();
        let mut frame = carrier.snapshot().clone();
        let facts = frame["canonical_inputs"]["attempts"]
            .as_array_mut()
            .unwrap();
        let fact = facts
            .iter_mut()
            .find(|value| value["attempt"]["submission_id"] == attempt.submission_id.as_str())
            .unwrap();
        fact["correlation"]["native_turn_id"] = json!(native);
        fact["correlation"]["native_session_id"] = json!(session);
        fact["correlation"]["local_turn_id"] = json!(local);
        super::super::session_set_handle_state_tx(&mut tx, &runtime, Some(frame))
            .await
            .unwrap();
        carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
            .await
            .unwrap();
        assert!(
            original_input_execution_tx(&mut tx, &carrier)
                .await
                .unwrap()
                .is_none()
        );
        tx.commit().await.unwrap();
    }
}

#[tokio::test]
async fn original_end_keeps_independent_runtime_transitions_for_their_own_owners() {
    for (key, value) in [
        ("phase", json!("wedged")),
        ("phase", json!("issuing_interrupt")),
        (
            "pending_rewind",
            json!({"provider":"codex_v2","thread_id":"thread","before_turn_id":"prior","reverted":false}),
        ),
        (
            "interruption_intent",
            serde_json::to_value(
                calm_types::harness::HarnessInterruptionIntent::from_request(
                    "native-original",
                    "max_turn_duration",
                )
                .unwrap(),
            )
            .unwrap(),
        ),
        (
            "wedged_reason",
            json!("other owning transition remains unresolved"),
        ),
    ] {
        let (repo, _, card) = fixture().await;
        let runtime = runtime(&repo, &card).await;
        original(&repo, &card, &runtime).await;
        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        let carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
            .await
            .unwrap();
        let fence = RuntimeExecutionFence::from_carrier(&carrier);
        let mut header = fence.header().clone();
        header[key] = value;
        persist_runtime_frame_tx(&mut tx, &card, &runtime, &fence, header)
            .await
            .unwrap();
        let current = current_input_carrier_tx(&mut tx, &card, &runtime)
            .await
            .unwrap();
        let original = original_input_execution_tx(&mut tx, &current)
            .await
            .unwrap()
            .unwrap();
        let expected = RuntimeExecutionFence::from_carrier(&current);
        assert!(
            complete_input_execution_tx(&mut tx, &card, &runtime, &expected, &original)
                .await
                .unwrap()
                .is_none(),
            "{key}"
        );
        let after = current_input_carrier_tx(&mut tx, &card, &runtime)
            .await
            .unwrap();
        assert_eq!(after.snapshot(), current.snapshot());
        tx.commit().await.unwrap();
    }
}
