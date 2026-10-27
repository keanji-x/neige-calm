use super::*;
use crate::agent_backend_pool::codex::codex_route_tests::{bind, pool_for_repo};
use crate::operation::launch_cleanup_test_support::{
    FIXTURE_CONTROL_VERSION, read_legacy_v2_request,
};
use crate::shared_codex_appserver::native_owner_tests::{
    assert_no_request, connect_with_reader, request, send,
};
use calm_session::control::{ControlMsg, ControlReply};

#[tokio::test]
async fn agent_backend_publish_worker_binds_before_thread_start() {
    let mut harness = worker_lease_harness().await;
    let (_root, pool) = pool_for_repo(harness.repo.clone()).await;
    let other = pool.instances()[1].clone();
    let (_b, mut peer, _fb) = connect_with_reader(&other).await;
    harness.adapter.codex_backends = pool.clone();
    let (op_repo, op, mut output) = prepare_worker_app_server_interact(&harness, "a").await;
    let session_id = output.output_string("runtime_id", "test").unwrap();
    let binding: (String, Option<String>) = sqlx::query_as(
        "SELECT b.backend_id,s.thread_id FROM worker_session_backends b
         JOIN worker_sessions s ON s.id=b.worker_session_id WHERE s.id=?",
    )
    .bind(&session_id)
    .fetch_one(harness.repo.pool())
    .await
    .unwrap();
    assert_eq!(binding, ("other".into(), None));
    assert_eq!(
        output.data["env"]["CODEX_HOME"],
        other.shared_home().path().to_str().unwrap()
    );
    assert_no_request(&mut peer).await;
    // A becomes ready after commit. The spawn and redrive must retain B.
    let (_a, mut default, _fa) = connect_with_reader(pool.default_only()).await;
    let dir = calm_test_sockets::socket_dir("pool-viewer");
    let sock = dir.path().join("viewer.sock");
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    let (captured, mut commands) = tokio::sync::mpsc::unbounded_channel();
    let endpoint = tokio::spawn(async move {
        loop {
            let (mut connection, _) = listener.accept().await.unwrap();
            let reply = match read_legacy_v2_request(&mut connection).await.unwrap() {
                ControlMsg::EnsureProc(request) => {
                    captured.send(request).unwrap();
                    ControlReply::SpawnFailed {
                        disposition: calm_session::control::SpawnFailedDisposition::NoChildCreated,
                        error: "fixture viewer unavailable".into(),
                        child_already_reaped: true,
                    }
                }
                ControlMsg::Probe(_) => ControlReply::ProbeOk {
                    supervisor_version: FIXTURE_CONTROL_VERSION,
                    proc_running: false,
                },
                other => panic!("unexpected {other:?}"),
            };
            let _ = calm_session::write_frame(&mut connection, &reply).await;
        }
    });
    let server = McpServer::new_for_test(crate::mcp_server::McpShimConfig {
        shim_bin: dir.path().join("shim"),
        socket_path: dir.path().join("mcp.sock"),
    });
    harness.adapter.mcp_server = Some(server);
    harness.adapter.codex_backends = pool.clone();
    let mut daemon = DaemonClient::new_stub();
    daemon.proc_supervisor_sock = Some(sock);
    let ctx = SpawnCtx::new(
        harness.repo.clone(),
        op_repo.clone(),
        Arc::new(daemon),
        TerminalRendererRegistry::new_with_repo(harness.repo.clone()),
        harness.events.clone(),
        OperationCompletionBus::new(),
    );
    // Use the adapter's production workspace verification and spawn checkpoint.
    harness
        .adapter
        .app_server_interact(&mut output, &op, &ctx)
        .await
        .unwrap();
    op_repo
        .set_phase(&op, Phase::SpawnStarted, None)
        .await
        .unwrap()
        .applied()
        .expect("the native spawn checkpoint is owned");
    let op = op_repo
        .claim_drive_batch(1)
        .await
        .unwrap()
        .into_iter()
        .find(|claimed| claimed.id == op.id)
        .expect("claim the native spawn checkpoint");
    let spawn = harness.adapter.spawn_side_effect(&output, &op, &ctx);
    let serve = async {
        let start = request(&mut peer).await;
        assert_eq!(start["method"], "thread/start");
        send(
            &mut peer,
            json!({"id":start["id"],"result":{"thread":{"id":"bound-thread"}}}),
        )
        .await;
        let turn = request(&mut peer).await;
        assert_eq!(turn["method"], "turn/start");
        assert_eq!(turn["params"]["threadId"], "bound-thread");
        send(
            &mut peer,
            json!({"id":turn["id"],"result":{"turn":{"id":"bound-turn"}}}),
        )
        .await;
        send(&mut peer,json!({"method":"turn/started","params":{"threadId":"bound-thread","turn":{"id":"bound-turn"}}})).await;
    };
    let (outcome, _) = tokio::join!(spawn, serve);
    outcome.unwrap();
    let command = tokio::time::timeout(Duration::from_secs(5), commands.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(command.args.join(" ").contains(&other.remote_uri()));
    assert_eq!(
        command
            .envs
            .iter()
            .find(|(key, _)| key == "CODEX_HOME")
            .map(|(_, value)| value.as_str()),
        other.shared_home().path().to_str()
    );
    assert_no_request(&mut default).await;
    // Re-drive the same persisted runtime: it attaches on B without minting another turn.
    harness
        .adapter
        .spawn_side_effect(&output, &op, &ctx)
        .await
        .unwrap();
    assert_no_request(&mut peer).await;
    assert_no_request(&mut default).await;
    endpoint.abort();
}

#[tokio::test]
async fn agent_backend_worker_compensation_timeout_and_settle_interrupt_bound_instance() {
    for action in ["compensate_started", "compensate", "timeout", "settle"] {
        let mut harness = worker_lease_harness().await;
        let (_root, pool) = pool_for_repo(harness.repo.clone()).await;
        let (output, _, op) = prepare_worker_and_op(&harness, "a", "a").await;
        let id = output.output_string("runtime_id", "test").unwrap();
        let card = output.output_string("card_id", "test").unwrap();
        let terminal = output.output_string("terminal_id", "test").unwrap();
        bind(&harness.repo, &id, Some("bound-thread")).await;
        sqlx::query(
            "UPDATE worker_sessions SET state='running',active_turn_id='bound-turn' WHERE id=?",
        )
        .bind(&id)
        .execute(harness.repo.pool())
        .await
        .unwrap();
        let (_a, mut default, _fa) = connect_with_reader(pool.default_only()).await;
        let other = pool.instances()[1].clone();
        let (_b, mut peer, _fb) = connect_with_reader(&other).await;
        other.set_active_turn_for_test("bound-thread", "bound-turn");
        harness.adapter.codex_backends = pool.clone();
        let completion = OperationCompletionBus::new();
        let op_repo = Arc::new(SqlxOperationRepo::new(harness.repo.db().clone()));
        let ctx = SpawnCtx::new(
            harness.repo.clone(),
            op_repo.clone(),
            Arc::new(DaemonClient::new_stub()),
            TerminalRendererRegistry::new_with_repo(harness.repo.clone()),
            harness.events.clone(),
            completion.clone(),
        )
        .with_codex_backends(pool.clone());
        let runtime = crate::operation::OperationRuntime::new(
            op_repo,
            vec![],
            harness.events.clone(),
            completion,
            ctx.clone(),
        )
        .await
        .unwrap();
        let wiring =
            crate::harness::wiring::PlannerWiring::unconfigured_for_test(harness.repo.clone());
        let mut state = crate::state::AppState::from_parts(
            harness.repo.clone(),
            harness.events.clone(),
            Arc::new(DaemonClient::new_stub()),
            wiring.plugin,
            Arc::new(CodexClient::new_stub()),
            None,
            None,
        );
        state.codex_backends = pool.clone();
        if action == "settle" {
            sqlx::query("UPDATE tasks SET status='done',finished_at_ms=? WHERE track_id=?")
                .bind(crate::model::now_ms() - 600_000)
                .bind(&harness.track_id)
                .execute(harness.repo.pool())
                .await
                .unwrap();
            sqlx::query("UPDATE tracks SET closed_at=? WHERE id=?")
                .bind(crate::model::now_ms())
                .bind(&harness.track_id)
                .execute(harness.repo.pool())
                .await
                .unwrap();
        }
        let cleanup = async {
            match action {
                "compensate_started" => {
                    let plan = harness
                        .adapter
                        .plan_compensation(PhaseTag::SpawnStarted, "failed", &output, &op)
                        .await
                        .unwrap();
                    let mut started = op.clone();
                    started.phase = Phase::SpawnStarted;
                    // Native interruption is still addressed to B before the missing
                    // supervisor proof refuses row/workspace disposal.
                    assert!(matches!(
                        harness
                            .adapter
                            .compensate_step(&plan.steps[0], &output, &started, &ctx)
                            .await,
                        Err(CalmError::Conflict(_))
                    ));
                    let retained: i64 =
                        sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions WHERE id=?")
                            .bind(&id)
                            .fetch_one(harness.repo.pool())
                            .await
                            .unwrap();
                    assert_eq!(retained, 1);
                }
                "compensate" => {
                    let plan = harness
                        .adapter
                        .plan_compensation(PhaseTag::SpawnStarted, "failed", &output, &op)
                        .await
                        .unwrap();
                    harness
                        .adapter
                        .compensate_step(&plan.steps[0], &output, &op, &ctx)
                        .await
                        .unwrap();
                }
                "timeout" => runtime.fail_running_worker_card(&card).await.unwrap(),
                "settle" => crate::terminal_sweeper::end_settled_worker_session(
                    &state,
                    &crate::terminal_sweeper::SettledWorkerSession {
                        id: id.clone(),
                        provider: "codex".into(),
                        card_id: card.clone(),
                        terminal_id: terminal,
                        thread_id: Some("bound-thread".into()),
                    },
                )
                .await
                .unwrap(),
                _ => unreachable!(),
            }
        };
        let serve = async {
            let frame = request(&mut peer).await;
            assert_eq!(frame["method"], "turn/interrupt", "{action}");
            assert_eq!(frame["params"]["threadId"], "bound-thread");
            assert_eq!(frame["params"]["turnId"], "bound-turn");
            send(&mut peer, json!({"id":frame["id"],"result":{}})).await;
        };
        tokio::join!(cleanup, serve);
        assert_no_request(&mut default).await;
    }
}
