//! Public stop races use the actual loop, backend notifications and durable checkpoint writer.
use super::completed_commit_tests::Fixture;
use super::handoff_pause_tests::{request, running, system};
use super::*;
use crate::codex_appserver::Notification;

async fn phase(f: &Fixture, expected: HarnessPhaseTag) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while f.harness.snapshot().await.phase != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("loop reached expected phase");
    // State changes precede their SQL writes. A real command reply proves notification/tick
    // persistence settled before a test captures its durable before-image.
    f.harness.persist_snapshot().await.unwrap();
}

async fn start(f: &Fixture) -> String {
    f.harness
        .observe_user_message_durable("start".into(), vec![], SendKey::unique_for_test())
        .await
        .unwrap();
    phase(f, HarnessPhaseTag::TurnRunning).await;
    // turn/start's return stages issued_turn_id before the separate TurnStarted echo
    // consumes it. Capture a confirmed running owner, so that echo cannot race a
    // later checkpoint-failure test's before-image.
    tokio::time::timeout(Duration::from_secs(10), async {
        while f.harness.inner.issued_turn_id.lock().await.is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("real TurnStarted confirmation consumed the issuance staging");
    f.harness.persist_snapshot().await.unwrap();
    f.harness.inner.state.lock().await.active_turn_id().unwrap()
}

async fn complete(f: &Fixture, turn: &str) {
    f.daemon.clear_active_turn_for_test("thread-commit-wake");
    f.daemon
        .emit_notification_for_test(Notification::TurnCompleted {
            thread_id: "thread-commit-wake".into(),
            turn: serde_json::json!({"id": turn, "status": "completed"}),
        });
    phase(f, HarnessPhaseTag::TurnCompleted).await;
}

#[tokio::test]
async fn handoff_pause_public_stop_sampled_before_completion_cannot_poison_ack() {
    let f = running().await;
    let turn = start(&f).await;
    let hook = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "interrupt-target-sampled",
        f.harness.inner.card_id.as_str(),
        hook.clone(),
    );
    let harness = f.harness.clone();
    let stop = tokio::spawn(async move { harness.interrupt("stop".into()).await });
    hook.entered.notified().await;
    complete(&f, &turn).await;
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    let before = f.stored().await;
    assert_eq!(
        before.handoff_pause,
        crate::harness::handoff_pause::PauseState::Paused {
            receipt: receipt.clone()
        }
    );
    hook.release.notify_one();
    let stopped = stop.await.unwrap();
    let after = f.harness.snapshot().await;
    let after_phase = after.phase;
    let ack = f
        .harness
        .observe_envelope(
            Observation::TrackGoal {
                text: "after stop".into(),
            },
            51,
        )
        .await;
    assert!(
        ack.is_ok(),
        "system ACK failed: {ack:?}; stop={stopped:?}; memory={after_phase:?}; SQL={:?}",
        f.stored().await.phase
    );
    assert!(stopped.is_err());
    assert_eq!(after, before);
    assert_eq!(after_phase, HarnessPhaseTag::TurnCompleted);
    assert!(f.harness.inner.interrupt_deadline.lock().await.is_none());
    assert!(f.daemon.interrupted_turns_for_test().is_empty());
    assert_eq!(f.stored().await.handoff_pause, before.handoff_pause);
    assert_eq!(f.stored().await.push_watermark, 51);
    assert_eq!(
        f.harness.resume_from_handoff(receipt).await.unwrap(),
        crate::harness::handoff_pause::ReleaseOutcome::HealthyReleased
    );
    f.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn handoff_pause_late_stop_does_not_retarget_next_turn() {
    let f = running().await;
    let old = start(&f).await;
    let hook = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "interrupt-target-sampled",
        f.harness.inner.card_id.as_str(),
        hook.clone(),
    );
    let harness = f.harness.clone();
    let stop = tokio::spawn(async move { harness.interrupt("stop".into()).await });
    hook.entered.notified().await;
    complete(&f, &old).await;
    let new = start(&f).await;
    assert_ne!(new, old);
    let before = f.stored().await;
    hook.release.notify_one();
    stop.await.unwrap().unwrap();
    assert_eq!(
        f.harness.inner.state.lock().await.active_turn_id(),
        Some(new)
    );
    assert_eq!(
        f.harness.snapshot().await.phase,
        HarnessPhaseTag::TurnRunning
    );
    assert_eq!(f.stored().await, before);
    assert!(f.harness.inner.interrupt_deadline.lock().await.is_none());
    assert!(f.daemon.interrupted_turns_for_test().is_empty());
    f.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn handoff_pause_stop_without_target_rechecks_pause_at_owner() {
    let f = running().await;
    let hook = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "interrupt-target-sampled",
        f.harness.inner.card_id.as_str(),
        hook.clone(),
    );
    let harness = f.harness.clone();
    let stop = tokio::spawn(async move { harness.interrupt("stop".into()).await });
    hook.entered.notified().await;
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    let before = f.stored().await;
    assert_eq!(
        before.handoff_pause,
        crate::harness::handoff_pause::PauseState::Paused {
            receipt: receipt.clone()
        }
    );
    hook.release.notify_one();
    assert!(stop.await.unwrap().is_err());
    assert!(f.harness.interrupt("already paused".into()).await.is_err());
    assert_eq!(f.stored().await, before);
    assert!(f.harness.inner.interrupt_deadline.lock().await.is_none());
    assert!(f.harness.snapshot().await.interruption_intent.is_none());
    assert!(f.daemon.interrupted_turns_for_test().is_empty());
    system(&f, 62).await;
    assert_eq!(
        f.harness.resume_from_handoff(receipt).await.unwrap(),
        crate::harness::handoff_pause::ReleaseOutcome::HealthyReleased
    );
    f.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn handoff_pause_interrupt_failed_checkpoint_restores_staging_and_stops_writes() {
    let f = running().await;
    start(&f).await;
    let before = f.harness.snapshot().await;
    let issued = f.harness.inner.issued_turn_id.lock().await.clone();
    sqlx::query(concat!(
        "CREATE TRIGGER refuse_interrupt BEFORE UPDATE OF handle_state_json ON worker_sessions BEGIN ",
        "SELECT RAISE(ABORT, 'stop db failure'); END",
    ))
    .execute(f.repo.pool())
    .await
    .unwrap();
    assert!(f.harness.interrupt("stop".into()).await.is_err());
    assert_eq!(f.harness.snapshot().await, before);
    assert_eq!(*f.harness.inner.issued_turn_id.lock().await, issued);
    assert!(f.harness.inner.interrupt_deadline.lock().await.is_none());
    assert!(f.harness.inner.shutting_down.load(Ordering::SeqCst));
    assert!(f.daemon.interrupted_turns_for_test().is_empty());
}

/// Wait for a real watchdog/issuance tick to finish, including the paused admission attempt.
pub(super) async fn tick_completed(f: &Fixture) {
    let hook = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "harness-tick-completed",
        f.harness.inner.card_id.as_str(),
        hook.clone(),
    );
    tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
        .await
        .unwrap();
    hook.release.notify_one();
}

#[tokio::test]
async fn handoff_pause_watchdog_stop_completion_then_pause_keeps_deadline_contract() {
    let f = running().await;
    let turn = start(&f).await;
    // Advance only the watchdog's sampled execution clock; the loop still owns the tick and RPC.
    if let HarnessState::TurnRunning { watchdog_from, .. } =
        &mut *f.harness.inner.state.lock().await
    {
        *watchdog_from = Instant::now() - Duration::from_secs(7200);
    }
    phase(&f, HarnessPhaseTag::IssuingInterrupt).await;
    assert!(f.harness.snapshot().await.interruption_intent.is_some());
    assert!(f.harness.inner.interrupt_deadline.lock().await.is_some());
    complete(&f, &turn).await;
    assert!(f.harness.inner.interrupt_deadline.lock().await.is_none());
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    let before = f.harness.snapshot().await;
    tick_completed(&f).await;
    assert_eq!(f.harness.snapshot().await, before);
    assert_eq!(
        f.daemon.interrupted_turns_for_test(),
        vec![("thread-commit-wake".into(), turn)]
    );
    system(&f, 73).await;
    assert_eq!(
        f.harness.resume_from_handoff(receipt).await.unwrap(),
        crate::harness::handoff_pause::ReleaseOutcome::HealthyReleased
    );
    f.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn handoff_pause_public_stop_during_start_is_served_by_issuance_owner() {
    let f = running().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    f.daemon.install_turn_start_return_hook_for_test(
        crate::shared_codex_appserver::TurnStartReturnHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );
    f.harness
        .observe_user_message_durable("start held".into(), vec![], SendKey::unique_for_test())
        .await
        .unwrap();
    entered.notified().await;
    let turn = format!("fake-turn-{:04}", f.daemon.turn_start_count_for_test());
    f.daemon
        .set_active_turn_for_test("thread-commit-wake", &turn);
    tokio::time::timeout(Duration::from_secs(10), f.harness.interrupt("stop".into()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        f.harness.snapshot().await.phase,
        HarnessPhaseTag::IssuingInterrupt
    );
    assert_eq!(
        f.daemon.interrupted_turns_for_test(),
        vec![("thread-commit-wake".into(), turn.clone())]
    );
    release.notify_one();
    // The pause reply is on the ordinary lane and proves turn/start settlement finished.
    assert!(f.harness.pause_for_handoff(request(&f)).await.is_err());
    complete(&f, &turn).await;
    assert!(f.harness.inner.interrupt_deadline.lock().await.is_none());
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    system(&f, 84).await;
    assert_eq!(
        f.harness.resume_from_handoff(receipt).await.unwrap(),
        crate::harness::handoff_pause::ReleaseOutcome::HealthyReleased
    );
    f.harness.shutdown().await.unwrap();
}
