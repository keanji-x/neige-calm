use super::*;

async fn closed_nonquiescent_automatic_install(work: &str) {
    use calm_server::harness::{ClaimMode, QueueEntry, spawn_recovered_harness};
    for deferred in [false, true] {
        let boot = boot_fake_running().await;
        boot.state
            .shared_codex_appserver
            .publish_readiness_for_test(1, true);
        boot.state.shared_codex_appserver.fail_turn_start_for_test();
        let card = seed_codex_card_with_role(&boot, CardRole::Planner).await;
        let thread = format!("thread-{}", new_id());
        let mut snapshot = idle_snapshot_value(&thread);
        if work == "queue" {
            let queued = HarnessSnapshot::initial(
                0,
                vec![QueueEntry::user_message("persisted".into(), None, vec![])],
            );
            snapshot["pending_queue"] =
                serde_json::to_value(queued).unwrap()["pending_queue"].clone();
        }
        let id = seed_planner_runtime_row_with_status(
            &boot,
            &card,
            Some(thread),
            Some(snapshot),
            if work == "turn" {
                WorkerSessionState::Running
            } else {
                WorkerSessionState::Idle
            },
        )
        .await;
        if work == "task" {
            let task = new_id();
            sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,created_at_ms,updated_at_ms) \
                VALUES(?,?,?,'codex','work','{}','running',?,?)")
                .bind(&task).bind(&boot.track_id).bind(&task).bind(now_ms()).bind(now_ms())
                .execute(boot.repo.pool()).await.unwrap();
        }
        if work == "assistant" || work == "plain_chat" {
            let role = if work == "assistant" {
                CardRole::Assistant
            } else {
                CardRole::Worker
            };
            sqlx::query("UPDATE cards SET role=?,payload=? WHERE id=?")
                .bind(role.as_db_str())
                .bind(json!({"schemaVersion":1,"harness_profile":work}).to_string())
                .bind(card.id.as_str())
                .execute(boot.repo.pool())
                .await
                .unwrap();
            sqlx::query("UPDATE worker_sessions SET contract='executor' WHERE id=?")
                .bind(&id)
                .execute(boot.repo.pool())
                .await
                .unwrap();
            boot.state
                .card_role_cache
                .insert(card.id.clone(), role, card.track_id.clone());
        }
        sqlx::query("UPDATE tracks SET closed_at=? WHERE id=?")
            .bind(now_ms())
            .bind(&boot.track_id)
            .execute(boot.repo.pool())
            .await
            .unwrap();
        let runtime = runtime_by_id_tx_snapshot(&boot.repo, &id).await.unwrap();
        let mode = if deferred {
            ClaimMode::SkipIfClaimed {
                expected_generation: 1,
            }
        } else {
            ClaimMode::ReplaceIfOpen
        };
        {
            let result = spawn_recovered_harness(
                boot.repo.clone(),
                boot.state.events.clone(),
                boot.state.card_role_cache.clone(),
                boot.state.track_area_cache.clone(),
                boot.state.shared_codex_appserver.clone(),
                boot.state.shared_codex_appserver.thread_seals().clone(),
                &boot.state.claude_planner_wiring(),
                &boot.state.harness,
                &calm_server::harness::new_track_delete_locks(),
                runtime.clone(),
                mode,
            )
            .await
            .unwrap();
            let installed = result.installed().is_some();
            if installed {
                boot.state.harness.shutdown_and_remove(&id).await.unwrap();
            }
            assert!(
                installed,
                "automatic installation must restore nonquiescent closed Planner (work={work}, mode={mode:?})"
            );
        }
    }
}

#[tokio::test]
async fn planner_hibernat_closed_assistant_automatic_install() {
    closed_nonquiescent_automatic_install("assistant").await;
}
#[tokio::test]
async fn planner_hibernat_closed_plain_chat_automatic_install() {
    closed_nonquiescent_automatic_install("plain_chat").await;
}

#[tokio::test]
async fn planner_hibernat_closed_queued_automatic_install() {
    closed_nonquiescent_automatic_install("queue").await;
}
#[tokio::test]
async fn planner_hibernat_closed_running_automatic_install() {
    closed_nonquiescent_automatic_install("turn").await;
}
#[tokio::test]
async fn planner_hibernat_closed_inflight_task_automatic_install() {
    closed_nonquiescent_automatic_install("task").await;
}

#[tokio::test]
async fn planner_hibernat_failed_unsubscribe_retries_and_cold_wire_wake() {
    let _env = ENV_LOCK.lock().await;
    let boot = boot_shared().await;
    let (card, runtime, handle) = loaded_planner(&boot).await;
    let thread = handle.thread_id_for_test().await.unwrap();
    let sock = boot._tmp.path().join("run/codex-appserver.sock");
    let failure = sock.with_extension("unsubscribe-fail");
    std::fs::write(&failure, "fail").unwrap();
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
    std::fs::remove_file(failure).unwrap();
    calm_server::terminal_sweeper::sweep(&boot.state)
        .await
        .unwrap();
    let released = std::fs::read_to_string(sock.with_extension("unsubscribed")).unwrap();
    assert_eq!(
        released.lines().collect::<Vec<_>>(),
        vec![thread.as_str(), thread.as_str()],
        "failed unsubscribe remains a retryable obligation after cache attribution is removed"
    );
    calm_server::terminal_sweeper::sweep(&boot.state)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(sock.with_extension("unsubscribed")).unwrap(),
        released
    );
    std::fs::write(
        sock.with_extension("thread-read"),
        json!({"thread":{"id":thread,"status":{"type":"notLoaded"}}}).to_string(),
    )
    .unwrap();
    std::fs::write(
        sock.with_extension("thread-resume"),
        json!({"thread":{"id":thread,"status":{"type":"idle"}}}).to_string(),
    )
    .unwrap();
    let (status, body) = post_planner_input(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({"text":"wake cold"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["worker_session_id"], runtime);
    let methods = std::fs::read_to_string(sock.with_extension("methods")).unwrap();
    assert!(methods.lines().any(|m| m == "thread/read"));
    assert!(methods.lines().any(|m| m == "thread/resume"));
    assert_eq!(
        boot.state
            .harness
            .get(&runtime)
            .unwrap()
            .thread_id_for_test()
            .await
            .as_deref(),
        Some(thread.as_str())
    );
    boot.state
        .harness
        .shutdown_and_remove(&runtime)
        .await
        .unwrap();
}

#[tokio::test]
async fn planner_hibernat_sleeping_tick_skips_global_drive_fence() {
    use axum::extract::FromRef;
    let boot = boot_fake_running().await;
    let (_, runtime, _) = loaded_planner(&boot).await;
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
    let route = calm_server::state::RouteState::from_ref(&boot.state);
    let _drive = route
        .operation_runtime
        .lock_for_track_delete_for_test()
        .await;
    tokio::time::timeout(
        Duration::from_millis(250),
        calm_server::terminal_sweeper::sweep(&boot.state),
    )
    .await
    .expect("an already sleeping row must not take the global drive fence")
    .unwrap();
}

#[tokio::test]
async fn planner_hibernat_provider_release_drops_global_drive_fence() {
    use axum::extract::FromRef;
    let _env = ENV_LOCK.lock().await;
    let boot = boot_shared().await;
    let (_, runtime, _) = loaded_planner(&boot).await;
    let sock = boot._tmp.path().join("run/codex-appserver.sock");
    let park = sock.with_extension("unsubscribe-park");
    std::fs::write(&park, "park").unwrap();
    sqlx::query("UPDATE tracks SET closed_at=? WHERE id=?")
        .bind(now_ms())
        .bind(&boot.track_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let state = boot.state.clone();
    let sweep = tokio::spawn(async move { calm_server::terminal_sweeper::sweep(&state).await });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !sock.with_extension("unsubscribed").exists() {
        assert!(Instant::now() < deadline, "release RPC never reached wire");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(boot.state.harness.get(&runtime).is_none());
    let route = calm_server::state::RouteState::from_ref(&boot.state);
    let acquired = tokio::time::timeout(
        Duration::from_millis(250),
        route.operation_runtime.lock_for_track_delete_for_test(),
    )
    .await;
    let free = acquired.is_ok();
    drop(acquired);
    std::fs::remove_file(park).unwrap();
    sweep.await.unwrap().unwrap();
    assert!(
        free,
        "provider unsubscribe must not hold the global drive fence after owner removal"
    );
}

#[tokio::test]
async fn planner_hibernat_loop_wait_is_bounded_and_cancelled_request_stays_live() {
    use calm_server::test_seams::{
        PLANNER_HIBERNATION_DECIDED, PLANNER_HIBERNATION_QUIESCE, PausePoint,
        install_pause_for_test,
    };
    use tokio::sync::Notify;
    let boot = boot_fake_running().await;
    let (card, runtime, handle) = loaded_planner(&boot).await;
    let pause = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(PLANNER_HIBERNATION_QUIESCE, card.id.as_str(), pause.clone());
    let decided = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(
        PLANNER_HIBERNATION_DECIDED,
        card.id.as_str(),
        decided.clone(),
    );
    sqlx::query("UPDATE tracks SET closed_at=? WHERE id=?")
        .bind(now_ms())
        .bind(&boot.track_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let state = boot.state.clone();
    let mut sweep = tokio::spawn(async move { calm_server::terminal_sweeper::sweep(&state).await });
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .unwrap();
    let bounded = tokio::time::timeout(Duration::from_secs(7), &mut sweep)
        .await
        .is_ok();
    let remains = boot.state.harness.get(&runtime).is_some();
    pause.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), decided.entered.notified())
        .await
        .unwrap();
    decided.release.notify_one();
    if !bounded {
        sweep.await.unwrap().unwrap();
    }
    // A durable delivery round-trips the loop, proving the delayed command has
    // finished and did not close ingress after its sweep timed out.
    let delivered = handle
        .observe_user_message_durable(
            "after cancelled sleep".into(),
            vec![],
            calm_server::harness::SendKey::unique_for_test(),
        )
        .await
        .is_ok();
    boot.state
        .harness
        .shutdown_and_remove(&runtime)
        .await
        .unwrap();
    assert!(
        bounded,
        "a loop blocked before snapshot persistence must not hold the sweep indefinitely"
    );
    assert!(
        remains && delivered,
        "a timed-out request must not close the owner later"
    );
}
