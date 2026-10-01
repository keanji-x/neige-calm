use super::*;

async fn managed_turn() -> (
    tempfile::TempDir,
    Arc<SqlxRepo>,
    Arc<SharedCodexAppServer>,
    String,
    String,
    String,
    String,
) {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let card = seed_card(&repo, 112).await;
    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let thread = daemon
        .thread_start_mint_for_card(
            &card,
            SharedThreadStartParams {
                cwd: root.path().to_str().unwrap().into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
            },
        )
        .await
        .unwrap();
    let turn = daemon
        .turn_start(
            &thread,
            vec![InputItem::text("pin generation")],
            &TurnModelSelection::inherit(),
            None,
        )
        .await
        .unwrap();
    let (lease,nonce):(String,String)=sqlx::query_as(
        "SELECT lease_id,native_client_id FROM workspace_leases WHERE holder_kind='native' AND holder_id=?1 AND state='held'"
    ).bind(&thread).fetch_one(repo.pool()).await.unwrap();
    (root, repo, daemon, thread, turn, lease, nonce)
}

#[tokio::test]
async fn native_fence_interrupt_history_losing_target_retains_generation() {
    let (root, repo, daemon, thread, turn, lease, nonce) = managed_turn().await;
    let socket = root.path().join("run/codex-appserver.sock");
    let active = json!({"thread":{"id":thread,"cwd":root.path(),"status":{"type":"active","activeFlags":[]},
        "turns":[{"id":turn,"status":"inProgress","items":[{"type":"userMessage","clientId":nonce}]}]}});
    std::fs::write(
        socket.with_extension("thread-read"),
        serde_json::to_vec(&active).unwrap(),
    )
    .unwrap();
    let disappeared =
        json!({"thread":{"id":thread,"cwd":root.path(),"status":{"type":"idle"},"turns":[]}});
    std::fs::write(
        socket.with_extension("thread-read-after-interrupt"),
        serde_json::to_vec(&disappeared).unwrap(),
    )
    .unwrap();
    assert!(
        !daemon
            .cancel_native_workspace_guard(&lease)
            .await
            .unwrap_or(false),
        "post-interrupt history must still positively identify the exact stopped turn"
    );
    let state: String = sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id=?1")
        .bind(&lease)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(state, "held");
}

#[tokio::test]
async fn native_fence_nonce_cannot_replace_acknowledged_turn() {
    let (root, repo, daemon, thread, turn, lease, nonce) = managed_turn().await;
    let socket = root.path().join("run/codex-appserver.sock");
    let wrong = json!({"thread":{"id":thread,"cwd":root.path(),"status":{"type":"idle"},
        "turns":[{"id":"another-generation","status":"completed","items":[{"type":"userMessage","clientId":nonce}]}]}});
    std::fs::write(
        socket.with_extension("thread-read"),
        serde_json::to_vec(&wrong).unwrap(),
    )
    .unwrap();
    assert!(
        daemon.cancel_native_workspace_guard(&lease).await.is_err(),
        "a provider nonce mapped to another acknowledged generation is contradictory evidence"
    );
    assert!(!socket.with_extension("interrupt-observed").exists());
    let stored: (String, Option<String>) = sqlx::query_as(
        "SELECT state,native_observed_turn_id FROM workspace_leases WHERE lease_id=?1",
    )
    .bind(&lease)
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(stored, ("held".into(), Some(turn)));
}

#[tokio::test]
async fn native_fence_late_sealed_notification_persists_stop_before_interrupt() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("run/codex-appserver.sock");
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    std::fs::write(socket.with_extension("hold-turn-started"), "1").unwrap();
    let repo = repo().await;
    let card = seed_card(&repo, 113).await;
    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let thread = daemon
        .thread_start_mint_for_card(
            &card,
            SharedThreadStartParams {
                cwd: root.path().to_str().unwrap().into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
            },
        )
        .await
        .unwrap();
    let turn = daemon
        .turn_start(
            &thread,
            vec![InputItem::text("late notification")],
            &TurnModelSelection::inherit(),
            None,
        )
        .await
        .unwrap();
    let (lease, nonce): (String, String) = sqlx::query_as(
        "SELECT lease_id,native_client_id FROM workspace_leases WHERE holder_kind='native' AND holder_id=?1 AND state='held'"
    ).bind(&thread).fetch_one(repo.pool()).await.unwrap();
    let active = json!({"thread":{"id":thread,"cwd":root.path(),"status":{"type":"active","activeFlags":[]},
        "turns":[{"id":turn,"status":"inProgress","items":[{"type":"userMessage","clientId":nonce}]}]}});
    std::fs::write(
        socket.with_extension("thread-read"),
        serde_json::to_vec(&active).unwrap(),
    )
    .unwrap();
    daemon.seal_turn_thread_for_deletion(&thread);
    std::fs::remove_file(socket.with_extension("hold-turn-started")).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !socket.with_extension("interrupt-observed").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let stored: (String, String) =
        sqlx::query_as("SELECT state,holder_phase FROM workspace_leases WHERE lease_id=?1")
            .bind(&lease)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(
        stored,
        ("held".into(), "stopping".into()),
        "late notifications must persist stop intent; an interrupt ACK cannot release execution"
    );
    let phase: String = sqlx::query_scalar(
        "SELECT scope_phase FROM workspace_execution_bindings WHERE provider='codex' AND holder_id=?1"
    ).bind(&thread).fetch_one(repo.pool()).await.unwrap();
    assert_eq!(
        phase, "closed",
        "late execution must close future admission durably"
    );
}
