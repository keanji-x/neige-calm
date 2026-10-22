use super::*;

async fn create_body(app: &axum::Router, cookie: &str) -> Value {
    let area = post(
        app,
        cookie,
        "/api/areas",
        "area",
        json!({"name":"Claude creation","color":"#123456"}),
    )
    .await;
    json!({"area_id":area["id"],"title":"","planner_provider":"claude",
        "theme":{"fg":[255,255,255],"bg":[0,0,0]}})
}

#[tokio::test]
async fn canonical_claude_creation_accepts_first_text_while_provider_down() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let mut body = create_body(&app, &cookie).await;
    body["first_message"] = json!(FIRST_BODY);
    let created = post(&app, &cookie, "/api/tracks", "first", body.clone()).await;
    let (card, _, frame) = fixture.carrier(created["id"].as_str().unwrap()).await;
    assert_eq!(frame.canonical_inputs.pending.len(), 1);
    assert!(frame.canonical_inputs.active.is_none());
    assert!(frame.canonical_inputs.attempts.is_empty());
    assert_eq!(fixture.counts(&card).await, (1, 1, 1, 0, 0));
    let receipt = fixture.receipt(body["area_id"].as_str().unwrap()).await;
    assert_eq!(
        receipt.source_user_id,
        calm_types::organization::LEGACY_OWNER_USER_ID
    );
    let replay = post(&app, &cookie, "/api/tracks", "first", body).await;
    assert_eq!(created["id"], replay["id"]);
    assert_eq!(fixture.counts(&card).await, (1, 1, 1, 0, 0));
    assert!(!state.shared_codex_appserver.is_running());
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn canonical_claude_creation_empty_keyed_and_unkeyed_are_canonical() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let body = create_body(&app, &cookie).await;
    let keyed = post(&app, &cookie, "/api/tracks", "empty", body.clone()).await;
    let replay = post(&app, &cookie, "/api/tracks", "empty", body.clone()).await;
    assert_eq!(keyed["id"], replay["id"]);
    let mut tracks = vec![keyed];
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tracks")
                    .header(header::COOKIE, &cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let value: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(status, StatusCode::CREATED, "{value}");
        assert!(tracks.iter().all(|t| t["id"] != value["id"]));
        tracks.push(value);
    }
    for track in tracks {
        let (card, _, frame) = fixture.carrier(track["id"].as_str().unwrap()).await;
        assert_eq!(frame.header.phase, HarnessPhaseTag::Idle);
        assert!(frame.canonical_inputs.pending.is_empty());
        assert!(frame.canonical_inputs.active.is_none());
        assert!(frame.canonical_inputs.attempts.is_empty());
        assert_eq!(fixture.counts(&card).await, (0, 0, 1, 0, 0));
        post(
            &app,
            &cookie,
            &format!("/api/cards/{card}/planner/input"),
            "later",
            json!({"text":FIRST_BODY}),
        )
        .await;
        let (_, _, frame) = fixture.carrier(track["id"].as_str().unwrap()).await;
        assert_eq!(frame.canonical_inputs.pending.len(), 1);
        assert_eq!(fixture.counts(&card).await, (1, 1, 1, 0, 0));
    }
    let receipts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM conversation_human_track_creations")
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        receipts, 0,
        "empty allocation has no fabricated Human receipt"
    );
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn canonical_claude_creation_key_replay_preserves_original_ack_and_source() {
    let fixture = Fixture::new().await;
    let (state, app, cookie, _auth) = fixture.application_with_auth(false).await;
    let mut body = create_body(&app, &cookie).await;
    body["first_message"] = json!(FIRST_BODY);
    body["model"] = json!("chosen-claude-model");
    let created = post(&app, &cookie, "/api/tracks", "first", body.clone()).await;
    let original = fixture.receipt(body["area_id"].as_str().unwrap()).await;
    let (card, origin, frame) = fixture.carrier(created["id"].as_str().unwrap()).await;
    let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
        .await
        .unwrap();
    use crate::db::sqlite::conversation::*;
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &origin)
        .await
        .unwrap();
    let plan =
        human_plan_for_pending_prefix_tx(&mut tx, &carrier, std::num::NonZeroU32::MIN, vec![])
            .await
            .unwrap();
    claim_initial_submission_tx(
        &mut tx,
        &mut carrier,
        plan,
        &calm_types::conversation::ConversationExecutionProvenance::LegacySingleUserDeployment {
            deployment_owner_user_id: calm_types::organization::LEGACY_OWNER_USER_ID.into(),
            provider: calm_types::runtime::AgentProvider::Claude,
            profile: crate::harness::profile::HarnessProfile::Planner,
        },
    )
    .await
    .unwrap();
    let checkpoint = carrier.checkpoint().clone();
    let snapshot = carrier.snapshot().clone();
    let replacement = crate::model::new_id();
    crate::db::sqlite::session_supersede_and_start_tx(
        &mut tx,
        &origin,
        crate::session_projection_repo::WorkerSessionInit::shared_planner(
            replacement.clone(),
            card.clone(),
            calm_types::runtime::AgentProvider::Claude,
            crate::session_projection_repo::WorkerSessionState::Idle,
            None,
            snapshot,
            crate::model::now_ms(),
        ),
    )
    .await
    .unwrap();
    crate::db::sqlite::session_delete_tx(&mut tx, &origin)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    post(&app, &cookie, "/api/tracks", "first", body.clone()).await;
    assert_eq!(
        fixture
            .receipt(body["area_id"].as_str().unwrap())
            .await
            .acknowledgement,
        original.acknowledgement
    );
    let (_, worker, after) = fixture.carrier(created["id"].as_str().unwrap()).await;
    assert_eq!(worker, replacement);
    assert_eq!(after.canonical_inputs, checkpoint);
    assert_eq!(frame.canonical_inputs.pending.len(), 1);
    for (field, value) in [
        ("planner_provider", json!("codex")),
        ("first_message", json!("changed")),
        ("model", json!("other-model")),
    ] {
        let mut changed = body.clone();
        changed[field] = value;
        assert_eq!(
            status(&app, &cookie, "/api/tracks", "first", changed).await,
            StatusCode::CONFLICT
        );
    }
    let area = body["area_id"].as_str().unwrap();
    sqlx::query("DELETE FROM area_members WHERE area_id=?1")
        .bind(area)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        status(&app, &cookie, "/api/tracks", "first", body.clone()).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        status(&app, &cookie, "/api/auth/logout", "logout", json!({})).await,
        StatusCode::OK
    );
    assert_eq!(
        status(&app, &cookie, "/api/tracks", "first", body).await,
        StatusCode::UNAUTHORIZED
    );
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn canonical_claude_creation_failure_never_mints_historical_delivery() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let mut body = create_body(&app, &cookie).await;
    body["first_message"] = json!(FIRST_BODY);
    let count_query = "SELECT (SELECT count(*) FROM tracks), \
        (SELECT count(*) FROM conversation_messages), \
        (SELECT count(*) FROM planner_input_idempotency), \
        (SELECT count(*) FROM operations)";
    let before: (i64, i64, i64, i64) = sqlx::query_as(count_query)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER refuse_canonical_creation BEFORE INSERT ON worker_sessions \
        BEGIN SELECT RAISE(ABORT,'canonical allocation refused'); END",
    )
    .execute(fixture.repo.pool())
    .await
    .unwrap();
    let status = status(&app, &cookie, "/api/tracks", "fresh-failure", body).await;
    assert!(status.is_server_error(), "{status}");
    let counts: (i64, i64, i64, i64) = sqlx::query_as(count_query)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        counts, before,
        "canonical failure is atomic and never tries a historical queue/start"
    );
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn canonical_claude_creation_shared_member_acceptance_is_not_execution_grant() {
    let fixture = Fixture::new().await;
    let (state, app, cookie, auth) = fixture.application_with_auth(false).await;
    let mut body = create_body(&app, &cookie).await;
    let area = body["area_id"].as_str().unwrap();
    let mut tx = fixture.repo.pool().begin().await.unwrap();
    crate::db::sqlite::area_membership::application_user_register(&mut tx, "alice", "Alice")
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO area_members(area_id,user_id,role,joined_at) VALUES(?1,'alice','member',0)",
    )
    .bind(area)
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
    body["first_message"] = json!(FIRST_BODY);
    let created = post(&app, &alice, "/api/tracks", "first", body.clone()).await;
    let receipt = fixture.receipt(body["area_id"].as_str().unwrap()).await;
    assert_eq!(receipt.source_user_id, "alice");
    let (_, _, frame) = fixture.carrier(created["id"].as_str().unwrap()).await;
    assert_eq!(frame.canonical_inputs.pending.len(), 1);
    assert!(frame.canonical_inputs.active.is_none());
    assert!(frame.canonical_inputs.attempts.is_empty());
    assert!(frame.header.last_thread_id.is_none());
    post(&app, &alice, "/api/tracks", "first", body).await;
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn canonical_claude_creation_first_send_reuses_checkpoint_and_h2_hold() {
    for action in ["keep", "retry"] {
        let mut fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let mut body = create_body(&app, &cookie).await;
        body["model"] = json!("claude-frozen");
        body["reasoning_effort"] = json!("high");
        let created = post(&app, &cookie, "/api/tracks", "empty", body).await;
        let track = created["id"].as_str().unwrap();
        let (card, worker, empty) = fixture.carrier(track).await;
        assert!(empty.canonical_inputs.pending.is_empty());
        fixture.shutdown_process(&state).await;
        drop(app);
        drop(state);
        let fake = fixture.root.path().join("fake");
        std::fs::create_dir_all(&fake).unwrap();
        std::fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/claude_planner_fake/claude.sh"
            ),
            fake.join("claude"),
        )
        .unwrap();
        std::fs::write(fake.join("scenario"), "hold").unwrap();
        let config = fixture.root.path().join("claude.json");
        std::fs::write(&config,json!({"claude_binary":fake.join("claude"),"claude_version":"2.1.280","config_dir":fixture.root.path().join("claude-config")}).to_string()).unwrap();
        fixture.claude_config = Some(config);
        let (state, app, cookie) = fixture.application().await;
        // Hold only the actual crossing-marker transaction. The production
        // route must install its retained session and claim the saved input.
        sqlx::query("CREATE TRIGGER hold_creation_marker BEFORE UPDATE OF handle_state_json ON worker_sessions \
            WHEN json_array_length(NEW.handle_state_json,'$.canonical_inputs.attempts') > 0 \
            BEGIN SELECT RAISE(ABORT,'hold creation crossing'); END")
            .execute(fixture.repo.pool()).await.unwrap();
        let input_uri = format!("/api/cards/{card}/planner/input");
        post(
            &app,
            &cookie,
            &input_uri,
            "later",
            json!({"text":FIRST_BODY}),
        )
        .await;
        let harness = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Some(harness) = state.harness.get(&worker) {
                    let (_, _, frame) = fixture.carrier(track).await;
                    if frame.canonical_inputs.active.is_some() {
                        break harness;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let session = harness.claude_session_for_test().unwrap();
        let pause = crate::claude_planner::session::SettlePause {
            entered: Arc::default(),
            release: Arc::default(),
        };
        session.fail_user_flush_for_test(pause.clone());
        sqlx::query("DROP TRIGGER hold_creation_marker")
            .execute(fixture.repo.pool())
            .await
            .unwrap();
        harness.refresh_canonical_input().await.unwrap();
        tokio::time::timeout(Duration::from_secs(30), pause.entered.notified())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if std::fs::read_to_string(fake.join("stdin"))
                    .is_ok_and(|line| line.contains(FIRST_BODY))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        pause.release.notify_one();
        let held = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let (_, _, frame) = fixture.carrier(track).await;
                if frame
                    .canonical_inputs
                    .attempts
                    .last()
                    .is_some_and(|a| matches!(a.delivery, InputDeliveryFact::Unknown))
                {
                    break frame;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(session.user_line_writes_for_test(), 1);
        harness.shutdown().await.unwrap();
        fixture.shutdown_process(&state).await;
        drop(harness);
        drop(session);
        drop(app);
        drop(state);
        let (state, app, cookie) = fixture.application().await;
        let (_, _, recovered) = fixture.carrier(track).await;
        assert_eq!(recovered.canonical_inputs, held.canonical_inputs);
        post(
            &app,
            &cookie,
            &input_uri,
            "later",
            json!({"text":FIRST_BODY}),
        )
        .await;
        assert_eq!(
            std::fs::read_to_string(fake.join("spawns"))
                .unwrap()
                .lines()
                .count(),
            1,
            "restart/replay never writes the original twice"
        );
        let (_, _, current) = fixture.carrier(track).await;
        let attempt = current.canonical_inputs.attempts.last().unwrap();
        std::fs::write(fake.join("scenario"), "exit").unwrap();
        let result = post(&app,&cookie,&format!("/api/cards/{card}/conversation/input/{action}"),action,
            json!({"carrier_id":worker,"submission_id":attempt.attempt.submission_id,"attempt_id":attempt.attempt.attempt_id,"revision":attempt.revision.to_string()})).await;
        assert_eq!(result["waiting_for_settlement"], true);
        if action == "retry" {
            // Reboot loses the predecessor's joined-driver evidence. H2
            // records the explicit successor but cannot fabricate settlement
            // or send beside an execution whose end is unconfirmed.
            tokio::time::sleep(Duration::from_millis(2200)).await;
            let (_, _, after) = fixture.carrier(track).await;
            assert_eq!(after.canonical_inputs.attempts.len(), 1);
            assert!(matches!(
                after.canonical_inputs.pending.as_slice(),
                [calm_types::conversation::PendingInput::ExplicitSuccessor { .. }]
            ));
            assert_eq!(
                std::fs::read_to_string(fake.join("spawns"))
                    .unwrap()
                    .lines()
                    .count(),
                1
            );
        } else {
            let (_, _, after) = fixture.carrier(track).await;
            assert_eq!(after.canonical_inputs.receipts.len(), 1);
            assert_eq!(
                std::fs::read_to_string(fake.join("spawns"))
                    .unwrap()
                    .lines()
                    .count(),
                1
            );
        }
        assert_eq!(
            fixture.counts(&card).await.0,
            1,
            "H2 reuses the original permanent Human row"
        );
        shutdown(&fixture, &state).await;
    }
}

#[tokio::test]
async fn canonical_claude_creation_excluded_none_keeps_missing_area_teardown_refusal() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let cwd = fixture.root.path().join("workspaces");
    std::fs::create_dir_all(&cwd).unwrap();
    let response=app.clone().oneshot(Request::builder().method("POST").uri("/api/tracks")
        .header(header::COOKIE,&cookie).header(header::CONTENT_TYPE,"application/json")
        .body(Body::from(json!({"area_id":"missing-area","title":"excluded none","planner_provider":"claude",
            "cwd":cwd,"attach_folder":true,"isolation":"none","theme":{"fg":[255,255,255],"bg":[0,0,0]}}).to_string())).unwrap()).await.unwrap();
    let status = response.status();
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("overlaps Neige-owned location"),
        "{body}"
    );
    fixture.shutdown_process(&state).await;
}
