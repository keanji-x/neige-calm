//! Recovery observations use the original driver and input checkpoint owners.
use super::*;

#[tokio::test]
async fn canonical_claude_recovery_lost_report_and_failed_fact_commit_stay_held() {
    for cancelled in [false, true] {
        let fx = Arc::new(Fixture::new("stall").await);
        let pause = crate::claude_planner::session::SettlePause {
            entered: Arc::default(),
            release: Arc::default(),
        };
        if cancelled {
            fx.session.hold_dispatch_result_for_test(pause.clone());
        } else {
            sqlx::query(
                "CREATE TRIGGER fail_fact BEFORE UPDATE OF handle_state_json ON worker_sessions
                WHEN json_extract(NEW.handle_state_json,
                    '$.canonical_inputs.attempts[0].delivery.kind')='unknown'
                BEGIN SELECT RAISE(ABORT,'fact failed'); END",
            )
            .execute(fx.owner.repo.pool())
            .await
            .unwrap();
        }
        let dispatch = tokio::spawn({
            let fx = fx.clone();
            async move { fx.dispatch().await }
        });
        if cancelled {
            tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
                .await
                .unwrap();
            dispatch.abort();
            let Err(cancelled) = dispatch.await else {
                panic!("dispatch must be cancelled")
            };
            assert!(cancelled.is_cancelled());
        } else {
            assert!(dispatch.await.unwrap().is_err());
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            while !fx.dir.path().join("spawns").exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let before = fx.checkpoint().await;
        assert!(matches!(
            before.attempts[0].delivery,
            InputDeliveryFact::CrossingUnknown
        ));
        assert!(before.active.is_some());
        fx.session.shutdown().await.unwrap();
        assert!(fx.complete().await.is_none());
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(fx.dispatch().await.is_err());
        assert_eq!(fx.checkpoint().await, before);
        assert_eq!(fx.session.user_line_writes_for_test(), 1);
        assert_eq!(
            std::fs::read_to_string(fx.dir.path().join("spawns"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn canonical_claude_recovery_exact_finished_local_target_can_close_once() {
    let fx = Fixture::new("exit").await;
    let mut events = fx.session.subscribe_events();
    fx.dispatch().await.unwrap();
    let target = fx.target().await;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if matches!(
                events.recv().await.unwrap().kind,
                provider::events::PlannerEventKind::TurnCompleted { .. }
            ) {
                break;
            }
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
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
    enum WrongTarget {
        Card,
        Runtime,
        Track,
        Proof,
        Thread,
        LocalTurn,
    }
    for wrong in [
        WrongTarget::Card,
        WrongTarget::Runtime,
        WrongTarget::Track,
        WrongTarget::Proof,
        WrongTarget::Thread,
        WrongTarget::LocalTurn,
    ] {
        let mut other = target.clone();
        match wrong {
            WrongTarget::Card => other.card_id = uuid::Uuid::new_v4().to_string(),
            WrongTarget::Runtime => other.worker_session_id = uuid::Uuid::new_v4().to_string(),
            WrongTarget::Track => other.track_id = uuid::Uuid::new_v4().to_string(),
            WrongTarget::Proof => other.proof = super::super::super::dispatch_tests::proof(),
            WrongTarget::Thread => {
                let ExecutionEndScope::LocalProcess { thread_id, .. } = &mut other.scope else {
                    panic!("local")
                };
                *thread_id = uuid::Uuid::new_v4();
            }
            WrongTarget::LocalTurn => {
                let ExecutionEndScope::LocalProcess { local_turn_id, .. } = &mut other.scope else {
                    panic!("local")
                };
                *local_turn_id = uuid::Uuid::new_v4();
            }
        }
        assert!(
            fx.backend
                .qualify_execution_end(&other)
                .await
                .unwrap()
                .is_none()
        );
    }
    assert!(
        fx.backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_some()
    );
    let before = fx.checkpoint().await;
    fx.wait_end().await;
    assert!(fx.complete().await.is_none());
    let after = fx.checkpoint().await;
    assert!(after.active.is_none());
    assert_eq!(after.attempts, before.attempts);
    assert!(matches!(
        after.attempts[0].delivery,
        InputDeliveryFact::Unknown
    ));
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_recovery_synthetic_restart_outcome_never_qualifies_end() {
    let fx = Fixture::new("stall").await;
    fail_terminal_outcome(fx.owner.repo.as_ref()).await;
    fx.dispatch().await.unwrap();
    let target = fx.target().await;
    let before = fx.checkpoint().await;
    fx.session.shutdown().await.unwrap();
    sqlx::query("DROP TRIGGER fail_outcome")
        .execute(fx.owner.repo.pool())
        .await
        .unwrap();
    let (thread, turn) = target.scope.thread_and_turn();
    crate::harness::turn_outcome::record(
        fx.owner.repo.as_ref(), &fx.owner.worker, &fx.owner.card, &fx.owner.track,
        &thread, &turn, &serde_json::json!({"id":turn, "status":"interrupted", "error":{"message":"neige restarted during this turn"}}),
        crate::harness::turn_outcome::Confirmation::Unconfirmed,
    ).await.unwrap();
    // Reopening the production session consumes only the actual stored native
    // identity. A new object has no joined driver evidence for this old target.
    let (reopened, session) = reopen_session(&fx, fx.owner.repo.clone()).await;
    assert!(
        reopened
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        crate::harness::input_dispatch::complete_once(
            fx.owner.repo.as_ref(),
            &reopened,
            &fx.owner.card,
            &fx.owner.worker
        )
        .await
        .unwrap()
        .is_none()
    );
    assert_eq!(fx.checkpoint().await, before);
    assert_eq!(fx.session.user_line_writes_for_test(), 1);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_recovery_cancel_before_first_write_survives_database_reopen() {
    let fx = Arc::new(Fixture::persistent("stall").await);
    let pause = crate::claude_planner::session::SettlePause {
        entered: Arc::default(),
        release: Arc::default(),
    };
    fx.session.hold_user_writer_for_test(pause.clone());
    let dispatch = tokio::spawn({
        let fx = fx.clone();
        async move { fx.dispatch().await }
    });
    tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
        .await
        .unwrap();
    dispatch.abort();
    let Err(cancelled) = dispatch.await else {
        panic!("dispatch must be cancelled")
    };
    assert!(cancelled.is_cancelled());
    pause.release.notify_one();
    fx.session.shutdown().await.unwrap();
    let before = fx.checkpoint().await;
    assert!(before.active.is_some());
    assert!(matches!(
        before.attempts[0].delivery,
        InputDeliveryFact::CrossingUnknown
    ));
    assert!(!fx.dir.path().join("stdin").exists());
    let spawns = || {
        std::fs::read_to_string(fx.dir.path().join("spawns"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    let original_spawns = spawns();
    assert!(original_spawns <= 1);
    // Reopen the original physical database after closing both connection roles.
    fx.owner.repo.db().close().await;
    let reopened = Arc::new(
        crate::db::sqlite::SqlxRepo::open(&fx.owner.db_url)
            .await
            .unwrap(),
    );
    let (backend, session) = reopen_session(&fx, reopened.clone()).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let replay = crate::harness::input_dispatch::initial(
        reopened.as_ref(),
        &fx.source,
        &fx.owner.card,
        &fx.owner.worker,
        &fx.submission,
        &backend,
        &TurnModelSelection::inherit(),
        PlannerPermissionMode::Never,
    )
    .await;
    session.shutdown().await.unwrap();
    assert!(replay.is_err());
    let mut tx = begin_immediate_tx(reopened.db()).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    assert_eq!(carrier.checkpoint(), &before);
    tx.commit().await.unwrap();
    assert_eq!(spawns(), original_spawns);
    assert!(!fx.dir.path().join("stdin").exists());
}

async fn reopen_session(
    fx: &Fixture,
    repo: Arc<crate::db::sqlite::SqlxRepo>,
) -> (PlannerBackend, Arc<ClaudePlannerSession>) {
    let session = Arc::new(
        ClaudePlannerSession::open(ClaudePlannerSessionParams {
            engine: fx.session.host().execution.engine().cloned(),
            host: fx.session.host().clone(),
            worker_session_id: fx.owner.worker.clone(),
            card_id: fx.owner.card.clone(),
            track_id: fx.owner.track.clone(),
            cwd: fx.dir.path().into(),
            instructions: "fixture".into(),
            calm_tools: provider::claude::translate::ToolNames::new("neige", Vec::<String>::new()),
            proxy: vec![],
            prior_total_tokens: 0,
            repo,
            seals: Arc::new(ThreadSeals::new()),
        })
        .await
        .unwrap(),
    );
    let (held, _requests) = tokio::sync::mpsc::unbounded_channel();
    session.mark_installed(held);
    (PlannerBackend::claude_for_test(session.clone()), session)
}
