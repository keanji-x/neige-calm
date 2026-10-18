//! Actual Codex RPC/notification evidence composed with the canonical END owner.
//! No synthetic Q or report constructor.
use super::*;
use crate::harness::backend::InputDelivery;
use crate::planner_model::TurnModelSelection;
use crate::planner_permission_mode::PlannerPermissionMode;
use crate::shared_codex_appserver::native_owner_tests::{
    Peer, connect, notify_thread, request, send,
};

async fn accepted_initial(fx: &Fixture, peer: &mut Peer, turn: &str) {
    let repo = fx.repo.clone();
    let source = fx.source.clone();
    let backend = fx.backend.clone();
    let card = fx.card.clone();
    let runtime = fx.runtime.clone();
    let submission = fx.submission.clone();
    let task = tokio::spawn(async move {
        crate::harness::input_dispatch::initial(
            repo.as_ref(),
            &source,
            &card,
            &runtime,
            &submission,
            &backend,
            &TurnModelSelection::inherit(),
            PlannerPermissionMode::Never,
        )
        .await
    });
    let call = request(peer).await;
    assert_eq!(call["method"], "turn/start");
    let thread = call["params"]["threadId"].as_str().unwrap();
    notify_thread(
        &fx.daemon,
        peer,
        thread,
        "turn/started",
        json!({"id":turn,"status":"inProgress"}),
    )
    .await;
    send(peer, json!({"id":call["id"],"result":{"turn":{"id":turn}}})).await;
    let report = task.await.unwrap().unwrap();
    assert!(matches!(
        report.report.delivery(),
        InputDelivery::Accepted(_)
    ));
    assert_eq!(report.report.outcome().as_ref().unwrap(), turn);

    // This is the same factual runtime/header writer the FSM consumes. Its
    // actual turn comes from the committed report, not a synthetic END setter.
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &fx.card, &fx.runtime)
        .await
        .unwrap();
    let _: crate::harness::snapshot::CanonicalHarnessSnapshot =
        serde_json::from_value(carrier.snapshot().clone()).unwrap();
    let fence = RuntimeExecutionFence::from_carrier(&carrier);
    let mut header = fence.header().clone();
    header["phase"] = json!("turn_running");
    header["last_turn_id"] = json!(turn);
    persist_runtime_frame_tx(&mut tx, &fx.card, &fx.runtime, &fence, header)
        .await
        .unwrap();
    session_set_harness_observation_runtime_tx(
        &mut tx,
        &fx.runtime,
        WorkerSessionState::Running,
        Some(thread),
        Some(turn),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn actual_end(fx: &Fixture, peer: &mut Peer, sampled: &Sample, turn: &str) {
    let ExecutionEndScope::NativeTurn { thread_id, .. } = &sampled.target.scope else {
        panic!("actual native target")
    };
    notify_thread(
        &fx.daemon,
        peer,
        thread_id,
        "turn/completed",
        json!({"id":turn,"status":"completed"}),
    )
    .await;
}

#[tokio::test]
async fn retired_native_source_cannot_requalify_but_observed_end_uses_current_header_cas() {
    let fx = Fixture::new(AgentProvider::Codex).await;
    let (_old, mut peer) = connect(&fx.daemon).await;
    accepted_initial(&fx, &mut peer, "ended").await;
    let sampled = fx.sample().await;
    actual_end(&fx, &mut peer, &sampled, "ended").await;
    let observed = fx.observe(&sampled).await;
    let (_current, _new_peer) = connect(&fx.daemon).await;
    assert!(
        fx.backend
            .qualify_execution_end(&sampled.target)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        complete_once(fx.repo.as_ref(), &fx.backend, &fx.card, &fx.runtime)
            .await
            .unwrap()
            .is_none()
    );

    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    let accepted_watermark = current_input_fact_carrier_tx(&mut tx, &fx.card, &fx.runtime)
        .await
        .unwrap()
        .snapshot()["push_watermark"]
        .clone();
    let mut header = sampled.fence.header().clone();
    // Acceptance owns its watermark; advance an actual runtime-owned field to
    // pin the same-phase whole-header CAS after the native source retires.
    header["push_watermark"] = json!(73);
    header["context_usage_invalidated"] = json!(true);
    persist_runtime_frame_tx(&mut tx, &fx.card, &fx.runtime, &sampled.fence, header)
        .await
        .unwrap();
    assert!(
        consume_tx(&mut tx, &fx.backend.provider(), &sampled, &observed)
            .await
            .unwrap()
            .is_none()
    );
    tx.commit().await.unwrap();
    let current = fx.sample().await;
    assert!(observed.matches(&current.target));
    let before = fx.checkpoint().await;
    let provider = fx.backend.provider();
    let result = write_in_tx_typed(fx.repo.as_ref(), move |tx| {
        Box::pin(async move { consume_tx(tx, &provider, &current, &observed).await })
    })
    .await
    .unwrap()
    .unwrap();
    let mut expected = before;
    expected.active = None;
    assert_eq!(fx.checkpoint().await, expected);
    assert_eq!(result.snapshot["push_watermark"], accepted_watermark);
    assert_eq!(result.snapshot["context_usage_invalidated"], true);
    assert_eq!(result.snapshot["phase"], "turn_completed");
    assert!(
        fx.backend
            .qualify_execution_end(&sampled.target)
            .await
            .unwrap()
            .is_none()
    );
}
