use super::*;
use crate::agent_backend_pool::codex::codex_route_tests::{bind, pool_for_repo};
use crate::db::RepoRead;
use crate::operation::launch_cleanup_test_support::{
    FIXTURE_CONTROL_VERSION, read_legacy_v2_request,
};
use crate::shared_codex_appserver::native_owner_tests::{
    assert_no_request, connect_with_reader, request, send,
};
use crate::worker_model_defaults::tests::Warnings;
use calm_session::control::{ControlMsg, ControlReply};
use calm_types::worker_model::WorkerModelProvider;

#[tokio::test]
async fn agent_backend_publish_worker_binds_before_thread_start() {
    publish_worker(false, false).await;
}

#[tokio::test]
async fn worker_model_defaults_spawn_and_redrive_keep_selection_and_backend() {
    publish_worker(true, false).await;
}

#[tokio::test]
async fn worker_model_defaults_legacy_operation_without_thread_warns_on_spawn_and_redrive() {
    publish_worker(false, true).await;
}

async fn publish_worker(with_defaults: bool, legacy: bool) {
    let mut harness = worker_lease_harness().await;
    if with_defaults {
        crate::db::RepoOutOfDomain::settings_upsert(
            harness.repo.as_ref(),
            "worker_model_defaults_codex",
            r#"{"model":"private-a","effort":"high"}"#,
        )
        .await
        .unwrap();
    }
    let (_root, pool) = pool_for_repo(harness.repo.clone()).await;
    let other = pool.instances()[1].clone();
    let (_b, mut peer, _fb) = connect_with_reader(&other).await;
    harness.adapter.codex_backends = pool.clone();
    let (op_repo, op, mut output) = prepare_worker_app_server_interact(&harness, "a").await;
    if legacy {
        // Base could publish this operation before it ever minted a thread.
        harness.repo.db().writer().execute(sqlx::query(
            "UPDATE cards SET payload=json_remove(payload,'$.worker_model_selection') WHERE id=?"
        ).bind(output.data["card_id"].as_str().unwrap())).await.unwrap();
        output.result["payload"]
            .as_object_mut()
            .unwrap()
            .remove("worker_model_selection");
        harness
            .repo
            .db()
            .writer()
            .execute(
                sqlx::query("UPDATE operations SET tx_output_json=? WHERE id=?")
                    .bind(serde_json::to_string(&output).unwrap())
                    .bind(&op.id),
            )
            .await
            .unwrap();
    }
    if with_defaults || legacy {
        crate::db::RepoOutOfDomain::settings_upsert(
            harness.repo.as_ref(),
            "worker_model_defaults_codex",
            r#"{"model":"private-b","effort":"low"}"#,
        )
        .await
        .unwrap();
    }
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
    let output = op.tx_output.as_ref().unwrap();
    let warnings = Warnings::default();
    let spawn = warnings.run(harness.adapter.spawn_side_effect(output, &op, &ctx));
    let serve = async {
        let start = request(&mut peer).await;
        assert_eq!(start["method"], "thread/start");
        if with_defaults {
            assert_eq!(start["params"]["config"]["model"], "private-a");
            assert_eq!(start["params"]["config"]["model_reasoning_effort"], "high");
        } else {
            assert!(start["params"]["config"].get("model").is_none());
            assert!(
                start["params"]["config"]
                    .get("model_reasoning_effort")
                    .is_none()
            );
        }
        send(
            &mut peer,
            json!({"id":start["id"],"result":{"thread":{"id":"bound-thread"}}}),
        )
        .await;
        let turn = request(&mut peer).await;
        assert_eq!(turn["method"], "turn/start");
        assert_eq!(turn["params"]["threadId"], "bound-thread");
        if with_defaults {
            assert_eq!(turn["params"]["model"], "private-a");
            assert_eq!(turn["params"]["effort"], "high");
        } else {
            assert!(turn["params"].get("model").is_none());
            assert!(turn["params"].get("effort").is_none());
        }
        send(
            &mut peer,
            json!({"id":turn["id"],"result":{"turn":{"id":"bound-turn"}}}),
        )
        .await;
        send(&mut peer,json!({"method":"turn/started","params":{"threadId":"bound-thread","turn":{"id":"bound-turn"}}})).await;
    };
    let (outcome, _) = tokio::join!(spawn, serve);
    outcome.unwrap();
    let card = output.data["card_id"].as_str().unwrap();
    if legacy {
        warnings.assert_legacy(card, WorkerModelProvider::Codex);
    } else {
        warnings.assert_no_legacy(card);
    }
    let command = tokio::time::timeout(Duration::from_secs(5), commands.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(command.args.join(" ").contains(&other.remote_uri()));
    if with_defaults {
        let shell = command.args.last().unwrap();
        let parsed = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                &format!("codex() {{ printf '%s\\0' \"$@\"; }}\n{shell}"),
            ])
            .output()
            .unwrap();
        assert!(parsed.status.success());
        let output = String::from_utf8(parsed.stdout).unwrap();
        let argv: Vec<_> = output.split_terminator('\0').collect();
        assert!(argv.contains(&"--config=model=\"private-a\""), "{argv:?}");
        assert!(
            argv.contains(&"--config=model_reasoning_effort=\"high\""),
            "{argv:?}"
        );
    } else {
        assert!(!command.args.last().unwrap().contains("--config=model"));
    }

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
    let warnings = Warnings::default();
    warnings
        .run(harness.adapter.spawn_side_effect(output, &op, &ctx))
        .await
        .unwrap();
    if legacy {
        warnings.assert_legacy(card, WorkerModelProvider::Codex);
        assert!(
            harness
                .repo
                .card_get(card)
                .await
                .unwrap()
                .unwrap()
                .payload
                .get("worker_model_selection")
                .is_none()
        );
    } else {
        warnings.assert_no_legacy(card);
    }
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
        (harness.repo.db()).writer().execute(sqlx::query(
            "UPDATE worker_sessions SET state='running',active_turn_id='bound-turn' WHERE id=?",
        )
        .bind(&id))
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
            (harness.repo.db())
                .writer()
                .execute(
                    sqlx::query("UPDATE tasks SET status='done',finished_at_ms=? WHERE track_id=?")
                        .bind(crate::model::now_ms() - 600_000)
                        .bind(&harness.track_id),
                )
                .await
                .unwrap();
            (harness.repo.db())
                .writer()
                .execute(
                    sqlx::query("UPDATE tracks SET closed_at=? WHERE id=?")
                        .bind(crate::model::now_ms())
                        .bind(&harness.track_id),
                )
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

// Corrupt only the temporary persisted card after a valid publication. Both
// stages use the production checkpoint/claim path; replay also retains a real
// thread/turn minted through the adapter before reloading its operation.
async fn rejects_snapshot_without_token_rotation(snapshot: Value, replay: bool) {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Abort(tokio::task::AbortHandle);
    impl Drop for Abort {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    let mut harness = worker_lease_harness().await;
    crate::db::RepoOutOfDomain::settings_upsert(
        harness.repo.as_ref(),
        "worker_model_defaults_codex",
        r#"{"model":"private-a","effort":"high"}"#,
    )
    .await
    .unwrap();
    let (_root, pool) = pool_for_repo(harness.repo.clone()).await;
    let other = pool.instances()[1].clone();
    let (_b, mut peer, forwarder) = connect_with_reader(&other).await;
    let _forwarder_abort = Abort(forwarder.abort_handle());
    harness.adapter.codex_backends = pool.clone();
    let (operations, op, mut output) = prepare_worker_app_server_interact(&harness, "a").await;
    // Only B is ready at publication. A becomes ready after its binding is frozen.
    let (_a, mut default, default_forwarder) = connect_with_reader(pool.default_only()).await;
    let _default_abort = Abort(default_forwarder.abort_handle());
    let dir = calm_test_sockets::socket_dir("snapshot-rejection");
    let sock = dir.path().join("viewer.sock");
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    let supervisor_requests = Arc::new(AtomicUsize::new(0));
    let requests = supervisor_requests.clone();
    // This endpoint observes every control request and never starts a process.
    let endpoint = tokio::spawn(async move {
        loop {
            let (mut connection, _) = listener.accept().await.unwrap();
            let message = read_legacy_v2_request(&mut connection).await.unwrap();
            requests.fetch_add(1, Ordering::SeqCst);
            let reply = match message {
                ControlMsg::EnsureProc(_) => ControlReply::SpawnFailed {
                    disposition: calm_session::control::SpawnFailedDisposition::NoChildCreated,
                    error: "fixture viewer unavailable".into(),
                    child_already_reaped: true,
                },
                ControlMsg::Probe(_) => ControlReply::ProbeOk {
                    supervisor_version: FIXTURE_CONTROL_VERSION,
                    proc_running: false,
                },
                ControlMsg::Attach(_) => ControlReply::Error {
                    kind: calm_session::control::ControlErrorKind::UnknownProc,
                    message: "fixture viewer was never created".into(),
                },
                other => panic!("unexpected {other:?}"),
            };
            let _ = calm_session::write_frame(&mut connection, &reply).await;
        }
    });
    let _endpoint_abort = Abort(endpoint.abort_handle());
    harness.adapter.mcp_server = Some(McpServer::new_for_test(crate::mcp_server::McpShimConfig {
        shim_bin: dir.path().join("shim"),
        socket_path: dir.path().join("mcp.sock"),
    }));
    let mut daemon = DaemonClient::new_stub();
    daemon.proc_supervisor_sock = Some(sock);
    let ctx = SpawnCtx::new(
        harness.repo.clone(),
        operations.clone(),
        Arc::new(daemon),
        TerminalRendererRegistry::new_with_repo(harness.repo.clone()),
        harness.events.clone(),
        OperationCompletionBus::new(),
    );
    harness
        .adapter
        .app_server_interact(&mut output, &op, &ctx)
        .await
        .unwrap();
    operations
        .set_phase_and_tx_output(&op, Phase::SpawnStarted, &output)
        .await
        .unwrap()
        .expect("persist the verified native spawn checkpoint");
    let mut op = operations
        .claim_drive_operation(&op.id)
        .await
        .unwrap()
        .expect("claim the first native launch");
    assert_eq!(op.phase, Phase::SpawnStarted);
    if replay {
        let spawn = harness
            .adapter
            .spawn_side_effect(op.tx_output.as_ref().unwrap(), &op, &ctx);
        let serve = async {
            let start = request(&mut peer).await;
            assert_eq!(start["method"], "thread/start");
            send(
                &mut peer,
                json!({"id":start["id"],"result":{"thread":{"id":"retained-thread"}}}),
            )
            .await;
            let turn = request(&mut peer).await;
            assert_eq!(turn["method"], "turn/start");
            assert_eq!(turn["params"]["threadId"], "retained-thread");
            send(
                &mut peer,
                json!({"id":turn["id"],"result":{"turn":{"id":"retained-turn"}}}),
            )
            .await;
            send(&mut peer, json!({"method":"turn/started","params":{"threadId":"retained-thread","turn":{"id":"retained-turn"}}})).await;
        };
        let (result, _) = tokio::join!(spawn, serve);
        result.unwrap();
        let previous_claim = op.lease_owner.clone();
        // Model a lost acknowledgement before SpawnSucceeded: release the real
        // SpawnStarted claim, preserving runtime and the latest durable output.
        operations
            .set_phase(&op, Phase::SpawnStarted, None)
            .await
            .unwrap()
            .applied()
            .expect("release the completed spawn claim for recovery");
        let recovered = SqlxOperationRepo::new(harness.repo.db().clone());
        op = recovered
            .claim_drive_operation(&op.id)
            .await
            .unwrap()
            .expect("recover the persisted SpawnStarted operation");
        assert_eq!(op.phase, Phase::SpawnStarted);
        assert_ne!(op.lease_owner, previous_claim);
    }
    let output = op.tx_output.as_ref().unwrap();
    let card = output.output_string("card_id", "test").unwrap();
    let session = output.output_string("runtime_id", "test").unwrap();
    let runtime_query = "SELECT b.backend_id,s.thread_id,s.active_turn_id FROM worker_sessions s \
        JOIN worker_session_backends b ON b.worker_session_id=s.id WHERE s.id=?";
    let before_runtime: (String, Option<String>, Option<String>) = sqlx::query_as(runtime_query)
        .bind(&session)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap();
    assert_eq!(before_runtime.0, "other");
    assert_eq!(
        before_runtime.1.as_deref(),
        replay.then_some("retained-thread")
    );
    assert_eq!(
        before_runtime.2.as_deref(),
        replay.then_some("retained-turn")
    );
    harness.repo.db().writer().execute(sqlx::query(
        "UPDATE cards SET payload=json_set(payload,'$.worker_model_selection',json(?)) WHERE id=?"
    ).bind(snapshot.to_string()).bind(&card)).await.unwrap();
    let token_query = "SELECT c.hashed_token,s.mcp_token_hash FROM card_mcp_tokens c \
        JOIN worker_sessions s ON s.card_id=c.card_id WHERE c.card_id=? AND s.id=?";
    // Codex publication already minted both credentials; absence is not the oracle.
    let before_tokens: (String, String) = sqlx::query_as(token_query)
        .bind(&card)
        .bind(&session)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap();
    assert!(!before_tokens.0.is_empty());
    assert_eq!(before_tokens.0, before_tokens.1);
    assert_no_request(&mut peer).await;
    assert_no_request(&mut default).await;
    let before_requests = supervisor_requests.load(Ordering::SeqCst);
    let result = harness.adapter.spawn_side_effect(output, &op, &ctx).await;
    assert!(
        matches!(result, Err(CalmError::Conflict(_))),
        "{:?}",
        result.err()
    );
    assert_no_request(&mut peer).await;
    assert_no_request(&mut default).await;
    assert_eq!(supervisor_requests.load(Ordering::SeqCst), before_requests);
    let after_runtime: (String, Option<String>, Option<String>) = sqlx::query_as(runtime_query)
        .bind(&session)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap();
    assert_eq!(after_runtime, before_runtime);
    let after_tokens: (String, String) = sqlx::query_as(token_query)
        .bind(&card)
        .bind(&session)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap();
    // Reap every fake task before the expected-red credential assertion.
    for task in [endpoint, forwarder, default_forwarder] {
        task.abort();
        if let Err(error) = task.await {
            assert!(error.is_cancelled(), "{error}");
        }
    }
    assert_eq!(
        after_tokens, before_tokens,
        "no token rotation on snapshot refusal"
    );
}

#[tokio::test]
async fn worker_model_defaults_codex_first_launch_rejects_null_snapshot_without_token_rotation() {
    rejects_snapshot_without_token_rotation(Value::Null, false).await;
}

#[tokio::test]
async fn worker_model_defaults_codex_first_launch_rejects_malformed_snapshot_without_token_rotation()
 {
    rejects_snapshot_without_token_rotation(
        json!({"provider":"codex","selection":{"kind":"explicit","value":{"model":"a"}}}),
        false,
    )
    .await;
}

#[tokio::test]
async fn worker_model_defaults_codex_first_launch_rejects_provider_mismatch_without_token_rotation()
{
    rejects_snapshot_without_token_rotation(
        json!({"provider":"claude","selection":{"kind":"inherit"}}),
        false,
    )
    .await;
}

#[tokio::test]
async fn worker_model_defaults_codex_replay_rejects_null_snapshot_without_token_rotation() {
    rejects_snapshot_without_token_rotation(Value::Null, true).await;
}

#[tokio::test]
async fn worker_model_defaults_codex_replay_rejects_malformed_snapshot_without_token_rotation() {
    rejects_snapshot_without_token_rotation(
        json!({"provider":"codex","selection":{"kind":"explicit","value":{"model":"a"}}}),
        true,
    )
    .await;
}

#[tokio::test]
async fn worker_model_defaults_codex_replay_rejects_provider_mismatch_without_token_rotation() {
    rejects_snapshot_without_token_rotation(
        json!({"provider":"claude","selection":{"kind":"inherit"}}),
        true,
    )
    .await;
}
