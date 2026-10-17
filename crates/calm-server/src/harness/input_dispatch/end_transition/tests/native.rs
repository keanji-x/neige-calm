//! Actual Codex RPC/notification evidence composed with the canonical END and
//! exact non-delivery release owners. No synthetic Q or report constructor.
use super::*;
use crate::harness::backend::{InputDelivery, NonDispatchBasis};
use crate::harness::input_dispatch::settle;
use crate::planner_model::TurnModelSelection;
use crate::planner_permission_mode::PlannerPermissionMode;
use crate::shared_codex_appserver::native_owner_tests::{
    Peer, connect, notify_thread, request, send,
};
use std::time::Duration;

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
            None,
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
    let _: crate::harness::snapshot::CanonicalHarnessSnapshot =
        serde_json::from_value(report.snapshot).unwrap();

    // This is the same factual runtime/header writer the FSM consumes. Its
    // actual turn comes from the committed report, not a synthetic END setter.
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &fx.card, &fx.runtime)
        .await
        .unwrap();
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
async fn native_end_waits_for_decided_nondispatch_to_settle_and_release_supplemental() {
    let fx = Fixture::new(AgentProvider::Codex).await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    accepted_initial(&fx, &mut peer, "original").await;
    let original_sample = fx.sample().await;

    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &fx.card, &fx.runtime)
        .await
        .unwrap();
    let accepted = crate::conversation_inputs::accept_human_tx(
        &mut tx,
        &fx.source.current_principal().unwrap(),
        &mut carrier,
        body("supplemental accepted before the actual end"),
    )
    .await
    .unwrap();
    let AcceptedInputRef::Human { message } = accepted.input_ref else {
        panic!("human")
    };
    let supplemental = crate::conversation_inputs::claim_human_supplemental_tx(
        &mut tx,
        &fx.source,
        &mut carrier,
        &fx.backend,
        &original_sample.target.logical_turn_id,
        "original",
        plan(message.clone()),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let prepared = prepare(
        fx.repo.as_ref(),
        &fx.source,
        &fx.card,
        &fx.runtime,
        &supplemental.submission_id,
        fx.backend.provider(),
        Target::Supplemental {
            native_turn_id: "original".into(),
        },
        None,
    )
    .await
    .unwrap();
    actual_end(&fx, &mut peer, &original_sample, "original").await;
    let observed = fx.observe(&original_sample).await;
    assert!(observed.matches(&original_sample.target));

    // The actual native owner now sees END, so it refuses steer before any RPC.
    // Retain that genuine report in memory until the existing settle transaction.
    let report = tokio::time::timeout(
        Duration::from_secs(1),
        fx.backend.dispatch_supplemental(
            &prepared.logical_turn_id,
            prepared.proof.attempt(),
            &prepared.thread_id,
            "original",
            &prepared.proof,
            prepared.items.clone(),
            &|| Box::pin(prepared.admit(fx.repo.as_ref())),
        ),
    )
    .await
    .unwrap();
    assert!(matches!(
        report.delivery(),
        InputDelivery::NotDispatched(proof) if matches!(proof.basis(),NonDispatchBasis::LocalPreflight)
    ));
    assert!(report.rpc_failure().is_none());
    let before = fx.checkpoint().await;
    assert_eq!(before.active.as_ref().unwrap().submission_ids.len(), 2);
    assert!(matches!(
        before
            .attempt(&supplemental.submission_id)
            .unwrap()
            .delivery,
        InputDeliveryFact::CrossingUnknown
    ));
    assert!(
        complete_once(fx.repo.as_ref(), &fx.backend, &fx.card, &fx.runtime)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(fx.checkpoint().await, before);
    let effective: i64 =
        sqlx::query_scalar("SELECT count(*) FROM conversation_submission_inputs WHERE effective=1")
            .fetch_one(fx.repo.pool())
            .await
            .unwrap();
    assert_eq!(effective, 2);

    let committed = settle(
        fx.repo.as_ref(),
        &fx.card,
        &fx.runtime,
        prepared.proof.attempt(),
        report,
    )
    .await
    .unwrap();
    assert!(matches!(
        committed.report.delivery(),
        InputDelivery::NotDispatched(_)
    ));
    let released = fx.checkpoint().await;
    assert_eq!(
        released.active.as_ref().unwrap().submission_ids,
        vec![fx.submission.clone()]
    );
    assert_eq!(released.pending.len(), 1);
    assert_eq!(
        released.pending[0].input_ref(),
        AcceptedInputRef::Human { message }
    );
    assert_eq!(released.pending[0].queue_position(), 1);
    assert_eq!(released.next_queue_position, 2);
    assert!(matches!(
        released
            .attempt(&supplemental.submission_id)
            .unwrap()
            .delivery,
        InputDeliveryFact::NotDispatched { .. }
    ));
    let effective: Vec<(String, i64)> = sqlx::query_as(
        "SELECT submission_id,effective FROM conversation_submission_inputs ORDER BY queue_position",
    )
    .fetch_all(fx.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        effective,
        vec![
            (fx.submission.to_string(), 1),
            (supplemental.submission_id.to_string(), 0)
        ]
    );
    let frame = complete(&fx).await;
    let mut expected = released;
    expected.active = None;
    assert_eq!(fx.checkpoint().await, expected);
    assert_eq!(frame.snapshot["phase"], "turn_completed");
    assert!(matches!(
        expected.attempt(&fx.submission).unwrap().delivery,
        InputDeliveryFact::Accepted { .. }
    ));
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
    let mut header = sampled.fence.header().clone();
    header["push_watermark"] = json!(73);
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
    assert_eq!(result.snapshot["push_watermark"], 73);
    assert_eq!(result.snapshot["phase"], "turn_completed");
    assert!(
        fx.backend
            .qualify_execution_end(&sampled.target)
            .await
            .unwrap()
            .is_none()
    );
}
