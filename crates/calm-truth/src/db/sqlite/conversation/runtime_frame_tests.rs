use super::super::{begin_immediate_tx, session_set_handle_state_tx};
use super::tests::{body, fixture, plan};
use super::transaction_tests::{provenance, runtime};
use super::*;
use calm_types::conversation::*;
use calm_types::organization::LEGACY_OWNER_USER_ID;
use calm_types::runtime::AgentProvider;
use serde_json::json;

#[tokio::test]
async fn runtime_frame_preserves_later_accepted_input_and_its_queue_counter() {
    let (repo, _, card) = fixture().await;
    let runtime = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    let expected = RuntimeExecutionFence::from_carrier(&carrier);
    let accepted_watermark = carrier.snapshot()["push_watermark"].clone();
    let accepted = accept_pending_human_tx(
        &mut tx,
        &mut carrier,
        LEGACY_OWNER_USER_ID,
        body("accepted after the runtime sampled its empty cache"),
    )
    .await
    .unwrap();
    let mut next = expected.header().clone();
    next["push_watermark"] = json!(5);
    let written = persist_runtime_frame_tx(&mut tx, &card, &runtime, &expected, next)
        .await
        .unwrap();
    let current = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    assert_eq!(current.checkpoint().pending.len(), 1);
    assert_eq!(
        current.checkpoint().pending[0].input_ref(),
        accepted.input_ref
    );
    assert_eq!(current.checkpoint().next_queue_position, 1);
    assert_eq!(
        written.snapshot["push_watermark"], accepted_watermark,
        "a runtime projection cannot invent the acceptance-owned event watermark"
    );
    assert_eq!(
        written.execution_fence,
        RuntimeExecutionFence::from_carrier(&current)
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn runtime_frame_cas_does_not_restore_a_same_phase_once_only_budget_marker() {
    let (repo, _, card) = fixture().await;
    let runtime = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    let mut before = carrier.snapshot().clone();
    before["turn_budget"] = json!({
        "kind":"planner", "budget":10,
        "start_attempt":"11111111111111111111111111111111", "warning_attempted":false,
    });
    session_set_handle_state_tx(&mut tx, &runtime, Some(before))
        .await
        .unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    let accepted_watermark = carrier.snapshot()["push_watermark"].clone();
    let stale = RuntimeExecutionFence::from_carrier(&carrier);
    let mut attempted = stale.header().clone();
    attempted["turn_budget"]["warning_attempted"] = json!(true);
    persist_runtime_frame_tx(&mut tx, &card, &runtime, &stale, attempted)
        .await
        .unwrap();
    let mut old = stale.header().clone();
    old["push_watermark"] = json!(99);
    assert!(
        persist_runtime_frame_tx(&mut tx, &card, &runtime, &stale, old)
            .await
            .is_err()
    );
    let current = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    assert_eq!(current.snapshot()["turn_budget"]["warning_attempted"], true);
    assert_eq!(current.snapshot()["push_watermark"], accepted_watermark);
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn runtime_frame_cas_rejects_changed_dispatch_attempt_without_a_phase_change() {
    let (repo, _, card) = fixture().await;
    let runtime = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    let accepted =
        accept_pending_human_tx(&mut tx, &mut carrier, LEGACY_OWNER_USER_ID, body("one"))
            .await
            .unwrap();
    let AcceptedInputRef::Human { message } = accepted.input_ref else {
        panic!("human fixture")
    };
    let claimed = claim_initial_submission_tx(&mut tx, &mut carrier, plan(message), &provenance())
        .await
        .unwrap();
    let stale = RuntimeExecutionFence::from_carrier(&carrier);
    let attempt = InputAttempt {
        submission_id: claimed.submission_id,
        attempt_id: InputAttemptId::parse(&crate::model::new_id()).unwrap(),
    };
    begin_input_attempt_tx(
        &mut tx,
        &mut carrier,
        attempt.clone(),
        InputCorrelationFact {
            provider: AgentProvider::Codex,
            thread_id: "thread".into(),
            native_session_id: None,
            local_turn_id: None,
            native_turn_id: None,
            rpc_request_id: None,
        },
    )
    .await
    .unwrap();
    let mut next = stale.header().clone();
    next["phase"] = json!("idle");
    assert!(
        persist_runtime_frame_tx(&mut tx, &card, &runtime, &stale, next)
            .await
            .is_err()
    );
    let current = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    assert_eq!(current.snapshot()["phase"], "issuing_turn");
    assert_eq!(
        current
            .checkpoint()
            .attempt(&attempt.submission_id)
            .unwrap()
            .attempt,
        attempt
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn runtime_frame_rejects_a_copied_input_checkpoint_as_runtime_authority() {
    let (repo, _, card) = fixture().await;
    let runtime = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    let fence = RuntimeExecutionFence::from_carrier(&carrier);
    assert!(
        persist_runtime_frame_tx(&mut tx, &card, &runtime, &fence, carrier.snapshot().clone())
            .await
            .is_err()
    );
    let mut missing_phase = fence.header().clone();
    missing_phase.as_object_mut().unwrap().remove("phase");
    assert!(
        persist_runtime_frame_tx(&mut tx, &card, &runtime, &fence, missing_phase)
            .await
            .is_err()
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn runtime_frame_writer_rejects_missing_required_turn_budget() {
    let (repo, _, card) = fixture().await;
    let worker = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &card, &worker)
        .await
        .unwrap();
    let before = carrier.snapshot().clone();
    let fence = RuntimeExecutionFence::from_carrier(&carrier);
    let mut missing = fence.header().clone();
    missing.as_object_mut().unwrap().remove("turn_budget");
    let result = persist_runtime_frame_tx(&mut tx, &card, &worker, &fence, missing).await;
    assert!(
        result.is_err(),
        "V2 writes must require their frozen budget"
    );
    let retained = current_input_carrier_tx(&mut tx, &card, &worker)
        .await
        .unwrap();
    assert_eq!(retained.snapshot(), &before);
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn runtime_frame_writer_rejects_unknown_runtime_field() {
    let (repo, _, card) = fixture().await;
    let worker = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &card, &worker)
        .await
        .unwrap();
    let before = carrier.snapshot().clone();
    let fence = RuntimeExecutionFence::from_carrier(&carrier);
    let mut unknown = fence.header().clone();
    unknown["unrecognized_runtime_authority"] = json!(true);
    let result = persist_runtime_frame_tx(&mut tx, &card, &worker, &fence, unknown).await;
    assert!(result.is_err(), "V2 writes must reject undeclared fields");
    let retained = current_input_carrier_tx(&mut tx, &card, &worker)
        .await
        .unwrap();
    assert_eq!(retained.snapshot(), &before);
    tx.commit().await.unwrap();
}
