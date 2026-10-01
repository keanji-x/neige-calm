//! The shared daemon must reconstruct policy from current persisted card roles.
use super::*;

#[tokio::test]
async fn cold_resume_selects_terminal_policy_from_current_card_role() {
    let _guard = ENV_LOCK.lock().await;
    let root = tempfile::tempdir().unwrap();
    let capture = root.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture);
    }
    let _env = EnvGuard("FAKE_CODEX_CAPTURE_REQUESTS");
    let repo = repo().await;
    let mut targets = Vec::new();
    for (i, role) in [CardRole::Planner, CardRole::Assistant, CardRole::Worker]
        .into_iter()
        .enumerate()
    {
        let existing = seed_card(&repo, i).await;
        let track = repo.card_get(&existing).await.unwrap().unwrap().track_id;
        let card = new_id();
        let mut tx = repo.pool().begin().await.unwrap();
        calm_server::db::sqlite::card_create_with_id_tx(
            &mut tx,
            card.clone(),
            NewCard {
                track_id: track,
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: if role == CardRole::Planner {
                    json!({"codex_source":"shared","planner_provider":"codex"})
                } else {
                    json!({"codex_source":"shared"})
                },
            },
            role,
            true,
            &calm_server::card_role_cache::CardRoleCache::new(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let thread = format!("approval-thread-{i}");
        let kind = if role == CardRole::Worker {
            WorkerSessionKind::CodexCard
        } else {
            WorkerSessionKind::SharedPlanner
        };
        seed_runtime_thread_with_kind(&repo, &card, &thread, kind).await;
        targets.push((card, thread, role));
    }
    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let rows = wait_for_requests(&capture, 4).await;
    for (_, thread, role) in &targets {
        let request = rows
            .iter()
            .find(|r| r["method"] == "thread/resume" && r["params"]["threadId"] == *thread)
            .unwrap();
        let tools = request.pointer("/params/config/mcp_servers/calm/tools");
        if *role == CardRole::Planner {
            assert_eq!(
                tools,
                Some(&json!({
                    "calm.terminal.open":{"approval_mode":"approve"},
                    "calm.terminal.control":{"approval_mode":"approve"},
                    "calm.terminal.input":{"approval_mode":"approve"}
                }))
            );
        } else {
            assert!(request.pointer("/params/config/mcp_servers").is_none());
        }
    }
    // A stale SharedPlanner session kind cannot retain delegation after demotion.
    sqlx::query("UPDATE cards SET role='assistant' WHERE id=?1")
        .bind(&targets[0].0)
        .execute(repo.pool())
        .await
        .unwrap();
    daemon.mark_needs_respawn();
    daemon.ensure_respawn_for_current_settings().await.unwrap();
    let after = wait_for_requests(&capture, rows.len() + 4).await;
    let resumed = after
        .iter()
        .rev()
        .find(|r| r["method"] == "thread/resume" && r["params"]["threadId"] == targets[0].1)
        .unwrap();
    assert!(resumed.pointer("/params/config/mcp_servers").is_none());
}

#[tokio::test]
async fn worker_mint_does_not_receive_planner_terminal_policy() {
    let _guard = ENV_LOCK.lock().await;
    let root = tempfile::tempdir().unwrap();
    let capture = root.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture);
    }
    let _env = EnvGuard("FAKE_CODEX_CAPTURE_REQUESTS");
    let repo = repo().await;
    let card = seed_card(&repo, 0).await;
    let daemon = server(&root, repo).await;
    daemon.start_or_takeover().await.unwrap();
    daemon
        .thread_start_mint_mcp_shell(
            &card,
            "/tmp".into(),
            None,
            root.path().join("mcp.sock"),
            "fixture-token".into(),
            "workspace-write",
        )
        .await
        .unwrap();
    let rows = wait_for_requests(&capture, 2).await;
    let request = rows.iter().find(|r| r["method"] == "thread/start").unwrap();
    assert_eq!(request["params"]["approvalPolicy"], "never");
    assert!(request.pointer("/params/config/mcp_servers").is_none());
    assert!(
        request
            .pointer("/params/config/shell_environment_policy/set/NEIGE_MCP_TOKEN")
            .is_some()
    );
}

#[tokio::test]
async fn read_task_initial_turn_uses_prepared_lease_before_worker_card_stamp() {
    let _guard = ENV_LOCK.lock().await;
    let root = tempfile::tempdir().unwrap();
    let (workspace, base_sha, common_dir) = pinned_read_workspace();
    let repo = repo().await;
    let card = seed_card(&repo, 0).await;
    let track = repo.card_get(&card).await.unwrap().unwrap().track_id;
    let task = new_id();
    let lease = new_id();
    sqlx::query(r#"
        INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,declared_by,created_at_ms,updated_at_ms)
        VALUES(?1,?2,?1,'codex','Review only',?3,'dispatched','user',1,1)
    "#).bind(&task).bind(track.as_str())
        .bind(json!({"neige_workspace":{"access":"read_only"}}).to_string()).execute(repo.pool()).await.unwrap();
    use calm_server::operation::OperationRepo;
    let operations = calm_server::operation::SqlxOperationRepo::new(repo.pool().clone());
    let owner = operations
        .insert_operation(
            "codex-worker",
            calm_server::operation::OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(task),
                payload_hash: "prepared-read".into(),
            },
            json!({}),
        )
        .await
        .unwrap();
    sqlx::query(r#"
        INSERT INTO workspace_leases(lease_id,card_id,track_id,path,state,lease_owner,
            created_at_ms,updated_at_ms,access_mode,base_sha,base_source,canonical_path,git_common_dir)
        VALUES(?1,?2,?3,?4,'held',?5,1,1,'read_only',?6,'commit',?4,?7)
    "#).bind(&lease).bind(&card).bind(track.as_str()).bind(workspace.path().to_str().unwrap())
        .bind(owner).bind(base_sha).bind(common_dir.to_str().unwrap()).execute(repo.pool()).await.unwrap();
    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let thread = daemon
        .thread_start_mint_mcp_shell(
            &card,
            workspace.path().to_str().unwrap().into(),
            None,
            root.path().join("mcp.sock"),
            "fixture-token".into(),
            "read-only",
        )
        .await
        .unwrap();
    // The real prepare intent exists, but the scheduler has not stamped worker_card_id.
    assert!(repo.task_for_worker_card(&card).await.unwrap().is_none());
    daemon
        .turn_start(
            &thread,
            vec![InputItem::text("Review only")],
            &TurnModelSelection::inherit(),
            None,
        )
        .await
        .unwrap();
    let writers: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workspace_leases WHERE card_id=?1 AND access_mode='read_write' AND state='held'"
    ).bind(&card).fetch_one(repo.pool()).await.unwrap();
    assert_eq!(
        writers, 0,
        "a prepared reader cannot acquire a native writer before the running stamp"
    );
}

#[tokio::test]
async fn native_turn_refusal_releases_writer_that_never_started() {
    let repo = repo().await;
    let card = seed_card(&repo, 0).await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let thread = daemon
        .thread_start_mint_mcp_shell(
            &card,
            "/tmp".into(),
            None,
            std::path::PathBuf::from("/tmp/mcp.sock"),
            "fixture-token".into(),
            "workspace-write",
        )
        .await
        .unwrap();
    daemon.reject_turn_start_for_test();
    assert!(
        daemon
            .turn_start(
                &thread,
                vec![InputItem::text("work")],
                &TurnModelSelection::inherit(),
                None
            )
            .await
            .is_err()
    );
    let held:i64=sqlx::query_scalar("SELECT count(*) FROM workspace_leases WHERE holder_kind='native' AND card_id=?1 AND state='held'")
        .bind(&card).fetch_one(repo.pool()).await.unwrap();
    assert_eq!(
        held, 0,
        "a provider refusal before issuing cannot retain a writer"
    );
}
