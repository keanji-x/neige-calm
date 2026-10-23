//! Claude END requires session absence and the correct worker-session binding.
use crate::harness::backend::claude_dispatch_tests::canonical::Fixture;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn canonical_claude_paused_driver_or_wrong_worker_session_cannot_close() {
    let fx = Fixture::new("exit").await;
    let pause = crate::claude_planner::session::SettlePause {
        entered: Arc::default(),
        release: Arc::default(),
    };
    fx.session
        .set_before_turn_completed_pause_for_test(pause.clone());
    fx.dispatch().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
        .await
        .unwrap();
    let target = fx.target().await;
    assert!(
        fx.backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_none()
    );
    pause.release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if fx
                .backend
                .qualify_execution_end(&target)
                .await
                .unwrap()
                .is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Local-turn and attempt identity are checked by complete_once/consume_tx
    // against committed input facts, not by session-level absence qualification.
    let mut wrong = target.clone();
    wrong.worker_session_id = "successor".into();
    assert!(
        fx.backend
            .qualify_execution_end(&wrong)
            .await
            .unwrap()
            .is_none()
    );
    fx.wait_end().await;
    fx.session.shutdown().await.unwrap();
}
