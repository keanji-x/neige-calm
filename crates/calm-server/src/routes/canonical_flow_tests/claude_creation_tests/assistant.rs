//! U4 uses authenticated HTTP, the production host, and the existing fake Claude CLI.
use super::*;
use crate::routes::canonical_flow_tests::assistant_canonical::{carrier, initial, track};

pub(in crate::routes::canonical_flow_tests) fn configure(fixture: &mut Fixture, scenario: &str) {
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
    std::fs::write(fake.join("scenario"), scenario).unwrap();
    let config = fixture.root.path().join("claude.json");
    std::fs::write(
        &config,
        json!({"claude_binary":fake.join("claude"),
        "claude_version":"2.1.280","config_dir":fixture.root.path().join("claude-config")})
        .to_string(),
    )
    .unwrap();
    fixture.claude_config = Some(config);
    fixture.execution = Some(crate::test_support::execution::Supervisor::new());
}

pub(in crate::routes::canonical_flow_tests) fn read(fixture: &Fixture, name: &str) -> String {
    std::fs::read_to_string(fixture.root.path().join("fake").join(name)).unwrap_or_default()
}

async fn get(app: &axum::Router, cookie: &str, uri: &str) -> Value {
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
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).unwrap()
}

pub(in crate::routes::canonical_flow_tests) async fn wait_frame(
    fixture: &Fixture,
    card: &str,
    predicate: impl Fn(&CanonicalHarnessSnapshot) -> bool,
) -> CanonicalHarnessSnapshot {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (_, frame) = carrier(fixture, card).await;
            if predicate(&frame) {
                return frame;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Claude canonical frame must settle")
}

#[tokio::test]
async fn claude_assistant_outage_acceptance_replay_and_retained_activation() {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track =
        crate::routes::canonical_flow_tests::assistant_canonical::canonical_track(&app, &cookie)
            .await;
    let uri = format!("/api/tracks/{track}/conversations");
    let body = json!({"provider":"claude","text":FIRST_BODY});
    let expected_card =
        crate::conversation_keys::derive_track_conversation_keys(&track, "assistant").card_id;
    let wake = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "canonical-human-wake-finished",
        &expected_card,
        wake.clone(),
    );
    let created = post(&app, &cookie, &uri, "assistant", body.clone()).await;
    let card = created["id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(5), wake.entered.notified())
        .await
        .expect("fresh acceptance wakes even during provider outage");
    wake.release.notify_one();
    let (worker, frame) = carrier(&fixture, card).await;
    assert_eq!(frame.header.phase, HarnessPhaseTag::Idle);
    assert!(frame.header.last_thread_id.is_none());
    assert_eq!(frame.canonical_inputs.pending.len(), 1);
    assert!(frame.canonical_inputs.attempts.is_empty());
    assert_eq!(fixture.counts(card).await, (1, 1, 1, 0, 0));
    assert_eq!(
        created["initialInput"]["receipt"],
        initial(&app, &cookie, &track, card, "assistant").await["receipt"]
    );
    let receipts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM conversation_assistant_creations WHERE card_id=?1",
    )
    .bind(card)
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap();
    assert_eq!(receipts, 1);
    for (field, value) in [("text", json!("changed")), ("provider", json!("codex"))] {
        let mut changed = body.clone();
        changed[field] = value;
        assert_eq!(
            status(&app, &cookie, &uri, "assistant", changed).await,
            StatusCode::CONFLICT
        );
    }
    fixture.shutdown_process(&state).await;
    drop(app);
    drop(state);
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    configure(&mut fixture, "exit");
    let (state, app, cookie) = fixture.application().await;
    let wake = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("canonical-human-wake-finished", card, wake.clone());
    let replay = post(&app, &cookie, &uri, "assistant", body.clone()).await;
    assert_eq!(replay["initialInput"], created["initialInput"]);
    let woke = tokio::time::timeout(Duration::from_secs(1), wake.entered.notified())
        .await
        .is_ok();
    wake.release.notify_one();
    assert!(
        !woke,
        "creation replay must not wake or resend accepted work"
    );
    assert!(read(&fixture, "spawns").is_empty());
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "later",
        json!({"text":"activate retained Claude"}),
    )
    .await;
    let frame = wait_frame(&fixture, card, |f| {
        f.idle_for_lifecycle() && f.canonical_inputs.attempts.len() == 2
    })
    .await;
    assert_eq!(carrier(&fixture, card).await.0, worker);
    let thread = frame.header.last_thread_id.as_deref().unwrap();
    assert!(uuid::Uuid::parse_str(thread).is_ok());
    let row: (String, String) =
        sqlx::query_as("SELECT thread_id,agent_session_id FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(row, (thread.to_owned(), thread.to_owned()));
    assert!(
        frame
            .canonical_inputs
            .attempts
            .iter()
            .all(|a| a.correlation.thread_id == thread)
    );
    assert_eq!(read(&fixture, "spawns").lines().count(), 2);
    let replay = post(&app, &cookie, &uri, "assistant", body).await;
    assert_eq!(replay["initialInput"], created["initialInput"]);
    assert_eq!(
        read(&fixture, "stdin")
            .lines()
            .filter(|l| l.contains(FIRST_BODY))
            .count(),
        1
    );
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn claude_assistant_acceptance_failure_is_atomic_without_historical_fallback() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let counts = "SELECT (SELECT count(*) FROM cards),(SELECT count(*) FROM worker_sessions),\
        (SELECT count(*) FROM conversation_messages),(SELECT count(*) FROM operations)";
    let before: (i64, i64, i64, i64) = sqlx::query_as(counts)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER refuse_assistant_acceptance BEFORE INSERT ON conversation_messages \
        BEGIN SELECT RAISE(ABORT,'refuse acceptance'); END",
    )
    .execute(fixture.repo.pool())
    .await
    .unwrap();
    assert!(
        status(
            &app,
            &cookie,
            &format!("/api/tracks/{track}/conversations"),
            "atomic",
            json!({"provider":"claude","text":FIRST_BODY})
        )
        .await
        .is_server_error()
    );
    let after: (i64, i64, i64, i64) = sqlx::query_as(counts)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    assert_eq!(after, before);
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn claude_assistant_side_context_tools_never_and_archived_input() {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture, "assistant-mcp");
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let planner: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(&track)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    let created = post(&app, &cookie, &format!("/api/tracks/{track}/conversations"), "side",
        json!({"provider":"claude","text":FIRST_BODY,"model":"claude-fable-5-1[1m]","reasoning_effort":"high",
            "side":{"source_card_id":planner,"context":"Frozen Claude side excerpt"}})).await;
    let card = created["id"].as_str().unwrap();
    let before = wait_frame(&fixture, card, |f| {
        f.idle_for_lifecycle() && f.canonical_inputs.attempts.len() == 1
    })
    .await;
    assert_eq!(created["sourceCardId"], planner);
    let text = read(&fixture, "stdin");
    assert!(
        text.contains("Frozen Claude side excerpt") && text.contains(FIRST_BODY),
        "{text}"
    );
    assert!(text.find("Frozen Claude side excerpt").unwrap() < text.find(FIRST_BODY).unwrap());
    let instructions = read(&fixture, "instructions");
    assert!(
        instructions.contains("What you cannot do"),
        "{instructions}"
    );
    let argv = read(&fixture, "argv");
    assert!(argv.contains("--model=claude-fable-5-1[1m]"));
    assert!(argv.contains("--effort=high"));
    assert!(argv.contains("--permission-prompts\nnone\n"), "{argv}");
    assert!(!argv.contains("--permission-mode"));
    assert!(!argv.contains("--permission-prompt-tool"));
    let catalog: Value = serde_json::from_str(&read(&fixture, "assistant-mcp-tools")).unwrap();
    let names: Vec<_> = catalog["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    for allowed in ["neige_report_read", "neige_report_commit"] {
        assert!(names.contains(&allowed), "{names:?}");
    }
    for denied in ["neige_task_done", "neige_track_add"] {
        assert!(!names.contains(&denied), "{names:?}");
    }
    let replies: Vec<Value> =
        serde_json::from_str(&read(&fixture, "assistant-mcp-results")).unwrap();
    for allowed in &replies[..2] {
        assert!(
            allowed.get("error").is_none() && allowed["result"].is_object(),
            "{allowed}"
        );
        assert_ne!(allowed["result"]["isError"], true, "{allowed}");
    }
    for denied in &replies[2..] {
        assert!(
            denied["error"].is_object() || denied["result"]["isError"] == true,
            "{denied}"
        );
    }
    let run = get(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/run?contract=conversation_v1"),
    )
    .await;
    assert!(
        run["permission_mode"].is_null(),
        "Assistant has no mutable permission mode"
    );
    let calm_types::conversation::InputInvocation::Recorded { configuration } =
        &before.canonical_inputs.attempts[0].invocation
    else {
        panic!("frozen invocation");
    };
    assert_eq!(
        configuration.permission,
        crate::planner_permission_mode::PlannerPermissionMode::Never
    );
    let response = app
        .clone()
        .oneshot(
            Request::put(format!("/api/cards/{card}/planner/permission-mode"))
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"permission_mode":"full"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(!response.status().is_success());
    let counts = fixture.counts(card).await;
    let response = app
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
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        status(
            &app,
            &cookie,
            &format!("/api/cards/{card}/planner/input"),
            "archived",
            json!({"text":"refused"})
        )
        .await,
        StatusCode::CONFLICT
    );
    let after = carrier(&fixture, card).await.1;
    assert_eq!(after.canonical_inputs, before.canonical_inputs);
    assert_eq!(after.header.phase, before.header.phase);
    assert_eq!(after.header.last_thread_id, before.header.last_thread_id);
    assert_eq!(fixture.counts(card).await, counts);
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn claude_assistant_stop_qualifies_original_driver_end() {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture, "hold");
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let created = post(
        &app,
        &cookie,
        &format!("/api/tracks/{track}/conversations"),
        "stop",
        json!({"provider":"claude","text":FIRST_BODY}),
    )
    .await;
    let card = created["id"].as_str().unwrap();
    wait_frame(&fixture, card, |f| {
        f.header.phase == HarnessPhaseTag::TurnRunning && f.canonical_inputs.attempts.len() == 1
    })
    .await;
    let worker = carrier(&fixture, card).await.0;
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/interrupt"),
        "interrupt",
        json!({}),
    )
    .await;
    let ended = wait_frame(&fixture, card, |f| f.idle_for_lifecycle()).await;
    assert!(ended.canonical_inputs.active.is_none());
    assert_eq!(ended.canonical_inputs.attempts.len(), 1);
    assert_eq!(carrier(&fixture, card).await.0, worker);
    assert_eq!(read(&fixture, "spawns").lines().count(), 1);
    assert!(read(&fixture, "stdin").contains("interrupt"));
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn claude_assistant_restart_human_resolution_waits_for_predecessor_settlement() {
    for action in ["keep", "retry"] {
        let mut fixture = Fixture::new().await;
        configure(&mut fixture, "hold");
        let (state, app, cookie) = fixture.application().await;
        let track = crate::routes::canonical_flow_tests::assistant_canonical::canonical_track(
            &app, &cookie,
        )
        .await;
        sqlx::query("CREATE TRIGGER hold_assistant_marker BEFORE UPDATE OF handle_state_json ON worker_sessions \
            WHEN json_array_length(NEW.handle_state_json,'$.canonical_inputs.attempts') > 0 \
            BEGIN SELECT RAISE(ABORT,'hold crossing'); END").execute(fixture.repo.pool()).await.unwrap();
        let uri = format!("/api/tracks/{track}/conversations");
        let body = json!({"provider":"claude","text":FIRST_BODY,"model":"claude-fable-5-1[1m]","reasoning_effort":"high"});
        let created = post(&app, &cookie, &uri, "held", body.clone()).await;
        let card = created["id"].as_str().unwrap();
        wait_frame(&fixture, card, |f| f.canonical_inputs.active.is_some()).await;
        let (worker, bound) = carrier(&fixture, card).await;
        let thread = bound.header.last_thread_id.clone().unwrap();
        assert!(uuid::Uuid::parse_str(&thread).is_ok());
        let harness = state.harness.get(&worker).unwrap();
        let session = harness.claude_session_for_test().unwrap();
        let pause = crate::claude_planner::session::SettlePause {
            entered: Arc::default(),
            release: Arc::default(),
        };
        session.fail_user_flush_for_test(pause.clone());
        sqlx::query("DROP TRIGGER hold_assistant_marker")
            .execute(fixture.repo.pool())
            .await
            .unwrap();
        harness.refresh_canonical_input().await.unwrap();
        tokio::time::timeout(Duration::from_secs(30), pause.entered.notified())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !read(&fixture, "stdin").contains(FIRST_BODY) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        pause.release.notify_one();
        let held = wait_frame(&fixture, card, |f| {
            f.canonical_inputs
                .attempts
                .last()
                .is_some_and(|a| matches!(a.delivery, InputDeliveryFact::Unknown))
        })
        .await;
        assert_eq!(session.user_line_writes_for_test(), 1);
        assert_eq!(
            held.canonical_inputs.attempts[0].correlation.thread_id,
            thread
        );
        harness.shutdown().await.unwrap();
        fixture.shutdown_process(&state).await;
        drop(harness);
        drop(session);
        drop(app);
        drop(state);
        fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
        let (state, app, cookie) = fixture.application().await;
        assert_eq!(
            carrier(&fixture, card).await.1.canonical_inputs.attempts,
            held.canonical_inputs.attempts
        );
        // END may already have cleared active; H2 still resolves the original Unknown attempt.
        assert_eq!(
            post(&app, &cookie, &uri, "held", body).await["initialInput"],
            created["initialInput"]
        );
        assert_eq!(read(&fixture, "spawns").lines().count(), 1);
        let attempt = &held.canonical_inputs.attempts[0];
        let args = json!({"carrier_id":worker,"submission_id":attempt.attempt.submission_id,
            "attempt_id":attempt.attempt.attempt_id,"revision":attempt.revision.to_string()});
        let action_uri = format!("/api/cards/{card}/conversation/input/{action}");
        std::fs::write(fixture.root.path().join("fake/scenario"), "exit").unwrap();
        let resolved = post(&app, &cookie, &action_uri, action, args.clone()).await;
        assert_eq!(resolved["waiting_for_settlement"], true);
        assert_eq!(
            post(&app, &cookie, &action_uri, action, args.clone()).await,
            resolved
        );
        let after = wait_frame(&fixture, card, |frame| {
            frame.canonical_inputs.active.is_none()
                && frame.canonical_inputs.attempts.len() == if action == "retry" { 2 } else { 1 }
        })
        .await;
        let (current, _) = carrier(&fixture, card).await;
        assert_eq!(current, worker);
        assert_eq!(
            after.header.last_thread_id.as_deref(),
            Some(thread.as_str())
        );
        assert!(after.canonical_inputs.active.is_none());
        assert!(after.canonical_inputs.pending.is_empty());
        assert_eq!(after.canonical_inputs.receipts.len(), 1);
        if action == "retry" {
            assert_eq!(read(&fixture, "spawns").lines().count(), 2);
            assert_eq!(read(&fixture, "stdin").lines().count(), 2);
            assert!(
                read(&fixture, "argv")
                    .lines()
                    .collect::<Vec<_>>()
                    .windows(2)
                    .any(|args| args == ["--resume", thread.as_str()]
                        || args == ["--session-id", thread.as_str()])
            );
        } else {
            let mut resolved_attempts = held.canonical_inputs.attempts.clone();
            resolved_attempts[0].revision += 1;
            assert_eq!(after.canonical_inputs.attempts, resolved_attempts);
            assert_eq!(read(&fixture, "spawns").lines().count(), 1);
        }
        for _ in 0..10 {
            assert_eq!(
                post(&app, &cookie, &action_uri, action, args.clone()).await,
                resolved
            );
            assert_eq!(
                carrier(&fixture, card).await.1.canonical_inputs,
                after.canonical_inputs
            );
            tokio::task::yield_now().await;
        }
        assert_eq!(fixture.counts(card).await.0, 1);
        let row: String = sqlx::query_scalar("SELECT thread_id FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
        assert_eq!(row, thread);
        fixture.shutdown_process(&state).await;
    }
}

#[tokio::test]
async fn claude_assistant_internal_historical_key_replays_original_evidence() {
    use crate::operation::planner_harness_start_adapter::OpeningBriefing;
    use crate::routes::track_conversations::{
        AssistantCreationEntry, AssistantProviderRequest, NewTrackConversationBody,
    };
    let mut fixture = Fixture::new().await;
    configure(&mut fixture, "exit");
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("Idempotency-Key", "historical-claude".parse().unwrap());
    let (_, crate::extract::Json(original)) =
        crate::routes::track_conversations::create_track_conversation_inner(
            crate::state::RouteState::from_ref(&state),
            crate::state::WorkerState::from_ref(&state),
            crate::actor::Actor::server_send(),
            headers,
            track.clone(),
            NewTrackConversationBody {
                provider: AssistantProviderRequest::Explicit(
                    calm_types::runtime::AgentProvider::Claude,
                ),
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
    let card = original["id"].as_str().unwrap();
    let uri = format!("/api/tracks/{track}/conversations");
    let body = json!({"provider":"claude","text":FIRST_BODY});
    let replay = post(&app, &cookie, &uri, "historical-claude", body.clone()).await;
    assert_eq!(replay["initialInput"], original["initialInput"]);
    assert_eq!(replay["id"], original["id"]);
    let canonical: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM conversation_assistant_creations WHERE card_id=?1",
    )
    .bind(card)
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap();
    assert_eq!(canonical, 0);
    assert_eq!(fixture.counts(card).await.0, 0);
    for (field, value) in [("text", json!("changed")), ("provider", json!("codex"))] {
        let mut changed = body.clone();
        changed[field] = value;
        assert_eq!(
            status(&app, &cookie, &uri, "historical-claude", changed).await,
            StatusCode::CONFLICT
        );
    }
    fixture.shutdown_process(&state).await;
    drop(app);
    drop(state);
    // Historical factual replay remains available in-process, but cannot boot
    // the deployment through the new format barrier. Compare raw durable JSON.
    // Dispatcher retirement does not join an in-flight historical harness.
    // Freeze a real crash image so that writer cannot race the boot byte checks.
    let snapshot = fixture.root.path().join("historical-boot.db");
    sqlx::query("VACUUM INTO ?1")
        .bind(snapshot.to_str().unwrap())
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    let url = format!("sqlite:{}", snapshot.display());
    let raw = calm_truth::db::sqlite::conversation_upgrade::install_schema(&url)
        .await
        .unwrap();
    let before: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id,handle_state_json FROM worker_sessions ORDER BY id")
            .fetch_all(raw.read_pool())
            .await
            .unwrap();
    let journals: Vec<(String, String)> =
        sqlx::query_as("SELECT id,payload_json FROM operations ORDER BY id")
            .fetch_all(raw.read_pool())
            .await
            .unwrap();
    use clap::Parser;
    let mut cfg = crate::config::Config::parse_from(["calm-server"]);
    cfg.db_url = url.clone();
    cfg.auth_password = Some("fixture-password".into());
    let error = AppState::boot(&cfg)
        .await
        .err()
        .expect("historical boot refused");
    assert!(
        error.to_string().contains("conversation upgrade"),
        "{error:#}"
    );
    let error = SqlxRepo::open(&url)
        .await
        .err()
        .expect("historical repo refused");
    assert!(
        error.to_string().contains("conversation upgrade"),
        "{error}"
    );
    assert_eq!(
        sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT id,handle_state_json FROM worker_sessions ORDER BY id"
        )
        .fetch_all(raw.read_pool())
        .await
        .unwrap(),
        before
    );
    assert_eq!(
        sqlx::query_as::<_, (String, String)>("SELECT id,payload_json FROM operations ORDER BY id")
            .fetch_all(raw.read_pool())
            .await
            .unwrap(),
        journals
    );
    raw.close().await;
}

#[tokio::test]
async fn claude_assistant_unavailable_version_keeps_durable_acceptance() {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture, "exit");
    std::fs::write(fixture.root.path().join("fake/version"), "0.0.1").unwrap();
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let expected_card =
        crate::conversation_keys::derive_track_conversation_keys(&track, "unavailable").card_id;
    let wake = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "canonical-human-wake-finished",
        &expected_card,
        wake.clone(),
    );
    let created = post(
        &app,
        &cookie,
        &format!("/api/tracks/{track}/conversations"),
        "unavailable",
        json!({"provider":"claude","text":FIRST_BODY}),
    )
    .await;
    let card = created["id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(5), wake.entered.notified())
        .await
        .unwrap();
    wake.release.notify_one();
    let (_, frame) = carrier(&fixture, card).await;
    assert_eq!(frame.header.phase, HarnessPhaseTag::Idle);
    assert_eq!(frame.canonical_inputs.pending.len(), 1);
    assert!(frame.canonical_inputs.attempts.is_empty());
    assert_eq!(fixture.counts(card).await, (1, 1, 1, 0, 0));
    assert!(read(&fixture, "spawns").is_empty());
    fixture.shutdown_process(&state).await;
}
