//! U5 crosses the real HTTP/host boundary with the local ACP process only.
use super::*;
use crate::routes::canonical_flow_tests::assistant_canonical::{carrier, initial, track};

fn requests(fixture: &Fixture) -> Vec<Value> {
    std::fs::read_to_string(fixture.root.path().join("requests.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

async fn wait_idle(fixture: &Fixture, card: &str, attempts: usize) -> CanonicalHarnessSnapshot {
    let mut last_snapshot = None;
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (_, snapshot) = carrier(fixture, card).await;
            if snapshot.idle_for_lifecycle()
                && snapshot.canonical_inputs.attempts.len() == attempts
                && snapshot.canonical_inputs.pending.is_empty()
            {
                return snapshot;
            }
            last_snapshot = Some(snapshot);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    match result {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let diagnostic = format!(
                "snapshot={last_snapshot:#?}\nrequests={:#?}\nMCP={:#?}\npermission={:#?}",
                requests(fixture),
                (
                    std::fs::read_to_string(fixture.root.path().join("assistant-mcp-progress")),
                    std::fs::read_to_string(fixture.root.path().join("assistant-mcp-results.json"))
                ),
                std::fs::read_to_string(fixture.root.path().join("permission-reply.json"))
            );
            // CI drops lines over 4096 bytes. Pretty Debug still leaves prompt strings on
            // one line, so bound each emitted chunk to at most 800 Unicode scalars (3200 bytes).
            for line in diagnostic.lines() {
                let chars: Vec<_> = line.chars().collect();
                for chunk in chars.chunks(800) {
                    eprintln!("{}", chunk.iter().collect::<String>());
                }
            }
            panic!("ACP END timeout: {error}");
        }
    }
}

#[tokio::test]
async fn acp_assistant_outage_acceptance_replay_and_retained_activation() {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let uri = format!("/api/tracks/{track}/conversations");
    let body = json!({"provider":"opencode","text":FIRST_BODY});
    let expected_card =
        crate::conversation_keys::derive_track_conversation_keys(&track, "assistant").card_id;
    let fresh_wake = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "canonical-human-wake-finished",
        &expected_card,
        fresh_wake.clone(),
    );
    let created = post(&app, &cookie, &uri, "assistant", body.clone()).await;
    let card = created["id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(5), fresh_wake.entered.notified())
        .await
        .expect("fresh acceptance must schedule its real wake even during outage");
    fresh_wake.release.notify_one();
    let (worker, accepted) = carrier(&fixture, card).await;
    assert_eq!(accepted.header.phase, HarnessPhaseTag::Idle);
    assert_eq!(accepted.canonical_inputs.pending.len(), 1);
    assert!(accepted.canonical_inputs.attempts.is_empty());
    assert_eq!(fixture.counts(card).await, (1, 1, 1, 0, 0));
    assert_eq!(
        initial(&app, &cookie, &track, card, "assistant").await["receipt"],
        created["initialInput"]["receipt"]
    );
    for (field, value) in [("text", json!("changed")), ("provider", json!("grok"))] {
        let mut changed = body.clone();
        changed[field] = value;
        assert_eq!(
            status(&app, &cookie, &uri, "assistant", changed).await,
            StatusCode::CONFLICT
        );
    }
    // A cold application with the provider now configured still only replays the ACK.
    fixture.shutdown_process(&state).await;
    drop(app);
    drop(state);
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    configure(&mut fixture);
    let (state, app, cookie) = fixture.application().await;
    let replay_wake = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "canonical-human-wake-finished",
        card,
        replay_wake.clone(),
    );
    let replay = post(&app, &cookie, &uri, "assistant", body.clone()).await;
    assert_eq!(replay["initialInput"], created["initialInput"]);
    let woke = tokio::time::timeout(Duration::from_secs(1), replay_wake.entered.notified())
        .await
        .is_ok();
    // Also releases the next legitimate wake when the replay correctly schedules none.
    replay_wake.release.notify_one();
    assert!(!woke, "creation receipt replay must not schedule a wake");
    assert_eq!(
        prompts(&fixture),
        0,
        "receipt replay cannot wake accepted work"
    );
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "wake",
        json!({"text":"retained activation"}),
    )
    .await;
    wait_idle(&fixture, card, 2).await;
    assert_eq!(carrier(&fixture, card).await.0, worker);
    assert_eq!(prompts(&fixture), 2);
    let identity: (String, String) =
        sqlx::query_as("SELECT thread_id,agent_session_id FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    // A second activation must load the original native session on the same carrier.
    state.harness.shutdown_and_remove(&worker).await.unwrap();
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "load",
        json!({"text":"load original worker"}),
    )
    .await;
    wait_idle(&fixture, card, 3).await;
    let loaded = requests(&fixture)
        .into_iter()
        .find(|call| call["method"] == "session/load")
        .expect("native load");
    assert_eq!(loaded["params"]["sessionId"], identity.1);
    assert_eq!(carrier(&fixture, card).await.0, worker);
    assert_eq!(
        post(&app, &cookie, &uri, "assistant", body).await["initialInput"],
        created["initialInput"]
    );
    assert_eq!(prompts(&fixture), 3);
    assert_eq!(
        requests(&fixture)
            .iter()
            .filter(|call| call["method"] == "session/prompt"
                && call["params"]["prompt"].to_string().contains(FIRST_BODY))
            .count(),
        1,
        "accepted first input was not resent"
    );
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn acp_assistant_activation_retains_source_across_busy_track() {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture);
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let locks = state.operation_runtime.track_locks_for_test();
    let guard = crate::per_card_lock::lock_key(locks, &track).await;
    let waiting = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("key-lock-waiting", &track, waiting.clone());
    let created = post(
        &app,
        &cookie,
        &format!("/api/tracks/{track}/conversations"),
        "busy",
        json!({"provider":"opencode","text":FIRST_BODY}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(30), waiting.entered.notified())
        .await
        .expect("activation must wait for the held track");
    drop(guard);
    waiting.release.notify_one();
    let card = created["id"].as_str().unwrap();
    wait_idle(&fixture, card, 1).await;
    assert_eq!(
        prompts(&fixture),
        1,
        "the original accepted input runs once"
    );
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn acp_assistant_side_context_tools_never_and_archived_input() {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture);
    std::fs::write(fixture.root.path().join("scenario"), "assistant-canonical").unwrap();
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let planner: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(&track)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    let uri = format!("/api/tracks/{track}/conversations");
    let created = post(
        &app,
        &cookie,
        &uri,
        "side",
        json!({"provider":"opencode","text":FIRST_BODY,
        "side":{"source_card_id":planner,"context":"Frozen ACP side excerpt"}}),
    )
    .await;
    let card = created["id"].as_str().unwrap();
    let before = wait_idle(&fixture, card, 1).await;
    assert_eq!(created["sourceCardId"], planner);
    let prompt = requests(&fixture)
        .into_iter()
        .find(|call| call["method"] == "session/prompt")
        .unwrap();
    let text = prompt["params"]["prompt"].to_string();
    assert!(
        text.contains("Frozen ACP side excerpt") && text.contains(FIRST_BODY),
        "{text}"
    );
    assert!(text.find("Frozen ACP side excerpt").unwrap() < text.find(FIRST_BODY).unwrap());
    let replies: Value = serde_json::from_slice(
        &std::fs::read(fixture.root.path().join("assistant-mcp-results.json")).unwrap(),
    )
    .unwrap();
    for allowed in &replies.as_array().unwrap()[..2] {
        assert!(
            allowed.get("error").is_none() && allowed["result"].is_object(),
            "Assistant Report tool must answer successfully: {allowed}"
        );
        assert_ne!(allowed["result"]["isError"], true, "{allowed}");
    }
    for denied in &replies.as_array().unwrap()[2..] {
        assert!(
            denied["error"].is_object() || denied["result"]["isError"] == true,
            "{denied}"
        );
    }
    let permission: Value = serde_json::from_slice(
        &std::fs::read(fixture.root.path().join("permission-reply.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        permission["result"]["outcome"],
        json!({"outcome":"cancelled"})
    );
    let counts = fixture.counts(card).await;
    let archived = app
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
    assert_eq!(archived.status(), StatusCode::NO_CONTENT);
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
    assert_eq!(carrier(&fixture, card).await.1, before);
    assert_eq!(fixture.counts(card).await, counts);
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn acp_assistant_stop_waits_for_original_driver_end() {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture);
    std::fs::write(fixture.root.path().join("scenario"), "hold").unwrap();
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let created = post(
        &app,
        &cookie,
        &format!("/api/tracks/{track}/conversations"),
        "stop",
        json!({"provider":"opencode","text":FIRST_BODY}),
    )
    .await;
    let card = created["id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        while carrier(&fixture, card).await.1.header.phase != HarnessPhaseTag::TurnRunning {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (worker, active) = carrier(&fixture, card).await;
    assert!(!active.idle_for_lifecycle());
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/interrupt"),
        "interrupt",
        json!({}),
    )
    .await;
    let ended = wait_idle(&fixture, card, 1).await;
    assert!(ended.canonical_inputs.active.is_none());
    assert_eq!(carrier(&fixture, card).await.0, worker);
    assert_eq!(
        requests(&fixture)
            .iter()
            .filter(|call| call["method"] == "session/cancel")
            .count(),
        1
    );
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn acp_assistant_lost_response_cold_restart_never_resends() {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture);
    std::fs::write(fixture.root.path().join("scenario"), "lost").unwrap();
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let uri = format!("/api/tracks/{track}/conversations");
    let body = json!({"provider":"opencode","text":FIRST_BODY});
    let created = post(&app, &cookie, &uri, "lost", body.clone()).await;
    let card = created["id"].as_str().unwrap();
    let (worker, before) = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let facts = carrier(&fixture, card).await;
            if facts
                .1
                .canonical_inputs
                .attempts
                .first()
                .is_some_and(|a| matches!(a.delivery, InputDeliveryFact::Unknown))
                && prompts(&fixture) == 1
            {
                break facts;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(prompts(&fixture), 1);
    state.harness.shutdown_and_remove(&worker).await.unwrap();
    fixture.shutdown_process(&state).await;
    drop(app);
    drop(state);
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, app, cookie) = fixture.application().await;
    let replay = post(&app, &cookie, &uri, "lost", body).await;
    assert_eq!(replay["initialInput"], created["initialInput"]);
    assert_eq!(
        initial(&app, &cookie, &track, card, "lost").await["receipt"],
        created["initialInput"]["receipt"]
    );
    // A fresh input exercises recovery; an Unknown original cannot be retried implicitly.
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "held",
        json!({"text":"must wait for original reconciliation"}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(prompts(&fixture), 1);
    let (current, after) = carrier(&fixture, card).await;
    assert_eq!(current, worker);
    assert_eq!(
        after.canonical_inputs.attempts,
        before.canonical_inputs.attempts
    );
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn acp_assistant_grok_explicitly_retains_historical_format() {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture);
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let uri = format!("/api/tracks/{track}/conversations");
    let body = json!({"provider":"grok","text":FIRST_BODY});
    let created = post(&app, &cookie, &uri, "grok", body.clone()).await;
    assert_eq!(
        post(&app, &cookie, &uri, "grok", body).await["initialInput"],
        created["initialInput"]
    );
    let card = created["id"].as_str().unwrap();
    let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
        .await
        .unwrap();
    assert!(
        !crate::db::sqlite::conversation::card_has_canonical_storage(&mut tx, card)
            .await
            .unwrap()
    );
    tx.rollback().await.unwrap();
    assert_eq!(fixture.counts(card).await.0, 0);
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn acp_assistant_historical_creation_key_replays_original_evidence() {
    use crate::operation::planner_harness_start_adapter::OpeningBriefing;
    use crate::routes::track_conversations::{
        AssistantCreationEntry, AssistantProviderRequest, NewTrackConversationBody,
    };
    let mut fixture = Fixture::new().await;
    configure(&mut fixture);
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("Idempotency-Key", "historical-acp".parse().unwrap());
    let (_, crate::extract::Json(original)) =
        crate::routes::track_conversations::create_track_conversation_inner(
            crate::state::RouteState::from_ref(&state),
            crate::state::WorkerState::from_ref(&state),
            crate::actor::Actor::server_send(),
            headers,
            track.clone(),
            NewTrackConversationBody {
                provider: AssistantProviderRequest::Explicit(
                    calm_types::runtime::AgentProvider::OpenCode,
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
    let replay = post(
        &app,
        &cookie,
        &format!("/api/tracks/{track}/conversations"),
        "historical-acp",
        json!({"provider":"opencode","text":FIRST_BODY}),
    )
    .await;
    assert_eq!(replay["id"], original["id"]);
    assert_eq!(replay["initialInput"], original["initialInput"]);
    let card = original["id"].as_str().unwrap();
    let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
        .await
        .unwrap();
    assert!(
        !crate::db::sqlite::conversation::card_has_canonical_storage(&mut tx, card)
            .await
            .unwrap()
    );
    tx.rollback().await.unwrap();
    assert_eq!(fixture.counts(card).await.0, 0);
    fixture.shutdown_process(&state).await;
}
