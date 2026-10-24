//! Claude turns and compaction use the real Engine -> SessionExecutor -> executor rig.
use super::claude_planner_session_fixture::{
    RegisteredRig, RegisteredRig as Rig, client_id, completed_turn, request, until_completed,
};
use calm_server::planner_permission_mode::PlannerPermissionMode;
use calm_truth::db::sqlite::provider_execution as sql;
use provider::{
    TurnModelSelection,
    control::{CompactSubmission, EditPreparation, TurnStartOutcome},
};
use std::time::Duration;

async fn cleared(rig: &RegisteredRig) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while sql::load(rig.repo.pool(), &rig.worker_session_id)
            .await
            .unwrap()
            .is_some()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
async fn turn(rig: &RegisteredRig) -> String {
    let mut events = rig.session().subscribe_events();
    let start = rig
        .session()
        .turn_start(
            &rig.thread,
            request(
                rig.text("alias regression"),
                &TurnModelSelection::default(),
                PlannerPermissionMode::Never,
                &client_id(),
            ),
        )
        .await
        .unwrap();
    let TurnStartOutcome::Started { turn } = start else {
        panic!("turn refused: {start:?}")
    };
    assert_eq!(
        completed_turn(&until_completed(&mut events).await)["status"],
        "completed"
    );
    cleared(rig).await;
    turn.into_string()
}
async fn aliases(rig: &RegisteredRig, count: usize) {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT execution_id, alias FROM workspace_executions")
            .fetch_all(rig.repo.pool())
            .await
            .unwrap();
    assert_eq!(rows.len(), count);
    let unique: std::collections::HashSet<_> = rows.iter().map(|(_, alias)| alias).collect();
    assert_eq!(unique.len(), count);
    for (id, alias) in rows {
        assert_eq!(
            alias,
            calm_server::provider_execution::sweep::alias(&rig.worker_session_id, &id)
        );
    }
}
#[tokio::test]
async fn c4_rewind_then_turn_launches_with_distinct_execution_aliases() {
    let rig = Rig::new("exit").await;
    assert!(matches!(
        rig.session()
            .prepare_edit_for_test(&rig.thread)
            .await
            .unwrap(),
        EditPreparation::Prepared { .. }
    ));
    turn(&rig).await;
    aliases(&rig, 2).await;
    assert_eq!(rig.read_bin("checks").unwrap().lines().count(), 1);
    assert_eq!(rig.read_bin("spawns").unwrap().lines().count(), 1);
}
#[tokio::test]
async fn c4_turn_compaction_turn_launches_with_distinct_execution_aliases() {
    let rig = RegisteredRig::new("exit").await;
    turn_compaction_turn(&rig).await;
}

pub(super) async fn turn_compaction_turn(rig: &RegisteredRig) {
    turn(rig).await;
    std::fs::write(rig.bin("scenario"), "compact").unwrap();
    let mut events = rig.session().subscribe_events();
    assert!(matches!(
        rig.session()
            .compact_start(&rig.thread, &TurnModelSelection::default())
            .await,
        CompactSubmission::Submitted { .. }
    ));
    assert_eq!(
        completed_turn(&until_completed(&mut events).await)["status"],
        "completed"
    );
    cleared(rig).await;
    std::fs::write(rig.bin("scenario"), "exit").unwrap();
    turn(rig).await;
    aliases(rig, 3).await;
    assert_eq!(rig.read_bin("spawns").unwrap().lines().count(), 3);
    assert_eq!(
        rig.outcomes().await.len(),
        2,
        "compaction creates no transcript turn"
    );
}
#[tokio::test]
async fn c4_turn_credentials_never_enter_intent_or_journal() {
    let rig = Rig::new("hold").await;
    let secret = "http://c4-private-user:c4-private-password@proxy.invalid:8080";
    let session = rig
        .open_session_with_environment(
            vec![("HTTP_PROXY".into(), "http_proxy".into(), secret.into())],
            rig.host.execution.engine().cloned(),
        )
        .await;
    let (held, _held_rx) = tokio::sync::mpsc::unbounded_channel();
    session.mark_installed(held);
    let mut events = session.subscribe_events();
    let start = session
        .turn_start(
            &rig.thread,
            request(
                rig.text("credential regression"),
                &TurnModelSelection::default(),
                PlannerPermissionMode::Never,
                &client_id(),
            ),
        )
        .await
        .unwrap();
    let TurnStartOutcome::Started { turn } = start else {
        panic!("turn refused: {start:?}")
    };
    let path = rig.bin("env");
    tokio::time::timeout(Duration::from_secs(15), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let env = std::fs::read_to_string(path).unwrap();
    let token = env
        .lines()
        .find_map(|line| line.strip_prefix("NEIGE_MCP_TOKEN="))
        .expect("provider received MCP credential");
    assert!(!token.is_empty());
    assert!(env.contains(secret));
    let intent: String = sqlx::query_scalar("SELECT intent_json FROM workspace_executions")
        .fetch_one(rig.repo.pool())
        .await
        .unwrap();
    for needle in [token, "c4-private-user", "c4-private-password"] {
        assert_eq!(
            (
                intent.contains(needle),
                rig.execution_journal_contains(needle)
            ),
            (false, false),
            "credentials stay outside both durable sinks"
        );
    }
    session
        .turn_interrupt(&rig.thread, &turn.into_string())
        .await;
    until_completed(&mut events).await;
    cleared(&rig).await;
}

#[tokio::test]
async fn c4_finish_turn_compares_identity_before_clearing_active() {
    let rig = Rig::new("exit").await;
    let old = turn(&rig).await;
    std::fs::write(rig.bin("scenario"), "hold").unwrap();
    let mut events = rig.session().subscribe_events();
    let start = rig
        .session()
        .turn_start(
            &rig.thread,
            request(
                rig.text("new turn"),
                &TurnModelSelection::default(),
                PlannerPermissionMode::Never,
                &client_id(),
            ),
        )
        .await
        .unwrap();
    let TurnStartOutcome::Started { turn } = start else {
        panic!("turn refused")
    };
    let current = turn.into_string();
    rig.session().finish_old_turn_for_test(&old);
    assert_eq!(
        rig.session()
            .active_turn_id_for_thread(&rig.thread)
            .as_deref(),
        Some(current.as_str())
    );
    rig.session().turn_interrupt(&rig.thread, &current).await;
    until_completed(&mut events).await;
    cleared(&rig).await;
}

#[tokio::test]
async fn c4_lost_driver_completion_is_idempotent_and_releases_the_live_slot() {
    use super::claude_planner_stack_fixture::{Root, Stack};
    use calm_server::{claude_planner::session::SettlePause, provider_execution as owner};
    use std::sync::Arc;
    for compaction in [false, true] {
        let root = Root::new("hold");
        let stack = Stack::boot(&root).await;
        let (_, card) = stack.create_claude_track().await;
        if compaction {
            stack.run_turn(&root, &card, "exit", "first").await;
        }
        let runtime = stack.runtime(&card).await;
        let session = stack
            .harness(&runtime.id)
            .claude_session_for_test()
            .unwrap();
        if compaction {
            let pause = SettlePause {
                entered: Arc::default(),
                release: Arc::default(),
            };
            session.set_before_turn_completed_pause_for_test(pause.clone());
            root.set_scenario("compact");
            let (status, body) = stack
                .send("POST", &format!("/api/cards/{card}/planner/compact"), None)
                .await;
            assert_eq!(status, axum::http::StatusCode::OK, "{body}");
            tokio::time::timeout(Duration::from_secs(15), pause.entered.notified())
                .await
                .unwrap();
        } else {
            let (status, body) = stack.post_input(&card, "lose this driver").await;
            assert_eq!(status, axum::http::StatusCode::OK, "{body}");
            stack.wait_phase(&runtime.id, "turn_running").await;
        }
        let runtime = stack.runtime(&card).await;
        let pool = stack.repo().sqlite_db().unwrap();
        let claim = sql::load(pool.read_pool(), &runtime.id)
            .await
            .unwrap()
            .unwrap();
        let id = owner::execution_id(claim.record()).unwrap();
        let turn = match &claim.record().purpose {
            owner::Purpose::Turn { turn_id } | owner::Purpose::Compaction { turn_id } => {
                turn_id.clone()
            }
            _ => panic!("wrong purpose"),
        };
        assert_eq!(
            session
                .active_turn_id_for_thread(&runtime.thread_id.clone().unwrap())
                .as_deref(),
            Some(turn.as_str())
        );
        session.abort_driver_for_test().await;
        let registered = &session.host().provider_execution;
        let engine = session.host().execution.engine();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                registered.sweep(&pool, engine).await.unwrap();
                if sql::load(pool.read_pool(), &runtime.id)
                    .await
                    .unwrap()
                    .is_none()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            session.active_turn_id_for_thread(&runtime.thread_id.clone().unwrap()),
            None
        );
        assert!(!registered.drivers.contains(&id));
        let before = stack.outcomes(&card).await;
        assert_eq!(before.len(), 1);
        if !compaction {
            assert_eq!(before[0]["status"], "interrupted");
        }
        registered.sweep(&pool, engine).await.unwrap();
        assert_eq!(stack.outcomes(&card).await, before);
        if compaction {
            stack.wait_phase(&runtime.id, "wedged").await;
        }
        stack.shutdown().await;
    }
}

#[tokio::test]
async fn c4_request_replacement_refuses_live_claim_and_allows_completed_owner() {
    use super::claude_planner_stack_fixture::{Root, Stack};
    let root = Root::new("hold");
    let stack = Stack::boot(&root).await;
    let (_, card) = stack.create_claude_track().await;
    let (status, body) = stack.post_input(&card, "retain this carrier").await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    let runtime = stack.runtime(&card).await;
    stack.wait_phase(&runtime.id, "turn_running").await;
    let pool = stack.repo().sqlite_db().unwrap();
    let before = sql::load(pool.read_pool(), &runtime.id)
        .await
        .unwrap()
        .unwrap();
    let hash = stack.row_hash(&runtime.id).await;
    let (status, body) = stack
        .send("POST", &format!("/api/cards/{card}/planner/restart"), None)
        .await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT, "{body}");
    assert_eq!(stack.runtime(&card).await.id, runtime.id);
    assert_eq!(stack.row_hash(&runtime.id).await, hash);
    assert_eq!(
        sql::load(pool.read_pool(), &runtime.id)
            .await
            .unwrap()
            .unwrap()
            .record()
            .execution_id,
        before.record().execution_id
    );
    let session = stack
        .harness(&runtime.id)
        .claude_session_for_test()
        .unwrap();
    let thread = runtime.thread_id.unwrap();
    let turn = session.active_turn_id_for_thread(&thread).unwrap();
    let mut events = session.subscribe_events();
    session.turn_interrupt(&thread, &turn).await;
    until_completed(&mut events).await;
    tokio::time::timeout(Duration::from_secs(15), async {
        while sql::load(pool.read_pool(), &runtime.id)
            .await
            .unwrap()
            .is_some()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (status, body) = stack
        .send("POST", &format!("/api/cards/{card}/planner/restart"), None)
        .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    assert_ne!(stack.runtime(&card).await.id, runtime.id);
    assert!(
        stack
            .outcomes(&card)
            .await
            .iter()
            .any(|outcome| outcome["id"] == turn)
    );
    stack.shutdown().await;
}

#[tokio::test]
async fn c4_protocol_result_precedes_late_stdin_failure() {
    use calm_server::test_seams::{PausePoint, install_pause_for_test};
    let rig = Rig::new("linger").await;
    let pause = PausePoint::default();
    install_pause_for_test("c4-protocol-ended", &rig.worker_session_id, pause.clone());
    let mut events = rig.session().subscribe_events();
    let start = rig
        .session()
        .turn_start(
            &rig.thread,
            request(
                rig.text("finish before a late control"),
                &TurnModelSelection::default(),
                PlannerPermissionMode::Never,
                &client_id(),
            ),
        )
        .await
        .unwrap();
    assert!(matches!(start, TurnStartOutcome::Started { .. }));
    tokio::time::timeout(Duration::from_secs(15), pause.entered.notified())
        .await
        .unwrap();
    let claim = sql::load(rig.repo.pool(), &rig.worker_session_id)
        .await
        .unwrap()
        .unwrap();
    let id = calm_server::provider_execution::execution_id(claim.record()).unwrap();
    rig.host
        .execution
        .engine()
        .unwrap()
        .close_stdin(&id)
        .await
        .unwrap();
    assert!(
        rig.session()
            .write_control_response_for_test()
            .await
            .is_err()
    );
    pause.release.notify_one();
    assert_eq!(
        completed_turn(&until_completed(&mut events).await)["status"],
        "completed"
    );
    cleared(&rig).await;
    assert_eq!(rig.outcomes().await[0]["status"], "completed");
}

#[tokio::test]
async fn c4_stdio_shutdown_is_idempotent() {
    use tokio::io::AsyncWriteExt;
    let rig = Rig::new("hold").await;
    let mut events = rig.session().subscribe_events();
    assert!(matches!(
        rig.session()
            .turn_start(
                &rig.thread,
                request(
                    rig.text("close the pipe"),
                    &TurnModelSelection::default(),
                    PlannerPermissionMode::Never,
                    &client_id()
                )
            )
            .await
            .unwrap(),
        TurnStartOutcome::Started { .. }
    ));
    let claim = sql::load(rig.repo.pool(), &rig.worker_session_id)
        .await
        .unwrap()
        .unwrap();
    let id = calm_server::provider_execution::execution_id(claim.record()).unwrap();
    let mut stream = calm_server::provider_execution::stdio::EngineStdio::new(
        rig.host.execution.engine().unwrap().clone(),
        id,
    );
    stream.shutdown().await.unwrap();
    stream.shutdown().await.unwrap();
    assert_eq!(
        completed_turn(&until_completed(&mut events).await)["status"],
        "failed"
    );
    cleared(&rig).await;
}

#[tokio::test]
async fn c4_shutdown_deadline_includes_provider_cleanup() {
    for scenario in ["term-flush", "term-ignore"] {
        let rig = Rig::new(scenario).await;
        assert!(matches!(
            rig.session()
                .turn_start(
                    &rig.thread,
                    request(
                        rig.text("shutdown cleanup"),
                        &TurnModelSelection::default(),
                        PlannerPermissionMode::Never,
                        &client_id()
                    )
                )
                .await
                .unwrap(),
            TurnStartOutcome::Started { .. }
        ));
        super::claude_planner_session_fixture::wait_for_file(&rig.bin("term-ready")).await;
        let start = std::time::Instant::now();
        rig.session().shutdown().await.unwrap();
        assert!(start.elapsed() < Duration::from_secs(20));
        if scenario == "term-flush" {
            assert_eq!(rig.read_bin("term-flushed").as_deref(), Some("flushed"));
        }
        assert!(
            sql::load(rig.repo.pool(), &rig.worker_session_id)
                .await
                .unwrap()
                .is_none()
        );
        let json: String = sqlx::query_scalar("SELECT settlement_json FROM workspace_executions")
            .fetch_one(rig.repo.pool())
            .await
            .unwrap();
        assert!(matches!(
            serde_json::from_str::<workspace_runtime::execution::Settlement>(&json).unwrap(),
            workspace_runtime::execution::Settlement::Journal(
                workspace_runtime::execution::ExecutionState::Drained { .. }
            )
        ));
    }
}

#[tokio::test]
async fn c4_track_stop_settles_within_quiesce_deadline_and_record_dies_with_row() {
    use calm_server::{claude_planner::session::SettlePause, db::prelude::*};
    use std::sync::Arc;
    let rig = Rig::new("hold").await;
    let pause = SettlePause {
        entered: Arc::default(),
        release: Arc::default(),
    };
    rig.session()
        .set_before_turn_completed_pause_for_test(pause.clone());
    assert!(matches!(
        rig.session()
            .turn_start(
                &rig.thread,
                request(
                    rig.text("stop this track"),
                    &TurnModelSelection::default(),
                    PlannerPermissionMode::Never,
                    &client_id()
                )
            )
            .await
            .unwrap(),
        TurnStartOutcome::Started { .. }
    ));
    let row = rig
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(
            rig.worker_session_id.clone(),
        ))
        .await
        .unwrap()
        .unwrap();
    let start = std::time::Instant::now();
    calm_server::claude_planner::lifecycle::sweep_track(
        rig.repo.as_ref(),
        &rig.host,
        row.track_id.as_str(),
    )
    .await
    .unwrap();
    assert!(start.elapsed() < workspace_runtime::execution::QUIESCE_DEADLINE);
    tokio::time::timeout(Duration::from_secs(15), pause.entered.notified())
        .await
        .unwrap();
    let claim = sql::load(rig.repo.pool(), &rig.worker_session_id)
        .await
        .unwrap()
        .unwrap();
    let id = calm_server::provider_execution::execution_id(claim.record()).unwrap();
    let mut tx = calm_truth::db::sqlite::begin_immediate_tx(rig.repo.db())
        .await
        .unwrap();
    calm_truth::db::sqlite::track_delete_tx(
        &mut tx,
        row.track_id.as_str(),
        rig.repo.track_area_cache(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(sql::list(rig.repo.pool()).await.unwrap().is_empty());
    rig.host
        .provider_execution
        .sweep(rig.repo.db(), rig.host.execution.engine())
        .await
        .unwrap();
    pause.release.notify_one();
    let _ = rig.session().wait_driver_for_test().await;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if matches!(
                rig.host.execution.engine().unwrap().output(&id, None).await,
                Ok(workspace_runtime::execution::ExecutionReply::Retired(_))
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn c4_interrupt_keeps_protocol_grace() {
    let rig = Rig::new("delayed-interrupt").await;
    let mut events = rig.session().subscribe_events();
    let start = rig
        .session()
        .turn_start(
            &rig.thread,
            request(
                rig.text("use the interrupt grace"),
                &TurnModelSelection::default(),
                PlannerPermissionMode::Never,
                &client_id(),
            ),
        )
        .await
        .unwrap();
    let TurnStartOutcome::Started { turn } = start else {
        panic!("turn refused")
    };
    super::claude_planner_session_fixture::wait_for_file(&rig.bin("interrupt-ready")).await;
    let at = std::time::Instant::now();
    rig.session()
        .turn_interrupt(&rig.thread, &turn.into_string())
        .await;
    tokio::time::sleep(Duration::from_secs(7)).await;
    let desired: String = sqlx::query_scalar("SELECT desired FROM workspace_executions")
        .fetch_one(rig.repo.pool())
        .await
        .unwrap();
    assert_eq!(desired, "run", "protocol grace must not signal Stop early");
    assert_eq!(
        completed_turn(&until_completed(&mut events).await)["status"],
        "interrupted"
    );
    assert!(at.elapsed() >= Duration::from_secs(8));
    cleared(&rig).await;
}

/// No harness consumes these events: the owner itself must write after the driver's budget.
#[tokio::test]
async fn c4_unconfirmed_completion_records_after_driver_budget_and_retains_claim() {
    let rig = Rig::new("term-ignore").await;
    let session = rig.session();
    session.set_settle_after_stop_for_test(Duration::from_millis(100));
    let mut events = session.subscribe_events();
    let start = session
        .turn_start(
            &rig.thread,
            request(
                rig.text("unconfirmed owner completion"),
                &TurnModelSelection::default(),
                PlannerPermissionMode::Never,
                &client_id(),
            ),
        )
        .await
        .unwrap();
    let TurnStartOutcome::Started { turn } = start else {
        panic!("turn refused: {start:?}")
    };
    super::claude_planner_session_fixture::wait_for_file(&rig.bin("term-ready")).await;
    sqlx::query(
        "CREATE TRIGGER c4_block_owner_stop BEFORE UPDATE OF desired ON workspace_executions
         WHEN NEW.desired='stop' BEGIN SELECT RAISE(FAIL, 'Stop blocked'); END",
    )
    .execute(rig.repo.pool())
    .await
    .unwrap();
    session
        .turn_interrupt(&rig.thread, &turn.into_string())
        .await;
    let seen = tokio::time::timeout(Duration::from_secs(15), until_completed(&mut events))
        .await
        .expect("owner completes after the driver settlement budget expires");
    assert_eq!(completed_turn(&seen)["status"], "interrupted");
    assert_eq!(rig.outcomes().await.len(), 1, "the owner wrote the outcome");
    let claim = sql::load(rig.repo.pool(), &rig.worker_session_id)
        .await
        .unwrap()
        .expect("unconfirmed completion retains admission ownership");
    sqlx::query("DROP TRIGGER c4_block_owner_stop")
        .execute(rig.repo.pool())
        .await
        .unwrap();
    let owner = &rig.host.provider_execution;
    let engine = rig.host.execution.engine().unwrap();
    let id = calm_server::provider_execution::execution_id(claim.record()).unwrap();
    owner
        .stop(
            rig.repo.db(),
            Some(engine),
            &rig.worker_session_id,
            &claim,
            calm_server::provider_execution::StopReason::Interrupted,
        )
        .await
        .unwrap();
    calm_server::provider_execution::wait_settled(engine, &id, Duration::from_secs(5))
        .await
        .unwrap();
    owner
        .finish(
            rig.repo.db(),
            Some(engine),
            &rig.worker_session_id,
            &claim,
            None,
        )
        .await
        .unwrap();
    cleared(&rig).await;
    assert_eq!(
        rig.outcomes().await.len(),
        1,
        "recovery preserves the outcome"
    );
}
