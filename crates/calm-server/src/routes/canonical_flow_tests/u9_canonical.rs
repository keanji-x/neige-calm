//! Managed allocation -> authenticated Human input -> original local native peer.
use super::*;
use crate::state::RouteState;

#[tokio::test]
async fn u9_canonical_managed_first_input_once() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let route = RouteState::from_ref(&state);
    let now = crate::daily_planner::tests::at("2026-10-04T01:00:00Z");
    let track = crate::daily_planner::reconcile(&route, now).await.unwrap();
    let (card, worker, empty) = fixture.carrier(track.id.as_str()).await;
    assert!(empty.canonical_inputs.pending.is_empty());
    assert!(empty.canonical_inputs.active.is_none());
    assert!(empty.canonical_inputs.attempts.is_empty());
    assert!(state.harness.get(&worker).is_none());
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let uri = format!("/api/cards/{card}/planner/input");
    let input = json!({"text": FIRST_BODY});
    let (ack, replay) = tokio::join!(
        post(&app, &cookie, &uri, "managed-first", input.clone()),
        post(&app, &cookie, &uri, "managed-first", input.clone())
    );
    assert_eq!(ack, replay);
    assert_eq!(ack["worker_session_id"], worker);
    let turn = tokio::time::timeout(Duration::from_secs(10), async {
        let mut starts = 0;
        loop {
            let call = request(&mut peer).await;
            let result = match call["method"].as_str().unwrap() {
                "config/read" => json!({"config":{}}),
                "thread/start" => {
                    starts += 1;
                    assert_eq!(starts, 1);
                    json!({"thread":{"id":THREAD}})
                }
                "turn/start" => {
                    assert_eq!(starts, 1);
                    break call;
                }
                method => panic!("unexpected managed activation RPC: {method}"),
            };
            send(&mut peer, json!({"id":call["id"],"result":result})).await;
        }
    })
    .await
    .expect("first authenticated input activates its original empty carrier");
    assert!(turn["params"]["input"].to_string().contains(FIRST_BODY));
    let payload = route.repo.card_get(&card).await.unwrap().unwrap().payload;
    let context: calm_types::conversation::SystemRenderContext = serde_json::from_value(
        payload[crate::validation::TRACK_CREATION_CONTEXT_PAYLOAD_KEY].clone(),
    )
    .unwrap();
    assert!(
        turn["params"]["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["text"].as_str())
            .any(|text| text.contains(context.text())),
        "frozen daily working method reaches the real first turn"
    );

    let crossing = fixture.carrier(track.id.as_str()).await;
    assert_eq!((&crossing.0, &crossing.1), (&card, &worker));
    assert_eq!(crossing.2.canonical_inputs.attempts.len(), 1);
    send(
        &mut peer,
        json!({"id":turn["id"],"result":{"turn":{"id":TURN}}}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/started",
        json!({"id":TURN,"status":"inProgress"}),
    )
    .await;
    fixture
        .wait_phase(track.id.as_str(), HarnessPhaseTag::TurnRunning)
        .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/completed",
        json!({"id":TURN,"status":"completed"}),
    )
    .await;
    let done = fixture
        .wait_phase(track.id.as_str(), HarnessPhaseTag::TurnCompleted)
        .await;
    assert_eq!(done.canonical_inputs.attempts.len(), 1);
    assert!(matches!(
        done.canonical_inputs.attempts[0].delivery,
        InputDeliveryFact::Accepted { .. }
    ));
    assert!(done.canonical_inputs.active.is_none());
    assert!(done.canonical_inputs.pending.is_empty());
    let (replay, daily) = tokio::join!(
        post(&app, &cookie, &uri, "managed-first", input),
        crate::daily_planner::reconcile(&route, now)
    );
    assert_eq!(ack, replay);
    assert_eq!(daily.unwrap().id, track.id);
    // Hold the lifecycle fence during teardown; the completed carrier has no
    // pending input that a same-key wake could deliver again.
    let _fence = crate::per_card_lock::lock_card(&route.planner_recovery_locks, &card).await;
    assert_eq!(
        fixture.carrier(track.id.as_str()).await.2.canonical_inputs,
        done.canonical_inputs
    );
    assert_eq!(fixture.counts(&card).await, (1, 1, 1, 1, 0));
    let human: String =
        sqlx::query_scalar("SELECT author_user_id FROM conversation_messages WHERE card_id=?1")
            .bind(&card)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(human, calm_types::organization::LEGACY_OWNER_USER_ID);
    // No second execution RPC may have been issued before the fence settled.
    use futures::{FutureExt, StreamExt};
    while let Some(Some(message)) = peer.next().now_or_never() {
        let message = message.unwrap();
        let call: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        assert_eq!(call["method"], "config/mcpServer/reload");
        send(&mut peer, json!({"id":call["id"],"result":{}})).await;
    }
    if let Some(handle) = state.harness.remove(&worker) {
        let mut stopping = tokio::spawn(async move { handle.shutdown().await });
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::select! {
                result = &mut stopping => result.unwrap().unwrap(),
                call = request(&mut peer) => {
                    assert_eq!(call["method"], "turn/interrupt");
                    send(&mut peer, json!({"id":call["id"],"result":{}})).await;
                    stopping.await.unwrap().unwrap();
                }
            }
        })
        .await
        .unwrap();
    }
    fixture.shutdown_process(&state).await;
}
