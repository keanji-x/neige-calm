//! Real password/session extraction and native peer through the canonical route.
use super::*;

async fn response(
    app: &axum::Router,
    cookie: &str,
    uri: &str,
    key: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .header("Idempotency-Key", key)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn canonical_keep_without_turn_requires_exact_settled_activity_for_next_input() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Keep original", "color":"#123456"}),
    )
    .await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let created = post(&app, &cookie, "/api/tracks", "create", json!({"area_id":area["id"],"title":"","planner_provider":"codex", "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY})).await;
    let track = created["id"].as_str().unwrap();
    let initial = super::native_attempt_recovery_tests::initial_request(&mut peer).await;
    send(&mut peer, json!({"id":initial["id"],"result":{"turn":{}}})).await;
    let (card, worker, before) = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let facts = fixture.carrier(track).await;
            if matches!(
                facts.2.canonical_inputs.attempts[0].delivery,
                InputDeliveryFact::Unknown
            ) {
                break facts;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let attempt = &before.canonical_inputs.attempts[0];
    let args = json!({"carrier_id":worker,"submission_id":attempt.attempt.submission_id,"attempt_id":attempt.attempt.attempt_id,"revision":attempt.revision.to_string()});
    let uri = format!("/api/cards/{card}/conversation/input/keep");
    let (status, kept) = response(&app, &cookie, &uri, "keep", args.clone()).await;
    assert_eq!(status, StatusCode::OK, "{kept}");
    assert_eq!(kept["waiting_for_settlement"], true);
    assert_eq!(
        response(&app, &cookie, &uri, "keep", args.clone()).await,
        (status, kept.clone())
    );
    let mut changed = args.clone();
    changed["revision"] = json!("0");
    assert_eq!(
        response(&app, &cookie, &uri, "keep", changed).await.0,
        StatusCode::CONFLICT
    );
    let (_, _, after) = fixture.carrier(track).await;
    assert_eq!(after.header.phase, before.header.phase);
    assert_eq!(
        after.canonical_inputs.attempts[0].delivery,
        attempt.delivery
    );
    assert_eq!(
        after.canonical_inputs.attempts[0].correlation,
        attempt.correlation
    );
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "next",
        json!({"text":"Following the kept original"}),
    )
    .await;
    // Every non-settled exact read must hold, including an idle response for a
    // different thread. No cached Idle or interrupt ACK can satisfy this test.
    for thread in [
        json!({"id":THREAD,"status":{"type":"active"}}),
        json!({"id":THREAD,"status":{"type":"notLoaded"}}),
        json!({"id":"other-thread","status":{"type":"idle"}}),
        json!({"id":THREAD,"status":{"type":"unknown"}}),
        json!({"status":{"type":"idle"}}),
    ] {
        loop {
            let call = tokio::time::timeout(Duration::from_secs(5), request(&mut peer))
                .await
                .unwrap();
            match call["method"].as_str().unwrap() {
                "config/read" => {
                    send(&mut peer, json!({"id":call["id"],"result":{"config":{}}})).await
                }
                "thread/read" => {
                    assert_eq!(call["params"]["threadId"], THREAD);
                    send(
                        &mut peer,
                        json!({"id":call["id"],"result":{"thread":thread}}),
                    )
                    .await;
                    break;
                }
                method => panic!("unsettled original permits no new execution: {method}"),
            }
        }
    }
    let mut observed = vec![];
    let outcome = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let call = request(&mut peer).await;
            observed.push(call["method"].clone());
            let result = match call["method"].as_str().unwrap() {
                "config/read" => json!({"config":{}}),
                "thread/read" => json!({"thread":{"id":THREAD,"status":{"type":"idle"}}}),
                "turn/start" => break call,
                method => panic!("Keep must reuse its original thread: {method}"),
            };
            send(&mut peer, json!({"id":call["id"],"result":result})).await;
        }
    })
    .await;
    let next = match outcome {
        Ok(call) => call,
        Err(error) => {
            let (_, _, frame) = fixture.carrier(track).await;
            let block = match state.harness.get(&worker) {
                Some(handle) => handle.issuance_block().await,
                None => None,
            };
            panic!(
                "exact settled activity must permit following input: {error}; methods={observed:?}; block={block:?}; frame={frame:?}"
            );
        }
    };
    assert!(
        next["params"]["input"]
            .to_string()
            .contains("Following the kept original")
    );
    assert!(!next["params"]["input"].to_string().contains(FIRST_BODY));
    send(
        &mut peer,
        json!({"id":next["id"],"result":{"turn":{"id":TURN}}}),
    )
    .await;
    fixture
        .wait_phase(track, HarnessPhaseTag::TurnRunning)
        .await;
    // The committed receipt survives an unavailable publisher and a real wake
    // refusal on the Failed carrier; neither is part of the immutable answer.
    let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
        .await
        .unwrap();
    crate::db::sqlite::session_fail_if_active_runtime_tx(&mut tx, &worker)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let removed = state.harness.remove(&worker);
    if let Some(handle) = &removed {
        handle.pause_issuance_for_dev();
    }
    let pause = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "canonical-human-wake-finished",
        &card,
        pause.clone(),
    );
    assert_eq!(
        response(&app, &cookie, &uri, "keep", args.clone()).await,
        (StatusCode::OK, kept.clone())
    );
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .expect("post-commit wake refusal finishes");
    pause.release.notify_one();
    let reopened = SqlxRepo::open(&fixture.url).await.unwrap();
    let raw: String =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(reopened.pool())
            .await
            .unwrap();
    let restored: CanonicalHarnessSnapshot = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        serde_json::to_value(&restored.canonical_inputs.receipts[0].result).unwrap(),
        kept
    );
    post(&app, &cookie, "/api/auth/logout", "logout", json!({})).await;
    assert_eq!(
        response(&app, &cookie, &uri, "keep", args).await.0,
        StatusCode::UNAUTHORIZED
    );
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn canonical_keep_agent_and_dispatcher_forbidden() {
    let fixture = Fixture::new().await;
    let (state, app, _) = fixture.application().await;
    for actor in ["ai:codex", "dispatcher"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/cards/inaccessible/conversation/input/keep")
                    .header("X-Calm-Actor", actor)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("Idempotency-Key", "keep")
                    .body(Body::from(
                        r#"{"carrier_id":"c","submission_id":"s","attempt_id":"a","revision":"1"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn canonical_keep_logout_or_access_change_forbidden() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Live Keep authority", "color":"#123456"}),
    )
    .await;
    let created = post(&app, &cookie, "/api/tracks", "create", json!({"area_id":area["id"],"title":"","planner_provider":"codex", "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY})).await;
    let (card, worker, _) = fixture.carrier(created["id"].as_str().unwrap()).await;
    let args = json!({"carrier_id":worker,"submission_id":"not-held","attempt_id":"not-crossed","revision":"0"});
    let uri = format!("/api/cards/{card}/conversation/input/keep");
    assert_eq!(
        response(&app, &cookie, &uri, "keep", args.clone()).await.0,
        StatusCode::CONFLICT
    );
    post(&app, &cookie, "/api/auth/logout", "logout", json!({})).await;
    assert_eq!(
        response(&app, &cookie, &uri, "keep", args).await.0,
        StatusCode::UNAUTHORIZED
    );
    fixture.shutdown_process(&state).await;
}
