use super::completed_commit_tests::Fixture;
use super::*;

async fn ready() -> Fixture {
    let fixture = Fixture::new().await;
    *fixture.harness.inner.last_turn_id.lock().await = Some("old-turn".into());
    fixture
}

#[tokio::test]
async fn compact_blocks_issuance_until_its_turn_starts_and_snapshot_recovers_busy() {
    let f = ready().await;
    f.harness.compact().await.unwrap();
    assert_eq!(
        f.daemon.compacted_threads_for_test(),
        ["thread-commit-wake"]
    );
    assert_eq!(
        f.harness.snapshot().await.phase,
        HarnessPhaseTag::Compacting
    );
    let snapshot = snapshot_for(&f.harness.inner).await;
    assert!(!state_from_snapshot(&snapshot).can_issue_turn());
    assert!(!f.harness.inner.state.lock().await.can_issue_turn());
}

#[tokio::test]
async fn compact_refuses_busy_queued_missing_history_and_shutting_down_without_provider_call() {
    let f = ready().await;
    *f.harness.inner.state.lock().await = HarnessState::TurnRunning {
        turn_id: "old-turn".into(),
        started_at: Instant::now(),
    };
    assert!(matches!(
        f.harness.compact().await,
        Err(CalmError::Conflict(_))
    ));
    *f.harness.inner.state.lock().await = HarnessState::Idle;
    f.harness
        .inner
        .pending_queue
        .lock()
        .await
        .push_back(QueueEntry::user_message("waiting".into(), None, vec![]));
    assert!(matches!(
        f.harness.compact().await,
        Err(CalmError::Conflict(_))
    ));
    f.harness.inner.pending_queue.lock().await.clear();
    *f.harness.inner.last_turn_id.lock().await = None;
    assert!(matches!(
        f.harness.compact().await,
        Err(CalmError::Conflict(_))
    ));
    *f.harness.inner.last_turn_id.lock().await = Some("old-turn".into());
    f.harness.inner.shutting_down.store(true, Ordering::SeqCst);
    assert!(matches!(
        f.harness.compact().await,
        Err(CalmError::Conflict(_))
    ));
    assert!(f.daemon.compacted_threads_for_test().is_empty());
}

#[tokio::test]
async fn compact_start_timeout_wedges_instead_of_silently_resuming() {
    let f = ready().await;
    *f.harness.inner.state.lock().await = HarnessState::Compacting {
        since: Instant::now() - Duration::from_secs(60),
    };
    watchdog_tick(&f.harness.inner).await.unwrap();
    assert_eq!(f.harness.snapshot().await.phase, HarnessPhaseTag::Wedged);
    assert!(!f.harness.inner.state.lock().await.can_issue_turn());
}

#[tokio::test]
async fn compact_accepts_only_new_turn_and_releases_issuance_on_its_completion() {
    use crate::harness::planner_event::PlannerEventKind;
    use serde_json::json;
    let f = ready().await;
    let live = f.harness.inner.live_claim.writer();
    f.harness.compact().await.unwrap();
    for (thread, turn) in [
        ("other-thread", "compact-turn"),
        ("thread-commit-wake", "old-turn"),
    ] {
        on_notification(
            &f.harness.inner,
            &live,
            PlannerEvent {
                thread_id: Some(thread.into()),
                kind: PlannerEventKind::TurnStarted {
                    turn_id: turn.into(),
                },
            },
        )
        .await
        .unwrap();
        assert_eq!(
            f.harness.snapshot().await.phase,
            HarnessPhaseTag::Compacting
        );
    }
    on_notification(
        &f.harness.inner,
        &live,
        PlannerEvent {
            thread_id: Some("thread-commit-wake".into()),
            kind: PlannerEventKind::TurnStarted {
                turn_id: "compact-turn".into(),
            },
        },
    )
    .await
    .unwrap();
    assert_eq!(
        f.harness.snapshot().await.phase,
        HarnessPhaseTag::Compacting
    );
    on_notification(
        &f.harness.inner,
        &live,
        PlannerEvent {
            thread_id: Some("thread-commit-wake".into()),
            kind: PlannerEventKind::TurnCompleted {
                turn: json!({"id":"old-turn", "status":"completed"}),
            },
        },
    )
    .await
    .unwrap();
    assert_eq!(
        f.harness.snapshot().await.phase,
        HarnessPhaseTag::Compacting
    );
    on_notification(
        &f.harness.inner,
        &live,
        PlannerEvent {
            thread_id: Some("thread-commit-wake".into()),
            kind: PlannerEventKind::TurnCompleted {
                turn: json!({"id":"compact-turn", "status":"completed"}),
            },
        },
    )
    .await
    .unwrap();
    assert!(f.harness.inner.state.lock().await.can_issue_turn());
    assert_eq!(
        f.harness.inner.last_turn_id.lock().await.as_deref(),
        Some("old-turn")
    );
    assert!(
        f.harness
            .inner
            .repo
            .transcript_rows_of_thread(f.harness.inner.card_id.as_str(), "thread-commit-wake")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn compact_recovery_requires_reset_instead_of_reissuing_maintenance() {
    let f = ready().await;
    f.harness.compact().await.unwrap();
    let snapshot = snapshot_for(&f.harness.inner).await;
    assert!(matches!(
        state_from_snapshot(&snapshot),
        HarnessState::Wedged { .. }
    ));
}
