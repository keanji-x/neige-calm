use super::*;
use calm_server::harness::{HarnessState, QueueEntry};

async fn failed_conversation(boot: &Boot) -> (Card, String, String, QueueEntry) {
    let card = seed_codex_card_with_role(boot, CardRole::Planner).await;
    sqlx::query("UPDATE cards SET role = 'planner' WHERE id = ?")
        .bind(card.id.as_str())
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let thread = format!("retained-thread-{}", new_id());
    let attachment_path = boot._tmp.path().join("retained.png");
    std::fs::write(&attachment_path, b"retained attachment").unwrap();
    let attachment = calm_server::planner_attachments::bind::BoundAttachment {
        id: calm_types::planner_attachment::AttachmentId::parse(&format!(
            "{}.png",
            uuid::Uuid::new_v4()
        ))
        .unwrap(),
        size: 19,
        path: attachment_path.to_string_lossy().into_owned(),
    };
    let entry = QueueEntry::user_message("already queued".into(), None, vec![attachment]);
    let mut snapshot = HarnessSnapshot::initial(0, vec![entry.clone()]);
    snapshot.phase = HarnessPhaseTag::Wedged;
    snapshot.wedged_reason = Some("system_error".into());
    snapshot.last_thread_id = Some(thread.clone());
    snapshot.last_turn_id = Some("failed-turn".into());
    let runtime = seed_planner_runtime_row_with_status(
        boot,
        &card,
        Some(thread.clone()),
        Some(serde_json::to_value(snapshot).unwrap()),
        WorkerSessionState::Failed,
    )
    .await;
    boot.repo
        .harness_item_insert(
            &runtime,
            card.id.as_str(),
            &boot.track_id,
            &thread,
            Some("old-turn"),
            Some("old-answer"),
            Some("agentMessage"),
            "item/completed",
            r#"{"item":{"id":"old-answer","type":"agentMessage","text":"history must survive"}}"#,
            None,
        )
        .await
        .unwrap();
    (card, runtime, thread, entry)
}

#[tokio::test]
async fn human_send_recovers_failed_conversation_without_reset_after_restart() {
    let boot = boot_fake_running().await;
    boot.state.shared_codex_appserver.fail_turn_start_for_test();
    let (card, runtime, thread, entry) = failed_conversation(&boot).await;
    let before = boot
        .repo
        .harness_item_list_by_card(card.id.as_str(), 0, 100, false)
        .await
        .unwrap();
    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({"text":"continue here"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["worker_session_id"], runtime);
    let row = runtime_by_id_tx_snapshot(&boot.repo, &runtime)
        .await
        .unwrap();
    assert_eq!(row.thread_id.as_deref(), Some(thread.as_str()));
    assert_ne!(row.status, WorkerSessionState::Failed);
    let snapshot = HarnessSnapshot::from_value_strict(row.handle_state_json.unwrap());
    assert!(
        snapshot.pending_entries().contains(&entry),
        "queued identity and contents survive"
    );
    let after = boot
        .repo
        .harness_item_list_by_card(card.id.as_str(), 0, 100, false)
        .await
        .unwrap();
    for item in before {
        assert!(
            after
                .iter()
                .any(|retained| retained.id == item.id && retained.params == item.params)
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM worker_sessions WHERE card_id = ?")
        .bind(card.id.as_str())
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 1, "recovery must not mint a new session");
    assert!(
        boot.state
            .shared_codex_appserver
            .started_thread_params_for_test()
            .is_empty()
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
async fn system_error_completion_keeps_original_error_without_automatic_retry() {
    let boot = boot_fake_running().await;
    let (card, runtime, thread, _) = failed_conversation(&boot).await;
    // Start in the state immediately before the production notification sequence.
    let mut tx = boot.repo.pool().begin().await.unwrap();
    sqlx::query("UPDATE worker_sessions SET state='turn_pending' WHERE id=?")
        .bind(&runtime)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::TurnRunning;
    snapshot.last_turn_id = Some("failed-turn".into());
    snapshot.last_thread_id = Some(thread.clone());
    let harness = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: runtime.clone(),
        card_id: card.id.clone(),
        track_id: card.track_id.clone(),
        thread_id: Some(thread.clone()),
        repo: boot.repo.clone(),
        events: boot.state.events.clone(),
        card_role_cache: boot.state.card_role_cache.clone(),
        track_area_cache: boot.state.track_area_cache.clone(),
        daemon: boot.state.shared_codex_appserver.clone(),
        config: HarnessConfig::default(),
        snapshot,
    });
    let daemon = &boot.state.shared_codex_appserver;
    daemon.emit_notification_for_test(
        calm_server::codex_appserver::Notification::ThreadStatusChanged {
            thread_id: thread.clone(),
            status: json!({"type":"systemError"}),
        },
    );
    daemon.emit_notification_for_test(calm_server::codex_appserver::Notification::TurnCompleted {
        thread_id: thread.clone(), turn: json!({"id":"failed-turn","status":"failed", "error":{
            "message":"Usage limit exceeded; retry after replenishing quota.","codexErrorInfo":"usageLimitExceeded"}}),
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let items = boot
            .repo
            .harness_item_list_by_card(card.id.as_str(), 0, 100, false)
            .await
            .unwrap();
        if let Some(item) = items.iter().find(|item| item.method == "turn/completed") {
            assert!(item.params.contains("Usage limit exceeded"));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "matching failed completion must be persisted after systemError"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM events WHERE scope_card=? AND kind='harness.item.added' AND json_extract(payload,'$.method')='turn/completed'")
            .bind(card.id.as_str()).fetch_one(boot.repo.pool()).await.unwrap();
        if count == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "saved error needs a post-insert browser invalidation"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(matches!(
        harness.state_for_test().await,
        HarnessState::Wedged { .. }
    ));
    assert_eq!(daemon.turn_start_count_for_test(), 0);
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_conversation_resume_failure_preserves_every_durable_field() {
    let boot = boot_fake_running().await;
    let (card, runtime, _, _) = failed_conversation(&boot).await;
    boot.state
        .shared_codex_appserver
        .fail_thread_resume_for_test();
    let before = runtime_by_id_tx_snapshot(&boot.repo, &runtime)
        .await
        .unwrap();
    let (status, _) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({"text":"must not be accepted"}),
    )
    .await;
    assert!(!status.is_success());
    let after = runtime_by_id_tx_snapshot(&boot.repo, &runtime)
        .await
        .unwrap();
    assert_eq!(before, after);
    assert!(boot.state.harness.get(&runtime).is_none());
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events WHERE scope_card=? AND kind='harness.user_message.enqueued'",
    )
    .bind(card.id.as_str())
    .fetch_one(boot.repo.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn concurrent_human_sends_recover_the_original_conversation_once() {
    let boot = boot_fake_running().await;
    boot.state.shared_codex_appserver.fail_turn_start_for_test();
    let (card, runtime, thread, _) = failed_conversation(&boot).await;
    let uri = format!("/api/cards/{}/planner/input", card.id);
    let (a, b) = tokio::join!(
        post_json(boot.app.clone(), &uri, json!({"text":"first"})),
        post_json(boot.app.clone(), &uri, json!({"text":"second"}))
    );
    assert_eq!(a.0, StatusCode::OK, "{:?}", a.1);
    assert_eq!(b.0, StatusCode::OK, "{:?}", b.1);
    assert_eq!(a.1["worker_session_id"], runtime);
    assert_eq!(b.1["worker_session_id"], runtime);
    assert_eq!(
        boot.state.shared_codex_appserver.resumed_threads_for_test(),
        vec![(thread, false)]
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events WHERE scope_card=? AND kind='harness.user_message.enqueued'",
    )
    .bind(card.id.as_str())
    .fetch_one(boot.repo.pool())
    .await
    .unwrap();
    assert_eq!(count, 2);
    boot.state
        .harness
        .remove(&runtime)
        .unwrap()
        .shutdown()
        .await
        .unwrap();
}

#[tokio::test]
async fn a_live_failed_loop_is_replaced_without_interrupting_or_resetting_its_thread() {
    let boot = boot_fake_running().await;
    boot.state.shared_codex_appserver.fail_turn_start_for_test();
    let (card, runtime, thread, _) = failed_conversation(&boot).await;
    let row = runtime_by_id_tx_snapshot(&boot.repo, &runtime)
        .await
        .unwrap();
    let old = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: runtime.clone(),
        card_id: card.id.clone(),
        track_id: card.track_id.clone(),
        thread_id: Some(thread),
        repo: boot.repo.clone(),
        events: boot.state.events.clone(),
        card_role_cache: boot.state.card_role_cache.clone(),
        track_area_cache: boot.state.track_area_cache.clone(),
        daemon: boot.state.shared_codex_appserver.clone(),
        config: HarnessConfig::default(),
        snapshot: HarnessSnapshot::from_value_strict(row.handle_state_json.unwrap()),
    });
    boot.state.harness.insert(runtime.clone(), old.clone());
    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({"text":"resume"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        old.observe(Observation::TrackGoal {
            text: "stale delivery".into()
        })
        .is_err()
    );
    assert!(
        boot.state
            .shared_codex_appserver
            .interrupted_turns_for_test()
            .is_empty()
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
async fn recovery_refuses_every_retired_or_untrusted_carrier() {
    for change in [
        "UPDATE worker_sessions SET state='exited' WHERE id=?",
        "UPDATE worker_sessions SET state='superseded' WHERE id=?",
        "UPDATE worker_sessions SET completed_at_ms=1 WHERE id=?",
        "UPDATE worker_sessions SET queue_harvested_at_ms=1 WHERE id=?",
        "UPDATE worker_sessions SET handle_state_json=json_set(handle_state_json,'$.wedged_reason','interrupt_timeout') WHERE id=?",
        "UPDATE worker_sessions SET handle_state_json=json_set(handle_state_json,'$.schema_version',999) WHERE id=?",
        "UPDATE worker_sessions SET handle_state_json=json_set(handle_state_json,'$.last_thread_id','different-thread') WHERE id=?",
        "UPDATE worker_sessions SET thread_id=NULL,handle_state_json=json_remove(handle_state_json,'$.last_thread_id') WHERE id=?",
        "UPDATE tracks SET lifecycle='done' WHERE id=(SELECT track_id FROM worker_sessions WHERE id=?)",
        "UPDATE cards SET session_id=NULL WHERE session_id=?",
    ] {
        let boot = boot_fake_running().await;
        let (card, runtime, _, _) = failed_conversation(&boot).await;
        sqlx::query(change)
            .bind(&runtime)
            .execute(boot.repo.pool())
            .await
            .unwrap();
        let before: (String, String) =
            sqlx::query_as("SELECT state,handle_state_json FROM worker_sessions WHERE id=?")
                .bind(&runtime)
                .fetch_one(boot.repo.pool())
                .await
                .unwrap();
        let (status, body) = post_json(
            boot.app.clone(),
            &format!("/api/cards/{}/planner/input", card.id),
            json!({"text":"do not revive"}),
        )
        .await;
        assert!(!status.is_success(), "{change}: {body}");
        let after: (String, String) =
            sqlx::query_as("SELECT state,handle_state_json FROM worker_sessions WHERE id=?")
                .bind(&runtime)
                .fetch_one(boot.repo.pool())
                .await
                .unwrap();
        assert_eq!(before, after, "{change}");
        assert!(
            boot.state
                .shared_codex_appserver
                .resumed_threads_for_test()
                .is_empty(),
            "{change}"
        );
    }
}

#[tokio::test]
async fn machine_authored_input_cannot_resume_a_failed_conversation() {
    for actor in ["ai:codex", "ai:planner", "ai:claude"] {
        let boot = boot_fake_running().await;
        let (card, runtime, _, _) = failed_conversation(&boot).await;
        let (status, body) = post_json_with_actor(
            boot.app.clone(),
            &format!("/api/cards/{}/planner/input", card.id),
            json!({"text":"machine retry"}),
            actor,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{actor}: {body}");
        assert_eq!(
            runtime_by_id_tx_snapshot(&boot.repo, &runtime)
                .await
                .unwrap()
                .status,
            WorkerSessionState::Failed
        );
        assert!(
            boot.state
                .shared_codex_appserver
                .resumed_threads_for_test()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn preserving_recovery_uses_exact_thread_resume_wire_for_hot_and_cold_threads() {
    let _env = ENV_LOCK.lock().await;
    for cold in [false, true] {
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
        let sock = boot._tmp.path().join("run/codex-appserver.sock");
        std::fs::write(
            sock.with_extension("thread-read"),
            json!({"thread":{
                "id":thread,"status":{"type":if cold {"notLoaded"} else {"systemError"}},"turns":[]
            }})
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            sock.with_extension("thread-resume"),
            json!({"thread":{
                "id":thread,"status":{"type":"idle"},"turns":[]
            }})
            .to_string(),
        )
        .unwrap();
        let (status, body) = post_json(
            boot.app.clone(),
            &format!("/api/cards/{}/planner/input", card.id),
            json!({"text":"continue"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "cold={cold}: {body}");
        let reads = request_lines_containing(&path, "thread/read", 1).await;
        assert_eq!(reads[0]["params"]["threadId"], thread);
        assert_eq!(reads[0]["params"]["includeTurns"], true);
        let resumes = request_lines_containing(&path, "thread/resume", 1).await;
        assert_eq!(resumes.len(), 1);
        assert_eq!(resumes[0]["params"]["threadId"], thread);
        let config = resumes[0]["params"].get("config");
        assert_eq!(
            config.is_some(),
            cold,
            "loaded thread credentials must not rotate"
        );
        if let Some(config) = config {
            let raw = config
                .pointer("/shell_environment_policy/set/NEIGE_MCP_TOKEN")
                .and_then(Value::as_str)
                .expect("cold resume credentials");
            use sha2::{Digest, Sha256};
            let hash = hex::encode(Sha256::digest(raw.as_bytes()));
            let persisted: String =
                sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?")
                    .bind(&runtime)
                    .fetch_one(boot.repo.pool())
                    .await
                    .unwrap();
            assert_eq!(
                hash, persisted,
                "resumed thread token must belong to the same carrier"
            );
        }
        let all = std::fs::read_to_string(&path).unwrap();
        assert!(
            !all.lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .any(|r| r["method"] == "thread/start")
        );
        assert_eq!(
            runtime_by_id_tx_snapshot(&boot.repo, &runtime)
                .await
                .unwrap()
                .thread_id
                .as_deref(),
            Some(thread.as_str())
        );
        boot.state
            .harness
            .remove(&runtime)
            .unwrap()
            .shutdown()
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn planner_run_exposes_retained_queue_and_send_to_resume_notice() {
    let boot = boot_fake_running().await;
    let (card, _, _, entry) = failed_conversation(&boot).await;
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
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        body["phase"].is_null(),
        "failed carrier must remain sendable in the existing composer"
    );
    assert!(body["worker_session_id"].is_null());
    assert!(
        body["blocked_reason"]
            .as_str()
            .unwrap()
            .contains("send a message to resume")
    );
    assert_eq!(body["pending"][0]["entry_id"], entry.id().unwrap().as_str());
}

#[tokio::test]
async fn recovery_never_interrupts_a_provider_turn_that_is_still_active() {
    let boot = boot_fake_running().await;
    let (card, runtime, thread, _) = failed_conversation(&boot).await;
    boot.state
        .shared_codex_appserver
        .set_active_turn_for_test(&thread, "still-running");
    let before = runtime_by_id_tx_snapshot(&boot.repo, &runtime)
        .await
        .unwrap();
    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({"text":"wait for settlement"}),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(
        runtime_by_id_tx_snapshot(&boot.repo, &runtime)
            .await
            .unwrap(),
        before
    );
    assert!(
        boot.state
            .shared_codex_appserver
            .interrupted_turns_for_test()
            .is_empty()
    );
    assert!(
        boot.state
            .shared_codex_appserver
            .resumed_threads_for_test()
            .is_empty()
    );
}

#[tokio::test]
async fn cold_daemon_replacement_leaves_failed_threads_for_credentialed_human_recovery() {
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
    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
    }
    let (card, runtime, _, _) = failed_conversation(&boot).await;
    // Populate the daemon's real attribution cache, as the original start did.
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
    sqlx::query("UPDATE worker_sessions SET thread_id=?2,handle_state_json=json_set(handle_state_json,'$.last_thread_id',?2) WHERE id=?1")
        .bind(&runtime).bind(&thread).execute(boot.repo.pool()).await.unwrap();
    assert!(
        boot.state
            .shared_codex_appserver
            .resume_candidates_for_test()
            .contains(&(thread.clone(), card.id.to_string()))
    );
    let sock = boot._tmp.path().join("run/codex-appserver.sock");
    boot.state
        .shared_codex_appserver
        .transition_replace_for_test("test cold failed harness", ReplacePrecondition::Always)
        .await
        .unwrap();
    let methods = std::fs::read_to_string(sock.with_extension("methods")).unwrap();
    assert!(
        !methods.lines().any(|m| m == "thread/resume"),
        "failed thread must not be cold-loaded without credentials: {methods}"
    );
    std::fs::write(
        sock.with_extension("thread-read"),
        json!({"thread":{"id":thread,"status":{"type":"notLoaded"},"turns":[]}}).to_string(),
    )
    .unwrap();
    std::fs::write(
        sock.with_extension("thread-resume"),
        json!({"thread":{"id":thread,"status":{"type":"idle"},"turns":[]}}).to_string(),
    )
    .unwrap();
    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({"text":"resume with tools intact"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Replacement is spawned after the global capture env was removed; the
    // socket fixture's method journal still proves the explicit resume happened.
    let methods = std::fs::read_to_string(sock.with_extension("methods")).unwrap();
    assert_eq!(methods.lines().filter(|m| *m == "thread/resume").count(), 1);
    let hash: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?")
            .bind(&runtime)
            .fetch_one(boot.repo.pool())
            .await
            .unwrap();
    assert!(
        hash.is_some(),
        "cold human recovery must install this session's MCP credential"
    );
    assert_eq!(
        runtime_by_id_tx_snapshot(&boot.repo, &runtime)
            .await
            .unwrap()
            .thread_id
            .as_deref(),
        Some(thread.as_str())
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
async fn recovery_backfills_the_matching_error_when_quiescence_precedes_completion() {
    let boot = boot_shared().await;
    let (card, runtime, thread, _) = failed_conversation(&boot).await;
    let before = runtime_by_id_tx_snapshot(&boot.repo, &runtime)
        .await
        .unwrap();
    let old = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: runtime.clone(),
        card_id: card.id.clone(),
        track_id: card.track_id.clone(),
        thread_id: Some(thread.clone()),
        repo: boot.repo.clone(),
        events: boot.state.events.clone(),
        card_role_cache: boot.state.card_role_cache.clone(),
        track_area_cache: boot.state.track_area_cache.clone(),
        daemon: boot.state.shared_codex_appserver.clone(),
        config: HarnessConfig::default(),
        snapshot: HarnessSnapshot::from_value_strict(before.handle_state_json.unwrap()),
    });
    boot.state.harness.insert(runtime.clone(), old);
    let sock = boot._tmp.path().join("run/codex-appserver.sock");
    std::fs::write(sock.with_extension("thread-read"),json!({"thread":{"id":thread,"status":{"type":"systemError"},"turns":[
        {"id":"unrelated-turn","status":"failed","error":{"message":"unrelated failure"}},
        {"id":"failed-turn","status":"failed","error":{"message":"Usage limit exceeded","codexErrorInfo":"usageLimitExceeded"},"items":[{"text":"must not duplicate historical messages"}]}
    ]}}).to_string()).unwrap();
    std::fs::write(
        sock.with_extension("thread-resume"),
        json!({"thread":{"id":thread,"status":{"type":"idle"}}}).to_string(),
    )
    .unwrap();
    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({"text":"continue after quota replenishment"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = boot
        .repo
        .harness_item_list_by_card(card.id.as_str(), 0, 100, false)
        .await
        .unwrap();
    let outcomes = rows
        .iter()
        .filter(|r| r.method == "turn/completed")
        .collect::<Vec<_>>();
    assert_eq!(outcomes.len(), 1);
    let outcome: Value = serde_json::from_str(&outcomes[0].params).unwrap();
    assert_eq!(outcome["id"], "failed-turn");
    assert_eq!(outcome["error"]["message"], "Usage limit exceeded");
    assert!(outcome.get("items").is_none());
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM events WHERE scope_card=? AND kind='harness.item.added' AND json_extract(payload,'$.item_db_id')=?")
        .bind(card.id.as_str()).bind(outcomes[0].id).fetch_one(boot.repo.pool()).await.unwrap();
    assert_eq!(
        count, 1,
        "recovered original error must refresh the open browser"
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
async fn a_successor_committed_during_provider_resume_cannot_be_overwritten() {
    let boot = boot_shared().await;
    let (card, runtime, thread, _) = failed_conversation(&boot).await;
    let sock = boot._tmp.path().join("run/codex-appserver.sock");
    std::fs::write(
        sock.with_extension("thread-read"),
        json!({"thread":{"id":thread,"status":{"type":"systemError"},"turns":[]}}).to_string(),
    )
    .unwrap();
    std::fs::write(
        sock.with_extension("thread-resume"),
        json!({"thread":{"id":thread,"status":{"type":"idle"}}}).to_string(),
    )
    .unwrap();
    std::fs::write(sock.with_extension("hold-first-resume"), "").unwrap();
    let app = boot.app.clone();
    let uri = format!("/api/cards/{}/planner/input", card.id);
    let pending = tokio::spawn(async move {
        post_json(
            app,
            &uri,
            json!({"text":"must not reach a different session"}),
        )
        .await
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !sock.with_extension("held-resume").exists() {
        assert!(
            Instant::now() < deadline,
            "provider resume did not reach its hold"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let successor = seed_planner_runtime_row_with_status(
        &boot,
        &card,
        Some("successor-thread".into()),
        Some(idle_snapshot_value("successor-thread")),
        WorkerSessionState::Idle,
    )
    .await;
    std::fs::write(sock.with_extension("release-resume"), "").unwrap();
    let (status, body) = pending.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let current = boot
        .repo
        .session_projection_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.id, successor);
    assert_eq!(current.thread_id.as_deref(), Some("successor-thread"));
    let old_state: String = sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id=?")
        .bind(&runtime)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_ne!(old_state, "idle");
    assert!(boot.state.harness.get(&runtime).is_none());
    let sent: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events WHERE scope_card=? AND kind='harness.user_message.enqueued'",
    )
    .bind(card.id.as_str())
    .fetch_one(boot.repo.pool())
    .await
    .unwrap();
    assert_eq!(sent, 0);
}
