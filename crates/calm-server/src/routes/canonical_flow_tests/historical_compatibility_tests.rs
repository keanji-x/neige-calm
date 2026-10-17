//! Released historical paths use the same public router and password issuer as
//! canonical creation. Neither an Actor nor a fixture grant authenticates them.
use super::*;

async fn area(app: &axum::Router, cookie: &str) -> Value {
    post(
        app,
        cookie,
        "/api/areas",
        "area",
        json!({"name":"Historical compatibility","color":"#123456"}),
    )
    .await
}

async fn shutdown(fixture: &Fixture, state: &AppState) {
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM worker_sessions")
        .fetch_all(fixture.repo.pool())
        .await
        .unwrap();
    for id in ids {
        if let Some(handle) = state.harness.remove(&id) {
            handle.shutdown().await.unwrap();
        }
    }
}

async fn status(app: &axum::Router, cookie: &str, uri: &str, key: &str, body: Value) -> StatusCode {
    app.clone()
        .oneshot(
            Request::post(uri)
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .header("Idempotency-Key", key)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn historical_template_first_message_keeps_its_original_creation_and_replay() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application_with_historical_native(true).await;
    let area = area(&app, &cookie).await;
    let request = json!({
        "area_id":area["id"], "planner_provider":"codex", "template_id":"small-change",
        "title":"", "theme":{"fg":[255,255,255],"bg":[0,0,0]},
        "first_message":"Preserve the released template first message"
    });
    let created = post(&app, &cookie, "/api/tracks", "template", request.clone()).await;
    let replay = post(&app, &cookie, "/api/tracks", "template", request.clone()).await;
    assert_eq!(created["id"], replay["id"]);
    let (_, offline_app, offline_cookie) = fixture.application().await;
    let offline_replay = post(
        &offline_app,
        &offline_cookie,
        "/api/tracks",
        "template",
        request,
    )
    .await;
    assert_eq!(
        created["id"], offline_replay["id"],
        "an original key does not require a new native start"
    );
    let changed = json!({
        "area_id":area["id"], "planner_provider":"codex", "template_id":"small-change",
        "title":"", "theme":{"fg":[255,255,255],"bg":[0,0,0]},
        "first_message":"A different original first message under the same key"
    });
    assert_eq!(
        status(
            &offline_app,
            &offline_cookie,
            "/api/tracks",
            "template",
            changed
        )
        .await,
        StatusCode::CONFLICT
    );
    let track = created["id"].as_str().unwrap();
    let (template,): (String,) = sqlx::query_as("SELECT template_id FROM tracks WHERE id=?1")
        .bind(track)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    assert_eq!(template, "small-change");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM conversation_messages")
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "historical input keeps its original contract and attribution"
    );
    shutdown(&fixture, &state).await;
}

async fn mutate(app: &axum::Router, cookie: &str, method: &str, uri: &str, body: Value) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(status.is_success(), "{method} {uri}: {status} {body}");
    body
}

#[tokio::test]
async fn historical_queue_edit_delete_steer_and_running_stop_remain_available() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application_with_historical_native(true).await;
    let area = area(&app, &cookie).await;
    let mut cards = vec![];
    for key in ["editable", "running"] {
        let created = post(
            &app,
            &cookie,
            "/api/tracks",
            key,
            json!({
                "area_id":area["id"],"planner_provider":"codex","title":"",
                "theme":{"fg":[255,255,255],"bg":[0,0,0]}
            }),
        )
        .await;
        let (card, worker): (String, String) =
            sqlx::query_as("SELECT id,session_id FROM cards WHERE track_id=?1 AND role='planner'")
                .bind(created["id"].as_str().unwrap())
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap();
        cards.push((card, state.harness.get(&worker).unwrap()));
    }
    let (editable, paused) = &cards[0];
    paused.pause_issuance_for_dev();
    let uri = format!("/api/cards/{editable}/planner/input");
    let input = post(
        &app,
        &cookie,
        &uri,
        "edit-me",
        json!({"text":"Original queued text"}),
    )
    .await;
    let entry = input["entry_id"].as_str().unwrap();
    let entry_uri = format!("{uri}/{entry}");
    let edited = mutate(
        &app,
        &cookie,
        "PATCH",
        &entry_uri,
        json!({"text":"Edited queued text","if_entry_rev":0}),
    )
    .await;
    assert_eq!(edited["entry_id"], entry);
    assert_eq!(edited["text"], "Edited queued text");
    assert_eq!(edited["rev"], 1);
    mutate(
        &app,
        &cookie,
        "DELETE",
        &entry_uri,
        json!({"if_entry_rev":1}),
    )
    .await;
    assert!(
        paused
            .snapshot()
            .await
            .unwrap()
            .pending_entries()
            .is_empty()
    );

    let (running, harness) = &cards[1];
    let uri = format!("/api/cards/{running}/planner/input");
    post(
        &app,
        &cookie,
        &uri,
        "start-turn",
        json!({"text":"Start the historical turn"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if harness.runtime_header().await.unwrap().phase == HarnessPhaseTag::TurnRunning {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the real historical driver starts its provider turn");
    let queued = post(
        &app,
        &cookie,
        &uri,
        "steer-me",
        json!({"text":"Supplement the running turn"}),
    )
    .await;
    let entry = queued["entry_id"].as_str().unwrap();
    let steered = post(
        &app,
        &cookie,
        &format!("{uri}/{entry}/steer"),
        "steer",
        json!({"if_entry_rev":0}),
    )
    .await;
    assert_eq!(steered["steered"], true);
    let stopped = post(
        &app,
        &cookie,
        &format!("/api/cards/{running}/planner/interrupt"),
        "stop",
        json!({}),
    )
    .await;
    assert_eq!(stopped["stopped"], true);
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn historical_public_send_keeps_its_v1_key_and_original_acknowledgement() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application_with_historical_native(true).await;
    let area = area(&app, &cookie).await;
    let created = post(
        &app,
        &cookie,
        "/api/tracks",
        "message-less",
        json!({
            "area_id":area["id"], "planner_provider":"codex", "title":"",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]}
        }),
    )
    .await;
    let card: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(created["id"].as_str().unwrap())
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    let uri = format!("/api/cards/{card}/planner/input");
    let input = json!({"text":"Keep this V1 ordinary input"});
    let accepted = post(&app, &cookie, &uri, "historical-send", input.clone()).await;
    let replay = post(&app, &cookie, &uri, "historical-send", input).await;
    assert_eq!(accepted, replay);
    let hash: String = sqlx::query_scalar("SELECT payload_hash FROM planner_input_idempotency WHERE card_id=?1 AND idempotency_key='historical-send'")
        .bind(&card).fetch_one(fixture.repo.pool()).await.unwrap();
    assert!(hash.starts_with("v1:"));
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn historical_daily_first_send_starts_and_replays_the_original_acknowledgement() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application_with_historical_native(true).await;
    let track = crate::daily_planner::reconcile(
        &axum::extract::FromRef::from_ref(&state),
        chrono::DateTime::parse_from_rfc3339("2026-10-04T01:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
    )
    .await
    .unwrap();
    let card: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(track.id.as_str())
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM worker_sessions WHERE card_id=?1")
        .bind(&card)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    assert_eq!(before, 0, "Daily starts model-free");
    let uri = format!("/api/cards/{card}/planner/input");
    let accepted = post(
        &app,
        &cookie,
        &uri,
        "daily-first",
        json!({"text":"Start today's original Daily Planner"}),
    )
    .await;
    let replay = post(
        &app,
        &cookie,
        &uri,
        "daily-first",
        json!({"text":"Start today's original Daily Planner"}),
    )
    .await;
    assert_eq!(accepted, replay);
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM worker_sessions WHERE card_id=?1")
        .bind(&card)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    assert_eq!(after, 1);
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn historical_recipe_and_single_field_fork_keep_their_starting_points() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application_with_historical_native(true).await;
    let area = area(&app, &cookie).await;
    let recipe = post(
        &app,
        &cookie,
        "/api/track-recipes",
        "recipe",
        json!({
            "title":"Original recipe", "body":"# Original recipe\n\nPreserve this recipe body."
        }),
    )
    .await;
    let recipe_track = post(
        &app,
        &cookie,
        "/api/tracks",
        "recipe-track",
        json!({
            "area_id":area["id"], "planner_provider":"codex", "title":"",
            "recipe_id":recipe["id"], "first_message":"Execute the original recipe",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]}
        }),
    )
    .await;
    let fork = post(
        &app,
        &cookie,
        "/api/tracks",
        "fork-track",
        json!({
            "area_id":area["id"], "planner_provider":"codex", "title":"",
            "fork_report_from":recipe_track["id"], "first_message":"Execute the original fork",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]}
        }),
    )
    .await;
    for track in [&recipe_track, &fork] {
        let report: String =
            sqlx::query_scalar("SELECT payload FROM cards WHERE track_id=?1 AND role='reportcard'")
                .bind(track["id"].as_str().unwrap())
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap();
        assert!(
            report.contains("Preserve this recipe body."),
            "starting point survives: {report}"
        );
    }
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn historical_admission_rejects_another_real_user_even_with_owner_profile_and_old_keys() {
    let fixture = Fixture::new().await;
    let (state, app, cookie, auth) = fixture.application_with_auth(true).await;
    let area = area(&app, &cookie).await;
    let create = json!({"area_id":area["id"],"planner_provider":"codex","template_id":"small-change",
        "title":"","theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":"Owner's historical input"});
    let created = post(
        &app,
        &cookie,
        "/api/tracks",
        "owned-history",
        create.clone(),
    )
    .await;
    let card: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(created["id"].as_str().unwrap())
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    let uri = format!("/api/cards/{card}/planner/input");
    let body = json!({"text":"Owner's original V1 keyed send"});
    post(&app, &cookie, &uri, "owned-send", body.clone()).await;
    let mut tx = fixture.repo.pool().begin().await.unwrap();
    crate::db::sqlite::area_membership::application_user_register(&mut tx, "alice", "Owner")
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO area_members(area_id,user_id,role,joined_at) VALUES(?1,'alice','member',0)",
    )
    .bind(area["id"].as_str().unwrap())
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let token = auth.sessions.create(
        crate::auth::SessionIdentity {
            user_id: "alice".into(),
            display_name: "Owner".into(),
            role: "owner".into(),
        },
        crate::auth::SessionAuthority::PasswordLogin,
    );
    let alice = format!("{}={token}", crate::auth::SESSION_COOKIE);
    for key in ["owned-history", "alice-fresh"] {
        assert_eq!(
            status(&app, &alice, "/api/tracks", key, create.clone()).await,
            StatusCode::FORBIDDEN
        );
    }
    for key in ["owned-send", "alice-fresh"] {
        assert_eq!(
            status(&app, &alice, &uri, key, body.clone()).await,
            StatusCode::FORBIDDEN
        );
    }
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn known_canonical_creation_with_missing_current_never_enters_historical_send_or_create() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = area(&app, &cookie).await;
    let create = json!({"area_id":area["id"],"planner_provider":"codex","title":"",
        "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":"Canonical original"});
    let created = post(&app, &cookie, "/api/tracks", "canonical", create.clone()).await;
    let card: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(created["id"].as_str().unwrap())
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    let original_runtime: String = sqlx::query_scalar("SELECT session_id FROM cards WHERE id=?1")
        .bind(&card)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?1")
        .bind(&card)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    let uri = format!("/api/cards/{card}/planner/input");
    assert_eq!(
        status(
            &app,
            &cookie,
            &uri,
            "no-downgrade",
            json!({"text":"Do not fall back"})
        )
        .await,
        StatusCode::CONFLICT
    );
    let mut changed = create;
    changed["template_id"] = json!("small-change");
    assert_eq!(
        status(&app, &cookie, "/api/tracks", "canonical", changed).await,
        StatusCode::CONFLICT
    );
    let bindings: i64 =
        sqlx::query_scalar("SELECT count(*) FROM planner_input_idempotency WHERE card_id=?1")
            .bind(&card)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(bindings, 0);
    sqlx::query("UPDATE cards SET session_id=?2 WHERE id=?1")
        .bind(&card)
        .bind(&original_runtime)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET handle_state_json=?2 WHERE id=?1")
        .bind(&original_runtime)
        .bind(r#"{"mode":"harness","schema_version":2,"canonical_inputs":false}"#)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    assert!(
        status(
            &app,
            &cookie,
            &uri,
            "malformed-current",
            json!({"text":"Never reinterpret malformed V2"})
        )
        .await
        .is_server_error()
    );
    let bindings: i64 =
        sqlx::query_scalar("SELECT count(*) FROM planner_input_idempotency WHERE card_id=?1")
            .bind(&card)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        bindings, 0,
        "a known V2 decoding failure never reaches the V1 queue"
    );
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn canonical_collaborator_content_is_accepted_independently_of_private_execution() {
    let fixture = Fixture::new().await;
    let (state, app, cookie, auth) = fixture.application_with_auth(true).await;
    let area = area(&app, &cookie).await;
    let area_id = area["id"].as_str().unwrap();
    let mut tx = fixture.repo.pool().begin().await.unwrap();
    crate::db::sqlite::area_membership::application_user_register(&mut tx, "alice", "Alice")
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO area_members(area_id,user_id,role,joined_at) VALUES(?1,'alice','member',0)",
    )
    .bind(area_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let token = auth.sessions.create(
        crate::auth::SessionIdentity {
            user_id: "alice".into(),
            display_name: "Alice".into(),
            role: "member".into(),
        },
        crate::auth::SessionAuthority::PasswordLogin,
    );
    let alice = format!("{}={token}", crate::auth::SESSION_COOKIE);
    for (key, history) in [
        ("alice-private", "private"),
        ("alice-shared", "shared_history"),
    ] {
        sqlx::query("UPDATE areas SET conversation_history_contract=?2 WHERE id=?1")
            .bind(area_id)
            .bind(history)
            .execute(fixture.repo.pool())
            .await
            .unwrap();
        let body = json!({"area_id":area_id,"planner_provider":"codex","title":"",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":"Alice may save content without a private execution grant"});
        let created = post(&app, &alice, "/api/tracks", key, body.clone()).await;
        let replayed = post(&app, &alice, "/api/tracks", key, body).await;
        assert_eq!(created["id"], replayed["id"]);
        let mut tx = fixture.repo.pool().begin().await.unwrap();
        let receipt =
            crate::db::sqlite::conversation::human_track_creation_get_tx(&mut tx, area_id, key)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(receipt.source_user_id, "alice");
        tx.rollback().await.unwrap();
        let (thread,): (Option<String>,) = sqlx::query_as("SELECT ws.thread_id FROM worker_sessions ws JOIN cards c ON c.session_id=ws.id WHERE c.track_id=?1 AND c.role='planner'")
            .bind(created["id"].as_str().unwrap()).fetch_one(fixture.repo.pool()).await.unwrap();
        assert_eq!(
            thread, None,
            "saving content does not grant use of deployment credentials"
        );
    }
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn historical_daily_start_rechecks_its_real_source_after_the_start_wait() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application_with_historical_native(true).await;
    let track = crate::daily_planner::reconcile(
        &axum::extract::FromRef::from_ref(&state),
        chrono::DateTime::parse_from_rfc3339("2026-10-04T01:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
    )
    .await
    .unwrap();
    let card: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(track.id.as_str())
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    let pause = crate::test_seams::PausePoint {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    crate::test_seams::install_pause_for_test(
        crate::test_seams::PLANNER_FIRST_START,
        &card,
        pause.clone(),
    );
    let request_app = app.clone();
    let request_cookie = cookie.clone();
    let uri = format!("/api/cards/{card}/planner/input");
    let sending = tokio::spawn(async move {
        status(
            &request_app,
            &request_cookie,
            &uri,
            "revoked-start",
            json!({"text":"Must not start after logout"}),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .expect("the actual lazy start reaches its existing wait");
    assert_eq!(
        status(&app, &cookie, "/api/auth/logout", "logout", json!({})).await,
        StatusCode::OK
    );
    pause.release.notify_one();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), sending)
            .await
            .unwrap()
            .unwrap(),
        StatusCode::UNAUTHORIZED
    );
    let effects: (i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM worker_sessions WHERE card_id=?1),(SELECT count(*) FROM planner_input_idempotency WHERE card_id=?1)")
        .bind(&card).fetch_one(fixture.repo.pool()).await.unwrap();
    assert_eq!(
        effects,
        (0, 0),
        "revoked input creates neither a runtime nor an acknowledgement"
    );
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn historical_source_revocation_blocks_start_but_preserves_kernel_compensation() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application_with_historical_native(true).await;
    let area = area(&app, &cookie).await;
    let gate = state
        .shared_codex_appserver
        .lock_thread_start_serial_for_test()
        .await;
    let request_app = app.clone();
    let request_cookie = cookie.clone();
    let request_area = area["id"].clone();
    let creating = tokio::spawn(async move {
        status(&request_app, &request_cookie, "/api/tracks", "revoked-native-start", json!({
            "area_id":request_area,"planner_provider":"codex","template_id":"small-change","title":"",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":"Retain the kernel's original cleanup"
        })).await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let prepared: i64 =
                sqlx::query_scalar("SELECT count(*) FROM worker_sessions WHERE state='starting'")
                    .fetch_one(fixture.repo.pool())
                    .await
                    .unwrap();
            if prepared == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the real historical adapter commits its original starting runtime");
    assert_eq!(
        status(&app, &cookie, "/api/auth/logout", "logout", json!({})).await,
        StatusCode::OK
    );
    drop(gate);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), creating)
            .await
            .unwrap()
            .unwrap(),
        StatusCode::UNAUTHORIZED
    );
    let active: i64 = sqlx::query_scalar("SELECT count(*) FROM worker_sessions WHERE state IN ('starting','running','idle','turn_pending')")
        .fetch_one(fixture.repo.pool()).await.unwrap();
    assert_eq!(
        active, 0,
        "source revocation cannot disable the kernel's original failed-start compensation"
    );
    let threaded: i64 =
        sqlx::query_scalar("SELECT count(*) FROM worker_sessions WHERE thread_id IS NOT NULL")
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        threaded, 0,
        "the revoked source never begins a native thread"
    );
    shutdown(&fixture, &state).await;
}
