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
                payload: json!({"codex_source":"shared"}),
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
