//! Exercise sleep -> committed input -> native execution -> END -> sleep on the real loop.
use super::*;
use crate::test_seams::{PausePoint, install_pause_for_test};

#[tokio::test]
async fn ordinary_turn_wakes_from_idle_and_returns_without_fifty_ms_probes() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    super::control_tests::running(&fx, &mut peer).await;
    let thread = fx.frame().await.header.last_thread_id.unwrap();
    let idle = PausePoint::default();
    let card = fx.harness.inner.card_id.as_str();
    install_pause_for_test("canonical-idle-wait", card, idle.clone());
    notify_thread(
        &fx.daemon,
        &mut peer,
        &thread,
        "turn/completed",
        json!({"id":"bounded-original","status":"completed"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), idle.entered.notified())
        .await
        .unwrap();
    let probe = PausePoint::default();
    install_pause_for_test("canonical-before-end-probe", card, probe.clone());
    idle.release.notify_one();
    let polled = tokio::time::timeout(Duration::from_millis(250), probe.entered.notified()).await;
    // Release even a regressed loop before reporting the failure.
    probe.release.notify_one();
    crate::test_seams::take("canonical-before-end-probe", card);
    assert!(
        polled.is_err(),
        "an unchanged idle conversation must not run the active polling pipeline"
    );
    let mut tx = begin_immediate_tx(fx.original.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, card, &fx.harness.inner.worker_session_id)
        .await
        .unwrap();
    let accepted = crate::conversation_inputs::accept_human_tx(
        &mut tx,
        &fx.source.current_principal().unwrap(),
        &mut carrier,
        HumanPayload {
            text: "wake the sleeping conversation".into(),
            bound_attachments: vec![],
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let AcceptedInputRef::Human { message } = accepted.input_ref else {
        panic!("human input")
    };
    fx.harness
        .observe_canonical_source(message, fx.source.clone())
        .await
        .unwrap();
    let call = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let call = request(&mut peer).await;
            if call["method"] == "turn/start" {
                break call;
            }
            assert_eq!(call["method"], "config/read");
            send(&mut peer, json!({"id":call["id"],"result":{"config":{}}})).await;
        }
    })
    .await
    .expect("a saved input must wake the loop without its 30-second reconciliation");
    assert!(
        call["params"]["input"]
            .to_string()
            .contains("wake the sleeping conversation")
    );
    notify_thread(
        &fx.daemon,
        &mut peer,
        &thread,
        "turn/started",
        json!({"id":"next-normal-turn","status":"inProgress"}),
    )
    .await;
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":"next-normal-turn"}}}),
    )
    .await;
    fx.wait_phase(HarnessPhaseTag::TurnRunning).await;
    let idle_again = PausePoint::default();
    install_pause_for_test("canonical-idle-wait", card, idle_again.clone());
    notify_thread(
        &fx.daemon,
        &mut peer,
        &thread,
        "turn/completed",
        json!({"id":"next-normal-turn","status":"completed"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), idle_again.entered.notified())
        .await
        .unwrap();
    let done = fx.frame().await;
    assert!(done.canonical_inputs.active.is_none());
    assert!(done.canonical_inputs.pending.is_empty());
    assert_eq!(
        done.header.last_turn_id.as_deref(),
        Some("next-normal-turn")
    );
    idle_again.release.notify_one();
    fx.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn declared_compaction_wakes_idle_and_rechecks_after_ingress_drains() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    super::control_tests::running(&fx, &mut peer).await;
    let thread = fx.frame().await.header.last_thread_id.unwrap();
    let card = fx.harness.inner.card_id.as_str();
    let idle = PausePoint::default();
    install_pause_for_test("canonical-idle-wait", card, idle.clone());
    notify_thread(
        &fx.daemon,
        &mut peer,
        &thread,
        "turn/completed",
        json!({"id":"bounded-original","status":"completed"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), idle.entered.notified())
        .await
        .unwrap();
    let progress = PausePoint::default();
    install_pause_for_test("canonical-before-progress-advance", card, progress.clone());
    let (_scheduler, _) = crate::harness::run_loop::compact_task_tests::declared_for(
        fx.original.repo.clone(),
        &fx.harness,
        fx.daemon.clone(),
    )
    .await;
    idle.release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), progress.entered.notified())
        .await
        .expect("a committed compact task must wake the idle loop");
    // Force maintenance to defer behind an actual queued read command. That
    // command itself changes no state and must not be required to wake work.
    let (answer, inspected) = oneshot::channel();
    let ObservationIngress::Running(sender) = &fx.harness.inner.observations else {
        panic!("running loop")
    };
    sender
        .try_send(HarnessObservationCommand::InspectInput {
            entry_id: QueueEntryId::from_wire("no-such-input".into()),
            answer,
        })
        .unwrap();
    progress.release.notify_one();
    let _ = tokio::time::timeout(Duration::from_secs(3), inspected)
        .await
        .unwrap()
        .unwrap();
    let call = tokio::time::timeout(Duration::from_secs(3), request(&mut peer))
        .await
        .expect("deferred maintenance must recheck after ingress drains");
    assert_eq!(call["method"], "thread/compact/start");
    send(&mut peer, json!({"id":call["id"],"result":{}})).await;
    notify_thread(
        &fx.daemon,
        &mut peer,
        &thread,
        "turn/started",
        json!({"id":"idle-compact"}),
    )
    .await;
    notify_thread(
        &fx.daemon,
        &mut peer,
        &thread,
        "turn/completed",
        json!({"id":"idle-compact","status":"completed"}),
    )
    .await;
    let done = fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    assert_eq!(
        done.header.compact_outcome.unwrap().status,
        crate::harness::compact_outcome::CompactStatus::Completed
    );
    super::compact_tests::shutdown(&fx.harness, &mut peer).await;
}

#[tokio::test]
async fn running_turn_waits_for_native_result_without_fifty_ms_probes() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    super::control_tests::running(&fx, &mut peer).await;
    let card = fx.harness.inner.card_id.as_str();
    let pass = PausePoint::default();
    install_pause_for_test("canonical-tick-returned", card, pass.clone());
    fx.harness.request_progress();
    tokio::time::timeout(Duration::from_secs(3), pass.entered.notified())
        .await
        .unwrap();
    let next = PausePoint::default();
    install_pause_for_test("canonical-before-end-probe", card, next.clone());
    pass.release.notify_one();
    let polled = tokio::time::timeout(Duration::from_millis(250), next.entered.notified())
        .await
        .is_ok();
    if polled {
        next.release.notify_one();
        crate::test_seams::take("canonical-before-end-probe", card);
        peer.close(None).await.unwrap();
        fx.harness.shutdown().await.unwrap();
        panic!("an unchanged running execution must wait for readiness or its actual deadline");
    }
    let original = fx.frame().await;
    notify_thread(
        &fx.daemon,
        &mut peer,
        original.header.last_thread_id.as_deref().unwrap(),
        "turn/completed",
        json!({"id":original.header.last_turn_id,"status":"completed"}),
    )
    .await;
    // Native readiness must enter the actual progress path before reconciliation.
    tokio::time::timeout(Duration::from_secs(1), next.entered.notified())
        .await
        .expect("native result must immediately wake the sleeping execution");
    next.release.notify_one();
    let done = fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    assert!(done.canonical_inputs.active.is_none());
    peer.close(None).await.unwrap();
    fx.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn stop_uses_its_actual_deadline_while_waiting_for_late_native_end() {
    let fx = Fixture::with_config(HarnessConfig {
        interrupt_completion_budget: Duration::from_millis(250),
        ..HarnessConfig::default()
    })
    .await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    super::control_tests::running(&fx, &mut peer).await;
    let original = fx.frame().await;
    let stop = tokio::spawn({
        let harness = fx.harness.clone();
        let source = fx.source.clone();
        async move { harness.stop_canonical(source).await }
    });
    let call = request(&mut peer).await;
    assert_eq!(call["method"], "turn/interrupt");
    send(&mut peer, json!({"id":call["id"],"result":{}})).await;
    stop.await.unwrap().unwrap();
    let wedged = tokio::time::timeout(
        Duration::from_millis(900),
        fx.wait_phase(HarnessPhaseTag::Wedged),
    )
    .await
    .expect("armed Stop deadline must preempt the 30-second result reconciliation");
    assert_eq!(
        wedged.header.wedged_reason.as_deref(),
        Some(calm_types::harness::HARNESS_INTERRUPT_TIMEOUT_REASON)
    );
    assert_eq!(
        wedged.canonical_inputs, original.canonical_inputs,
        "timeout cannot certify END"
    );
    notify_thread(
        &fx.daemon,
        &mut peer,
        original.header.last_thread_id.as_deref().unwrap(),
        "turn/completed",
        json!({"id":original.header.last_turn_id,"status":"interrupted"}),
    )
    .await;
    let done = fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    assert!(done.canonical_inputs.active.is_none());
    assert!(done.header.interruption_intent.is_none());
    peer.close(None).await.unwrap();
    fx.harness.shutdown().await.unwrap();
}
