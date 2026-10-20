//! The real joined Claude driver supplies END; correlation and events cannot.
use crate::harness::backend::claude_dispatch_tests::canonical::Fixture;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn canonical_claude_wrong_turn_proof_or_unjoined_driver_cannot_close() {
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
    let mut wrong = target.clone();
    let crate::harness::backend::ExecutionEndScope::LocalProcess { local_turn_id, .. } =
        &mut wrong.scope
    else {
        panic!("local fixture")
    };
    *local_turn_id = uuid::Uuid::new_v4();
    assert!(
        fx.backend
            .qualify_execution_end(&wrong)
            .await
            .unwrap()
            .is_none()
    );
    let mut wrong = target.clone();
    let calm_types::conversation::SubmissionDispatchProof::V2 { attempt, .. } = &mut wrong.proof;
    attempt.attempt_id =
        calm_types::conversation::InputAttemptId::parse(&crate::model::new_id()).unwrap();
    assert!(
        fx.backend
            .qualify_execution_end(&wrong)
            .await
            .unwrap()
            .is_none()
    );
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
