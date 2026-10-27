//! Publication tests use real worker preparation/commit and two native fixture peers.
use super::*;
use crate::agent_backend_pool::codex::codex_route_tests::{pool_for_repo, pool_with_weights, seed};
use crate::shared_codex_appserver::native_owner_tests::{assert_no_request, connect_with_reader};
use calm_truth::session_projection_repo::WorkerSessionProjectionRepo;

async fn worker_session_id(h: &WorkerLeaseHarness, output: &TxOutput) -> String {
    h.repo
        .session_projection_active_for_card(&output.output_string("card_id", "test").unwrap())
        .await
        .unwrap()
        .unwrap()
        .id
}

async fn binding(h: &WorkerLeaseHarness, output: &TxOutput) -> String {
    sqlx::query_scalar("SELECT backend_id FROM worker_session_backends WHERE worker_session_id=?")
        .bind(worker_session_id(h, output).await)
        .fetch_one(h.repo.pool())
        .await
        .unwrap()
}

fn authentication_hold(instance: &SharedCodexAppServer) {
    instance.emit_notification_for_test(crate::codex_appserver::Notification::Other {
        method: "error".into(),
        params: json!({"error":{"message":"Sign in again","codexErrorInfo":"unauthorized"}}),
    });
    assert!(instance.authentication_hold().is_some());
}

#[tokio::test]
async fn agent_backend_selection_respects_weight_inflight_and_readiness() {
    for scenario in ["weight", "inflight", "not_running", "authentication", "tie"] {
        let mut h = worker_lease_harness().await;
        let weights = if scenario == "weight" { [1, 2] } else { [1, 1] };
        let (_root, pool) = pool_with_weights(h.repo.clone(), &weights).await;
        let mut connections = Vec::new();
        for (index, instance) in pool.instances().iter().enumerate() {
            if scenario == "not_running" && index == 0 {
                continue;
            }
            connections.push(connect_with_reader(instance).await);
        }
        if scenario == "authentication" {
            authentication_hold(pool.default_only());
        }
        // Real storage is the load observation, including Starting rows without a thread.
        if matches!(scenario, "weight" | "inflight") {
            for id in if scenario == "weight" {
                vec!["default", "other"]
            } else {
                vec!["default"]
            } {
                let (_, session) = seed(&h.repo).await;
                sqlx::query("UPDATE worker_sessions SET thread_id=NULL WHERE id=?")
                    .bind(&session)
                    .execute(h.repo.pool())
                    .await
                    .unwrap();
                let mut tx = h.repo.pool().begin().await.unwrap();
                calm_truth::db::sqlite::session_backend::insert_tx(&mut tx, &session, "codex", id)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
            }
        }
        h.adapter.codex_backends = pool;
        let (output, _) = prepare_worker(&h, "selected").await;
        assert_eq!(
            binding(&h, &output).await,
            if scenario == "tie" {
                "default"
            } else {
                "other"
            },
            "{scenario}"
        );
        for (_, peer, _) in &mut connections {
            assert_no_request(peer).await;
        }
    }
}

#[tokio::test]
async fn agent_backend_unconfigured_and_single_backend_publish_writes_no_binding() {
    for configured in [false, true] {
        let mut h = worker_lease_harness().await;
        let root = if configured {
            let (root, pool) = pool_with_weights(h.repo.clone(), &[1]).await;
            // Not running: Single must not apply pool readiness policy.
            h.adapter.codex_backends = pool;
            Some(root)
        } else {
            None
        };
        let (output, _) = prepare_worker(&h, "single").await;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worker_session_backends")
            .fetch_one(h.repo.pool())
            .await
            .unwrap();
        assert_eq!(count, 0);
        assert_eq!(
            output.data["env"]["CODEX_HOME"],
            h.adapter.codex.codex_home_dir().to_str().unwrap()
        );
        drop(root);
    }
}

#[tokio::test]
async fn agent_backend_no_eligible_backend_rolls_back_without_rows_or_native_effect() {
    let mut h = worker_lease_harness().await;
    let (_root, pool) = pool_for_repo(h.repo.clone()).await;
    let mut peers = Vec::new();
    for instance in pool.instances() {
        peers.push(connect_with_reader(instance).await);
        authentication_hold(instance);
    }
    h.adapter.codex_backends = pool.clone();
    let error = try_prepare_worker_and_op(&h, "refused", "refused")
        .await
        .unwrap_err();
    assert!(matches!(error, CalmError::Internal(_)));
    let main_error = pool.default_only().not_running_error();
    assert_eq!(error.code(), main_error.code());
    assert_eq!(error.status(), main_error.status());
    // Both errors propagate to the driver's existing internal-error recovery;
    // neither is stored as a terminal keyed preparation refusal.
    assert!(crate::routes::idempotency_key::operation_failure_parts(&error).is_none());
    assert!(crate::routes::idempotency_key::operation_failure_parts(&main_error).is_none());
    for table in [
        "cards",
        "worker_sessions",
        "worker_session_backends",
        "workspace_leases",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(h.repo.pool())
            .await
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
    let phase: String =
        sqlx::query_scalar("SELECT phase FROM operations WHERE kind='codex-worker'")
            .fetch_one(h.repo.pool())
            .await
            .unwrap();
    assert_eq!(phase, "pending");
    for (_, peer, _) in &mut peers {
        assert_no_request(peer).await;
    }
}

#[tokio::test]
async fn agent_backend_worker_env_codex_home_matches_binding() {
    let mut h = worker_lease_harness().await;
    let (_root, pool) = pool_for_repo(h.repo.clone()).await;
    let other = pool.instances()[1].clone();
    let (_client, mut peer, _forwarder) = connect_with_reader(&other).await;
    h.adapter.codex_backends = pool;
    let (output, _) = prepare_worker(&h, "home").await;
    assert_eq!(binding(&h, &output).await, "other");
    let env: String = sqlx::query_scalar("SELECT env FROM terminals WHERE id=?")
        .bind(output.output_string("terminal_id", "test").unwrap())
        .fetch_one(h.repo.pool())
        .await
        .unwrap();
    let env: Value = serde_json::from_str(&env).unwrap();
    assert_eq!(
        env["CODEX_HOME"],
        other.shared_home().path().to_str().unwrap()
    );
    assert_eq!(output.data["env"], env);
    assert_no_request(&mut peer).await;
}

#[tokio::test]
async fn agent_backend_bound_backend_down_or_unknown_mint_never_reselects() {
    use crate::agent_backend_pool::codex::codex_route_tests::rebuild_pool;
    use crate::shared_codex_appserver::native_owner_tests::request;
    for unknown in [false, true] {
        let mut h = worker_lease_harness().await;
        let (root, pool) = pool_for_repo(h.repo.clone()).await;
        let other = pool.instances()[1].clone();
        let (_b, mut peer, _fb) = connect_with_reader(&other).await;
        h.adapter.codex_backends = pool;
        let (operations, op, mut output) = prepare_worker_app_server_interact(&h, "sticky").await;
        assert_eq!(binding(&h, &output).await, "other");
        let (_original_a, mut original_default, _original_fa) =
            connect_with_reader(h.adapter.codex_backends.default_only()).await;
        let restarted = rebuild_pool(root.path(), h.repo.clone()).await;
        let (_a, mut default, _fa) = connect_with_reader(restarted.default_only()).await;
        if !unknown {
            h.adapter.codex_backends = restarted.clone();
        }
        let dir = calm_test_sockets::socket_dir("bound-mint");
        h.adapter.mcp_server = Some(McpServer::new_for_test(crate::mcp_server::McpShimConfig {
            shim_bin: dir.path().join("shim"),
            socket_path: dir.path().join("mcp.sock"),
        }));
        let ctx = SpawnCtx::new_with_codex_backends(
            h.repo.clone(),
            operations.clone(),
            Arc::new(DaemonClient::new_stub()),
            TerminalRendererRegistry::new_with_repo(h.repo.clone()),
            h.events.clone(),
            OperationCompletionBus::new(),
            restarted.clone(),
        );
        h.adapter
            .app_server_interact(&mut output, &op, &ctx)
            .await
            .unwrap();
        operations
            .set_phase(&op, Phase::SpawnStarted, None)
            .await
            .unwrap()
            .applied()
            .unwrap();
        let op = operations
            .claim_drive_batch(1)
            .await
            .unwrap()
            .into_iter()
            .find(|o| o.id == op.id)
            .unwrap();
        if unknown {
            let serve = async {
                let frame = request(&mut peer).await;
                assert_eq!(frame["method"], "thread/start");
                // Native start may have succeeded; its receipt is lost.
                peer.close(None).await.unwrap();
            };
            let (outcome, _) = tokio::join!(h.adapter.spawn_side_effect(&output, &op, &ctx), serve);
            assert!(outcome.is_err());
            assert_no_request(&mut original_default).await;
            // Process reconstruction loses all in-memory selection state.
            h.adapter.codex_backends = restarted;
        }
        for _ in 0..2 {
            assert!(matches!(
                h.adapter.spawn_side_effect(&output, &op, &ctx).await,
                Err(CalmError::Internal(_))
            ));
            assert_eq!(binding(&h, &output).await, "other");
            assert_no_request(&mut default).await;
        }
    }
}

#[tokio::test]
async fn agent_backend_restart_resumes_bound_worker_only_on_its_backend() {
    use crate::agent_backend_pool::codex::codex_route_tests::rebuild_pool;
    use crate::shared_codex_appserver::native_owner_tests::{
        connect_for_cold_replay, rebuild_cache, request, resume_cached_without_source, send,
    };
    let mut h = worker_lease_harness().await;
    let (root, pool) = pool_for_repo(h.repo.clone()).await;
    let (_b, _peer, _fb) = connect_with_reader(&pool.instances()[1]).await;
    h.adapter.codex_backends = pool;
    let (output, _) = prepare_worker(&h, "restart").await;
    let session = worker_session_id(&h, &output).await;
    sqlx::query("UPDATE worker_sessions SET thread_id='restart-thread',state='running' WHERE id=?")
        .bind(&session)
        .execute(h.repo.pool())
        .await
        .unwrap();
    let restarted = rebuild_pool(root.path(), h.repo.clone()).await;
    for instance in restarted.instances() {
        rebuild_cache(instance).await;
        let expected = if instance.backend_id().is_default() {
            vec![]
        } else {
            vec![(
                "restart-thread".into(),
                output.output_string("card_id", "test").unwrap(),
            )]
        };
        assert_eq!(instance.resume_candidates_for_test(), expected);
        let (_client, mut peer, _forwarder) = connect_for_cold_replay(instance).await;
        if instance.backend_id().is_default() {
            resume_cached_without_source(instance).await;
            assert_no_request(&mut peer).await;
        } else {
            let serve = async {
                let frame = request(&mut peer).await;
                assert_eq!(frame["method"], "thread/resume");
                assert_eq!(frame["params"]["threadId"], "restart-thread");
                send(
                    &mut peer,
                    json!({"id":frame["id"],"result":{"thread":{"id":"restart-thread"}}}),
                )
                .await;
            };
            tokio::join!(resume_cached_without_source(instance), serve);
        }
    }
}

#[tokio::test]
async fn agent_backend_no_eligible_and_single_down_share_task_failure_semantics() {
    for pooled in [false, true] {
        let mut h = worker_lease_harness().await;
        let root = if pooled {
            let (root, pool) = pool_for_repo(h.repo.clone()).await;
            h.adapter.codex_backends = pool;
            Some(root)
        } else {
            None
        };
        // Seed the real task/operation and return its claim to the normal driver.
        let op = claim_worker_operation(&h, "unavailable", "unavailable").await;
        let operations = Arc::new(SqlxOperationRepo::new(h.repo.db().clone()));
        operations.release_pending_for_retry(&op.id).await.unwrap();
        let completion = OperationCompletionBus::new();
        let ctx = SpawnCtx::new_with_codex_backends(
            h.repo.clone(),
            operations.clone(),
            Arc::new(DaemonClient::new_stub()),
            TerminalRendererRegistry::new_with_repo(h.repo.clone()),
            h.events.clone(),
            completion.clone(),
            h.adapter.codex_backends.clone(),
        );
        let ctx =
            ctx.with_location_admission(crate::operation::location_admission::LocationAdmission {
                managed_root: h.repo_root.path().join("managed"),
                objective_checkout_root: h.repo_root.path().join("objectives"),
                teardown_targets: Default::default(),
                area_locks: crate::per_card_lock::new_keyed_locks(),
                track_locks: crate::per_card_lock::new_keyed_locks(),
            });
        let runtime = crate::operation::OperationRuntime::new_unchecked(
            operations,
            vec![Arc::new(h.adapter.clone())],
            h.events.clone(),
            completion,
            ctx,
        );
        runtime.drive().await.unwrap();
        let settled = tokio::time::timeout(Duration::from_secs(5), runtime.wait(&op.id))
            .await
            .unwrap()
            .unwrap();
        if pooled {
            assert!(matches!(
                settled.outcome,
                crate::operation::OperationOutcome::Stuck {
                    from_phase: PhaseTag::Pending,
                    ..
                }
            ));
        }
        let published: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions")
            .fetch_one(h.repo.pool())
            .await
            .unwrap();
        if pooled {
            assert_eq!(published, 0);
        } else {
            match &settled.outcome {
                crate::operation::OperationOutcome::Failed { from_phase, .. }
                | crate::operation::OperationOutcome::Stuck { from_phase, .. } => {
                    assert_ne!(*from_phase, PhaseTag::Pending);
                }
                other => panic!("expected unavailable-daemon failure, got {other:?}"),
            }
        }
        let failure =
            crate::routes::idempotency_key::keyed_create_result(settled.outcome).unwrap_err();
        assert_eq!(failure.code(), "operation_failed", "pooled={pooled}");
        assert_eq!(
            failure.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        // Re-driving cannot select again or turn an unavailable start into success.
        runtime.drive().await.unwrap();
        let settled = runtime.wait(&op.id).await.unwrap();
        assert!(crate::routes::idempotency_key::keyed_create_result(settled.outcome).is_err());
        drop(root);
    }
}
