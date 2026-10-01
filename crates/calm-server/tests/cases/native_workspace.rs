//! Real protocol responses and production native workspace entry points.
use super::*;

#[tokio::test]
async fn native_workspace_definite_rejection_releases_unissued_guard() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let card = seed_card(&repo, 100).await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
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
    daemon.reject_turn_start_for_test();
    assert!(
        daemon
            .turn_start(
                &thread,
                vec![InputItem::text("rejected")],
                &TurnModelSelection {
                    model: None,
                    effort: None
                },
                Some("rejected-input")
            )
            .await
            .is_err()
    );
    let held:i64=sqlx::query_scalar("SELECT count(*) FROM workspace_leases WHERE holder_kind='native' AND state IN ('held','releasing')").fetch_one(repo.pool()).await.unwrap();
    assert_eq!(
        held, 0,
        "an explicit provider rejection is positive unissued proof"
    );
}

#[tokio::test]
async fn native_workspace_legacy_cold_resume_binds_provider_cwd() {
    let root = tempfile::tempdir().unwrap();
    let actual = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let card = seed_card(&repo, 101).await;
    seed_runtime_thread(&repo, &card, "legacy-thread").await;
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    std::fs::write(sock.with_extension("thread-resume"),serde_json::to_vec(&json!({"thread":{"id":"legacy-thread","cwd":actual.path(),"status":{"type":"idle"},"turns":[]},"model":"fake-model"})).unwrap()).unwrap();
    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let cwd:Option<String>=sqlx::query_scalar("SELECT cwd FROM workspace_execution_bindings WHERE provider='codex' AND holder_id='legacy-thread' AND card_id=?1").bind(&card).fetch_optional(repo.pool()).await.unwrap();
    let declared = tempfile::tempdir().unwrap();
    let track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
        .bind(&card)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET workspace_path=?2 WHERE id=?1")
        .bind(&track)
        .bind(declared.path().to_str().unwrap())
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO workspace_leases(lease_id,card_id,track_id,path,state,lease_owner, \
        created_at_ms,updated_at_ms,access_mode) VALUES('track-reader',?1,?2,?3,'held','read',0,0,'read_only')")
        .bind(new_id()).bind(&track).bind(declared.path().to_str().unwrap()).execute(repo.pool()).await.unwrap();
    let started = daemon
        .turn_start(
            "legacy-thread",
            vec![InputItem::text("actual provider workspace")],
            &TurnModelSelection {
                model: None,
                effort: None,
            },
            None,
        )
        .await;
    assert!(
        started.is_ok(),
        "a reader of Track's unrelated declared path must not block the provider's actual cwd"
    );
    drop(daemon);
    assert_eq!(
        cwd.as_deref(),
        actual.path().to_str(),
        "legacy native workspace must come from the provider response, not Track cwd"
    );
}

#[tokio::test]
async fn native_workspace_unknown_response_recovers_exact_nonce_and_retains_unknown_facts() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let card = seed_card(&repo, 102).await;
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
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::write(sock.with_extension("turn-start-no-id"), "1").unwrap();
    assert!(
        daemon
            .turn_start(
                &thread,
                vec![InputItem::text("accepted without reply identity")],
                &TurnModelSelection {
                    model: None,
                    effort: None
                },
                None
            )
            .await
            .is_err()
    );
    let (lease,phase,nonce):(String,String,String)=sqlx::query_as("SELECT lease_id,holder_phase,native_client_id FROM workspace_leases WHERE holder_kind='native' AND state='held'").fetch_one(repo.pool()).await.unwrap();
    assert_eq!(phase, "issuing");
    assert!(!nonce.is_empty());
    let path = sock.with_extension("thread-read");
    let original = std::fs::read(&path).unwrap();
    std::fs::write(
        &path,
        serde_json::to_vec(
            &json!({"thread":{"id":thread,"cwd":root.path(),"status":{"type":"idle"},"turns":[]}}),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(
        daemon.cancel_native_workspace_guard(&lease).await.is_err(),
        "an empty local cache or absent nonce is not never-issued proof"
    );
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workspace_leases WHERE lease_id=?1 AND state='held'",
    )
    .bind(&lease)
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(held, 1);
    assert!(
        daemon.turn_thread_is_sealed_for_test(&thread),
        "explicit unknown issuance cancellation seals new turns"
    );
    let mut wrong: Value = serde_json::from_slice(&original).unwrap();
    wrong["thread"]["turns"][0]["items"][0]["clientId"] = json!("another-request-identity");
    std::fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
    assert!(
        daemon
            .reconcile_native_workspace_guard(&lease)
            .await
            .is_err(),
        "terminal history for another request cannot recover this unknown issuance"
    );
    let mut active: Value = serde_json::from_slice(&original).unwrap();
    active["thread"]["status"] = json!({"type":"active","activeFlags":[]});
    active["thread"]["turns"][0]["status"] = json!("inProgress");
    std::fs::write(&path, serde_json::to_vec(&active).unwrap()).unwrap();
    assert!(
        !daemon
            .reconcile_native_workspace_guard(&lease)
            .await
            .unwrap(),
        "interrupt/clean ACK cannot release while provider still reports an active turn"
    );
    std::fs::write(&path, original).unwrap();
    assert!(
        daemon
            .reconcile_native_workspace_guard(&lease)
            .await
            .unwrap(),
        "the exact nonce's terminal turn and empty background roster allow recovery"
    );
    let stopped: String =
        sqlx::query_scalar("SELECT holder_phase FROM workspace_leases WHERE lease_id=?1")
            .bind(&lease)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(stopped, "stopped");
}

#[tokio::test]
async fn native_workspace_resume_missing_provider_cwd_cannot_invent_a_binding() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let card = seed_card(&repo, 103).await;
    seed_runtime_thread(&repo, &card, "missing-cwd-thread").await;
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    std::fs::write(
        sock.with_extension("thread-resume"),
        serde_json::to_vec(&json!({"thread":{"id":"missing-cwd-thread"},"model":"fake-model"}))
            .unwrap(),
    )
    .unwrap();
    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workspace_execution_bindings WHERE holder_id='missing-cwd-thread'",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(
        count, 0,
        "a missing required upstream cwd must never fall back to current Track"
    );
    assert!(
        daemon
            .turn_start(
                "missing-cwd-thread",
                vec![InputItem::text("must wait for actual cwd")],
                &TurnModelSelection {
                    model: None,
                    effort: None
                },
                None
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn native_workspace_readonly_lost_response_retains_independent_reader_until_exact_stop() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let card = seed_card(&repo, 104).await;
    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let thread = daemon
        .thread_start_mint_for_card(
            &card,
            SharedThreadStartParams {
                cwd: root.path().to_str().unwrap().into(),
                approval_policy: "never".into(),
                sandbox_mode: "read-only".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
            },
        )
        .await
        .unwrap();
    let track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
        .bind(&card)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    use calm_server::operation::OperationRepo;
    let task = new_id();
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id, \
        declared_by,created_at_ms,updated_at_ms) VALUES(?1,?2,?1,'codex','read',?3,'running',?4,'user',0,0)")
        .bind(&task).bind(&track).bind(json!({"neige_workspace":{"access":"read_only"}}).to_string()).bind(&card).execute(repo.pool()).await.unwrap();
    let operations = calm_server::operation::SqlxOperationRepo::new(repo.pool().clone());
    let owner = operations
        .insert_operation(
            "codex-worker",
            calm_server::operation::OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(task),
                payload_hash: "read-scope".into(),
            },
            json!({}),
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO workspace_leases(lease_id,card_id,track_id,path,state,lease_owner, \
        created_at_ms,updated_at_ms,access_mode) VALUES('task-read',?1,?2,?3,'held',?4,0,0,'read_only')")
        .bind(&card).bind(&track).bind(root.path().to_str().unwrap()).bind(owner).execute(repo.pool()).await.unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::write(sock.with_extension("turn-start-no-id"), "1").unwrap();
    assert!(
        daemon
            .turn_start(
                &thread,
                vec![InputItem::text("read under pinned snapshot")],
                &TurnModelSelection {
                    model: None,
                    effort: None
                },
                None
            )
            .await
            .is_err()
    );
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT lease_id,native_client_id FROM workspace_leases \
        WHERE holder_kind='native' AND access_mode='read_only' AND state='held'",
    )
    .fetch_optional(repo.pool())
    .await
    .unwrap();
    // The Task lease's old lightweight stop proof may arrive before this request is accepted.
    sqlx::query("UPDATE workspace_leases SET state='released' WHERE lease_id='task-read'")
        .execute(repo.pool())
        .await
        .unwrap();
    let (lease, nonce) =
        row.expect("a read-only native request needs its own durable issuance reference");
    let read_path = sock.with_extension("thread-read");
    let original = std::fs::read(&read_path).unwrap();
    std::fs::write(&read_path,serde_json::to_vec(&json!({"thread":{"id":thread,"cwd":root.path(),"status":{"type":"idle"},"turns":[{"id":"old-completed","status":"completed","items":[]}]}})).unwrap()).unwrap();
    assert!(
        daemon
            .reconcile_native_workspace_guard(&lease)
            .await
            .is_err(),
        "old completed turns cannot settle the delayed reader nonce"
    );
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workspace_leases WHERE access_mode='read_only' AND state='held'",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(held, 1);
    assert!(!nonce.is_empty());
    std::fs::write(&read_path, original).unwrap();
    assert!(
        daemon
            .reconcile_native_workspace_guard(&lease)
            .await
            .unwrap()
    );
}
