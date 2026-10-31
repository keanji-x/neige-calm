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
