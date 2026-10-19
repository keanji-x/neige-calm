//! Real result completion through watchdog and concurrent restart.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn release_commit_watchdog_commits_the_unfinished_result() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    boot.daemon
        .install_turn_start_return_hook_for_test(TurnStartReturnHook {
            entered: entered.clone(),
            release: release.clone(),
        });
    enqueue(&boot, TEXT).await;
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    CheckpointFault::UpdateError.install(&boot).await;
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !matches!(
            boot.harness.state_for_test().await,
            HarnessState::TurnRunning { .. }
        ) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(stored_ids(&boot).await.len(), 1);
    let tick = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(
        "conversation-watchdog-tick",
        boot.planner_card.id.as_str(),
        tick.clone(),
    );
    tokio::time::timeout(Duration::from_secs(10), tick.entered.notified())
        .await
        .unwrap();
    // This is Stop's production entry point, which bypasses command admission.
    assert!(boot.harness.interrupt("user stop".into()).await.is_err());
    sqlx::query("DROP TRIGGER refuse_result_checkpoint")
        .execute(boot.repo.db().writer_pool())
        .await
        .unwrap();
    boot.harness.expire_interrupt_deadline_for_test().await;
    tick.release.notify_one();
    let committed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let value = boot
                .repo
                .session_projection_handle_state_by_id(&boot.worker_session_id)
                .await
                .unwrap()
                .unwrap();
            let snapshot = HarnessSnapshot::from_value_strict(value);
            if snapshot.wedged_reason.as_deref()
                == Some(calm_types::harness::HARNESS_INTERRUPT_TIMEOUT_REASON)
            {
                assert!(snapshot.pending_entries().is_empty());
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if committed.is_err() {
        let state = boot.harness.state_for_test().await;
        assert!(
            matches!(&state, HarnessState::Wedged { reason, .. }
            if reason == calm_types::harness::HARNESS_INTERRUPT_TIMEOUT_REASON),
            "{state:?}"
        );
        let durable = boot
            .repo
            .session_projection_handle_state_by_id(&boot.worker_session_id)
            .await
            .unwrap();
        panic!("watchdog entered interrupt-timeout but did not commit its snapshot: {durable:?}");
    }

    assert_eq!(
        boot.repo
            .session_projection_state_by_id(&boot.worker_session_id)
            .await
            .unwrap(),
        Some(calm_server::session_projection_repo::WorkerSessionState::Failed)
    );
    boot.registry
        .shutdown_and_remove(&boot.worker_session_id)
        .await
        .expect("the watchdog committed the same accepted result");
    assert_eq!(delivery_count(&boot), 1);
    let (status, body) = send_json(
        boot.app.clone(),
        "POST",
        format!("/api/cards/{}/planner/restart", boot.planner_card.id),
        "user",
        json!(null),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn release_commit_accepted_steer_across_committed_move() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    enqueue(&boot, "first turn").await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while !matches!(
            boot.harness.state_for_test().await,
            HarnessState::TurnRunning { .. }
        ) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    boot.daemon
        .install_turn_steer_return_hook_for_test(TurnStartReturnHook {
            entered: entered.clone(),
            release: release.clone(),
        });
    let id = enqueue(&boot, TEXT).await;
    let steer = tokio::spawn(send_json(
        boot.app.clone(),
        "POST",
        format!(
            "/api/cards/{}/planner/input/{id}/steer",
            boot.planner_card.id
        ),
        "user",
        json!({"if_entry_rev":0}),
    ));
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    assert_eq!(stored_ids(&boot).await, vec![id]);
    let restart = tokio::spawn(send_json(
        boot.app.clone(),
        "POST",
        format!("/api/cards/{}/planner/restart", boot.planner_card.id),
        "user",
        json!(null),
    ));
    wait_until("MOVE closes predecessor ingress", || {
        boot.harness.is_closing_for_test()
    })
    .await;
    assert!(!restart.is_finished());
    assert_eq!(
        boot.repo
            .session_projection_state_by_id(&boot.worker_session_id)
            .await
            .unwrap(),
        Some(calm_server::session_projection_repo::WorkerSessionState::Superseded)
    );
    let old = boot
        .repo
        .session_projection_handle_state_by_id(&boot.worker_session_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        HarnessSnapshot::from_value_strict(old)
            .pending_entries()
            .is_empty(),
        "MOVE committed before backend return"
    );
    release.notify_one();
    let (status, body) = joined(steer, "accepted steer after MOVE").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = joined(
        restart,
        "restart must retain its original success semantics",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let next = boot
        .repo
        .session_projection_active_for_card(&boot.planner_card.id.to_string())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(next.id, boot.worker_session_id);
    assert!(boot.registry.get(&boot.worker_session_id).is_none());
    assert!(!boot.registry.get(&next.id).unwrap().is_closing_for_test());
    enqueue(&boot, "successor accepts input").await;
    boot.registry.shutdown_and_remove(&next.id).await.unwrap();
}
