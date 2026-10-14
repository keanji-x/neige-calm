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
    *f.harness.inner.state.lock().await =
        HarnessState::turn_running("old-turn".into(), Instant::now());
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

#[tokio::test]
async fn compact_only_completed_confirms_success_and_invalidates_reading() {
    use serde_json::json;
    for status in ["completed", "interrupted", "failed", "unknown"] {
        let f = ready().await;
        let live = f.harness.inner.live_claim.writer();
        f.harness.compact().await.unwrap();
        for kind in [
            PlannerEventKind::TurnStarted {
                turn_id: "maintenance".into(),
            },
            PlannerEventKind::TurnCompleted {
                turn: json!({"id":"maintenance", "status":status}),
            },
        ] {
            on_notification(
                &f.harness.inner,
                &live,
                PlannerEvent {
                    thread_id: Some("thread-commit-wake".into()),
                    kind,
                },
            )
            .await
            .unwrap();
        }
        let snapshot = serde_json::to_value(snapshot_for(&f.harness.inner).await).unwrap();
        let expected = match status {
            "completed" => "completed",
            "interrupted" => "interrupted",
            "failed" => "failed",
            _ => "unconfirmed",
        };
        assert_eq!(
            snapshot["compact_outcome"]["status"], expected,
            "{snapshot}"
        );
        assert_eq!(
            snapshot["context_usage_invalidated"],
            status == "completed",
            "{snapshot}"
        );
        let previous = snapshot.clone();
        on_notification(
            &f.harness.inner,
            &live,
            PlannerEvent {
                thread_id: Some("thread-commit-wake".into()),
                kind: PlannerEventKind::TurnCompleted {
                    turn: json!({"id":"maintenance", "status":"completed"}),
                },
            },
        )
        .await
        .unwrap();
        let snapshot = serde_json::to_value(snapshot_for(&f.harness.inner).await).unwrap();
        assert_eq!(snapshot["compact_outcome"], previous["compact_outcome"]);
        assert_eq!(snapshot["last_turn_id"], "old-turn");
    }
}

#[tokio::test]
async fn compact_boundary_invalidates_even_when_maintenance_fails_or_is_interrupted() {
    use serde_json::json;
    for terminal in ["failed", "interrupted", "systemError", "aborted"] {
        let f = ready().await;
        let live = f.harness.inner.live_claim.writer();
        *f.harness.inner.token_usage.lock().await = Some(TokenUsage {
            used_tokens: 100_000,
            total_tokens: 15,
            context_window: Some(200_000),
            at_ms: 1,
        });
        f.harness.compact().await.unwrap();
        for kind in [
            PlannerEventKind::TurnStarted {
                turn_id: "maintenance".into(),
            },
            PlannerEventKind::ContextReplaced {
                turn_id: "maintenance".into(),
            },
            match terminal {
                "systemError" => PlannerEventKind::ThreadSystemError,
                "aborted" => PlannerEventKind::TurnAborted {
                    turn_id: "maintenance".into(),
                },
                _ => PlannerEventKind::TurnCompleted {
                    turn: json!({"id":"maintenance", "status":terminal}),
                },
            },
            PlannerEventKind::TokenUsage {
                params: json!({"turnId":"maintenance", "tokenUsage":{"last":{"totalTokens":1500},"total":{"totalTokens":1500}}}),
            },
        ] {
            on_notification(
                &f.harness.inner,
                &live,
                PlannerEvent {
                    thread_id: Some("thread-commit-wake".into()),
                    kind,
                },
            )
            .await
            .unwrap();
        }
        let snapshot = snapshot_for(&f.harness.inner).await;
        assert!(snapshot.context_usage_invalidated, "{terminal}");
        assert_ne!(
            snapshot.compact_outcome.unwrap().status,
            crate::harness::compact_outcome::CompactStatus::Completed
        );
        assert_eq!(snapshot.token_usage.unwrap().total_tokens, 15);
    }
}

#[tokio::test]
async fn compact_automatic_boundary_is_not_manual_success_and_needs_a_new_turn_reading() {
    use serde_json::json;
    let f = ready().await;
    let live = f.harness.inner.live_claim.writer();
    *f.harness.inner.state.lock().await =
        HarnessState::turn_running("old-turn".into(), Instant::now());
    for (thread, turn) in [("foreign", "old-turn"), ("thread-commit-wake", "stale")] {
        on_notification(
            &f.harness.inner,
            &live,
            PlannerEvent {
                thread_id: Some(thread.into()),
                kind: PlannerEventKind::ContextReplaced {
                    turn_id: turn.into(),
                },
            },
        )
        .await
        .unwrap();
        assert!(
            !snapshot_for(&f.harness.inner)
                .await
                .context_usage_invalidated
        );
    }
    on_notification(
        &f.harness.inner,
        &live,
        PlannerEvent {
            thread_id: Some("thread-commit-wake".into()),
            kind: PlannerEventKind::ContextReplaced {
                turn_id: "old-turn".into(),
            },
        },
    )
    .await
    .unwrap();
    let reading = |turn: &str| PlannerEventKind::TokenUsage {
        params: json!({"turnId":turn, "tokenUsage":{"last":{"totalTokens":16000},"total":{"totalTokens":30},"modelContextWindow":200000}}),
    };
    for turn in ["old-turn", "foreign"] {
        on_notification(
            &f.harness.inner,
            &live,
            PlannerEvent {
                thread_id: Some("thread-commit-wake".into()),
                kind: reading(turn),
            },
        )
        .await
        .unwrap();
        assert!(
            snapshot_for(&f.harness.inner)
                .await
                .context_usage_invalidated
        );
    }
    *f.harness.inner.last_turn_id.lock().await = Some("next".into());
    *f.harness.inner.state.lock().await = HarnessState::turn_running("next".into(), Instant::now());
    on_notification(
        &f.harness.inner,
        &live,
        PlannerEvent {
            thread_id: Some("thread-commit-wake".into()),
            kind: reading("next"),
        },
    )
    .await
    .unwrap();
    let snapshot = snapshot_for(&f.harness.inner).await;
    assert!(!snapshot.context_usage_invalidated);
    assert!(snapshot.compact_outcome.is_none());
    assert_eq!(snapshot.token_usage.unwrap().used_tokens, 16000);
}
