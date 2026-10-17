//! The ordinary application POST creates, executes, and stops its original carrier.
use super::*;

#[tokio::test]
async fn canonical_http_stop_controls_the_original_execution_and_waits_for_real_end() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "stop-area",
        json!({"name":"Stop original","color":"#123456"}),
    )
    .await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let created = post(
        &app,
        &cookie,
        "/api/tracks",
        "first",
        json!({
            "area_id":area["id"],"title":"","planner_provider":"codex",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY
        }),
    )
    .await;
    let track = created["id"].as_str().unwrap();
    let call = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let call = request(&mut peer).await;
            let result = match call["method"].as_str().unwrap() {
                "turn/start" => break call,
                "thread/start" => json!({"thread":{"id":THREAD}}),
                "config/read" => json!({"config":{}}),
                method => panic!("unexpected activation request: {method}"),
            };
            send(&mut peer, json!({"id":call["id"],"result":result})).await;
        }
    })
    .await
    .unwrap();
    let (card, _, before_ack) = fixture.carrier(track).await;
    assert_eq!(before_ack.header.phase, HarnessPhaseTag::IssuingTurn);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{card}/planner/interrupt"))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "unconfirmed issuance must not pretend a Stop was dispatched"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(80), request(&mut peer))
            .await
            .is_err()
    );
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/started",
        json!({"id":TURN}),
    )
    .await;
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":TURN}}}),
    )
    .await;
    let original = fixture
        .wait_phase(track, HarnessPhaseTag::TurnRunning)
        .await;
    let (card, worker, _) = fixture.carrier(track).await;
    let uri = format!("/api/cards/{card}/planner/interrupt");
    // Collaboration access alone cannot control this deployment's private
    // execution environment. This changes policy facts, never a runtime phase.
    let area_id = area["id"].as_str().unwrap();
    sqlx::query("UPDATE area_members SET role='member' WHERE area_id=?1 AND user_id=?2")
        .bind(area_id)
        .bind(calm_types::organization::LEGACY_OWNER_USER_ID)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    let denied = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert_eq!(fixture.carrier(track).await.2, original);
    assert!(
        tokio::time::timeout(Duration::from_millis(80), request(&mut peer))
            .await
            .is_err()
    );
    sqlx::query("UPDATE area_members SET role='owner' WHERE area_id=?1 AND user_id=?2")
        .bind(area_id)
        .bind(calm_types::organization::LEGACY_OWNER_USER_ID)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    let mut stop = tokio::spawn({
        let (app, cookie, uri) = (app.clone(), cookie.clone(), uri.clone());
        async move { post(&app, &cookie, &uri, "stop", json!({})).await }
    });
    let interrupt = tokio::select! {
        answer = &mut stop => panic!("Stop did not reach its actual native request: {answer:?}"),
        call = tokio::time::timeout(Duration::from_secs(2), request(&mut peer)) => call.expect("actual Stop must issue a native interrupt"),
    };
    assert_eq!(interrupt["method"], "turn/interrupt");
    assert_eq!(interrupt["params"]["threadId"], THREAD);
    assert_eq!(interrupt["params"]["turnId"], TURN);
    let stopping = fixture
        .wait_phase(track, HarnessPhaseTag::IssuingInterrupt)
        .await;
    assert_eq!(stopping.canonical_inputs, original.canonical_inputs);
    send(&mut peer, json!({"id":interrupt["id"],"result":{}})).await;
    let answer = stop.await.unwrap();
    assert_eq!(answer["worker_session_id"], worker);
    assert_eq!(answer["stopped"], true);
    assert_eq!(
        fixture.carrier(track).await.2.canonical_inputs,
        original.canonical_inputs,
        "native ACK cannot settle the original work"
    );
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/completed",
        json!({"id":TURN,"status":"interrupted"}),
    )
    .await;
    let done = fixture
        .wait_phase(track, HarnessPhaseTag::TurnCompleted)
        .await;
    assert!(done.canonical_inputs.active.is_none());
    assert_eq!(
        done.canonical_inputs.attempts,
        original.canonical_inputs.attempts
    );
    peer.close(None).await.unwrap();
    state
        .harness
        .get(&worker)
        .unwrap()
        .shutdown()
        .await
        .unwrap();
}
