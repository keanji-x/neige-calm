//! Display actual owning execution feedback; no reader acquires execution authority.
use super::*;

async fn read_run(app: &axum::Router, cookie: &str, card: &str) -> Value {
    let response = app.clone().oneshot(Request::get(format!(
        "/api/cards/{card}/planner/run?contract=conversation_v1"
    )).header(header::COOKIE, cookie).body(Body::empty()).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn save_frame(fixture: &Fixture, worker: &str, frame: &CanonicalHarnessSnapshot) {
    let worker = worker.to_owned();
    let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.pool()).await.unwrap();
    assert!(crate::db::sqlite::session_set_handle_state_tx(
        &mut tx, &worker, Some(serde_json::to_value(frame).unwrap())).await.unwrap());
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_run_reads_the_actual_live_source_refusal_after_config() {
    let fixture = Fixture::new().await;
    let (state, app, cookie, auth) = fixture.application_with_auth(false).await;
    let area = post(&app, &cookie, "/api/areas", "source-feedback-area",
        json!({"name":"Source feedback","color":"#123456"})).await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let created = post(&app, &cookie, "/api/tracks", "source-feedback",
        json!({"area_id":area["id"],"planner_provider":"codex","title":"",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY})).await;
    let config = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let call = request(&mut peer).await;
            match call["method"].as_str().unwrap() {
                "thread/start" => send(&mut peer, json!({"id":call["id"],"result":{"thread":{"id":THREAD}}})).await,
                "config/read" => break call,
                method => panic!("unexpected pre-admission request: {method}"),
            }
        }
    }).await.unwrap();
    auth.sessions.remove(cookie.split_once('=').unwrap().1).expect("actual issuing session");
    send(&mut peer, json!({"id":config["id"],"result":{"config":{"model":"private-model"}}})).await;
    let (card, worker, _) = fixture.carrier(created["id"].as_str().unwrap()).await;
    let handle = state.harness.get(&worker).expect("actual original handle");
    let reason = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(reason) = handle.issuance_block().await { break reason; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.expect("the actual refusal owner must publish safe feedback");
    assert!(reason.contains("session"));
    // A new verified login can read the refusal. GET does not re-admit the old source.
    let response = app.clone().oneshot(Request::post("/api/auth/login")
        .extension(ConnectInfo("127.0.0.1:4444".parse::<SocketAddr>().unwrap()))
        .header(header::CONTENT_TYPE,"application/json")
        .body(Body::from(json!({"username":"jian","password":"fixture-password"}).to_string())).unwrap())
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap();
    let before = fixture.carrier(created["id"].as_str().unwrap()).await.2;
    let run = read_run(&app, cookie, &card).await;
    assert_eq!(run["blocked_reason"], reason, "{run}");
    assert_eq!(fixture.carrier(created["id"].as_str().unwrap()).await.2, before);
    handle.shutdown().await.unwrap();
    peer.close(None).await.unwrap();
}

#[tokio::test]
async fn canonical_run_reads_only_the_exact_current_terminal_timeout_and_still_saves() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = post(&app, &cookie, "/api/areas", "timeout-read-area",
        json!({"name":"Timeout feedback","color":"#123456"})).await;
    let track = post(&app, &cookie, "/api/tracks", "timeout-read",
        json!({"area_id":area["id"],"planner_provider":"codex","title":"",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY})).await;
    let track_id = track["id"].as_str().unwrap();
    let (card, worker, mut frame) = fixture.carrier(track_id).await;
    // A persisted read fixture, not a claim of real native END. Mint protected
    // evidence through the existing terminal recorder and preserve original input.
    let request = calm_types::harness::HarnessTimeoutRequest {
        session_id: worker.clone(), card_id: card.clone(), track_id: track_id.into(),
        thread_id: THREAD.into(), turn_id: TURN.into(),
        reason: calm_types::harness::HarnessInterruptionReason::MaxTurnDuration,
        requested_at_ms: 1, effective_elapsed_ms: 61000, budget_ms: 60000,
    };
    frame.header.phase = HarnessPhaseTag::TurnCompleted;
    frame.header.last_thread_id = Some(THREAD.into());
    frame.header.last_turn_id = Some(TURN.into());
    frame.header.interruption_intent = Some(calm_types::harness::HarnessInterruptionIntent::Versioned(
        calm_types::harness::HarnessTimeoutIntent::V1(request.clone())));
    save_frame(&fixture, &worker, &frame).await;
    crate::harness::turn_outcome::record(fixture.repo.as_ref(), &worker, &card, track_id, THREAD, TURN,
        &json!({"id":TURN,"status":"interrupted"}), crate::harness::turn_outcome::Confirmation::ProviderTerminal)
        .await.unwrap();
    frame.header.interruption_intent = None;
    save_frame(&fixture, &worker, &frame).await;
    let run = read_run(&app, &cookie, &card).await;
    assert_eq!(run["runtime_state"], "idle");
    assert_eq!(run["phase"], "turn_completed");
    assert_eq!(run["terminal_timeout"]["request"], serde_json::to_value(&request).unwrap());
    assert_eq!(run["history_replacement_supported"], false);
    for changed in ["session", "thread", "turn"] {
        let mut other = request.clone();
        match changed {
            "session" => other.session_id = "old-session".into(),
            "thread" => other.thread_id = "old-thread".into(),
            "turn" => other.turn_id = "old-turn".into(),
            _ => unreachable!(),
        }
        let evidence = calm_types::harness::HarnessTimeoutEvidence::V1 { request: other.clone(), ended_at_ms: 2 };
        fixture.repo.harness_turn_outcome_put(&other.session_id, &card, track_id, &other.thread_id, &other.turn_id,
            &json!({"id":other.turn_id,"status":"interrupted","harness_interruption_reason":evidence}).to_string())
            .await.unwrap();
        assert_eq!(read_run(&app, &cookie, &card).await["terminal_timeout"], Value::Null, "{changed}");
    }
    let saved = post(&app, &cookie, &format!("/api/cards/{card}/planner/input"), "after-timeout",
        json!({"text":"A confirmed timeout does not refuse future content"})).await;
    assert_eq!(saved["worker_session_id"], worker);
    assert_eq!(fixture.counts(&card).await.0, 2);
    assert_eq!(fixture.carrier(track_id).await.2.canonical_inputs.pending.len(), 2);
    assert!(state.harness.get(&worker).is_none(), "read never manufactures a live runtime");
}
