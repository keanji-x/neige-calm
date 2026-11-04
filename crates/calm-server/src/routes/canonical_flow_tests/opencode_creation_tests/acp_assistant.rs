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
        prompts(&fixture, &AgentProvider::OpenCode),
        0,
        "receipt replay cannot wake accepted work"
    );
    let later = post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "wake",
        json!({"text":"retained activation"}),
    )
    .await;
    let batch = wait_idle(&fixture, card, 1).await;
    use crate::test_support::canonical_batch::{
        assert_native_text, assert_submission, human_ref, native_text,
    };
    assert_eq!(batch.header.phase, HarnessPhaseTag::TurnCompleted);
    assert!(batch.canonical_inputs.active.is_none());
    assert!(matches!(
        batch.canonical_inputs.attempts[0].delivery,
        InputDeliveryFact::Accepted { .. }
    ));
    assert_eq!(fixture.counts(card).await, (2, 2, 1, 1, 0));
    assert_submission(
        &fixture.repo,
        card,
        &batch.canonical_inputs.attempts[0].attempt.submission_id,
        &[
            (
                human_ref(
                    created["initialInput"]["receipt"]["entryId"]
                        .as_str()
                        .unwrap(),
                ),
                FIRST_BODY,
            ),
            (
                human_ref(later["entry_id"].as_str().unwrap()),
                "retained activation",
            ),
        ],
        (calm_types::organization::LEGACY_OWNER_USER_ID, "jian"),
    )
    .await;
    let sent: Vec<_> = requests(&fixture)
        .into_iter()
        .filter(|call| call["method"] == "session/prompt")
        .collect();
    assert_eq!(sent.len(), 1);
    assert_native_text(
        &native_text(&sent[0]["params"]["prompt"]),
        &[FIRST_BODY, "retained activation"],
        "jian",
    );
    assert_eq!(
        fixture
            .transcript_count(card, Some(&worker), Some("turn/completed"), None)
            .await,
        1
    );
    assert_eq!(carrier(&fixture, card).await.0, worker);
    assert_eq!(prompts(&fixture, &AgentProvider::OpenCode), 1);
    let identity: (String, String) =
        sqlx::query_as("SELECT thread_id,agent_session_id FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        batch.header.last_thread_id.as_deref(),
        Some(identity.0.as_str())
    );
    assert_eq!(
        batch.canonical_inputs.attempts[0]
            .correlation
            .native_session_id
            .as_deref(),
        Some(identity.1.as_str())
    );
    assert_eq!(sent[0]["params"]["sessionId"], identity.1);
    // A second activation must load the original native session on the same carrier.
    state.harness.shutdown_and_remove(&worker).await.unwrap();
    let continuation = post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "load",
        json!({"text":"load original worker"}),
    )
    .await;
    let continued = wait_idle(&fixture, card, 2).await;
    assert_eq!(continued.header.phase, HarnessPhaseTag::TurnCompleted);
    assert!(continued.canonical_inputs.active.is_none());
    assert_eq!(
        continued.canonical_inputs.attempts[0],
        batch.canonical_inputs.attempts[0]
    );
    let next = &continued.canonical_inputs.attempts[1];
    assert!(matches!(next.delivery, InputDeliveryFact::Accepted { .. }));
    assert_eq!(
        next.correlation.native_session_id.as_deref(),
        Some(identity.1.as_str())
    );
    assert_eq!(
        continued.header.last_thread_id.as_deref(),
        Some(identity.0.as_str())
    );
    assert_eq!(fixture.counts(card).await, (3, 3, 1, 2, 0));
    assert_submission(
        &fixture.repo,
        card,
        &next.attempt.submission_id,
        &[(
            human_ref(continuation["entry_id"].as_str().unwrap()),
            "load original worker",
        )],
        (calm_types::organization::LEGACY_OWNER_USER_ID, "jian"),
    )
    .await;
    let sent: Vec<_> = requests(&fixture)
        .into_iter()
        .filter(|call| call["method"] == "session/prompt")
        .collect();
    assert_eq!(sent.len(), 2);
    let text = native_text(&sent[1]["params"]["prompt"]);
    assert_native_text(&text, &["load original worker"], "jian");
    assert!(!text.contains(FIRST_BODY));
    assert!(!text.contains("retained activation"));
    assert_eq!(sent[1]["params"]["sessionId"], identity.1);
    assert_eq!(
        fixture
            .transcript_count(card, Some(&worker), Some("turn/completed"), None)
            .await,
        2
    );
    let native_requests = requests(&fixture);
    let loaded: Vec<_> = native_requests
        .iter()
        .filter(|call| call["method"] == "session/resume")
        .collect();
    assert_eq!(
        loaded.len(),
        1,
        "one native resume on the subsequent activation"
    );
    assert_eq!(loaded[0]["params"]["sessionId"], identity.1);
    assert_eq!(
        native_requests
            .iter()
            .filter(|call| call["method"] == "session/new")
            .count(),
        1
    );
    assert_eq!(carrier(&fixture, card).await.0, worker);
    let replay_wake = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "canonical-human-wake-finished",
        card,
        replay_wake.clone(),
    );
    assert_eq!(
        post(&app, &cookie, &uri, "assistant", body).await["initialInput"],
        created["initialInput"]
    );
    let woke = tokio::time::timeout(Duration::from_secs(1), replay_wake.entered.notified())
        .await
        .is_ok();
    replay_wake.release.notify_one();
    assert!(!woke, "completed receipt replay must not wake");
    let current_identity: (String, String) =
        sqlx::query_as("SELECT thread_id,agent_session_id FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(current_identity, identity);
    assert_eq!(prompts(&fixture, &AgentProvider::OpenCode), 2);
    assert_eq!(
        carrier(&fixture, card).await.1.canonical_inputs,
        continued.canonical_inputs
    );
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
        prompts(&fixture, &AgentProvider::OpenCode),
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
                && prompts(&fixture, &AgentProvider::OpenCode) == 1
            {
                break facts;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(prompts(&fixture, &AgentProvider::OpenCode), 1);
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
    assert_eq!(prompts(&fixture, &AgentProvider::OpenCode), 1);
    let (current, after) = carrier(&fixture, card).await;
    assert_eq!(current, worker);
    assert_eq!(
        after.canonical_inputs.attempts,
        before.canonical_inputs.attempts
    );
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn acp_assistant_historical_creation_key_replays_original_evidence() {
    historical_creation_key_replays_original_evidence(AgentProvider::OpenCode).await;
}

#[tokio::test]
async fn g3_grok_assistant_historical_key_replays_original_evidence() {
    historical_creation_key_replays_original_evidence(AgentProvider::Grok).await;
}

async fn historical_creation_key_replays_original_evidence(provider: AgentProvider) {
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
                provider: AssistantProviderRequest::Explicit(provider.clone()),
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
        json!({"provider":provider.wire_name(),"text":FIRST_BODY}),
    )
    .await;
    assert_eq!(replay["id"], original["id"]);
    assert_eq!(replay["initialInput"], original["initialInput"]);
    let card = original["id"].as_str().unwrap();
    let write_db = (fixture.repo.db()).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await.unwrap();
                assert!(
                    !crate::db::sqlite::conversation::card_has_canonical_storage(&mut tx, card)
                        .await
                        .unwrap()
                );
                tx.rollback().await.unwrap();
                assert_eq!(fixture.counts(card).await.0, 0);
                fixture.shutdown_process(&state).await;
            })
        })
        .await
        .expect("independent write operation")
}

#[tokio::test]
async fn c4_acp_planner_switch_next_turns_use_engine_and_inherit_permission() {
    use crate::planner_switch::{SwitchPlannerRequest, switch_planner};
    use calm_truth::db::sqlite::track_planner::PlannerRevision;
    let mut fixture = Fixture::new().await;
    configure(&mut fixture);
    let (state, app, cookie, auth) = fixture.application_with_auth(false).await;
    state.dispatcher.abort_event_listener_for_test();
    let request = body(&app, &cookie, &AgentProvider::OpenCode).await;
    let created = post(&app, &cookie, "/api/tracks", "switch-track", request).await;
    let track = created["id"].as_str().unwrap();
    let (planner, _, _) = fixture.carrier(track).await;
    wait_idle(&fixture, &planner, 1).await;
    let assistant = post(
        &app,
        &cookie,
        &format!("/api/tracks/{track}/conversations"),
        "switch-assistant",
        json!({"provider":"opencode","text":"Assistant before switch"}),
    )
    .await;
    let assistant = assistant["id"].as_str().unwrap();
    wait_idle(&fixture, assistant, 1).await;
    // The promoted card inherits the outgoing Planner's mode, then its profile
    // composes the next process environment through the execution owner.
    (fixture.repo.db())
        .writer()
        .execute(
            sqlx::query(
                "UPDATE cards SET payload=json_set(payload,'$.permission_mode','full') WHERE id=?",
            )
            .bind(&planner),
        )
        .await
        .unwrap();
    for card in [&planner, assistant] {
        let (worker, frame) = carrier(&fixture, card).await;
        // The exact completed prompt receipt confirms delivery. Switching must
        // no longer require a Human decision for an ordinary completed turn.
        assert!(matches!(
            frame.canonical_inputs.attempts[0].delivery,
            InputDeliveryFact::Accepted { .. }
        ));
        assert!(frame.canonical_inputs.receipts.is_empty());
        let handle = state.harness.get(&worker).unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while handle.idle_carrier_owner().await.is_err() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(header::COOKIE, cookie.parse().unwrap());
    let (_, human) = crate::auth::authenticated_source(auth, headers).await;
    let receipt = switch_planner(
        &crate::state::RouteState::from_ref(&state),
        &crate::actor::Actor("user".into()),
        &human.unwrap(),
        SwitchPlannerRequest {
            track_id: track.into(),
            expected_planner_card_id: planner.clone().into(),
            target_assistant_card_id: assistant.into(),
            expected_revision: PlannerRevision(0),
        },
    )
    .await
    .unwrap();
    for (card, replacement) in [
        (planner.as_str(), &receipt.demoted),
        (assistant, &receipt.promoted),
    ] {
        post(
            &app,
            &cookie,
            &format!("/api/cards/{card}/planner/input"),
            &format!("after-switch-{card}"),
            json!({"text":"After switch through engine"}),
        )
        .await;
        wait_idle(&fixture, card, 1).await;
        let metadata: Vec<String> = sqlx::query_scalar(
            "SELECT intent_json FROM workspace_executions WHERE json_extract(intent_json,'$.owner')=?",
        )
        .bind(format!("worker-session:{}", replacement.new_worker.id))
        .fetch_all(fixture.repo.pool())
        .await
        .unwrap();
        assert_eq!(metadata.len(), 1, "one Engine process for the new carrier");
        let metadata: workspace_runtime::execution::ExecutionMetadata =
            serde_json::from_str(&metadata[0]).unwrap();
        assert_eq!(metadata.consumer, "acp-planner".into());
        assert!(
            !metadata
                .program
                .environment
                .iter()
                .any(|(key, _)| key == "NEIGE_MCP_TOKEN")
        );
        let config = metadata
            .program
            .environment
            .iter()
            .find(|(key, _)| key == "OPENCODE_CONFIG_CONTENT")
            .unwrap();
        if card == assistant {
            assert_eq!(
                serde_json::from_str::<Value>(&config.1).unwrap()["permissions"][0]["effect"],
                "allow"
            );
        }
    }
    assert_eq!(prompts(&fixture, &AgentProvider::OpenCode), 4);
    fixture.shutdown_process(&state).await;
}
