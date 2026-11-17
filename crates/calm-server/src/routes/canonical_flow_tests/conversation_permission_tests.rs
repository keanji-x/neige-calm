//! Approval choices through authenticated creation, the dedicated writer and native wire.
use super::*;
use crate::shared_codex_appserver::native_owner_tests::Peer;
use calm_truth::db::RepoRead;

pub(super) async fn set_mode(app: &axum::Router, cookie: &str, card: &str, mode: &str) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/cards/{card}/planner/permission-mode"))
                .header(header::COOKIE, cookie)
                .header("X-Calm-Actor", "user")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"permission_mode":mode}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

pub(super) async fn next_turn(peer: &mut Peer) -> Value {
    next_turn_from(peer, false).await
}

pub(super) async fn cold_next_turn(peer: &mut Peer) -> Value {
    next_turn_from(peer, true).await
}

async fn next_turn_from(peer: &mut Peer, cold: bool) -> Value {
    let mut loaded = !cold;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let call = request(peer).await;
            let result = match call["method"].as_str().unwrap() {
                "config/read" => json!({"config":{}}),
                "thread/start" | "thread/resume" => {
                    loaded = true;
                    json!({"thread":{"id":THREAD,"status":{"type":"idle"},"turns":[]}})
                },
                "thread/read" => {
                    json!({"thread":{"id":THREAD,"status":{"type":if loaded { "idle" } else { "notLoaded" }},"turns":[]}})
                }
                "turn/start" => break call,
                method => panic!("unexpected native request {method}"),
            };
            send(peer, json!({"id":call["id"],"result":result})).await;
        }
    })
    .await
    .unwrap()
}

pub(super) fn assert_wire(call: &Value, mode: &str) {
    assert_eq!(call["params"]["approvalsReviewer"], "user");
    assert_eq!(
        call["params"]["sandboxPolicy"]["type"],
        if mode == "full" {
            "dangerFullAccess"
        } else {
            "workspaceWrite"
        }
    );
    if mode == "ask" {
        assert_eq!(
            call["params"]["approvalPolicy"]["granular"]["sandbox_approval"],
            true
        );
    } else {
        assert_eq!(call["params"]["approvalPolicy"], "never");
    }
}

pub(super) async fn finish(
    fixture: &Fixture,
    state: &AppState,
    peer: &mut Peer,
    call: &Value,
    card: &str,
    turn: &str,
) {
    send(peer, json!({"id":call["id"],"result":{"turn":{"id":turn}}})).await;
    notify_thread(
        &state.shared_codex_appserver,
        peer,
        THREAD,
        "turn/started",
        json!({"id":turn,"status":"inProgress"}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        peer,
        THREAD,
        "turn/completed",
        json!({"id":turn,"status":"completed"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (_, snapshot) = assistant_canonical::carrier(fixture, card).await;
            if snapshot.header.phase == HarnessPhaseTag::TurnCompleted {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn explicit_first_and_next(assistant: bool) {
    for mode in ["never", "ask", "full"] {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let (client, mut peer) = connect(&state.shared_codex_appserver).await;
        let (card, uri, body) = if assistant {
            let parent = assistant_canonical::track(&app, &cookie).await;
            let planner = fixture.carrier(&parent).await.0;
            set_mode(&app, &cookie, &planner, "full").await;
            let uri = format!("/api/tracks/{parent}/conversations");
            let body = json!({"provider":"codex","text":FIRST_BODY,"permission_mode":mode});
            let created = post(&app, &cookie, &uri, "permission-first", body.clone()).await;
            (created["id"].as_str().unwrap().to_owned(), uri, body)
        } else {
            let area = post(
                &app,
                &cookie,
                "/api/areas",
                "area",
                json!({"name":"Permissions","color":"#123456"}),
            )
            .await;
            let body = json!({"area_id":area["id"],"planner_provider":"codex","permission_mode":mode,"theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY});
            let created = post(
                &app,
                &cookie,
                "/api/tracks",
                "permission-first",
                body.clone(),
            )
            .await;
            (
                fixture.carrier(created["id"].as_str().unwrap()).await.0,
                "/api/tracks".to_owned(),
                body,
            )
        };
        let call = next_turn(&mut peer).await;
        assert_wire(&call, mode);
        let saved_card = fixture.repo.card_get(&card).await.unwrap().unwrap();
        assert_eq!(saved_card.payload["permission_mode"], mode);
        let added: String = sqlx::query_scalar("SELECT payload FROM events WHERE kind='card.added' AND json_extract(payload,'$.id')=?1")
            .bind(&card).fetch_one(fixture.repo.pool()).await.unwrap();
        let added: Value = serde_json::from_str(&added).unwrap();
        assert_eq!(added["payload"]["permission_mode"], mode);
        let (_, frame) = assistant_canonical::carrier(&fixture, &card).await;
        let invocation = &frame.canonical_inputs.attempts[0].invocation;
        let calm_types::conversation::InputInvocation::Recorded { configuration } = invocation
        else {
            panic!("frozen invocation required");
        };
        assert_eq!(
            serde_json::to_value(configuration.permission).unwrap(),
            mode
        );
        // PUT during the running turn changes only the next issuance.
        set_mode(&app, &cookie, &card, "never").await;
        let replay = post(&app, &cookie, &uri, "permission-first", body.clone()).await;
        let mut changed = body;
        changed["permission_mode"] = json!(if mode == "ask" { "full" } else { "ask" });
        assert_eq!(
            status(&app, &cookie, &uri, "permission-first", changed).await,
            StatusCode::CONFLICT
        );
        assert!(replay["id"].is_string());
        let saved: String = sqlx::query_scalar(
            "SELECT json_extract(payload,'$.permission_mode') FROM cards WHERE id=?1",
        )
        .bind(&card)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
        assert_eq!(saved, "never", "creation replay cannot undo the PUT");
        finish(
            &fixture,
            &state,
            &mut peer,
            &call,
            &card,
            "first-permission-turn",
        )
        .await;
        post(
            &app,
            &cookie,
            &format!("/api/cards/{card}/planner/input"),
            "next-input",
            json!({"text":"Next issuance"}),
        )
        .await;
        let next = next_turn(&mut peer).await;
        assert_wire(&next, "never");
        finish(
            &fixture,
            &state,
            &mut peer,
            &next,
            &card,
            "next-permission-turn",
        )
        .await;
        drop(client);
        fixture.shutdown_process(&state).await;
    }
}

#[tokio::test]
async fn conversation_permission_assistant_explicit_first_turn_and_next_turn() {
    explicit_first_and_next(true).await;
}

#[tokio::test]
async fn conversation_permission_track_explicit_first_turn_and_next_turn() {
    explicit_first_and_next(false).await;
}

#[tokio::test]
async fn conversation_permission_assistant_malformed_choice_retains_queued_input() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let parent = assistant_canonical::track(&app, &cookie).await;
    let (client, mut peer) = connect(&state.shared_codex_appserver).await;
    let created = post(
        &app,
        &cookie,
        &format!("/api/tracks/{parent}/conversations"),
        "malformed-first",
        json!({"provider":"codex","permission_mode":"never","text":FIRST_BODY}),
    )
    .await;
    let card = created["id"].as_str().unwrap();
    let call = next_turn(&mut peer).await;
    finish(
        &fixture,
        &state,
        &mut peer,
        &call,
        card,
        "before-corruption",
    )
    .await;
    fixture
        .repo
        .db()
        .writer()
        .execute(
            sqlx::query(
                "UPDATE cards SET payload=json_set(payload,'$.permission_mode','Full') WHERE id=?1",
            )
            .bind(card),
        )
        .await
        .unwrap();
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "malformed-next",
        json!({"text":"Keep this queued"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let run = app
                .clone()
                .oneshot(
                    Request::get(format!(
                        "/api/cards/{card}/planner/run?contract=conversation_v1"
                    ))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
                )
                .await
                .unwrap();
            let run: Value =
                serde_json::from_slice(&run.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            if run["blocked_reason"].is_string() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (_, frame) = assistant_canonical::carrier(&fixture, card).await;
    assert_eq!(frame.canonical_inputs.pending.len(), 1);
    assert_eq!(frame.canonical_inputs.attempts.len(), 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), request(&mut peer))
            .await
            .is_err()
    );
    drop(client);
    fixture.shutdown_process(&state).await;
}

pub(super) async fn assert_first_native_admission(
    fixture: &Fixture,
    app: &axum::Router,
    cookie: &str,
    card: &str,
) {
    let frame =
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let (_, frame) = assistant_canonical::carrier(fixture, card).await;
                if frame.canonical_inputs.attempts.iter().any(|attempt| {
                    matches!(attempt.delivery, InputDeliveryFact::NotDispatched { .. })
                }) || (!frame.canonical_inputs.attempts.is_empty() && frame.idle_for_lifecycle())
                {
                    break frame;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("first native admission must settle");
    if frame
        .canonical_inputs
        .attempts
        .iter()
        .any(|attempt| matches!(attempt.delivery, InputDeliveryFact::NotDispatched { .. }))
    {
        let run = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/cards/{card}/planner/run?contract=conversation_v1"
                ))
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        let run: Value =
            serde_json::from_slice(&run.into_body().collect().await.unwrap().to_bytes()).unwrap();
        panic!(
            "native fixture preflight refused: {}",
            run["blocked_reason"]
        );
    }
}
