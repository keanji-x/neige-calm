//! Canonical input feedback has one typed source, independent of provider and execution admission.
use super::assistant_canonical::track;
use super::claude_creation_tests::assistant::{configure, wait_frame};
use super::*;

async fn run(app: &axum::Router, cookie: &str, uri: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let response = app
                .clone()
                .oneshot(
                    Request::get(uri)
                        .header(header::COOKIE, cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            // The read contract rejects a snapshot whose runtime fence changed mid-read.
            if status == StatusCode::CONFLICT
                && value["code"] == "conflict"
                && value["error"] == "conversation runtime changed while reading"
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
            assert_eq!(status, StatusCode::OK, "{value}");
            return value;
        }
    })
    .await
    .expect("the run projection must reach a stable runtime fence")
}

async fn wait_run(
    app: &axum::Router,
    cookie: &str,
    card: &str,
    predicate: impl Fn(&Value) -> bool,
) -> Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let value = run(
                app,
                cookie,
                &format!("/api/cards/{card}/planner/run?contract=conversation_v1"),
            )
            .await;
            if predicate(&value) {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("production run projection must reach the native boundary")
}

#[tokio::test]
async fn claude_running_input_has_one_typed_delivery_feedback_source() {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture, "hold-input-replay");
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let created = post(
        &app,
        &cookie,
        &format!("/api/tracks/{track}/conversations"),
        "awaiting-native-ack",
        json!({"provider":"claude","text":FIRST_BODY}),
    )
    .await;
    let card = created["id"].as_str().unwrap();
    wait_frame(&fixture, card, |frame| {
        frame.header.phase == HarnessPhaseTag::TurnRunning
            && matches!(
                frame.canonical_inputs.attempts.first().map(|a| &a.delivery),
                Some(InputDeliveryFact::Unknown)
            )
    })
    .await;
    let running = run(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/run?contract=conversation_v1"),
    )
    .await;
    std::fs::write(fixture.root.path().join("fake/release-replay"), "release").unwrap();
    wait_frame(&fixture, card, |frame| {
        frame.idle_for_lifecycle()
            && matches!(
                frame.canonical_inputs.attempts.first().map(|a| &a.delivery),
                Some(InputDeliveryFact::Accepted { .. })
            )
    })
    .await;
    fixture.shutdown_process(&state).await;
    assert_eq!(running["phase"], "turn_running");
    assert_eq!(
        running["active_input"]["submissions"][0]["attempt"]["delivery"],
        "unknown"
    );
    assert!(running["active_input"]["current_execution_id"].is_string());
    assert!(
        running["blocked_reason"].is_null(),
        "delivery feedback must come from active_input, not a duplicate blocking string: {running}"
    );
}

#[tokio::test]
async fn canonical_saved_queue_has_no_duplicate_execution_error_for_all_providers() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    for provider in ["codex", "claude", "opencode", "grok"] {
        let created = post(
            &app,
            &cookie,
            &format!("/api/tracks/{track}/conversations"),
            provider,
            json!({"provider":provider,"text":FIRST_BODY}),
        )
        .await;
        let card = created["id"].as_str().unwrap();
        let value = run(
            &app,
            &cookie,
            &format!("/api/cards/{card}/planner/run?contract=conversation_v1"),
        )
        .await;
        assert_eq!(
            value["pending_inputs"].as_array().unwrap().len(),
            1,
            "{provider}: {value}"
        );
        assert_eq!(value["pending_inputs"][0]["kind"], "human");
        assert!(value["active_input"].is_null());
        assert!(
            value["blocked_reason"].is_null(),
            "{provider}: a saved queue is not an execution error: {value}"
        );
    }
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn canonical_ended_unknown_input_retains_typed_warning_and_human_action() {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture, "no-input-replay");
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let created = post(
        &app,
        &cookie,
        &format!("/api/tracks/{track}/conversations"),
        "unconfirmed",
        json!({"provider":"claude","text":FIRST_BODY}),
    )
    .await;
    let card = created["id"].as_str().unwrap();
    let value = wait_run(&app, &cookie, card, |r| r["phase"] == "turn_completed").await;
    fixture.shutdown_process(&state).await;
    assert!(value["active_input"]["current_execution_id"].is_null());
    assert_eq!(
        value["active_input"]["submissions"][0]["attempt"]["delivery"],
        "unknown"
    );
    assert_eq!(
        value["active_input"]["submissions"][0]["attempt"]["actions"]["keep"]["kind"],
        "available"
    );
    assert!(
        value["blocked_reason"].is_null(),
        "the typed held input owns this warning: {value}"
    );
}

#[tokio::test]
async fn codex_start_receipt_has_one_typed_delivery_feedback_source() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Input feedback","color":"#123456"}),
    )
    .await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let create = post(
        &app,
        &cookie,
        "/api/tracks",
        "create",
        json!({"area_id":area["id"],"title":"",
        "planner_provider":"codex","theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY}),
    );
    let native = super::native_attempt_recovery_tests::initial_request(&mut peer);
    let (created, initial) = tokio::join!(create, native);
    let (card, _, _) = fixture.carrier(created["id"].as_str().unwrap()).await;
    let pending = wait_run(&app, &cookie, &card, |r| {
        r["active_input"]["submissions"][0]["attempt"]["delivery"] == "crossing_unknown"
    })
    .await;
    send(
        &mut peer,
        json!({"id":initial["id"],"result":{"turn":{"id":TURN}}}),
    )
    .await;
    let accepted = wait_run(&app, &cookie, &card, |r| {
        r["active_input"]["submissions"][0]["attempt"]["delivery"] == "accepted"
    })
    .await;
    send(&mut peer, json!({"method":"turn/completed","params":{"threadId":THREAD,"turn":{"id":TURN,"status":"completed","error":null}}})).await;
    wait_run(&app, &cookie, &card, |r| r["phase"] == "turn_completed").await;
    fixture.shutdown_process(&state).await;
    assert!(
        pending["blocked_reason"].is_null(),
        "in-flight RPC is not held delivery: {pending}"
    );
    assert!(accepted["blocked_reason"].is_null());
}

#[tokio::test]
async fn opencode_running_input_has_one_typed_delivery_feedback_source() {
    acp_running_input_has_one_typed_delivery_feedback_source(
        calm_types::runtime::AgentProvider::OpenCode,
    )
    .await;
}

#[tokio::test]
async fn grok_running_input_has_one_typed_delivery_feedback_source() {
    acp_running_input_has_one_typed_delivery_feedback_source(
        calm_types::runtime::AgentProvider::Grok,
    )
    .await;
}

async fn acp_running_input_has_one_typed_delivery_feedback_source(
    provider: calm_types::runtime::AgentProvider,
) {
    let mut fixture = Fixture::new().await;
    super::opencode_creation_tests::configure(&mut fixture);
    std::fs::write(
        super::opencode_creation_tests::peer_root(&fixture, &provider).join("scenario"),
        "hold",
    )
    .unwrap();
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let created = post(
        &app,
        &cookie,
        &format!("/api/tracks/{track}/conversations"),
        "native-pending",
        json!({"provider":provider.wire_name(),"text":FIRST_BODY}),
    )
    .await;
    let card = created["id"].as_str().unwrap();
    let pending = wait_run(&app, &cookie, card, |r| {
        r["phase"] == "turn_running"
            && r["active_input"]["submissions"][0]["attempt"]["delivery"] == "unknown"
    })
    .await;
    fixture.shutdown_process(&state).await;
    assert!(pending["active_input"]["current_execution_id"].is_string());
    assert!(
        pending["recovery"].is_null(),
        "live prompt needs no recovery: {pending}"
    );
    assert!(
        pending["blocked_reason"].is_null(),
        "native prompt is running and awaiting its receipt: {pending}"
    );
}
