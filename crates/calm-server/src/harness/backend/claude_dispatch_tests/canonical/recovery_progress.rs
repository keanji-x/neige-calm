//! An early completion event still needs the post-driver claim-cleanup hint.
use super::*;

#[tokio::test]
async fn canonical_claude_fresh_completion_wakes_recovery_after_early_event() {
    completion_progress(false).await;
}

#[tokio::test]
async fn canonical_claude_reopened_completion_wakes_recovery_after_early_event() {
    completion_progress(true).await;
}

async fn completion_progress(reopened: bool) {
    let mut fx = Fixture::new("exit").await;
    if reopened {
        let (backend, session) = recovery::reopen_session(&fx, fx.owner.repo.clone()).await;
        fx.backend = backend;
        fx.session = session;
    }
    let completion = c5d::Pause::arm("claude-completed-before-claim-clear", &fx.owner.worker);
    let native = c5d::Pause::arm("canonical-native-end-observed", &fx.owner.card);
    let registry = crate::harness::HarnessRegistry::new();
    // The real loop dispatches and publishes the running phase. The low-level
    // fixture dispatch helper only records delivery and cannot stand in for it.
    let harness = fx.start_loop_in(registry.clone()).await;
    completion.entered().await;
    native.entered().await;
    assert_eq!(fx.frame().await["phase"], "turn_running");
    let tick = c5d::Pause::arm("canonical-tick-returned", &fx.owner.card);
    harness.request_progress();
    drop(native);
    tick.entered().await;
    // Drain our own startup/manual hints in a second pass. The first hint may
    // have arrived after an already-running pass consumed its notification.
    let drained = c5d::Pause::arm("canonical-tick-returned", &fx.owner.card);
    harness.request_progress();
    drop(tick);
    drained.entered().await;
    assert!(fx.checkpoint().await.active.is_some());
    let target = fx.target().await;
    assert!(
        fx.backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_none()
    );
    // Pin the scheduler's waiting path and its completed probe calculation.
    // The source is already installed; there is no pending admission handoff.
    let waiting = c5d::Pause::arm("canonical-result-wait", &fx.owner.card);
    let probe = c5d::Pause::arm("canonical-recovery-probe-ready", &fx.owner.card);
    drop(drained);
    waiting.entered().await;
    drop(waiting);
    probe.entered().await;
    drop(completion);
    assert!(
        tokio::time::timeout(Duration::from_secs(10), fx.session.wait_driver_for_test())
            .await
            .expect("original driver must finish after releasing completion")
    );
    drop(probe);
    // This deadline is strictly below the scheduler's 25–30 second fallback.
    let completed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let cp = fx.checkpoint().await;
            if cp.active.is_none() {
                return cp;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let diagnostic = fx.checkpoint().await;
    harness.shutdown().await.unwrap();
    let after = completed.unwrap_or_else(|_| {
        panic!("completion hint did not settle END before periodic recovery: {diagnostic:?}")
    });
    assert_eq!(after.attempts.len(), 1);
    assert!(after.pending.is_empty());
    assert_eq!(fx.session.user_line_writes_for_test(), 1);
    assert!(fx.complete().await.is_none());
    fx.session.shutdown().await.unwrap();
}
