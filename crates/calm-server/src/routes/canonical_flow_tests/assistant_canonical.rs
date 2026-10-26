#![cfg(test)]
//! Authenticated production HTTP and a fake native protocol peer only.
use super::*;
use calm_truth::db::RepoRead;

pub(super) async fn track(app: &axum::Router, cookie: &str) -> String {
    let area = post(
        app,
        cookie,
        "/api/areas",
        "area",
        json!({"name":"Assistant canonical","color":"#123456"}),
    )
    .await;
    let created = post(
        app,
        cookie,
        "/api/tracks",
        "track",
        json!({
            "area_id":area["id"],"title":"Assistant parent","planner_provider":"codex",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]}
        }),
    )
    .await;
    created["id"].as_str().unwrap().to_owned()
}

pub(super) async fn carrier(fixture: &Fixture, card: &str) -> (String, CanonicalHarnessSnapshot) {
    let (worker, snapshot): (String, String) = sqlx::query_as(
        "SELECT ws.id,ws.handle_state_json FROM cards c JOIN worker_sessions ws ON c.session_id=ws.id WHERE c.id=?1",
    ).bind(card).fetch_one(fixture.repo.pool()).await.unwrap();
    let value: Value = serde_json::from_str(&snapshot).unwrap();
    assert!(value.get("pending_queue").is_none(), "no V1 queue write");
    (worker, serde_json::from_value(value).unwrap())
}

pub(super) async fn initial(
    app: &axum::Router,
    cookie: &str,
    track: &str,
    card: &str,
    key: &str,
) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/tracks/{track}/conversations/{card}/initial-input"
                ))
                .header(header::COOKIE, cookie)
                .header("Idempotency-Key", key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
async fn outage_replay_preserves_original_ack_without_resend() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let uri = format!("/api/tracks/{track}/conversations");
    let body = json!({"provider":"codex","text":FIRST_BODY});
    let created = post(&app, &cookie, &uri, "assistant", body.clone()).await;
    let card = created["id"].as_str().unwrap();
    let (worker, before) = carrier(&fixture, card).await;
    assert_eq!(before.canonical_inputs.pending.len(), 1);
    assert_eq!(
        before.canonical_inputs.pending[0]
            .accepted_input_ref()
            .unwrap(),
        calm_types::conversation::AcceptedInputRef::Human {
            message: calm_types::conversation::MessageRevisionRef {
                message_id: calm_types::conversation::ConversationMessageId::parse(
                    created["initialInput"]["receipt"]["entryId"]
                        .as_str()
                        .unwrap()
                )
                .unwrap(),
                revision: std::num::NonZeroU32::new(1).unwrap(),
            }
        }
    );
    assert_initial_carrier(&created["initialInput"]["receipt"], &worker);
    assert!(before.canonical_inputs.attempts.is_empty());
    // Let the actual post-commit wake finish with the backend down.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let down = carrier(&fixture, card).await;
    assert_eq!(down.0, worker);
    assert_eq!(down.1.canonical_inputs, before.canonical_inputs);
    let (client, mut peer) = connect(&state.shared_codex_appserver).await;
    for _ in 0..2 {
        let replay = post(&app, &cookie, &uri, "assistant", body.clone()).await;
        assert_eq!(replay["id"], created["id"]);
        assert_eq!(replay["initialInput"], created["initialInput"]);
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(150), request(&mut peer))
            .await
            .is_err(),
        "receipt replay must not start native work"
    );
    assert_eq!(
        carrier(&fixture, card).await.1.canonical_inputs,
        before.canonical_inputs
    );
    let observed = initial(&app, &cookie, &track, card, "assistant").await;
    assert_eq!(observed["receipt"], created["initialInput"]["receipt"]);
    assert_eq!(fixture.counts(card).await, (1, 1, 1, 0, 0));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .header("Idempotency-Key", "assistant")
                .body(Body::from(
                    json!({"provider":"codex","text":"changed"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    drop(client);
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn side_context_activation_uses_assistant_policy_and_original_ack_after_crossing() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let planner: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(&track)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    let (client, mut peer) = connect(&state.shared_codex_appserver).await;
    let uri = format!("/api/tracks/{track}/conversations");
    let body = json!({"provider":"codex","text":FIRST_BODY,"side":{"source_card_id":planner,"context":"Frozen side excerpt"}});
    let created = post(&app, &cookie, &uri, "side", body.clone()).await;
    let card = created["id"].as_str().unwrap();
    assert_eq!(created["sourceCardId"], planner);
    let start = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let call = request(&mut peer).await;
            match call["method"].as_str().unwrap() {
                "config/read" => {
                    send(&mut peer, json!({"id":call["id"],"result":{"config":{}}})).await
                }
                "thread/start" => {
                    let params = call["params"].to_string();
                    assert!(
                        params.contains("assistant"),
                        "Assistant profile instructions and MCP role: {params}"
                    );
                    assert!(!params.contains("neige_task_done"), "no worker tools");
                    let tools = &call["params"]["config"]["mcp_servers"]["neige"]["tools"];
                    assert!(
                        tools.get("neige_terminal_open").is_none(),
                        "Assistant must not gain Planner terminal creation"
                    );
                    assert!(params.contains("neige_terminal_control"));
                    assert!(params.contains("neige_terminal_input"));
                    send(
                        &mut peer,
                        json!({"id":call["id"],"result":{"thread":{"id":"assistant-thread"}}}),
                    )
                    .await;
                }
                "turn/start" => break call,
                method => panic!("unexpected activation call {method}: {call}"),
            }
        }
    })
    .await
    .expect("retained Assistant activates accepted input");
    let input = start["params"]["input"].to_string();
    assert!(
        input.contains("Frozen side excerpt"),
        "frozen context in the original render plan: {input}"
    );
    assert!(input.contains(FIRST_BODY));
    assert!(input.find("Frozen side excerpt").unwrap() < input.find(FIRST_BODY).unwrap());
    let before = carrier(&fixture, card).await;
    assert!(before.1.canonical_inputs.pending.is_empty());
    assert_eq!(before.1.canonical_inputs.attempts.len(), 1);
    // Lose the provider answer after crossing. A replay is acceptance evidence only.
    let replay = post(&app, &cookie, &uri, "side", body).await;
    assert_eq!(replay["initialInput"], created["initialInput"]);
    assert_eq!(
        initial(&app, &cookie, &track, card, "side").await["receipt"],
        created["initialInput"]["receipt"]
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(150), request(&mut peer))
            .await
            .is_err(),
        "no resend of an unresolved accepted turn"
    );
    assert_eq!(
        carrier(&fixture, card).await.1.canonical_inputs,
        before.1.canonical_inputs
    );
    assert_eq!(fixture.counts(card).await, (1, 1, 1, 1, 0));
    drop(client);
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn receipt_failure_rolls_back_card_carrier_and_input() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let card = crate::conversation_keys::derive_track_conversation_keys(&track, "rollback").card_id;
    sqlx::query("CREATE TRIGGER reject_assistant_receipt BEFORE INSERT ON conversation_assistant_creations BEGIN SELECT RAISE(ABORT,'fixture receipt rejection'); END")
        .execute(fixture.repo.pool()).await.unwrap();
    let uri = format!("/api/tracks/{track}/conversations");
    let request = |cookie: Option<&str>| {
        let mut request = Request::builder()
            .method("POST")
            .uri(&uri)
            .header(header::CONTENT_TYPE, "application/json")
            .header("Idempotency-Key", "rollback");
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        request
            .body(Body::from(
                json!({"provider":"codex","text":FIRST_BODY}).to_string(),
            ))
            .unwrap()
    };
    assert_eq!(
        app.clone().oneshot(request(None)).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.clone()
            .oneshot(request(Some(&cookie)))
            .await
            .unwrap()
            .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(fixture.repo.card_get(&card).await.unwrap().is_none());
    assert_eq!(fixture.counts(&card).await, (0, 0, 0, 0, 0));
    sqlx::query("DROP TRIGGER reject_assistant_receipt")
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    let created = post(
        &app,
        &cookie,
        &uri,
        "rollback",
        json!({"provider":"codex","text":FIRST_BODY}),
    )
    .await;
    assert_eq!(created["id"], card);
    for sql in [
        "UPDATE conversation_assistant_creations SET request_sha256=request_sha256",
        "DELETE FROM conversation_assistant_creations",
    ] {
        assert!(
            sqlx::query(sql).execute(fixture.repo.pool()).await.is_err(),
            "original receipt is immutable: {sql}"
        );
    }
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn followup_binds_attachment_before_canonical_acceptance() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let created = post(
        &app,
        &cookie,
        &format!("/api/tracks/{track}/conversations"),
        "image",
        json!({"provider":"codex","text":FIRST_BODY}),
    )
    .await;
    let card = created["id"].as_str().unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{card}/planner/attachments"))
                .header(header::COOKIE, &cookie)
                .body(Body::from(b"\x89PNG\r\n\x1a\nfixture bytes".to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let upload: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(status, StatusCode::CREATED, "{upload}");
    let sent = post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "image-input",
        json!({"text":"See this image","attachments":[upload["attachmentId"]]}),
    )
    .await;
    let message =
        calm_types::conversation::ConversationMessageId::parse(sent["entry_id"].as_str().unwrap())
            .unwrap();
    let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
        .await
        .unwrap();
    let revision = crate::db::sqlite::conversation::revision_tx(
        &mut tx,
        &calm_types::conversation::MessageRevisionRef {
            message_id: message,
            revision: std::num::NonZeroU32::new(1).unwrap(),
        },
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    let calm_types::conversation::PrivateHistoryPayload::Present { value } = revision.payload
    else {
        panic!("accepted image payload")
    };
    assert_eq!(value.bound_attachments.len(), 1);
    assert!(std::path::Path::new(&value.bound_attachments[0].path).is_file());
    assert!(value.bound_attachments[0].path.contains("/bound/"));
    let replay = post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "image-input",
        json!({"text":"See this image","attachments":[upload["attachmentId"]]}),
    )
    .await;
    assert_eq!(replay, sent);
    assert_eq!(fixture.counts(card).await, (2, 2, 1, 0, 0));
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn historical_create_key_is_factual_replay_without_canonical_adoption() {
    use crate::operation::planner_harness_start_adapter::OpeningBriefing;
    use crate::routes::track_conversations::{
        AssistantCreationEntry, AssistantProviderRequest, NewTrackConversationBody,
    };
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application_with_historical_native(true).await;
    let track = track(&app, &cookie).await;
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("Idempotency-Key", "historical".parse().unwrap());
    let (_, crate::extract::Json(original)) =
        crate::routes::track_conversations::create_track_conversation_inner(
            crate::state::RouteState::from_ref(&state),
            crate::state::WorkerState::from_ref(&state),
            crate::actor::Actor::server_send(),
            headers,
            track.clone(),
            NewTrackConversationBody {
                provider: AssistantProviderRequest::LegacyCodex,
                side: None,
                text: FIRST_BODY.into(),
                model: None,
                reasoning_effort: None,
            },
            OpeningBriefing::TodaysActivityOnTheLaunchpad,
            AssistantCreationEntry::InternalHistorical,
        )
        .await
        .unwrap();
    let original = serde_json::to_value(original).unwrap();
    let replay = post(
        &app,
        &cookie,
        &format!("/api/tracks/{track}/conversations"),
        "historical",
        json!({"text":FIRST_BODY}),
    )
    .await;
    assert_eq!(replay["id"], original["id"]);
    assert_eq!(replay["initialInput"], original["initialInput"]);
    assert_eq!(
        fixture.counts(original["id"].as_str().unwrap()).await,
        (0, 0, 1, 0, 0)
    );
    fixture.shutdown_process(&state).await;
}

pub(super) async fn complete_assistant_turn(
    fixture: &Fixture,
    state: &AppState,
    peer: &mut crate::shared_codex_appserver::native_owner_tests::Peer,
    card: &str,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let call = request(peer).await;
            let result = match call["method"].as_str().unwrap() {
                "config/read" => json!({"config":{}}),
                "thread/start" => json!({"thread":{"id":THREAD}}),
                "turn/start" => {
                    send(peer, json!({"id":call["id"],"result":{"turn":{"id":TURN}}})).await;
                    notify_thread(
                        &state.shared_codex_appserver,
                        peer,
                        THREAD,
                        "turn/started",
                        json!({"id":TURN,"status":"inProgress"}),
                    )
                    .await;
                    notify_thread(
                        &state.shared_codex_appserver,
                        peer,
                        THREAD,
                        "turn/completed",
                        json!({"id":TURN,"status":"completed"}),
                    )
                    .await;
                    break;
                }
                method => panic!("unexpected activation request {method}"),
            };
            send(peer, json!({"id":call["id"],"result":result})).await;
        }
        loop {
            let (_, snapshot) = carrier(fixture, card).await;
            if snapshot.idle_for_lifecycle() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Assistant must complete its real native turn");
}

#[tokio::test]
async fn archived_assistant_refuses_fresh_input_without_changing_history() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let (client, mut peer) = connect(&state.shared_codex_appserver).await;
    let created = post(
        &app,
        &cookie,
        &format!("/api/tracks/{track}/conversations"),
        "archive-create",
        json!({"provider":"codex","text":FIRST_BODY}),
    )
    .await;
    let card = created["id"].as_str().unwrap();
    complete_assistant_turn(&fixture, &state, &mut peer, card).await;
    let before = carrier(&fixture, card).await;
    let counts = fixture.counts(card).await;
    let archive_uri = format!("/api/tracks/{track}/conversations/{card}/archive");
    let response = app
        .clone()
        .oneshot(
            Request::patch(&archive_uri)
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"archived":true}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = app
        .clone()
        .oneshot(
            Request::post(format!("/api/cards/{card}/planner/input"))
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .header("Idempotency-Key", "archived-fresh-send")
                .body(Body::from(
                    json!({"text":"must not enter history"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(carrier(&fixture, card).await, before);
    assert_eq!(fixture.counts(card).await, counts);
    drop(client);
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn archive_rechecks_canonical_input_accepted_after_idle_check() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let (client, mut peer) = connect(&state.shared_codex_appserver).await;
    let created = post(
        &app,
        &cookie,
        &format!("/api/tracks/{track}/conversations"),
        "archive-race-create",
        json!({"provider":"codex","text":FIRST_BODY}),
    )
    .await;
    let card = created["id"].as_str().unwrap();
    complete_assistant_turn(&fixture, &state, &mut peer, card).await;
    let pause = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "conversation-archive-before-commit",
        card,
        pause.clone(),
    );
    let archive = tokio::spawn({
        let app = app.clone();
        let cookie = cookie.clone();
        let uri = format!("/api/tracks/{track}/conversations/{card}/archive");
        async move {
            app.oneshot(
                Request::patch(uri)
                    .header(header::COOKIE, cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({"archived":true}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    });
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .expect("archive passes its early idle check");
    let accepted = post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "archive-race-send",
        json!({"text":"accepted while archive is paused"}),
    )
    .await;
    let pending = carrier(&fixture, card).await.1;
    assert_eq!(pending.canonical_inputs.pending.len(), 1);
    assert!(!pending.idle_for_lifecycle());
    assert!(accepted["entry_id"].is_string());
    pause.release.notify_one();
    let response = tokio::time::timeout(Duration::from_secs(5), archive)
        .await
        .expect("archive resumes")
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let error: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(error.to_string().contains(
        "Wait for the reply and queued messages to finish before archiving this conversation."
    ));
    let archive_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM assistant_conversation_archives WHERE card_id=?1")
            .bind(card)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(archive_rows, 0);
    drop(client);
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn archived_assistant_refuses_retry_without_changing_history() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let (client, mut peer) = connect(&state.shared_codex_appserver).await;
    let create_uri = format!("/api/tracks/{track}/conversations");
    let body = json!({"provider":"codex","text":FIRST_BODY,"model":"retry-model","reasoning_effort":"high"});
    let create = post(
        &app,
        &cookie,
        &create_uri,
        "archive-retry-create",
        body.clone(),
    );
    let native = super::native_attempt_recovery_tests::initial_request(&mut peer);
    let (created, initial) = tokio::join!(create, native);
    let card = created["id"].as_str().unwrap();
    send(&mut peer, json!({"id":initial["id"],"result":{"turn":{}}})).await;
    let (worker, mut held) = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let facts = carrier(&fixture, card).await;
            if facts
                .1
                .canonical_inputs
                .attempts
                .first()
                .is_some_and(|attempt| matches!(attempt.delivery, InputDeliveryFact::Unknown))
            {
                break facts;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Retain the real frozen invocation and Unknown delivery, with execution
    // settled: a held historical attempt remains retryable on an idle carrier.
    state
        .harness
        .remove(&worker)
        .unwrap()
        .pause_issuance_for_dev();
    held.header.phase = crate::harness::HarnessPhaseTag::Idle;
    held.header.wedged_reason = None;
    held.canonical_inputs.active = None;
    assert!(held.idle_for_lifecycle());
    let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
        .await
        .unwrap();
    assert!(
        crate::db::sqlite::session_set_handle_state_tx(
            &mut tx,
            &worker,
            Some(serde_json::to_value(&held).unwrap()),
        )
        .await
        .unwrap()
    );
    crate::db::sqlite::session_set_harness_observation_runtime_tx(
        &mut tx,
        &worker,
        calm_types::worker::WorkerSessionState::Idle,
        held.header.last_thread_id.as_deref(),
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let attempt = &held.canonical_inputs.attempts[0];
    let args = json!({"carrier_id":worker,"submission_id":attempt.attempt.submission_id,"attempt_id":attempt.attempt.attempt_id,"revision":attempt.revision.to_string()});
    let archive = app
        .clone()
        .oneshot(
            Request::patch(format!("/api/tracks/{track}/conversations/{card}/archive"))
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"archived":true}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let archive_status = archive.status();
    let archive_body = archive.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(archive_status, StatusCode::NO_CONTENT, "{archive_body:?}");
    let before = carrier(&fixture, card).await;
    let counts = fixture.counts(card).await;
    let response = app
        .clone()
        .oneshot(
            Request::post(format!("/api/cards/{card}/conversation/input/retry"))
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .header("Idempotency-Key", "archived-retry")
                .body(Body::from(args.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let error: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let denied = (status, error);
    assert_eq!(denied.0, StatusCode::CONFLICT, "{denied:?}");
    assert!(
        denied
            .1
            .to_string()
            .contains("This conversation is archived. Restore it before continuing.")
    );
    assert_eq!(carrier(&fixture, card).await, before);
    assert_eq!(fixture.counts(card).await, counts);
    let relations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM conversation_input_retry_relations WHERE card_id=?1",
    )
    .bind(card)
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap();
    assert_eq!(relations, 0);
    let replay = post(&app, &cookie, &create_uri, "archive-retry-create", body).await;
    assert_eq!(replay["initialInput"], created["initialInput"]);
    drop(client);
    fixture.shutdown_process(&state).await;
}

pub(super) fn assert_initial_carrier(receipt: &Value, worker: &str) {
    assert_eq!(receipt["initialRuntimeId"], worker);
}
