use super::*;

async fn loaded_planner(boot: &Boot) -> (Card, String, PlannerHarness) {
    use calm_server::shared_codex_appserver::{SharedThreadStartParams, ThreadConfig};
    let card = seed_codex_card_with_role(boot, CardRole::Planner).await;
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
    let mut snapshot = idle_snapshot_value(&thread);
    snapshot["phase"] = serde_json::to_value(HarnessPhaseTag::TurnCompleted).unwrap();
    snapshot["last_turn_id"] = json!("old-turn");
    let runtime =
        seed_active_planner_runtime_row(boot, &card, Some(thread.clone()), Some(snapshot.clone()))
            .await;
    let handle = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: runtime.clone(),
        card_id: card.id.clone(),
        track_id: card.track_id.clone(),
        thread_id: Some(thread.clone()),
        repo: boot.repo.clone(),
        events: boot.state.events.clone(),
        card_role_cache: boot.state.card_role_cache.clone(),
        track_area_cache: boot.state.track_area_cache.clone(),
        backend: boot.state.shared_codex_appserver.clone().into(),
        live_replies: calm_server::harness::LiveReplies::for_test(),
        config: HarnessConfig {
            debounce_min_idle: Duration::from_secs(60),
            debounce_max_wait: Duration::from_secs(60),
            ..HarnessConfig::default()
        },
        snapshot: HarnessSnapshot::from_value_strict(snapshot),
    });
    boot.state.harness.insert(runtime.clone(), handle.clone());
    assert_eq!(
        boot.state
            .shared_codex_appserver
            .cached_card_for_thread(&thread),
        Some(card.id.to_string())
    );
    seed_conversation_history(boot, &card, &runtime, &thread, "saved history").await;
    (card, runtime, handle)
}

pub(super) async fn seed_conversation_history(
    boot: &Boot,
    card: &Card,
    runtime: &str,
    thread: &str,
    text: &str,
) {
    let params = json!({"item":{"id":"old-answer","type":"agentMessage","text":text}}).to_string();
    boot.repo
        .harness_item_insert(
            runtime,
            card.id.as_str(),
            &boot.track_id,
            thread,
            Some("old-turn"),
            Some("old-answer"),
            Some("agentMessage"),
            "item/completed",
            &params,
            None,
        )
        .await
        .unwrap();
}

// Pin the prerequisite: recovery of an active row currently installs a harness and
// goes straight to turn/start. An unloaded thread requires an explicit resume first.
#[tokio::test]
async fn planner_hibernat_registry_miss_resumes_unloaded_thread_before_next_turn() {
    use crate::support::planner_queue_fixture::{
        SEED_THREAD_ID, boot_with, idle_snapshot, post_input, send_json,
    };
    let boot = boot_with(idle_snapshot(vec![])).await;
    let (status, body) = send_json(
        boot.app.clone(),
        "PUT",
        format!("/api/cards/{}/planner/model", boot.planner_card.id),
        "user",
        json!({"model":"gpt-5","reasoning_effort":"high"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    boot.registry
        .shutdown_and_remove(&boot.worker_session_id)
        .await
        .unwrap();
    assert_eq!(
        boot.daemon.cached_card_for_thread(SEED_THREAD_ID),
        None,
        "unloaded premise"
    );
    let (status, body) = post_input(
        boot.app.clone(),
        boot.planner_card.id.as_str(),
        "continue the original conversation",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["worker_session_id"], boot.worker_session_id);
    let deadline = Instant::now() + Duration::from_secs(5);
    while boot.daemon.turn_start_count_for_test() == 0 {
        assert!(Instant::now() < deadline, "next turn never started");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        boot.daemon.started_turn_selections_for_test()[0].0,
        SEED_THREAD_ID
    );
    let resumed = boot.daemon.resumed_threads_for_test();
    boot.registry
        .shutdown_and_remove(&boot.worker_session_id)
        .await
        .unwrap();
    assert_eq!(
        resumed,
        vec![(SEED_THREAD_ID.into(), true)],
        "unloaded active carrier resumes the same thread before its next turn"
    );
}

#[tokio::test]
async fn planner_hibernat_closed_idle_session_sleeps_and_send_wakes_same_thread() {
    let boot = boot_fake_running().await;
    let (card, runtime, handle) = loaded_planner(&boot).await;
    let thread = handle.thread_id_for_test().await.unwrap();
    sqlx::query("UPDATE tracks SET closed_at=? WHERE id=?")
        .bind(now_ms())
        .bind(&boot.track_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    calm_server::terminal_sweeper::sweep(&boot.state)
        .await
        .unwrap();
    let remains = boot.state.harness.get(&runtime).is_some();
    let unsubscribed = boot
        .state
        .shared_codex_appserver
        .unsubscribed_threads_for_test();
    if remains {
        handle.shutdown().await.unwrap();
    }
    assert!(
        !remains,
        "closed quiescent Planner must leave the registry after one sweep"
    );
    assert_eq!(unsubscribed, vec![thread.clone()]);
    assert!(
        boot.state
            .shared_codex_appserver
            .interrupted_turns_for_test()
            .is_empty(),
        "hibernation releases the subscription without interrupting a provider turn"
    );
    assert_eq!(
        boot.state
            .shared_codex_appserver
            .cached_card_for_thread(&thread),
        None
    );
    calm_server::terminal_sweeper::sweep(&boot.state)
        .await
        .unwrap();
    assert_eq!(
        boot.state
            .shared_codex_appserver
            .unsubscribed_threads_for_test(),
        unsubscribed,
        "sleep is idempotent"
    );
    let row = runtime_by_id_tx_snapshot(&boot.repo, &runtime)
        .await
        .unwrap();
    assert_eq!(row.status, WorkerSessionState::Idle);
    assert_eq!(row.thread_id.as_deref(), Some(thread.as_str()));
    let (status, body) = post_planner_input(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({"text":"wake here"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["worker_session_id"], runtime);
    assert!(conversation_rows(&boot, &card).await.iter().any(|item| {
        item["params"]
            .as_str()
            .is_some_and(|text| text.contains("saved history"))
    }));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM worker_sessions WHERE card_id=?")
        .bind(card.id.as_str())
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        boot.state.shared_codex_appserver.resumed_threads_for_test(),
        vec![(thread, true)]
    );
    boot.state
        .harness
        .shutdown_and_remove(&runtime)
        .await
        .unwrap();
}

#[tokio::test]
async fn planner_hibernat_open_tracks_inflight_tasks_running_turns_and_queue_stay_awake() {
    use calm_server::harness::HarnessState;
    let boot = boot_fake_running().await;
    let (_, runtime, handle) = loaded_planner(&boot).await;
    calm_server::terminal_sweeper::sweep(&boot.state)
        .await
        .unwrap();
    assert!(boot.state.harness.get(&runtime).is_some(), "open track");
    sqlx::query("UPDATE tracks SET closed_at=? WHERE id=?")
        .bind(now_ms())
        .bind(&boot.track_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    for status in ["dispatched", "running", "verifying"] {
        let task = new_id();
        sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,created_at_ms,updated_at_ms) \
            VALUES(?,?,?,'codex','work','{}',?,?,?)")
            .bind(&task).bind(&boot.track_id).bind(&task).bind(status).bind(now_ms()).bind(now_ms())
            .execute(boot.repo.pool()).await.unwrap();
        calm_server::terminal_sweeper::sweep(&boot.state)
            .await
            .unwrap();
        assert!(boot.state.harness.get(&runtime).is_some(), "task {status}");
        sqlx::query("UPDATE tasks SET status='done' WHERE id=?")
            .bind(&task)
            .execute(boot.repo.pool())
            .await
            .unwrap();
    }
    handle
        .set_state_for_test(HarnessState::turn_running(
            "live-turn".into(),
            Instant::now(),
        ))
        .await;
    calm_server::terminal_sweeper::sweep(&boot.state)
        .await
        .unwrap();
    assert!(
        boot.state.harness.get(&runtime).is_some(),
        "running turn even before projection catches up"
    );
    handle.set_state_for_test(HarnessState::Idle).await;
    let thread = handle.thread_id_for_test().await.unwrap();
    boot.state
        .shared_codex_appserver
        .set_active_turn_for_test(&thread, "provider-turn");
    calm_server::terminal_sweeper::sweep(&boot.state)
        .await
        .unwrap();
    assert!(
        boot.state.harness.get(&runtime).is_some(),
        "provider turn notification is still in flight"
    );
    boot.state
        .shared_codex_appserver
        .clear_active_turn_for_test(&thread);
    handle
        .observe_user_message_durable(
            "queued".into(),
            vec![],
            calm_server::harness::SendKey::unique_for_test(),
        )
        .await
        .unwrap();
    calm_server::terminal_sweeper::sweep(&boot.state)
        .await
        .unwrap();
    assert!(boot.state.harness.get(&runtime).is_some(), "queued input");
    assert!(
        boot.state
            .shared_codex_appserver
            .unsubscribed_threads_for_test()
            .is_empty()
    );
    boot.state
        .harness
        .shutdown_and_remove(&runtime)
        .await
        .unwrap();
}

#[tokio::test]
async fn planner_hibernat_cold_daemon_does_not_reload_closed_idle_planner() {
    use calm_server::shared_codex_appserver::{SharedThreadStartParams, ThreadConfig};
    let _env = ENV_LOCK.lock().await;
    let boot = boot_shared().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Planner).await;
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
    seed_active_planner_runtime_row(
        &boot,
        &card,
        Some(thread.clone()),
        Some(idle_snapshot_value(&thread)),
    )
    .await;
    sqlx::query("UPDATE tracks SET closed_at=? WHERE id=?")
        .bind(now_ms())
        .bind(&boot.track_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    // The boot path rebuilds attribution from the retained rows, unlike replacement.
    boot.state
        .shared_codex_appserver
        .start_or_takeover()
        .await
        .unwrap();
    assert_eq!(
        boot.state
            .shared_codex_appserver
            .cached_card_for_thread(&thread),
        None
    );
    let sock = boot._tmp.path().join("run/codex-appserver.sock");
    let methods = std::fs::read_to_string(sock.with_extension("methods")).unwrap();
    assert!(
        !methods.lines().any(|m| m == "thread/resume"),
        "closed idle thread must stay unloaded: {methods}"
    );
}

#[tokio::test]
async fn planner_hibernat_reopen_resumes_same_carrier() {
    let boot = boot_fake_running().await;
    let (card, runtime, handle) = loaded_planner(&boot).await;
    let thread = handle.thread_id_for_test().await.unwrap();
    sqlx::query("UPDATE tracks SET closed_at=? WHERE id=?")
        .bind(now_ms())
        .bind(&boot.track_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    calm_server::terminal_sweeper::sweep(&boot.state)
        .await
        .unwrap();
    assert!(boot.state.harness.get(&runtime).is_none());
    let response = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/tracks/{}", boot.track_id))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"closed":false}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let awakened = boot
        .state
        .harness
        .get(&runtime)
        .expect("reopen restored the same carrier");
    assert_eq!(
        awakened.thread_id_for_test().await.as_deref(),
        Some(thread.as_str())
    );
    assert_eq!(
        boot.state.shared_codex_appserver.resumed_threads_for_test(),
        vec![(thread, true)]
    );
    assert_eq!(
        runtime_by_id_tx_snapshot(&boot.repo, &runtime)
            .await
            .unwrap()
            .card_id,
        card.id.as_str()
    );
    boot.state
        .harness
        .shutdown_and_remove(&runtime)
        .await
        .unwrap();
}

async fn dispatch_wake(settlement: bool) {
    let boot = boot_fake_running().await;
    let (card, runtime, _) = loaded_planner(&boot).await;
    sqlx::query("UPDATE tracks SET closed_at=? WHERE id=?")
        .bind(now_ms())
        .bind(&boot.track_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    calm_server::terminal_sweeper::sweep(&boot.state)
        .await
        .unwrap();
    assert!(boot.state.harness.get(&runtime).is_none());
    let event = if settlement {
        calm_server::event::Event::TaskCompleted {
            idempotency_key: new_id(),
            result: json!({"done":true}),
            artifacts: vec![],
            agent_message: None,
        }
    } else {
        calm_server::event::Event::TrackWakeRequested {
            track_id: card.track_id.clone(),
            source: "test".into(),
            key: "wake".into(),
            text: "resume retained history".into(),
        }
    };
    let track = boot.repo.track_get(&boot.track_id).await.unwrap().unwrap();
    let scope = calm_server::event::EventScope::Track {
        track: track.id,
        area: track.area_id,
    };
    let id = boot
        .repo
        .log_pure_event(
            calm_server::ids::ActorId::Kernel,
            scope,
            None,
            &boot.state.events,
            &boot.state.card_role_cache,
            &boot.state.track_area_cache,
            event.clone(),
        )
        .await
        .unwrap();
    boot.state
        .dispatcher
        .catch_up_push(card.track_id.clone(), event, id)
        .await;
    let awakened = boot
        .state
        .harness
        .get(&runtime)
        .expect("Dispatcher restored sleeping Planner");
    assert!(awakened.snapshot().await.push_watermark >= id);
    assert_eq!(
        boot.state
            .shared_codex_appserver
            .resumed_threads_for_test()
            .len(),
        1
    );
    boot.state
        .harness
        .shutdown_and_remove(&runtime)
        .await
        .unwrap();
}

#[tokio::test]
async fn planner_hibernat_dispatcher_wake_restores_same_conversation() {
    dispatch_wake(false).await;
}

#[tokio::test]
async fn planner_hibernat_task_settlement_restores_same_conversation() {
    dispatch_wake(true).await;
}

#[tokio::test]
async fn planner_hibernat_failed_snapshot_write_keeps_live_owner_retryable() {
    let boot = boot_fake_running().await;
    let (_, runtime, handle) = loaded_planner(&boot).await;
    sqlx::query("UPDATE tracks SET closed_at=? WHERE id=?")
        .bind(now_ms())
        .bind(&boot.track_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER hibernation_write_failure BEFORE UPDATE OF handle_state_json ON worker_sessions \
        BEGIN SELECT RAISE(ABORT,'snapshot unavailable'); END")
        .execute(boot.repo.pool()).await.unwrap();
    calm_server::terminal_sweeper::sweep(&boot.state)
        .await
        .unwrap();
    sqlx::query("DROP TRIGGER hibernation_write_failure")
        .execute(boot.repo.pool())
        .await
        .unwrap();
    assert!(boot.state.harness.get(&runtime).is_some());
    assert!(
        boot.state
            .shared_codex_appserver
            .unsubscribed_threads_for_test()
            .is_empty()
    );
    let accepted = handle
        .observe_user_message_durable(
            "still awake".into(),
            vec![],
            calm_server::harness::SendKey::unique_for_test(),
        )
        .await;
    boot.state
        .harness
        .shutdown_and_remove(&runtime)
        .await
        .unwrap();
    assert!(
        accepted.is_ok(),
        "an unsuccessful snapshot write must leave input open: {accepted:?}"
    );
}

#[tokio::test]
async fn planner_hibernat_send_resubscribes_thread_kept_loaded_by_another_client() {
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
    let (card, runtime, handle) = loaded_planner(&boot).await;
    let thread = handle.thread_id_for_test().await.unwrap();
    sqlx::query("UPDATE tracks SET closed_at=? WHERE id=?")
        .bind(now_ms())
        .bind(&boot.track_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    calm_server::terminal_sweeper::sweep(&boot.state)
        .await
        .unwrap();
    assert!(boot.state.harness.get(&runtime).is_none());
    assert_eq!(
        boot.state
            .shared_codex_appserver
            .cached_card_for_thread(&thread),
        None
    );
    let sock = boot._tmp.path().join("run/codex-appserver.sock");
    let loaded = json!({"thread":{"id":thread,"status":{"type":"idle"},"turns":[]}}).to_string();
    std::fs::write(sock.with_extension("thread-read"), &loaded).unwrap();
    std::fs::write(sock.with_extension("thread-resume"), &loaded).unwrap();
    let (status, body) = post_planner_input(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({"text":"continue here"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["worker_session_id"], runtime);
    let resumes = request_lines_containing(&path, "thread/resume", 0).await;
    boot.state
        .harness
        .shutdown_and_remove(&runtime)
        .await
        .unwrap();
    assert_eq!(
        resumes.len(),
        1,
        "a loaded thread still needs this connection's subscription restored"
    );
    assert_eq!(resumes[0]["params"]["threadId"], thread);
    assert!(
        resumes[0]["params"].get("config").is_none(),
        "loaded credentials stay in place"
    );
}

#[tokio::test]
async fn planner_hibernat_late_boot_recovery_cannot_reinstall_after_sleep() {
    use calm_server::test_seams::{PLANNER_RECOVERY_INSTALL, PausePoint, install_pause_for_test};
    use tokio::sync::Notify;
    let boot = boot_fake_running().await;
    let (card, runtime, handle) = loaded_planner(&boot).await;
    let thread = handle.thread_id_for_test().await.unwrap();
    let pause = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(PLANNER_RECOVERY_INSTALL, card.id.as_str(), pause.clone());
    let state = boot.state.clone();
    let recovery = tokio::spawn(async move {
        calm_server::recover_harnesses_after_daemon_boot(&state, Ok(()))
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET closed_at=? WHERE id=?")
        .bind(now_ms())
        .bind(&boot.track_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    calm_server::terminal_sweeper::sweep(&boot.state)
        .await
        .unwrap();
    assert!(boot.state.harness.get(&runtime).is_none());
    pause.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), recovery)
        .await
        .unwrap()
        .unwrap();
    let resurrected = boot.state.harness.get(&runtime).is_some();
    if resurrected {
        boot.state
            .harness
            .shutdown_and_remove(&runtime)
            .await
            .unwrap();
    }
    assert!(
        !resurrected,
        "a stale automatic recovery must recheck closure at installation"
    );
    assert_eq!(
        boot.state
            .shared_codex_appserver
            .cached_card_for_thread(&thread),
        None
    );
}

#[path = "planner_hibernation_failures.rs"]
mod failures;

#[path = "planner_hibernation_lifecycle.rs"]
mod lifecycle;
