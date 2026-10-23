//! Human retry uses the authenticated H2 route and the canonical loop/writer.
use super::*;
use crate::claude_planner::session::SettlePause;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test]
async fn canonical_claude_retry_live_waits_then_dispatches_once() {
    retry_sequence(false, false).await;
}

#[tokio::test]
async fn canonical_claude_retry_uncertain_successor_holds_again() {
    retry_sequence(true, false).await;
}

#[tokio::test]
async fn canonical_claude_recovery_h2_actions_restart_held_keep_and_retry() {
    retry_sequence(false, true).await;
}

pub(super) async fn action(
    fx: &Fixture,
    app: &axum::Router,
    action: &str,
    key: &str,
) -> serde_json::Value {
    let checkpoint = fx.checkpoint().await;
    let attempt = checkpoint.attempts.last().unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/cards/{}/conversation/input/{action}",
                    fx.owner.card
                ))
                .header(
                    header::COOKIE,
                    format!("{}={}", crate::auth::SESSION_COOKIE, fx.login),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .header("Idempotency-Key", key)
                .body(Body::from(
                    serde_json::json!({
                        "carrier_id":fx.owner.worker,
                        "submission_id":attempt.attempt.submission_id,
                        "attempt_id":attempt.attempt.attempt_id,
                        "revision":attempt.revision.to_string()
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).unwrap()
}

async fn retry_sequence(uncertain: bool, recover: bool) {
    let mut fx = if recover {
        Fixture::persistent("stall").await
    } else {
        Fixture::new("stall").await
    };
    let selection = TurnModelSelection {
        model: Some("claude-frozen".into()),
        effort: Some("high".into()),
    };
    fx.dispatch_selection(&selection).await.unwrap();
    let original = fx.checkpoint().await;
    let InputInvocation::Recorded { configuration } = &original.attempts[0].invocation else {
        panic!("recorded")
    };
    assert_eq!(configuration.model, selection.model);
    assert_eq!(configuration.effort, selection.effort);
    assert!(original.attempts[0].correlation.local_turn_id.is_some());
    assert!(matches!(
        original.attempts[0].delivery,
        InputDeliveryFact::Unknown
    ));
    if recover {
        // Reopening preserves delivery facts; the boot proof permits END separately.
        fx.session.shutdown().await.unwrap();
        fx.owner.repo.db().close().await;
        let repo = Arc::new(
            crate::db::sqlite::SqlxRepo::open(&fx.owner.db_url)
                .await
                .unwrap(),
        );
        let (backend, session) = recovery::reopen_session(&fx, repo.clone()).await;
        fx.owner.repo = repo;
        fx.backend = backend;
        fx.session = session;
        assert_eq!(fx.checkpoint().await, original);
    }
    let (state, app) = settled::application(&fx).await;
    let harness = fx.start_loop_in(state.harness.clone()).await;
    for _ in 0..10 {
        harness.refresh_canonical_input().await.unwrap();
        assert_eq!(
            fx.checkpoint().await.attempts.len(),
            1,
            "held is never auto-retried"
        );
        tokio::task::yield_now().await;
    }
    if recover {
        std::fs::write(fx.dir.path().join("scenario"), "exit").unwrap();
    }
    let result = action(&fx, &app, "retry", "retry-original").await;
    assert_eq!(result["waiting_for_settlement"], true);
    if recover {
        tokio::time::timeout(Duration::from_secs(25), async {
            while {
                let cp = fx.checkpoint().await;
                cp.attempts.len() != 2 || cp.active.is_some()
            } {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        fx.phase("turn_completed").await;
        let after = fx.checkpoint().await;
        assert!(after.active.is_none());
        assert!(after.pending.is_empty());
        assert_eq!(fx.session.user_line_writes_for_test(), 1);
        let lines = std::fs::read_to_string(fx.dir.path().join("stdin")).unwrap();
        assert_eq!(lines.lines().count(), 1);
        assert!(lines.contains("canonical human"));
        let argv = std::fs::read_to_string(fx.dir.path().join("argv")).unwrap();
        assert!(
            argv.lines()
                .collect::<Vec<_>>()
                .windows(2)
                .any(|args| args == ["--session-id", fx.owner.thread.as_str()])
        );
        for _ in 0..10 {
            harness.refresh_canonical_input().await.unwrap();
            assert_eq!(fx.checkpoint().await.attempts, after.attempts);
            tokio::task::yield_now().await;
        }
        harness.shutdown().await.unwrap();
        state.dispatcher.retire();
        recovered_keep().await;
        return;
    }
    let pause = SettlePause {
        entered: Arc::default(),
        release: Arc::default(),
    };
    if uncertain {
        fx.session.fail_user_flush_for_test(pause.clone());
    }
    for _ in 0..10 {
        harness.refresh_canonical_input().await.unwrap();
        tokio::task::yield_now().await;
    }
    assert_eq!(
        fx.session.user_line_writes_for_test(),
        1,
        "retry cannot run beside original driver"
    );
    assert_eq!(fx.checkpoint().await.attempts.len(), 1);
    std::fs::write(fx.dir.path().join("scenario"), "exit").unwrap();
    let turn = fx
        .session
        .active_turn_id_for_thread(&fx.owner.thread)
        .unwrap();
    assert!(matches!(
        fx.session.turn_interrupt(&fx.owner.thread, &turn).await,
        provider::control::InterruptOutcome::LocalSignalDelivered
    ));

    if uncertain {
        tokio::time::timeout(Duration::from_secs(25), pause.entered.notified())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !std::fs::read_to_string(fx.dir.path().join("stdin"))
                .is_ok_and(|lines| lines.contains("canonical human"))
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        pause.release.notify_one();
        tokio::time::timeout(Duration::from_secs(25), async {
            loop {
                let cp = fx.checkpoint().await;
                if cp.attempts.len() == 2
                    && matches!(cp.attempts[1].delivery, InputDeliveryFact::Unknown)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    } else {
        tokio::time::timeout(Duration::from_secs(25), async {
            while fx.checkpoint().await.attempts.len() != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        fx.phase("turn_completed").await;
    }
    tokio::time::timeout(Duration::from_secs(25), async {
        while fx.checkpoint().await.active.is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let after = fx.checkpoint().await;
    assert_eq!(after.attempts.len(), 2);
    let predecessor = &after.attempts[0];
    let successor = &after.attempts[1];
    assert_ne!(
        predecessor.attempt.submission_id,
        successor.attempt.submission_id
    );
    assert_ne!(predecessor.attempt.attempt_id, successor.attempt.attempt_id);
    assert_ne!(
        predecessor.correlation.local_turn_id,
        successor.correlation.local_turn_id
    );
    assert_eq!(
        predecessor.invocation, successor.invocation,
        "frozen invocation survives retry"
    );
    assert!(matches!(successor.delivery, InputDeliveryFact::Unknown));
    assert!(after.active.is_none());
    for _ in 0..10 {
        harness.refresh_canonical_input().await.unwrap();
        assert_eq!(fx.checkpoint().await.attempts, after.attempts);
        tokio::task::yield_now().await;
    }
    let lines = std::fs::read_to_string(fx.dir.path().join("stdin")).unwrap();
    assert_eq!(
        lines.lines().count(),
        1,
        "stall consumed no predecessor line; successor written exactly once"
    );
    assert!(lines.contains("canonical human"));
    let argv = std::fs::read_to_string(fx.dir.path().join("argv")).unwrap();
    assert!(argv.lines().any(|arg| arg == "--model=claude-frozen"));
    assert!(argv.lines().any(|arg| arg == "--effort=high"));
    assert!(
        !fx.dir.path().join("init-calls").exists(),
        "retry never resamples the model catalog"
    );
    harness.shutdown().await.unwrap();
    state.dispatcher.retire();
}

async fn recovered_keep() {
    let mut fx = Fixture::persistent("stall").await;
    fx.dispatch_selection(&TurnModelSelection {
        model: Some("claude-frozen".into()),
        effort: Some("high".into()),
    })
    .await
    .unwrap();
    let before = fx.checkpoint().await;
    fx.session.shutdown().await.unwrap();
    fx.owner.repo.db().close().await;
    let repo = Arc::new(
        crate::db::sqlite::SqlxRepo::open(&fx.owner.db_url)
            .await
            .unwrap(),
    );
    let (backend, session) = recovery::reopen_session(&fx, repo.clone()).await;
    fx.owner.repo = repo;
    fx.backend = backend;
    fx.session = session;
    let (state, app) = settled::application(&fx).await;
    let harness = fx.start_loop_in(state.harness.clone()).await;
    action(&fx, &app, "keep", "keep-recovered").await;
    tokio::time::timeout(Duration::from_secs(25), async {
        while fx.checkpoint().await.active.is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let after = fx.checkpoint().await;
    assert!(after.active.is_none());
    assert!(after.pending.is_empty());
    assert!(matches!(
        after.receipts.as_slice(),
        [HumanInputResolutionReceipt {
            decision: HumanInputResolutionDecision::Keep { .. },
            ..
        }]
    ));
    assert_eq!(after.attempts.len(), 1);
    assert_eq!(after.attempts[0].delivery, before.attempts[0].delivery);
    assert_eq!(fx.session.user_line_writes_for_test(), 0);
    assert!(!fx.dir.path().join("stdin").exists());
    harness.shutdown().await.unwrap();
    state.dispatcher.retire();
}
