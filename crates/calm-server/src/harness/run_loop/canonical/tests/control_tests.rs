//! Controls exercise the real loop and protocol owner; no fixture creates a claim or END.
use super::*;

async fn running(fx: &Fixture, peer: &mut crate::shared_codex_appserver::native_owner_tests::Peer) {
    fx.original
        .repo
        .settings_upsert(crate::planner_turn_budget::KEY, "1")
        .await
        .unwrap();
    fx.harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone())
        .await
        .unwrap();
    let call = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let call = request(peer).await;
            match call["method"].as_str().unwrap() {
                "turn/start" => break call,
                "config/read" => {
                    send(peer, json!({"id":call["id"],"result":{"config":{}}})).await;
                }
                method => panic!("unexpected native request before running: {method}"),
            }
        }
    })
    .await
    .unwrap();
    let thread = call["params"]["threadId"].as_str().unwrap();
    notify_thread(
        &fx.daemon,
        peer,
        thread,
        "turn/started",
        json!({"id":"bounded-original"}),
    )
    .await;
    send(
        peer,
        json!({"id":call["id"],"result":{"turn":{"id":"bounded-original"}}}),
    )
    .await;
    fx.wait_phase(HarnessPhaseTag::TurnRunning).await;
}

#[tokio::test]
async fn canonical_budget_warns_once_and_hard_stops_the_actual_original_execution() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    running(&fx, &mut peer).await;
    let original = fx.frame().await;
    let thread = original.header.last_thread_id.as_deref().unwrap();
    let turn = original.header.last_turn_id.as_deref().unwrap();
    // The existing timer seam moves only a running turn's monotonic clocks.
    fx.harness
        .rewind_turn_clock_for_test(Duration::from_secs(49))
        .await;
    let warning = tokio::time::timeout(Duration::from_secs(2), request(&mut peer))
        .await
        .expect("the actual canonical scheduler must issue its configured budget warning");
    assert_eq!(warning["method"], "turn/steer");
    assert_eq!(warning["params"]["threadId"], thread);
    assert_eq!(warning["params"]["expectedTurnId"], turn);
    assert!(
        warning["params"]["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| {
                item["text"].as_str().is_some_and(|text| {
                    text.starts_with("[Trusted Planner turn budget reminder]\n")
                        && text.contains("12 seconds of execution remain")
                })
            })
    );
    send(
        &mut peer,
        json!({"id":warning["id"],"result":{"turnId":turn}}),
    )
    .await;
    let warned = fx.frame().await;
    assert!(matches!(
        warned.header.turn_budget,
        budget::FrozenBudget::Planner {
            warning_attempted: true,
            ..
        }
    ));
    assert_eq!(warned.canonical_inputs, original.canonical_inputs);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), request(&mut peer))
            .await
            .is_err(),
        "warning is attempted only once"
    );
    fx.harness
        .rewind_turn_clock_for_test(Duration::from_secs(20))
        .await;
    let stop = tokio::time::timeout(Duration::from_secs(2), request(&mut peer))
        .await
        .expect("the same scheduler must enforce the frozen hard stop");
    assert_eq!(stop["method"], "turn/interrupt");
    assert_eq!(stop["params"]["threadId"], thread);
    assert_eq!(stop["params"]["turnId"], turn);
    let stopping = fx.wait_phase(HarnessPhaseTag::IssuingInterrupt).await;
    assert_eq!(stopping.canonical_inputs, original.canonical_inputs);
    assert!(stopping.header.interruption_intent.is_some());
    send(&mut peer, json!({"id":stop["id"],"result":{}})).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        fx.frame().await.canonical_inputs,
        original.canonical_inputs,
        "control ACK is not END"
    );
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/completed",
        json!({"id":turn,"status":"interrupted"}),
    )
    .await;
    let done = fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    assert!(done.canonical_inputs.active.is_none());
    assert_eq!(
        done.canonical_inputs.attempts,
        original.canonical_inputs.attempts
    );
    assert!(done.header.interruption_intent.is_none());
    fx.harness.shutdown().await.unwrap();
}
