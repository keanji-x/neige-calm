//! Actual managed-driver quiescence precedes recovery and replacement.
use super::*;

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
