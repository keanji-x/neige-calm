use super::*;

#[tokio::test]
async fn planner_hibernat_refused_checkpoint_keeps_live_owner_retryable() {
    let boot = boot_fake_running().await;
    let (_, runtime, handle) = loaded_planner(&boot).await;
    let thread = handle.thread_id_for_test().await.unwrap();
    sqlx::query("UPDATE tracks SET closed_at=? WHERE id=?")
        .bind(now_ms())
        .bind(&boot.track_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER hibernation_write_refused BEFORE UPDATE OF handle_state_json ON worker_sessions \
        BEGIN SELECT RAISE(IGNORE); END")
        .execute(boot.repo.pool()).await.unwrap();
    calm_server::terminal_sweeper::sweep(&boot.state)
        .await
        .unwrap();
    sqlx::query("DROP TRIGGER hibernation_write_refused")
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
    assert!(
        boot.state
            .shared_codex_appserver
            .cached_card_for_thread(&thread)
            .is_some()
    );
    handle
        .observe_user_message_durable(
            "retry after refused sleep".into(),
            vec![],
            calm_server::harness::SendKey::unique_for_test(),
        )
        .await
        .unwrap();
    boot.state
        .harness
        .shutdown_and_remove(&runtime)
        .await
        .unwrap();
}

#[tokio::test]
async fn planner_hibernat_checkpoint_commits_before_owner_and_provider_release() {
    use calm_server::test_seams::{
        PLANNER_HIBERNATION_DECIDED, PausePoint, install_pause_for_test,
    };
    use tokio::sync::Notify;
    let boot = boot_fake_running().await;
    let (card, runtime, handle) = loaded_planner(&boot).await;
    let thread = handle.thread_id_for_test().await.unwrap();
    // A separate read sees this marker only after the actual snapshot transaction commits.
    sqlx::query("CREATE TABLE hibernation_checkpoint (runtime TEXT NOT NULL)")
        .execute(boot.repo.pool())
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER hibernation_checkpoint_committed AFTER UPDATE OF handle_state_json ON worker_sessions \
        BEGIN INSERT INTO hibernation_checkpoint VALUES (NEW.id); END")
        .execute(boot.repo.pool()).await.unwrap();
    sqlx::query("UPDATE tracks SET closed_at=? WHERE id=?")
        .bind(now_ms())
        .bind(&boot.track_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let decided = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(
        PLANNER_HIBERNATION_DECIDED,
        card.id.as_str(),
        decided.clone(),
    );
    let state = boot.state.clone();
    let sweep = tokio::spawn(async move { calm_server::terminal_sweeper::sweep(&state).await });
    tokio::time::timeout(Duration::from_secs(5), decided.entered.notified())
        .await
        .unwrap();
    let committed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM hibernation_checkpoint WHERE runtime=?")
            .bind(&runtime)
            .fetch_one(boot.repo.pool())
            .await
            .unwrap();
    let installed = boot.state.harness.get(&runtime).is_some();
    let subscribed = boot
        .state
        .shared_codex_appserver
        .unsubscribed_threads_for_test()
        .is_empty();
    let attributed = boot
        .state
        .shared_codex_appserver
        .cached_card_for_thread(&thread)
        .is_some();
    decided.release.notify_one();
    sweep.await.unwrap().unwrap();
    assert_eq!(
        committed, 1,
        "sleep must commit its guarded checkpoint before acknowledging closure"
    );
    assert!(
        installed && subscribed && attributed,
        "resource release follows checkpoint acknowledgement"
    );
    assert!(boot.state.harness.get(&runtime).is_none());
    assert_eq!(
        boot.state
            .shared_codex_appserver
            .unsubscribed_threads_for_test(),
        vec![thread]
    );
    assert!(
        handle
            .observe_user_message_durable(
                "sleeping".into(),
                vec![],
                calm_server::harness::SendKey::unique_for_test(),
            )
            .await
            .is_err()
    );
}
