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
        f.harness.snapshot().await.unwrap().phase,
        HarnessPhaseTag::Compacting
    );
    let snapshot = snapshot_for(&f.harness.inner).await.unwrap();
    assert!(
        !state_from_runtime_header(&snapshot.runtime_header(), &f.harness.inner.inputs)
            .can_issue_turn()
    );
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
        .historical_queue()
        .unwrap()
        .lock()
        .await
        .push_back(QueueEntry::user_message("waiting".into(), None, vec![]));
    assert!(matches!(
        f.harness.compact().await,
        Err(CalmError::Conflict(_))
    ));
    f.harness
        .inner
        .historical_queue()
        .unwrap()
        .lock()
        .await
        .clear();
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
    assert_eq!(
        f.harness.snapshot().await.unwrap().phase,
        HarnessPhaseTag::Wedged
    );
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
            f.harness.snapshot().await.unwrap().phase,
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
        f.harness.snapshot().await.unwrap().phase,
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
        f.harness.snapshot().await.unwrap().phase,
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
    let snapshot = snapshot_for(&f.harness.inner).await.unwrap();
    assert!(matches!(
        state_from_runtime_header(&snapshot.runtime_header(), &f.harness.inner.inputs),
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
        let snapshot = serde_json::to_value(snapshot_for(&f.harness.inner).await.unwrap()).unwrap();
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
        let snapshot = serde_json::to_value(snapshot_for(&f.harness.inner).await.unwrap()).unwrap();
        assert_eq!(snapshot["compact_outcome"], previous["compact_outcome"]);
        assert_eq!(snapshot["last_turn_id"], "old-turn");
    }
}

#[tokio::test]
async fn compact_boundary_invalidates_even_when_maintenance_fails_or_is_interrupted() {
    use serde_json::json;
    for terminal in ["failed", "interrupted", "systemError", "aborted", "timeout"] {
        let f = ready().await;
        let live = f.harness.inner.live_claim.writer();
        deliver(&f, PlannerEventKind::TokenUsage { params: json!({"turnId":"old-turn", "tokenUsage":{"last":{"totalTokens":100000},"total":{"totalTokens":15},"modelContextWindow":200000}}) }).await;
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
                "timeout" => PlannerEventKind::Ignored,
                "aborted" => PlannerEventKind::TurnAborted {
                    turn_id: "maintenance".into(),
                },
                _ => PlannerEventKind::TurnCompleted {
                    turn: json!({"id":"maintenance", "status":terminal}),
                },
            },
            PlannerEventKind::TokenUsage {
                params: json!({"turnId":"old-turn", "tokenUsage":{"last":{"totalTokens":1500},"total":{"totalTokens":1500}}}),
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
        if terminal == "timeout" {
            *f.harness.inner.state.lock().await = HarnessState::CompactionRunning {
                turn_id: "maintenance".into(),
                started_at: Instant::now() - Duration::from_secs(3600),
            };
            watchdog_tick(&f.harness.inner).await.unwrap();
            deliver(&f, reading("old-turn")).await;
        }
        let snapshot = snapshot_for(&f.harness.inner).await.unwrap();
        assert!(snapshot.context_usage_invalidated, "{terminal}");
        assert!(snapshot.visible_context_usage().is_none());
        assert_ne!(
            snapshot.compact_outcome.as_ref().unwrap().status,
            crate::harness::compact_outcome::CompactStatus::Completed
        );
        assert_eq!(
            snapshot.context_usage_invalidated_turn.as_deref(),
            Some("old-turn")
        );
        let stored: String =
            sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id = ?")
                .bind(&f.harness.inner.worker_session_id)
                .fetch_one(f.repo.pool())
                .await
                .unwrap();
        let snapshot = HarnessSnapshot::from_value_strict(serde_json::from_str(&stored).unwrap());
        let old = &f.harness.inner;
        let (reloaded, _ingress) = PlannerHarness::run_unstarted_for_test(
            PlannerHarnessParams {
                worker_session_id: old.worker_session_id.clone(),
                track_id: old.track_id.clone(),
                card_id: old.card_id.clone(),
                thread_id: old.thread_id.read().await.clone(),
                repo: old.repo.clone(),
                events: old.events.clone(),
                card_role_cache: old.card_role_cache.clone(),
                track_area_cache: old.track_area_cache.clone(),
                backend: old.backend.clone(),
                registry: crate::harness::HarnessRegistry::new(),
                config: HarnessConfig::default(),
                snapshot: (snapshot)
                    .try_into()
                    .expect("historical fixture snapshot contract"),
            },
            8,
        );
        let live = reloaded.inner.live_claim.writer();
        on_notification(
            &reloaded.inner,
            &live,
            PlannerEvent {
                thread_id: Some("thread-commit-wake".into()),
                kind: reading("old-turn"),
            },
        )
        .await
        .unwrap();
        assert!(
            reloaded.snapshot().await.unwrap().context_usage_invalidated,
            "reload {terminal}"
        );
        *reloaded.inner.state.lock().await = HarnessState::Issuing {
            since: Instant::now(),
            kind: IssuingKind::TurnStart,
        };
        *reloaded.inner.issued_turn_id.lock().await = Some("next".into());
        on_notification(
            &reloaded.inner,
            &live,
            PlannerEvent {
                thread_id: Some("thread-commit-wake".into()),
                kind: PlannerEventKind::TurnStarted {
                    turn_id: "next".into(),
                },
            },
        )
        .await
        .unwrap();
        on_notification(
            &reloaded.inner,
            &live,
            PlannerEvent {
                thread_id: Some("thread-commit-wake".into()),
                kind: reading("next"),
            },
        )
        .await
        .unwrap();
        assert!(!reloaded.snapshot().await.unwrap().context_usage_invalidated);
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
                .unwrap()
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
                .unwrap()
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
    let snapshot = snapshot_for(&f.harness.inner).await.unwrap();
    assert!(!snapshot.context_usage_invalidated);
    assert!(snapshot.compact_outcome.is_none());
    assert_eq!(snapshot.token_usage.unwrap().used_tokens, 16000);
}

async fn deliver(f: &Fixture, kind: PlannerEventKind) {
    on_notification(
        &f.harness.inner,
        &f.harness.inner.live_claim.writer(),
        PlannerEvent {
            thread_id: Some("thread-commit-wake".into()),
            kind,
        },
    )
    .await
    .unwrap();
}

fn reading(turn: &str) -> PlannerEventKind {
    PlannerEventKind::TokenUsage {
        params: serde_json::json!({"turnId":turn,"tokenUsage":{"last":{"totalTokens":1500},"total":{"totalTokens":30}}}),
    }
}

#[tokio::test]
async fn compact_boundary_during_real_interrupt_and_after_completion_is_correlated() {
    for failed in [false, true] {
        let f = ready().await;
        *f.harness.inner.state.lock().await =
            HarnessState::turn_running("old-turn".into(), Instant::now());
        deliver(&f, reading("old-turn")).await;
        f.daemon.fail_turn_interrupt_for_test(failed);
        let (entered, release) = f.daemon.hold_turn_interrupt_for_test();
        let interrupt = f.harness.interrupt("stop".into());
        tokio::pin!(interrupt);
        tokio::select! {
            _ = entered.notified() => {},
            result = &mut interrupt => panic!("interrupt was not held: {result:?}"),
        }

        assert!(matches!(
            *f.harness.inner.state.lock().await,
            HarnessState::Issuing {
                kind: IssuingKind::Interrupt { .. },
                ..
            }
        ));
        deliver(
            &f,
            PlannerEventKind::ContextReplaced {
                turn_id: "old-turn".into(),
            },
        )
        .await;
        assert!(
            f.harness
                .snapshot()
                .await
                .unwrap()
                .context_usage_invalidated
        );
        release.notify_one();
        interrupt.await.unwrap();
        deliver(
            &f,
            PlannerEventKind::TurnCompleted {
                turn: serde_json::json!({"id":"old-turn","status":if failed { "failed" } else { "interrupted" }}),
            },
        )
        .await;
        deliver(&f, reading("old-turn")).await;
        assert!(
            f.harness
                .snapshot()
                .await
                .unwrap()
                .context_usage_invalidated
        );
        assert_eq!(
            f.harness
                .snapshot()
                .await
                .unwrap()
                .token_usage
                .unwrap()
                .total_tokens,
            30
        );
    }
    let f = ready().await;
    *f.harness.inner.state.lock().await =
        HarnessState::turn_running("old-turn".into(), Instant::now());
    deliver(
        &f,
        PlannerEventKind::TurnCompleted {
            turn: serde_json::json!({"id":"old-turn","status":"completed"}),
        },
    )
    .await;
    deliver(
        &f,
        PlannerEventKind::ContextReplaced {
            turn_id: "old-turn".into(),
        },
    )
    .await;
    assert!(
        f.harness
            .snapshot()
            .await
            .unwrap()
            .context_usage_invalidated
    );
    *f.harness.inner.state.lock().await = HarnessState::Issuing {
        since: Instant::now(),
        kind: IssuingKind::TurnStart,
    };
    *f.harness.inner.issued_turn_id.lock().await = Some("next".into());
    // An old boundary cannot cross admission of a new user operation.
    deliver(
        &f,
        PlannerEventKind::TurnStarted {
            turn_id: "next".into(),
        },
    )
    .await;
    deliver(&f, reading("next")).await;
    for turn in ["old-turn", "foreign"] {
        deliver(
            &f,
            PlannerEventKind::ContextReplaced {
                turn_id: turn.into(),
            },
        )
        .await;
        assert!(
            !f.harness
                .snapshot()
                .await
                .unwrap()
                .context_usage_invalidated
        );
    }
}

#[tokio::test]
async fn compact_feedback_is_cleared_only_by_an_accepted_new_user_start() {
    let f = ready().await;
    f.harness.compact().await.unwrap();
    deliver(
        &f,
        PlannerEventKind::TurnStarted {
            turn_id: "maintenance".into(),
        },
    )
    .await;
    deliver(
        &f,
        PlannerEventKind::TurnCompleted {
            turn: serde_json::json!({"id":"maintenance","status":"completed"}),
        },
    )
    .await;
    assert!(
        f.harness
            .snapshot()
            .await
            .unwrap()
            .compact_outcome
            .is_some()
    );
    deliver(
        &f,
        PlannerEventKind::TurnStarted {
            turn_id: "foreign".into(),
        },
    )
    .await;
    assert!(
        f.harness
            .snapshot()
            .await
            .unwrap()
            .compact_outcome
            .is_some()
    );
    *f.harness.inner.state.lock().await = HarnessState::Issuing {
        since: Instant::now(),
        kind: IssuingKind::TurnStart,
    };
    *f.harness.inner.issued_turn_id.lock().await = Some("next".into());
    // turn/start's successful response records this before its queued TurnStarted arrives.
    *f.harness.inner.last_turn_id.lock().await = Some("next".into());
    deliver(
        &f,
        PlannerEventKind::TurnStarted {
            turn_id: "next".into(),
        },
    )
    .await;
    assert!(
        f.harness
            .snapshot()
            .await
            .unwrap()
            .compact_outcome
            .is_none()
    );
}

#[tokio::test]
async fn compact_codex_fresh_recv_keeps_ordinary_boundary_item_and_billing() {
    use crate::codex_appserver::Notification;
    use crate::harness::codex_events::CodexEvents;
    let f = ready().await;
    f.daemon
        .emit_notification_for_test(Notification::TurnStarted {
            thread_id: "thread-commit-wake".into(),
            turn: serde_json::json!({"id":"historical"}),
        });
    let mut stream = CodexEvents::subscribe(&f.daemon);
    *f.harness.inner.state.lock().await =
        HarnessState::turn_running("old-turn".into(), Instant::now());
    let params = serde_json::json!({"threadId":"thread-commit-wake","turnId":"old-turn","item":{"id":"boundary","type":"contextCompaction"}});
    f.daemon
        .emit_notification_for_test(Notification::parse("item/completed".into(), params.clone()));
    let boundary = stream.recv().await.unwrap();
    assert!(
        matches!(&boundary.kind, PlannerEventKind::ContextReplaced { turn_id } if turn_id == "old-turn")
    );
    on_notification(
        &f.harness.inner,
        &f.harness.inner.live_claim.writer(),
        boundary,
    )
    .await
    .unwrap();
    let item = stream.recv().await.unwrap();
    assert!(
        matches!(&item.kind, PlannerEventKind::Item { params: carried, .. } if carried == &params)
    );
    on_notification(&f.harness.inner, &f.harness.inner.live_claim.writer(), item)
        .await
        .unwrap();
    deliver(&f, reading("old-turn")).await;
    let snapshot = f.harness.snapshot().await.unwrap();
    assert!(snapshot.context_usage_invalidated);
    assert!(snapshot.visible_context_usage().is_none());
    assert_eq!(snapshot.token_usage.unwrap().total_tokens, 30);
    let rows = f
        .harness
        .inner
        .repo
        .transcript_rows_of_thread(f.harness.inner.card_id.as_str(), "thread-commit-wake")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].turn_id.as_deref(), Some("old-turn"));
}
