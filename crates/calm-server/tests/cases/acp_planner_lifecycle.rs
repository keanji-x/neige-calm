//! Actual managed-driver quiescence precedes recovery and replacement.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_replacement_retains_the_discoverable_receipt_writer() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    let runtime = stack.runtime(&card).await;
    let pause = calm_server::acp_planner::test_seams::pause_settlement(&runtime.id);
    let (status, body) = stack
        .post_input(&card, "keep the predecessor discoverable")
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
        .await
        .unwrap();
    let cancelled = tokio::time::timeout(
        Duration::from_millis(100),
        stack
            .state
            .harness
            .reserve_after_shutdown(runtime.id.clone()),
    )
    .await;
    assert!(cancelled.is_err());
    let retained = stack.state.harness.get(&runtime.id).is_some();
    pause.release.notify_one();
    // Release the real writer before reporting the regression so no test leaves it paused.
    if let Some(handle) = stack.state.harness.get(&runtime.id) {
        handle.shutdown().await.unwrap();
    }
    stack.wait_outcomes(&card, 1).await;
    assert!(
        retained,
        "cancellation must not discard the discoverable predecessor"
    );
    let _replacement = stack
        .state
        .harness
        .reserve_after_shutdown(runtime.id.clone())
        .await
        .unwrap();
    let state: String =
        sqlx::query_scalar("SELECT state FROM acp_submissions WHERE worker_session_id=?1")
            .bind(&runtime.id)
            .fetch_one(&stack.repo().sqlite_pool().unwrap())
            .await
            .unwrap();
    assert_eq!(state, "completed");
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_waits_for_delayed_receipt_writer_before_returning() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    let runtime = stack.runtime(&card).await;
    let pause = calm_server::acp_planner::test_seams::pause_settlement(&runtime.id);
    let (status, body) = stack.post_input(&card, "settle before replacement").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
        .await
        .unwrap();
    let harness = stack.harness(&runtime.id);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), harness.shutdown())
            .await
            .is_err()
    );
    let shutdown = harness.shutdown();
    tokio::pin!(shutdown);
    let premature = tokio::time::timeout(Duration::from_secs(6), shutdown.as_mut()).await;
    pause.release.notify_one();
    let completed_early = premature.is_ok();
    if !completed_early {
        tokio::time::timeout(Duration::from_secs(10), shutdown)
            .await
            .unwrap()
            .unwrap();
    }
    assert!(
        !completed_early,
        "recovery must never overlap the previous receipt writer"
    );
    let pool = stack.repo().sqlite_pool().unwrap();
    let state: String =
        sqlx::query_scalar("SELECT state FROM acp_submissions WHERE worker_session_id=?1")
            .bind(&runtime.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "completed");
    stack.shutdown().await;
}
