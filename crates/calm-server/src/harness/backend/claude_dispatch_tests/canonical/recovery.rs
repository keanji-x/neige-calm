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
    }
    for wrong in [WrongTarget::Card, WrongTarget::Runtime, WrongTarget::Track] {
        let mut other = target.clone();
        match wrong {
            WrongTarget::Card => other.card_id = uuid::Uuid::new_v4().to_string(),
            WrongTarget::Runtime => other.worker_session_id = uuid::Uuid::new_v4().to_string(),
            WrongTarget::Track => other.track_id = uuid::Uuid::new_v4().to_string(),
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
async fn canonical_claude_recovery_synthetic_outcome_holds_until_owner_sweep_then_retries_once() {
    let mut fx = Fixture::new("stall").await;
    fail_terminal_outcome(fx.owner.repo.as_ref()).await;
    fx.dispatch_selection(&TurnModelSelection {
        model: Some("claude-frozen".into()),
        effort: Some("high".into()),
    })
    .await
    .unwrap();
    let target = fx.target().await;
    let before = fx.checkpoint().await;
    assert!(
        fx.session.shutdown().await.is_err(),
        "outcome write failure must retain its claim"
    );
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
    // A synthetic outcome cannot clear the retained provider execution claim.
    let seals = Arc::new(ThreadSeals::new());
    let (reopened, session) =
        reopen_session_with_boot(&fx, fx.owner.repo.clone(), true, seals.clone()).await;
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
    // The real owner sweep, not the synthetic outcome, completes the settled claim.
    let host = session.host();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            host.provider_execution
                .sweep(fx.owner.repo.db(), host.execution.engine())
                .await
                .unwrap();
            if calm_truth::db::sqlite::provider_execution::load(
                fx.owner.repo.pool(),
                &fx.owner.worker,
            )
            .await
            .unwrap()
            .is_none()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        reopened
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_some()
    );
    seals.seal_for_deletion(&fx.owner.thread);
    assert!(
        crate::harness::input_dispatch::complete_once(
            fx.owner.repo.as_ref(),
            &reopened,
            &fx.owner.card,
            &fx.owner.worker,
        )
        .await
        .unwrap()
        .is_none(),
        "sealed pre-boot target must not consume END"
    );
    assert_eq!(fx.checkpoint().await, before);
    seals.unseal_after_rollback(&fx.owner.thread);
    fx.backend = reopened;
    fx.session = session;
    std::fs::write(fx.dir.path().join("scenario"), "exit").unwrap();
    let (state, app) = settled::application(&fx).await;
    let harness = fx.start_loop_in(state.harness.clone()).await;
    retry::action(&fx, &app, "retry", "retry-lost-driver").await;
    tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            let cp = fx.checkpoint().await;
            if cp.attempts.len() == 2 && cp.active.is_none() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let after = fx.checkpoint().await;
    assert!(after.pending.is_empty());
    assert_eq!(fx.session.user_line_writes_for_test(), 1);
    assert_eq!(
        std::fs::read_to_string(fx.dir.path().join("stdin"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let argv = std::fs::read_to_string(fx.dir.path().join("argv")).unwrap();
    assert!(
        argv.lines()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|args| args == ["--session-id", fx.owner.thread.as_str()])
    );
    for _ in 0..10 {
        harness.refresh_canonical_input().await.unwrap();
        assert_eq!(fx.checkpoint().await, after);
        tokio::task::yield_now().await;
    }
    harness.shutdown().await.unwrap();
    state.dispatcher.retire();
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

pub(super) async fn reopen_session(
    fx: &Fixture,
    repo: Arc<crate::db::sqlite::SqlxRepo>,
) -> (PlannerBackend, Arc<ClaudePlannerSession>) {
    reopen_session_with_boot(fx, repo, true, Arc::new(ThreadSeals::new())).await
}

async fn reopen_session_with_boot(
    fx: &Fixture,
    repo: Arc<crate::db::sqlite::SqlxRepo>,
    boot: bool,
    seals: Arc<ThreadSeals>,
) -> (PlannerBackend, Arc<ClaudePlannerSession>) {
    fx.session.abort_driver_for_test().await;
    let old = fx.session.host();
    let host = Arc::new(
        ClaudePlannerHost::new(
            Some(old.configured().unwrap().clone()),
            old.instructions_dir.parent().unwrap().parent().unwrap(),
            old.mcp_shim.clone(),
            old.mcp_socket.clone(),
        )
        .unwrap(),
    );
    if boot {
        crate::claude_planner::lifecycle::boot(repo.as_ref(), &host)
            .await
            .unwrap();
    }
    host.completion.bind(repo.clone());
    host.execution
        .bind(fx.dir._execution.new_kernel(repo.db().clone()).await);
    let session = Arc::new(
        ClaudePlannerSession::open(ClaudePlannerSessionParams {
            engine: host.execution.engine().cloned(),
            host,
            worker_session_id: fx.owner.worker.clone(),
            card_id: fx.owner.card.clone(),
            track_id: fx.owner.track.clone(),
            cwd: fx.dir.path().into(),
            instructions: "fixture".into(),
            calm_tools: provider::claude::translate::ToolNames::new("neige", Vec::<String>::new()),
            proxy: vec![],
            prior_total_tokens: 0,
            repo,
            seals,
        })
        .await
        .unwrap(),
    );
    let (held, _requests) = tokio::sync::mpsc::unbounded_channel();
    session.mark_installed(held);
    (PlannerBackend::claude_for_test(session.clone()), session)
}

#[tokio::test]
async fn canonical_claude_recovery_without_boot_proof_holds_but_live_provenance_ends() {
    use crate::harness::backend::SettledActivity;
    use crate::harness::native_admission::NativePromptAdmission;
    let fx = Fixture::with_storage("exit", FixtureStorage::Memory, false).await;
    let admission = NativePromptAdmission::new(|| Ok(()), || Box::pin(async { Ok(()) }));
    assert!(matches!(
        fx.backend
            .settled_activity(&fx.owner.card, &fx.owner.thread, &admission)
            .await
            .unwrap(),
        SettledActivity::Held
    ));
    fx.dispatch().await.unwrap();
    let target = fx.target().await;
    // With no boot proof, a driver spawned by this instance still proves C4 provenance.
    tokio::time::timeout(Duration::from_secs(15), async {
        while fx
            .backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_none()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        fx.backend
            .settled_activity(&fx.owner.card, &fx.owner.thread, &admission)
            .await
            .unwrap(),
        SettledActivity::Held
    ));
    let (backend, session) = reopen_session_with_boot(
        &fx,
        fx.owner.repo.clone(),
        false,
        Arc::new(ThreadSeals::new()),
    )
    .await;
    assert!(
        backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        backend
            .settled_activity(&fx.owner.card, &fx.owner.thread, &admission)
            .await
            .unwrap(),
        SettledActivity::Held
    ));
    assert!(
        crate::harness::input_dispatch::complete_once(
            fx.owner.repo.as_ref(),
            &backend,
            &fx.owner.card,
            &fx.owner.worker
        )
        .await
        .unwrap()
        .is_none()
    );
    session.shutdown().await.unwrap();
    fx.session.shutdown().await.unwrap();
}
