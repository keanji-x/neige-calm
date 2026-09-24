use super::*;

async fn assert_cold_profile(role: CardRole, marker: &str, expects_mcp: bool) {
    let _env = ENV_LOCK.lock().await;
    let capture = TempDir::new().unwrap();
    let path = capture.path().join("wire.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &path);
    }
    let boot = boot_shared().await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
    }
    let (card, runtime, thread, _) = failed_conversation(&boot).await;
    sqlx::query("UPDATE cards SET role=?,payload=? WHERE id=?")
        .bind(role.as_db_str())
        .bind(json!({"schemaVersion":1,"harness_profile":marker}).to_string())
        .bind(card.id.as_str())
        .execute(boot.repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET contract='executor' WHERE id=?")
        .bind(&runtime)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    boot.state
        .card_role_cache
        .insert(card.id.clone(), role, card.track_id.clone());
    let sock = boot._tmp.path().join("run/codex-appserver.sock");
    std::fs::write(
        sock.with_extension("thread-read"),
        json!({"thread":{
            "id":thread,"status":{"type":"notLoaded"},"turns":[]
        }})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        sock.with_extension("thread-resume"),
        json!({"thread":{
            "id":thread,"status":{"type":"idle"}
        }})
        .to_string(),
    )
    .unwrap();
    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({"text":"continue plain chat"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let requests = request_lines_containing(&path, "thread/resume", 1).await;
    assert_eq!(
        requests[0]["params"].get("config").is_some(),
        expects_mcp,
        "recovery must preserve {marker}'s capability policy"
    );
    let token: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?")
            .bind(&runtime)
            .fetch_one(boot.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        token.is_some(),
        expects_mcp,
        "plain chat must not mint credentials"
    );
    boot.state
        .harness
        .remove(&runtime)
        .unwrap()
        .shutdown()
        .await
        .unwrap();
}

#[tokio::test]
async fn cold_plain_chat_recovery_preserves_its_no_mcp_profile() {
    assert_cold_profile(CardRole::Worker, "plain_chat", false).await;
}

#[tokio::test]
async fn cold_assistant_recovery_preserves_its_mcp_profile() {
    assert_cold_profile(CardRole::Assistant, "assistant", true).await;
}

#[tokio::test]
async fn ineligible_failed_sessions_do_not_advertise_send_to_resume() {
    for change in [
        "UPDATE worker_sessions SET queue_harvested_at_ms=1 WHERE id=?",
        "UPDATE tracks SET lifecycle='done' WHERE id=(SELECT track_id FROM worker_sessions WHERE id=?)",
        "UPDATE worker_sessions SET handle_state_json=json_set(handle_state_json,'$.last_thread_id','other-thread') WHERE id=?",
    ] {
        let boot = boot_fake_running().await;
        let (card, runtime, _, _) = failed_conversation(&boot).await;
        sqlx::query(change)
            .bind(&runtime)
            .execute(boot.repo.pool())
            .await
            .unwrap();
        let response = boot
            .app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/cards/{}/planner/run", card.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert!(
            !body["blocked_reason"]
                .as_str()
                .is_some_and(|text| text.contains("send a message to resume")),
            "{change}: {body}"
        );
    }
}

#[tokio::test]
async fn duplicate_failed_completions_have_one_persisted_outcome() {
    let boot = boot_fake_running().await;
    let (card, runtime, thread, _) = failed_conversation(&boot).await;
    let row = runtime_by_id_tx_snapshot(&boot.repo, &runtime)
        .await
        .unwrap();
    let harness = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: runtime,
        card_id: card.id.clone(),
        track_id: card.track_id.clone(),
        thread_id: Some(thread.clone()),
        repo: boot.repo.clone(),
        events: boot.state.events.clone(),
        card_role_cache: boot.state.card_role_cache.clone(),
        track_area_cache: boot.state.track_area_cache.clone(),
        backend: boot.state.shared_codex_appserver.clone().into(),
        config: HarnessConfig::default(),
        snapshot: HarnessSnapshot::from_value_strict(row.handle_state_json.unwrap()),
    });
    let daemon = &boot.state.shared_codex_appserver;
    for _ in 0..2 {
        daemon.emit_notification_for_test(calm_server::codex_appserver::Notification::TurnCompleted {
            thread_id:thread.clone(),turn:json!({"id":"failed-turn","status":"failed","error":{"message":"Usage limit exceeded"}}),
        });
    }
    // This later notification on the same FIFO proves both completions ran.
    daemon.emit_notification_for_test(calm_server::codex_appserver::Notification::Item {
        method:"item/completed".into(), params:json!({"threadId":thread,"turnId":"failed-turn","item":{"id":"completion-barrier","type":"agentMessage","text":"barrier"}}),
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let rows = conversation_rows(&boot, &card).await;
        if rows
            .iter()
            .any(|r| r["item_uuid"].as_str() == Some("completion-barrier"))
        {
            assert_eq!(
                rows.iter()
                    .filter(|r| r["method"] == "turn/completed")
                    .count(),
                1,
                "duplicate provider notification must not duplicate the visible error"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "completion barrier was not consumed"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn cold_daemon_replay_keeps_active_plain_chat_without_mcp() {
    use calm_server::shared_codex_appserver::{
        ReplacePrecondition, SharedThreadStartParams, ThreadConfig,
    };
    let _env = ENV_LOCK.lock().await;
    let capture = TempDir::new().unwrap();
    let path = capture.path().join("wire.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &path);
    }
    let boot = boot_shared().await;
    let (card, runtime, _, _) = failed_conversation(&boot).await;
    sqlx::query("UPDATE cards SET role='worker',payload=? WHERE id=?")
        .bind(json!({"schemaVersion":1,"harness_profile":"plain_chat"}).to_string())
        .bind(card.id.as_str())
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let thread = boot
        .state
        .shared_codex_appserver
        .thread_start_for_card(
            card.id.as_str(),
            CardRole::Worker,
            Some(&boot.track_id),
            SharedThreadStartParams {
                cwd: boot._tmp.path().to_string_lossy().into_owned(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
            },
        )
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET state='idle',contract='executor',thread_id=?2,handle_state_json=?3 WHERE id=?1")
        .bind(&runtime).bind(&thread).bind(idle_snapshot_value(&thread).to_string()).execute(boot.repo.pool()).await.unwrap();
    let replaced = boot
        .state
        .shared_codex_appserver
        .transition_replace_for_test("plain chat capability replay", ReplacePrecondition::Always)
        .await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
    }
    replaced.unwrap();
    let requests = request_lines_containing(&path, "thread/resume", 1).await;
    assert!(
        requests[0]["params"].get("config").is_none(),
        "ordinary daemon replay must honor the same no-MCP profile"
    );
    let token: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?")
            .bind(&runtime)
            .fetch_one(boot.repo.pool())
            .await
            .unwrap();
    assert!(token.is_none());
}

/// #1791: a cold daemon replay classifies a Planner card by `PlannerBinding`, so a Planner card
/// without a known `planner_provider` is not a harness card and gets no MCP credential.
async fn cold_daemon_replay_of_planner(payload: Value) -> (bool, bool) {
    use calm_server::shared_codex_appserver::{
        ReplacePrecondition, SharedThreadStartParams, ThreadConfig,
    };
    let _env = ENV_LOCK.lock().await;
    let capture = TempDir::new().unwrap();
    let path = capture.path().join("wire.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &path);
    }
    let boot = boot_shared().await;
    let (card, runtime, _, _) = failed_conversation(&boot).await;
    sqlx::query("UPDATE cards SET payload=? WHERE id=?")
        .bind(payload.to_string())
        .bind(card.id.as_str())
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let thread = boot
        .state
        .shared_codex_appserver
        .thread_start_for_card(
            card.id.as_str(),
            CardRole::Planner,
            Some(&boot.track_id),
            SharedThreadStartParams {
                cwd: boot._tmp.path().to_string_lossy().into_owned(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
            },
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE worker_sessions SET state='idle',thread_id=?2,handle_state_json=?3 WHERE id=?1",
    )
    .bind(&runtime)
    .bind(&thread)
    .bind(idle_snapshot_value(&thread).to_string())
    .execute(boot.repo.pool())
    .await
    .unwrap();
    let replaced = boot
        .state
        .shared_codex_appserver
        .transition_replace_for_test("planner provider replay", ReplacePrecondition::Always)
        .await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
    }
    replaced.unwrap();
    let requests = request_lines_containing(&path, "thread/resume", 1).await;
    let token: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?")
            .bind(&runtime)
            .fetch_one(boot.repo.pool())
            .await
            .unwrap();
    (
        requests[0]["params"].get("config").is_some(),
        token.is_some(),
    )
}

#[tokio::test]
async fn cold_daemon_replay_gives_a_codex_planner_its_mcp_credential() {
    let payload = json!({"schemaVersion":1,"planner_harness":true,"planner_provider":"codex"});
    assert_eq!(cold_daemon_replay_of_planner(payload).await, (true, true));
}

#[tokio::test]
async fn cold_daemon_replay_gives_a_planner_without_a_codex_provider_no_mcp() {
    for payload in [
        json!({"schemaVersion":1,"planner_harness":true}),
        json!({"schemaVersion":1,"planner_harness":true,"planner_provider":"gpt"}),
        json!({"schemaVersion":1,"planner_harness":true,"planner_provider":"claude"}),
    ] {
        assert_eq!(
            cold_daemon_replay_of_planner(payload.clone()).await,
            (false, false),
            "{payload}"
        );
    }
}

/// #1791: preserving recovery resumes on the Codex app-server, so only a Codex binding may take it.
#[tokio::test]
async fn a_failed_planner_bound_to_claude_is_not_resumed_on_codex() {
    let boot = boot_fake_running().await;
    let (card, runtime, _, _) = failed_conversation(&boot).await;
    sqlx::query(
        "UPDATE cards SET payload = json_set(payload, '$.planner_provider', 'claude') WHERE id = ?",
    )
    .bind(card.id.as_str())
    .execute(boot.repo.pool())
    .await
    .unwrap();
    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({"text":"must not resume on codex"}),
    )
    .await;
    assert!(!status.is_success(), "{status} {body}");
    let row = runtime_by_id_tx_snapshot(&boot.repo, &runtime)
        .await
        .unwrap();
    assert_eq!(row.status, WorkerSessionState::Failed);
}
