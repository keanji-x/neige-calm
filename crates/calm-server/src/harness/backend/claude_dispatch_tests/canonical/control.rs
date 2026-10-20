use super::*;
use crate::harness::native_admission::NativePromptAdmission;

#[tokio::test]
async fn canonical_claude_refused_stop_has_no_local_effect() {
    refused_stop(false).await;
}

#[tokio::test]
async fn canonical_claude_timed_out_stop_has_no_local_effect() {
    refused_stop(true).await;
}

async fn refused_stop(timeout: bool) {
    let fx = Fixture::new("stream").await;
    std::fs::write(fx.dir.path().join("stream"), "").unwrap();
    std::fs::write(fx.dir.path().join("stream-rest"), "").unwrap();
    let mut events = fx.session.subscribe_events();
    assert!(fx.dispatch().await.unwrap().report.outcome().is_ok());
    let target = fx.target().await;
    let admission = NativePromptAdmission::new(
        || {
            Err(crate::error::CalmError::Conflict(
                "superseded control".into(),
            ))
        },
        move || {
            Box::pin(async move {
                if timeout {
                    std::future::pending::<()>().await;
                }
                Ok(())
            })
        },
    );
    let error = fx
        .backend
        .interrupt_execution(&target, &admission)
        .await
        .unwrap_err();
    assert!(error.to_string().contains(if timeout {
        "timed out"
    } else {
        "superseded control"
    }));
    assert_eq!(fx.session.control_state_for_test(), (false, false));
    // Wait past the old kill deadline: a refusal must leave the real driver running.
    tokio::time::sleep(crate::claude_planner::session::STOP_TIMER + Duration::from_millis(200))
        .await;
    assert!(!fx.session.qualify_execution_end(&target).await.unwrap());
    assert_eq!(fx.session.control_state_for_test(), (false, false));
    std::fs::write(fx.dir.path().join("release"), "").unwrap();
    let turn = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let provider::events::PlannerEventKind::TurnCompleted { turn, .. } =
                events.recv().await.unwrap().kind
            {
                break turn;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(turn["status"], "completed");
    fx.wait_end().await;
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_admitted_stop_with_blocked_crossed_write_is_bounded() {
    let fx = Fixture::new("stall").await;
    assert!(fx.dispatch().await.unwrap().report.outcome().is_ok());
    let target = fx.target().await;
    let pause = crate::claude_planner::session::SettlePause {
        entered: Arc::default(),
        release: Arc::default(),
    };
    fx.session.block_control_flush_for_test(pause.clone()).await;
    let admission = NativePromptAdmission::new(|| Ok(()), || Box::pin(async { Ok(()) }));
    let started = tokio::time::Instant::now();
    let stop = fx.backend.interrupt_execution(&target, &admission);
    let observe = async {
        pause.entered.notified().await;
        assert_eq!(fx.session.control_state_for_test(), (true, true));
    };
    let (result, ()) = tokio::join!(stop, observe);
    assert!(result.is_err(), "the real writer's flush must time out");
    tokio::time::timeout(Duration::from_secs(20), fx.wait_end())
        .await
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(23),
        "Stop plus settlement bound"
    );
    fx.session.shutdown().await.unwrap();
}
