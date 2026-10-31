//! Real authenticated message/claim/native/END owners; only the external peer is scripted.
use super::input_feedback_tests::run;
use super::*;

#[tokio::test]
async fn pending_controls_http_disconnect_does_not_strand_native_steer_ticket() {
    let fixture = Fixture::new().await;
    let mut r = running(&fixture).await;
    let queued = post(
        &r.app,
        &r.cookie,
        &format!("/api/cards/{}/planner/input", r.card),
        "disconnect-message",
        json!({"text":"accepted before disconnect"}),
    )
    .await;
    let path = format!(
        "/api/cards/{}/conversation/messages/{}/steer",
        r.card,
        queued["entry_id"].as_str().unwrap()
    );
    let mut waiter = tokio::spawn({
        let (app, cookie, worker) = (r.app.clone(), r.cookie.clone(), r.worker.clone());
        async move {
            control(
                &app,
                &cookie,
                "POST",
                &path,
                json!({"carrier_id":worker,"if_revision":1,"turn_id":TURN}),
            )
            .await
        }
    });
    let native = tokio::select! {
        result=&mut waiter=>panic!("steer did not reach the native boundary: {result:?}"),
        call=tokio::time::timeout(Duration::from_secs(5),request(&mut r.peer))=>call.unwrap(),
    };
    let read = run(
        &r.app,
        &r.cookie,
        &format!("/api/cards/{}/planner/run?contract=conversation_v1", r.card),
    )
    .await;
    let supplemental = read["active_input"]["submissions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|submission| submission["ordinal"] == 1)
        .unwrap()["submission_id"]
        .as_str()
        .unwrap()
        .to_owned();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    // The owned operation still records uncertainty and clears its native ticket.
    send(
        &mut r.peer,
        json!({"id":native["id"],"result":{"turnId":"unrelated"}}),
    )
    .await;
    let held = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = fixture.carrier(&r.track).await.2;
            if let Some(attempt) = frame.canonical_inputs.attempts.iter().find(|attempt| {
                attempt.attempt.submission_id.as_str() == supplemental
                    && matches!(attempt.delivery, InputDeliveryFact::Unknown)
            }) {
                break attempt.clone();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    post(&r.app,&r.cookie,&format!("/api/cards/{}/conversation/input/keep",r.card),"keep-disconnected",json!({
        "carrier_id":r.worker,"submission_id":held.attempt.submission_id,"attempt_id":held.attempt.attempt_id,"revision":held.revision.to_string(),
    })).await;
    notify_thread(
        &r.state.shared_codex_appserver,
        &mut r.peer,
        THREAD,
        "turn/completed",
        json!({"id":TURN,"status":"completed"}),
    )
    .await;
    let done = fixture
        .wait_phase(&r.track, HarnessPhaseTag::TurnCompleted)
        .await;
    assert!(done.canonical_inputs.active.is_none());
    assert!(done.canonical_inputs.pending.is_empty());
    assert!(matches!(
        done.canonical_inputs
            .attempt(&held.attempt.submission_id)
            .unwrap()
            .delivery,
        InputDeliveryFact::Unknown
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), request(&mut r.peer))
            .await
            .is_err()
    );
    r.peer.close(None).await.unwrap();
    r.state
        .harness
        .get(&r.worker)
        .unwrap()
        .shutdown()
        .await
        .unwrap();
}

#[tokio::test]
async fn pending_controls_revalidate_actual_session_after_the_native_sink_wait() {
    sink_wait_case(None).await;
}
#[tokio::test]
async fn pending_controls_keep_before_final_gate_does_not_block_original_end() {
    sink_wait_case(Some("keep")).await;
}
#[tokio::test]
async fn pending_controls_retry_before_final_gate_waits_for_original_end() {
    sink_wait_case(Some("retry")).await;
}

async fn sink_wait_case(decision: Option<&str>) {
    let fixture = Fixture::new().await;
    let mut r = running(&fixture).await;
    let queued = post(
        &r.app,
        &r.cookie,
        &format!("/api/cards/{}/planner/input", r.card),
        "revoked-message",
        json!({"text":"must never cross revoked source"}),
    )
    .await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let blocker = tokio::spawn({
        let (client, entered, release) = (r.client.clone(), entered.clone(), release.clone());
        async move {
            client
                .config_read_gated(
                    None,
                    tokio::time::Instant::now() + Duration::from_secs(10),
                    &provider::send_boundary::PreSendGate::new(|| {
                        Box::pin(async {
                            entered.notify_one();
                            release.notified().await;
                            Ok(())
                        })
                    }),
                )
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let path = format!(
        "/api/cards/{}/conversation/messages/{}/steer",
        r.card,
        queued["entry_id"].as_str().unwrap()
    );
    let steering = tokio::spawn({
        let (app, cookie, worker) = (r.app.clone(), r.cookie.clone(), r.worker.clone());
        async move {
            control(
                &app,
                &cookie,
                "POST",
                &path,
                json!({"carrier_id":worker,"if_revision":1,"turn_id":TURN}),
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while r.client.pending_requests_for_test() < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    if let Some(decision) = decision {
        let read = run(
            &r.app,
            &r.cookie,
            &format!("/api/cards/{}/planner/run?contract=conversation_v1", r.card),
        )
        .await;
        let supplemental = read["active_input"]["submissions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["ordinal"] == 1)
            .unwrap();
        let attempt = &supplemental["attempt"];
        assert_eq!(attempt["actions"][decision]["kind"], "available");
        post(&r.app,&r.cookie,&format!("/api/cards/{}/conversation/input/{decision}",r.card),"resolve-before-gate",json!({
            "carrier_id":r.worker,"submission_id":supplemental["submission_id"],"attempt_id":attempt["attempt_id"],"revision":attempt["revision"],
        })).await;
    } else {
        let (_, token) = r.cookie.split_once('=').unwrap();
        r.auth.sessions.remove(token).unwrap();
    }
    release.notify_one();
    let config = request(&mut r.peer).await;
    assert_eq!(config["method"], "config/read");
    send(
        &mut r.peer,
        json!({"id":config["id"],"result":{"config":{}}}),
    )
    .await;
    blocker.await.unwrap().unwrap();
    let (status, result) = steering.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["delivery"], "not_dispatched");
    let restored = fixture.carrier(&r.track).await.2;
    assert_eq!(
        restored
            .canonical_inputs
            .active
            .as_ref()
            .unwrap()
            .submission_ids
            .len(),
        if decision.is_some() { 2 } else { 1 }
    );
    if decision.is_some() {
        assert!(
            !restored
                .canonical_inputs
                .pending
                .iter()
                .any(|p| p.message_id().is_some())
        );
        assert_eq!(
            restored.canonical_inputs.pending.len(),
            usize::from(decision == Some("retry"))
        );
    } else {
        assert_eq!(restored.canonical_inputs.pending.len(), 1);
        assert_eq!(
            restored.canonical_inputs.pending[0]
                .message_id()
                .unwrap()
                .as_str(),
            queued["entry_id"].as_str().unwrap()
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(100), request(&mut r.peer))
            .await
            .is_err(),
        "no steer may cross a revoked actual source"
    );
    notify_thread(
        &r.state.shared_codex_appserver,
        &mut r.peer,
        THREAD,
        "turn/completed",
        json!({"id":TURN,"status":"completed"}),
    )
    .await;
    if decision == Some("retry") {
        let mut observed_idle = false;
        let next = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let call = request(&mut r.peer).await;
                let result = match call["method"].as_str().unwrap() {
                    "config/read" => json!({"config":{}}),
                    "thread/read" => {
                        assert_eq!(call["params"]["threadId"], THREAD);
                        observed_idle = true;
                        json!({"thread":{"id":THREAD,"status":{"type":"idle"}}})
                    }
                    "turn/start" => break call,
                    method => panic!("unexpected explicit Retry native request: {method}"),
                };
                send(&mut r.peer, json!({"id":call["id"],"result":result})).await;
            }
        })
        .await
        .unwrap();
        assert!(
            observed_idle,
            "explicit Retry must confirm settlement of its original thread"
        );
        assert!(
            next["params"]["input"]
                .to_string()
                .contains("must never cross revoked source")
        );
        notify_thread(
            &r.state.shared_codex_appserver,
            &mut r.peer,
            THREAD,
            "turn/started",
            json!({"id":"retry-turn"}),
        )
        .await;
        send(
            &mut r.peer,
            json!({"id":next["id"],"result":{"turn":{"id":"retry-turn"}}}),
        )
        .await;
        fixture
            .wait_phase(&r.track, HarnessPhaseTag::TurnRunning)
            .await;
        notify_thread(
            &r.state.shared_codex_appserver,
            &mut r.peer,
            THREAD,
            "turn/completed",
            json!({"id":"retry-turn","status":"completed"}),
        )
        .await;
    }
    fixture
        .wait_phase(&r.track, HarnessPhaseTag::TurnCompleted)
        .await;
    r.peer.close(None).await.unwrap();
    r.state
        .harness
        .get(&r.worker)
        .unwrap()
        .shutdown()
        .await
        .unwrap();
}

struct Running {
    state: AppState,
    app: axum::Router,
    cookie: String,
    peer: crate::shared_codex_appserver::native_owner_tests::Peer,
    track: String,
    card: String,
    worker: String,
    auth: AuthState,
    client: Arc<crate::codex_appserver::CodexAppServer>,
}

async fn running(fixture: &Fixture) -> Running {
    let (state, app, cookie, auth) = fixture.application_with_auth(false).await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "queue-area",
        json!({"name":"Canonical controls","color":"#123456"}),
    )
    .await;
    let (client, mut peer) = connect(&state.shared_codex_appserver).await;
    let create = post(
        &app,
        &cookie,
        "/api/tracks",
        "queue-first",
        json!({
            "area_id":area["id"],"title":"","planner_provider":"codex",
            "model":"queue-model","reasoning_effort":"high",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY
        }),
    );
    let native = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let call = request(&mut peer).await;
            if call["method"] == "model/list" {
                send(&mut peer,json!({"id":call["id"],"error":{"code":-32603,"message":"catalog unavailable"}})).await;
                continue;
            }
            let result = match call["method"].as_str().unwrap() {
                "turn/start" => break call,
                "thread/start" => json!({"thread":{"id":THREAD}}),
                "config/read" => json!({"config":{}}),
                other => panic!("unexpected native setup: {other}"),
            };
            send(&mut peer, json!({"id":call["id"],"result":result})).await;
        }
    });
    let (created, start) = tokio::join!(create, native);
    let start = start.unwrap();
    let track = created["id"].as_str().unwrap().to_owned();
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
        json!({"id":start["id"],"result":{"turn":{"id":TURN}}}),
    )
    .await;
    fixture
        .wait_phase(&track, HarnessPhaseTag::TurnRunning)
        .await;
    let (card, worker, _) = fixture.carrier(&track).await;
    Running {
        auth,
        client,
        state,
        app,
        cookie,
        peer,
        track,
        card,
        worker,
    }
}

async fn control(
    app: &axum::Router,
    cookie: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn pending_controls_http_edit_withdraw_steer_and_end_preserve_original_execution() {
    edit_withdraw_steer_case(false).await;
}

#[tokio::test]
async fn pending_controls_disconnected_report_wakes_after_end_was_observed() {
    edit_withdraw_steer_case(true).await;
}

async fn edit_withdraw_steer_case(disconnected: bool) {
    let fixture = Fixture::new().await;
    let mut r = running(&fixture).await;
    let queued = post(
        &r.app,
        &r.cookie,
        &format!("/api/cards/{}/planner/input", r.card),
        "queued-edit",
        json!({"text":"queued original"}),
    )
    .await;
    let withdrawn = post(
        &r.app,
        &r.cookie,
        &format!("/api/cards/{}/planner/input", r.card),
        "queued-withdraw",
        json!({"text":"take this back"}),
    )
    .await;
    let message = queued["entry_id"].as_str().unwrap();
    let path = format!("/api/cards/{}/conversation/messages/{message}", r.card);
    let (status, edited) = control(
        &r.app,
        &r.cookie,
        "PATCH",
        &path,
        json!({"carrier_id":r.worker,"if_revision":1,"text":"edited instruction"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    assert_eq!(edited["rev"], 2);
    let (status, stale) = control(
        &r.app,
        &r.cookie,
        "DELETE",
        &path,
        json!({"carrier_id":r.worker,"if_revision":1}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(stale["code"], "planner_input_stale");
    assert_eq!(stale["text"], "edited instruction");
    let (status, _) = control(
        &r.app,
        &r.cookie,
        "DELETE",
        &format!(
            "/api/cards/{}/conversation/messages/{}",
            r.card,
            withdrawn["entry_id"].as_str().unwrap()
        ),
        json!({"carrier_id":r.worker,"if_revision":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let read = run(
        &r.app,
        &r.cookie,
        &format!("/api/cards/{}/planner/run?contract=conversation_v1", r.card),
    )
    .await;
    assert_eq!(read["pending"].as_array().unwrap().len(), 1);
    assert_eq!(read["pending"][0]["text"], "edited instruction");
    let (status, _) = control(
        &r.app,
        &r.cookie,
        "POST",
        &format!("{path}/steer"),
        json!({"carrier_id":r.worker,"if_revision":2,"turn_id":"stale-turn"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    use crate::test_seams::{PausePoint, install_pause_for_test};
    let harness = r.state.harness.get(&r.worker).unwrap();
    let quiet = PausePoint::default();
    install_pause_for_test("canonical-result-wait", &r.card, quiet.clone());
    harness.request_progress();
    tokio::time::timeout(Duration::from_secs(5), quiet.entered.notified())
        .await
        .unwrap();
    let end_observed = PausePoint::default();
    install_pause_for_test(
        "canonical-native-end-observed",
        &r.card,
        end_observed.clone(),
    );
    quiet.release.notify_one();
    let mut steering = tokio::spawn({
        let (app, cookie, worker) = (r.app.clone(), r.cookie.clone(), r.worker.clone());
        async move {
            control(
                &app,
                &cookie,
                "POST",
                &format!("{path}/steer"),
                json!({"carrier_id":worker,"if_revision":2,"turn_id":TURN}),
            )
            .await
        }
    });
    let native = tokio::select! {
        result=&mut steering=>panic!("steer never reached native transport: {result:?}"),
        call=tokio::time::timeout(Duration::from_secs(5),request(&mut r.peer))=>call.unwrap(),
    };
    assert_eq!(native["method"], "turn/steer");
    assert_eq!(native["params"]["expectedTurnId"], TURN);
    assert!(
        native["params"]["input"]
            .to_string()
            .contains("edited instruction")
    );
    if disconnected {
        steering.abort();
        assert!((&mut steering).await.unwrap_err().is_cancelled());
    }
    // END arrives first: it cannot consume the supplemental crossing marker.
    notify_thread(
        &r.state.shared_codex_appserver,
        &mut r.peer,
        THREAD,
        "turn/completed",
        json!({"id":TURN,"status":"completed"}),
    )
    .await;
    // Hold after the real END observation, while the supplemental crossing is
    // still unresolved. Its later committed report must supply its own wake.
    tokio::time::timeout(Duration::from_secs(5), end_observed.entered.notified())
        .await
        .unwrap();
    let crossing = fixture.carrier(&r.track).await.2;
    assert_eq!(
        crossing
            .canonical_inputs
            .active
            .as_ref()
            .unwrap()
            .submission_ids
            .len(),
        2
    );
    send(
        &mut r.peer,
        json!({"id":native["id"],"result":{"turnId":TURN}}),
    )
    .await;
    if disconnected {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let frame = fixture.carrier(&r.track).await.2;
                if frame
                    .canonical_inputs
                    .attempts
                    .iter()
                    .filter(|attempt| {
                        matches!(attempt.delivery, InputDeliveryFact::Accepted { .. })
                    })
                    .count()
                    == 2
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned report must commit after the HTTP request is dropped");
    } else {
        let (status, result) = steering.await.unwrap();
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(result["delivery"], "accepted");
    }
    end_observed.release.notify_one();
    let done = fixture
        .wait_phase(&r.track, HarnessPhaseTag::TurnCompleted)
        .await;
    assert!(done.canonical_inputs.active.is_none());
    assert!(done.canonical_inputs.pending.is_empty());
    let read = run(
        &r.app,
        &r.cookie,
        &format!("/api/cards/{}/planner/run?contract=conversation_v1", r.card),
    )
    .await;
    assert_eq!(read["terminal_outcome"]["status"], "completed");
    assert_eq!(read["pending"], json!([]));
    assert!(read["active_input"].is_null());
    r.peer.close(None).await.unwrap();
    r.state
        .harness
        .get(&r.worker)
        .unwrap()
        .shutdown()
        .await
        .unwrap();
}

#[tokio::test]
async fn pending_controls_unknown_supplement_survives_end_without_automatic_resend() {
    let fixture = Fixture::new().await;
    let mut r = running(&fixture).await;
    let queued = post(
        &r.app,
        &r.cookie,
        &format!("/api/cards/{}/planner/input", r.card),
        "queued-unknown",
        json!({"text":"must not be sent twice"}),
    )
    .await;
    let path = format!(
        "/api/cards/{}/conversation/messages/{}/steer",
        r.card,
        queued["entry_id"].as_str().unwrap()
    );
    let (status, _) = control(
        &r.app,
        "",
        "POST",
        &path,
        json!({"carrier_id":r.worker,"if_revision":1,"turn_id":TURN}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let mut steering = tokio::spawn({
        let (app, cookie, worker) = (r.app.clone(), r.cookie.clone(), r.worker.clone());
        async move {
            control(
                &app,
                &cookie,
                "POST",
                &path,
                json!({"carrier_id":worker,"if_revision":1,"turn_id":TURN}),
            )
            .await
        }
    });
    let native = tokio::select! {
        result=&mut steering=>panic!("steer never reached native transport: {result:?}"),
        call=tokio::time::timeout(Duration::from_secs(5),request(&mut r.peer))=>call.unwrap(),
    };
    send(
        &mut r.peer,
        json!({"id":native["id"],"result":{"turnId":"unrelated-turn"}}),
    )
    .await;
    let (status, result) = steering.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["delivery"], "unknown");
    let active_read = run(
        &r.app,
        &r.cookie,
        &format!("/api/cards/{}/planner/run?contract=conversation_v1", r.card),
    )
    .await;
    let supplemental = active_read["active_input"]["submissions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["submission_id"] == result["submission_id"])
        .unwrap();
    assert_eq!(supplemental["ordinal"], 1);
    let mut stopping = tokio::spawn({
        let (app, cookie, path) = (
            r.app.clone(),
            r.cookie.clone(),
            format!("/api/cards/{}/planner/interrupt", r.card),
        );
        async move { control(&app, &cookie, "POST", &path, json!({})).await }
    });
    let interrupt = tokio::select! {
        result=&mut stopping=>panic!("held supplemental blocked Stop: {result:?}"),
        call=tokio::time::timeout(Duration::from_secs(5),request(&mut r.peer))=>call.unwrap(),
    };
    assert_eq!(interrupt["method"], "turn/interrupt");
    send(&mut r.peer, json!({"id":interrupt["id"],"result":{}})).await;
    assert_eq!(stopping.await.unwrap().0, StatusCode::OK);
    notify_thread(
        &r.state.shared_codex_appserver,
        &mut r.peer,
        THREAD,
        "turn/completed",
        json!({"id":TURN,"status":"completed"}),
    )
    .await;
    let done = fixture
        .wait_phase(&r.track, HarnessPhaseTag::TurnCompleted)
        .await;
    assert!(done.canonical_inputs.pending.is_empty());
    let read = run(
        &r.app,
        &r.cookie,
        &format!("/api/cards/{}/planner/run?contract=conversation_v1", r.card),
    )
    .await;
    let held = read["active_input"]["submissions"].as_array().unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0]["submission_id"], result["submission_id"]);
    assert_eq!(held[0]["attempt"]["delivery"], "unknown");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), request(&mut r.peer))
            .await
            .is_err(),
        "uncertain supplemental input must not become a new turn"
    );
    r.peer.close(None).await.unwrap();
    r.state
        .harness
        .get(&r.worker)
        .unwrap()
        .shutdown()
        .await
        .unwrap();
}
