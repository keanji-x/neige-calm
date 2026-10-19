//! Interrupt acknowledgements do not settle a turn or disarm its completion watchdog.
use super::*;

#[tokio::test]
async fn unconfirmed_interrupt_keeps_completion_watchdog_armed() {
    let repo = fresh_repo().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    daemon.fail_turn_interrupt_for_test(true);
    let config = HarnessConfig {
        max_turn_duration: Duration::from_millis(10),
        interrupt_completion_budget: Duration::from_millis(10),
        ..HarnessConfig::default()
    };
    let (_repo, harness, _worker_session_id, _thread_id) =
        harness_with(repo, daemon, HarnessPhaseTag::TurnRunning, config).await;
    harness
        .set_state_for_test(HarnessState::turn_running(
            "turn-timeout".into(),
            Instant::now() - Duration::from_secs(1),
        ))
        .await;
    wait_for_state(
        &harness,
        |s| matches!(s, HarnessState::Wedged { reason, .. } if reason == "interrupt_timeout"),
    )
    .await;
    harness.shutdown().await.unwrap();
}
