//! ACP regressions use production boot, Engine, SessionExecutor and executor.
use super::*;
use calm_server::provider_execution as execution;
use calm_truth::db::sqlite::{provider_execution as sql, workspace_execution as executions};

async fn wait(mut ready: impl AsyncFnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !ready().await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("ACP Engine progress");
}
async fn claim(stack: &Stack, worker: &str) -> Option<sql::Claim> {
    sql::load(stack.repo().sqlite_db().unwrap().read_pool(), worker)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c4_acp_existing_execution_refuses_before_credential_mint() {
    use workspace_runtime::execution::*;
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (track, card) = create(&stack).await;
    let worker = stack.runtime(&card).await.id;
    let session = session(&stack, &worker).await;
    let owner = &session.host().provider_execution;
    let pool = stack.repo().sqlite_db().unwrap();
    let mut intent = test_support::intent(root.path().to_owned());
    let id = intent.metadata.id.clone();
    intent.metadata.consumer = "acp-planner".into();
    intent.metadata.alias = ProcessAlias(execution::sweep::alias(&worker, id.as_str()));
    intent.metadata.owner = OperationRef(format!("worker-session:{worker}"));
    intent.metadata.track = TrackId(track);
    intent.metadata.program.program = "/bin/sh".into();
    intent.metadata.program.args = vec!["-c".into(), "read held".into()];
    let (admitted, guard) = owner
        .admit(
            &pool,
            session.host().execution.engine().unwrap(),
            &worker,
            intent,
            execution::Purpose::Turn {
                turn_id: uuid::Uuid::new_v4().to_string(),
            },
        )
        .await
        .unwrap();
    // Observe transient minting too: later cleanup must not hide a fence bypass.
    sqlx::query("CREATE TABLE c43_mints (token TEXT)")
        .execute(pool.writer_pool())
        .await
        .unwrap();
    sqlx::query(concat!(
        "CREATE TRIGGER c43_mint AFTER UPDATE OF mcp_token_hash ON worker_sessions WHEN ",
        "NEW.mcp_token_hash IS NOT NULL BEGIN INSERT INTO c43_mints VALUES (NEW.mcp_token_hash); ",
        "END",
    ))
    .execute(pool.writer_pool())
    .await
    .unwrap();
    let before = requests(&root, "initialize").len();
    let (status, body) = stack.post_input(&card, "must not mint").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let harness = stack.harness(&worker);
    wait(async || harness.refused_issuances_for_test() > 0).await;
    let mints: i64 = sqlx::query_scalar("SELECT count(*) FROM c43_mints")
        .fetch_one(pool.read_pool())
        .await
        .unwrap();
    assert_eq!(
        mints, 0,
        "retained provider claim fences credential minting"
    );
    assert_eq!(requests(&root, "initialize").len(), before);
    assert!(requests(&root, "session/prompt").is_empty());
    assert_eq!(claim(&stack, &worker).await.unwrap(), admitted);
    drop(guard);
    stack.shutdown().await;
}

async fn session(
    stack: &Stack,
    worker: &str,
) -> std::sync::Arc<calm_server::acp_planner::session::AcpPlannerSession> {
    stack.harness(worker).acp_session_for_test().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c4_acp_registration_env_only_in_payload() {
    use sha2::{Digest, Sha256};
    let root = Root::new("unused");
    let secret = "c43-operator-secret-credential";
    let proxy = "http://c43-user:c43-password@proxy.invalid:8080";
    let stack = boot_with(
        &root,
        json!({"OPERATOR_SECRET": secret,"HTTP_PROXY":proxy,
        "ACP_FIXTURE_PROBE_KEYS":"OPERATOR_SECRET,HTTP_PROXY,NEIGE_MCP_TOKEN"}),
        "1.0.0",
    )
    .await;
    let (_, card) = create(&stack).await;
    std::fs::write(root.path().join("scenario"), "hold").unwrap();
    let (status, body) = stack.post_input(&card, "payload").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait(async || requests(&root, "session/prompt").len() == 1).await;
    let environments = std::fs::read_to_string(root.path().join("environment.jsonl")).unwrap();
    let env: Value = environments
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|env| env["NEIGE_MCP_TOKEN"] == true)
        .unwrap();
    let digest = |value: &str| format!("{:x}", Sha256::digest(value.as_bytes()));
    assert_eq!(env["probe"]["OPERATOR_SECRET"], digest(secret));
    assert_eq!(env["probe"]["HTTP_PROXY"], digest(proxy));
    let rows: Vec<String> = sqlx::query_scalar("SELECT intent_json FROM workspace_executions WHERE json_extract(intent_json,'$.consumer')='acp-planner'")
        .fetch_all(stack.repo().sqlite_db().unwrap().read_pool()).await.unwrap();
    assert_eq!(rows.len(), 1);
    for needle in [secret, proxy] {
        assert!(!rows[0].contains(needle));
        root.assert_execution_journal_excludes(needle);
    }
    let metadata: workspace_runtime::execution::ExecutionMetadata =
        serde_json::from_str(&rows[0]).unwrap();
    for key in ["OPERATOR_SECRET", "HTTP_PROXY", "NEIGE_MCP_TOKEN"] {
        assert!(
            !metadata
                .program
                .environment
                .iter()
                .any(|(name, _)| name == key)
        );
    }
    assert!(
        !metadata
            .program
            .environment
            .iter()
            .any(|(name, _)| name == "NEIGE_ACP_PLANNER")
    );
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c4_acp_setup_failure_settles_before_retire() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    let worker = stack.runtime(&card).await.id;
    std::fs::write(root.path().join("scenario"), "wrong-identity").unwrap();
    let (status, body) = stack.post_input(&card, "setup refuses").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait(async || {
        sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM workspace_executions WHERE desired='retire' AND settled_at_ms IS NOT NULL)")
            .fetch_one(stack.repo().sqlite_db().unwrap().read_pool()).await.unwrap()
    }).await;
    assert!(claim(&stack, &worker).await.is_none());
    let token: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?")
            .bind(&worker)
            .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
    assert!(token.is_none());
    assert!(requests(&root, "session/prompt").is_empty());
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c4_lost_acp_driver_revokes_and_settles_receipt() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    let worker = stack.runtime(&card).await.id;
    std::fs::write(root.path().join("scenario"), "hold").unwrap();
    let (status, body) = stack.post_input(&card, "lose driver").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait(async || requests(&root, "session/prompt").len() == 1).await;
    let session = session(&stack, &worker).await;
    let owner = &session.host().provider_execution;
    let admitted = claim(&stack, &worker).await.unwrap();
    let id = execution::execution_id(admitted.record()).unwrap();
    session.abort_driver_for_test().await;
    wait(async || !owner.drivers.contains(&id)).await;
    wait(async || {
        owner
            .sweep(
                &stack.repo().sqlite_db().unwrap(),
                session.host().execution.engine(),
            )
            .await
            .unwrap();
        claim(&stack, &worker).await.is_none()
    })
    .await;
    let (token, state, cleanup): (Option<String>, String, Option<String>) =
        sqlx::query_as(concat!(
            "SELECT ws.mcp_token_hash,r.state,r.kept_cleanup_json FROM worker_sessions ws JOIN ",
            "acp_submissions r ON r.worker_session_id=ws.id WHERE ws.id=?",
        ))
        .bind(&worker)
        .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
        .await
        .unwrap();
    assert!(token.is_none());
    assert_eq!(state, "unknown");
    assert!(cleanup.is_none(), "a lost driver is not a normal join");
    assert_eq!(
        executions::load(stack.repo().sqlite_db().unwrap().read_pool(), id.as_str())
            .await
            .unwrap()
            .unwrap()
            .desired,
        "retire"
    );
    stack.wait_phase(&worker, "turn_completed").await;
    let (status, body) = stack.post_input(&card, "must stay held").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let harness = stack.harness(&worker);
    wait(async || harness.refused_issuances_for_test() > 0).await;
    assert!(harness.issuance_block().await.is_some());
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c4_acp_profile_env_public_registration_env_secret() {
    let root = Root::new("unused");
    let overridden = "c43-overridden-operator-value";
    let stack = boot_as_with_creation_mode(
        &root,
        "opencode",
        json!({"OPERATOR_SECRET":overridden}),
        "1.0.0",
        Some("full"),
    )
    .await;
    let (_, card) = create(&stack).await;
    std::fs::write(root.path().join("scenario"), "hold").unwrap();
    let (status, body) = stack.post_input(&card, "overrides").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait(async || requests(&root, "session/prompt").len() == 1).await;
    let json: String = sqlx::query_scalar("SELECT intent_json FROM workspace_executions WHERE json_extract(intent_json,'$.consumer')='acp-planner'")
        .fetch_one(stack.repo().sqlite_db().unwrap().read_pool()).await.unwrap();
    let metadata: workspace_runtime::execution::ExecutionMetadata =
        serde_json::from_str(&json).unwrap();
    let permission = metadata
        .program
        .environment
        .iter()
        .find(|(key, _)| key == "OPENCODE_CONFIG_CONTENT")
        .unwrap()
        .1
        .clone();
    assert_eq!(
        serde_json::from_str::<Value>(&permission).unwrap()["permissions"][0],
        json!({"action":"shell","resource":"*","effect":"allow"})
    );
    let env = std::fs::read_to_string(root.path().join("environment.jsonl")).unwrap();
    let env: Value = env
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|env| env["NEIGE_MCP_TOKEN"] == true)
        .unwrap();
    assert_eq!(env["opencode_config_content"], permission);
    assert!(!json.contains(overridden));
    root.assert_execution_journal_excludes(overridden);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c4_existing_bad_acp_key_refuses_before_mint() {
    for key in ["IFS", "neige_forge_credential", "BAD-KEY"] {
        let root = Root::new("unused");
        let stack = boot_with(&root, json!({key:"must-not-launch"}), "1.0.0").await;
        let (_, card) = create(&stack).await;
        let worker = stack.runtime(&card).await.id;
        let (status, body) = stack.post_input(&card, "invalid key").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let harness = stack.harness(&worker);
        wait(async || harness.refused_issuances_for_test() > 0).await;
        let pool = stack.repo().sqlite_db().unwrap();
        let token: Option<String> =
            sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?")
                .bind(&worker)
                .fetch_one(pool.read_pool())
                .await
                .unwrap();
        assert!(token.is_none());
        assert!(claim(&stack, &worker).await.is_none());
        assert!(requests(&root, "session/prompt").is_empty());
        stack.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c4_acp_release_precedes_first_protocol_byte() {
    use calm_server::test_seams::{PausePoint, install_pause_for_test};
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    let worker = stack.runtime(&card).await.id;
    let before = requests(&root, "initialize").len();
    let pause = PausePoint::default();
    install_pause_for_test("c4-release-authorized", &worker, pause.clone());
    let (status, body) = stack
        .post_input(&card, "release before ACP initialization")
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
        .await
        .unwrap();
    let held = claim(&stack, &worker).await.unwrap();
    let observed = requests(&root, "initialize").len();
    pause.release.notify_one();
    assert!(matches!(
        held.record().release,
        sql::Release::Releasing { .. }
    ));
    assert_eq!(observed, before, "no ACP protocol byte before release");
    assert_eq!(
        stack.wait_outcomes(&card, 1).await[0]["status"],
        "completed"
    );
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c4_every_acp_turn_end_drives_retire() {
    for scenario in ["reply", "lost", "prompt-error", "closed-input"] {
        let root = Root::new("unused");
        let stack = boot(&root).await;
        let (_, card) = create(&stack).await;
        let worker = stack.runtime(&card).await.id;
        std::fs::write(root.path().join("scenario"), scenario).unwrap();
        turn(&stack, &card, scenario, 1).await;
        wait(async || claim(&stack, &worker).await.is_none()).await;
        let pool = stack.repo().sqlite_db().unwrap();
        let (desired, settled): (String, Option<i64>) = sqlx::query_as(
            "SELECT desired,settled_at_ms FROM workspace_executions WHERE json_extract(intent_json,'$.consumer')='acp-planner'"
        ).fetch_one(pool.read_pool()).await.unwrap();
        assert_eq!(desired, "retire", "{scenario}");
        assert!(settled.is_some(), "{scenario}: Retire requires settlement");
        let token: Option<String> =
            sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?")
                .bind(&worker)
                .fetch_one(pool.read_pool())
                .await
                .unwrap();
        assert!(token.is_none(), "{scenario}");
        stack.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c4_acp_lost_completion_retains_claim_until_abort_join_finishes() {
    use calm_server::test_seams::{PausePoint, install_pause_for_test};
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    let worker = stack.runtime(&card).await.id;
    std::fs::write(root.path().join("scenario"), "hold").unwrap();
    let (status, body) = stack.post_input(&card, "abort join").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait(async || requests(&root, "session/prompt").len() == 1).await;
    let session = session(&stack, &worker).await;
    let admitted = claim(&stack, &worker).await.unwrap();
    let id = execution::execution_id(admitted.record()).unwrap();
    let pause = PausePoint::default();
    install_pause_for_test("c4-driver-dropped", id.as_str(), pause.clone());
    session.abort_driver_for_test().await;
    tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
        .await
        .unwrap();
    let pool = stack.repo().sqlite_db().unwrap();
    let engine = session.host().execution.engine().unwrap();
    let owner = &session.host().provider_execution;
    owner
        .stop(
            &pool,
            Some(engine),
            &worker,
            &admitted,
            execution::StopReason::DriverLost,
        )
        .await
        .unwrap();
    execution::wait_settled(engine, &id, Duration::from_secs(12))
        .await
        .unwrap();
    owner.sweep(&pool, Some(engine)).await.unwrap();
    let retained = claim(&stack, &worker).await.is_some();
    pause.release.notify_one();
    assert!(retained, "an unresolved abort join is not owner completion");
    wait(async || {
        owner.sweep(&pool, Some(engine)).await.unwrap();
        claim(&stack, &worker).await.is_none()
    })
    .await;
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c4_acp_boot_never_resends_held_or_releasing_payload() {
    use workspace_runtime::execution::*;
    for releasing in [false, true] {
        let root = Root::new("unused");
        let stack = boot(&root).await;
        let (track, card) = create(&stack).await;
        turn(&stack, &card, "establish ACP ownership", 1).await;
        let worker = stack.runtime(&card).await.id;
        let session = session(&stack, &worker).await;
        let owner = &session.host().provider_execution;
        let engine = session.host().execution.engine().unwrap();
        let pool = stack.repo().sqlite_db().unwrap();
        let mut intent = workspace_runtime::execution::test_support::intent(root.path().to_owned());
        let id = intent.metadata.id.clone();
        intent.metadata.consumer = "acp-planner".into();
        intent.metadata.alias = ProcessAlias(execution::sweep::alias(&worker, id.as_str()));
        intent.metadata.owner = OperationRef(format!("worker-session:{worker}"));
        intent.metadata.track = TrackId(track);
        intent.metadata.program.program = "/bin/sh".into();
        intent.metadata.program.args =
            vec!["-c".into(), "read value && touch forbidden-release".into()];
        let (admitted, guard) = owner
            .admit(
                &pool,
                engine,
                &worker,
                intent,
                execution::Purpose::Turn {
                    turn_id: uuid::Uuid::new_v4().to_string(),
                },
            )
            .await
            .unwrap();
        wait(async || {
            matches!(
                engine.output(&id, None).await,
                Ok(ExecutionReply::Observation {
                    state: ExecutionState::Spawned,
                    ..
                })
            )
        })
        .await;
        if releasing {
            let mut tx = calm_truth::db::sqlite::begin_immediate_tx(&pool)
                .await
                .unwrap();
            assert!(matches!(
                sql::advance_release(&mut tx, &worker, &admitted, 1)
                    .await
                    .unwrap(),
                sql::Cas::Applied(_)
            ));
            tx.commit().await.unwrap();
        }
        drop(guard); // a new kernel has no live protocol driver
        // A new kernel owns a fresh boot proof. Keep the original owner/Engine
        // below only to observe settlement of the exact registered execution.
        let rebooted_host = calm_server::acp_planner::config::AcpPlannerHost::new(
            None,
            &root.data_dir(),
            session.host().mcp_shim.clone(),
            session.host().mcp_socket.clone(),
        )
        .unwrap();
        calm_server::acp_planner::config::boot(stack.repo(), &rebooted_host)
            .await
            .unwrap();
        let after = claim(&stack, &worker).await.unwrap();
        assert!(matches!(
            after.record().decision,
            sql::Decision::Stopping {
                reason: sql::StopReason::KernelRestarted,
                ..
            }
        ));
        wait(async || {
            owner.sweep(&pool, Some(engine)).await.unwrap();
            claim(&stack, &worker).await.is_none()
        })
        .await;
        assert!(!root.path().join("forbidden-release").exists());
        assert_eq!(
            requests(&root, "session/prompt").len(),
            1,
            "boot cannot resend a prompt"
        );
        stack.shutdown().await;
    }
}
