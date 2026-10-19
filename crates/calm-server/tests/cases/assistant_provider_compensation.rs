//! Failed native start returns the same Codex Assistant's authority, never its successor's.
use super::*;
use calm_server::db::sqlite::session_set_handle_state_tx;
use calm_server::harness::QueueEntry;
use calm_server::mcp_server::{McpServer, build_default_registry};
use calm_server::operation::planner_harness_start_adapter::PlannerHarnessStartAdapter;
use calm_server::operation::{
    CompensationStateVersioned, OperationCompletionBus, OperationRepo, ProviderAdapter, SpawnCtx,
    SqlxOperationRepo,
};

#[tokio::test]
async fn assistant_provider_failed_native_start_restores_only_predecessor_credential() {
    let _guard = ENV_LOCK.lock().await;
    let tmp = TempDir::new().unwrap();
    let capture_file = tmp.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture_file);
    }
    let _env = EnvGuard("FAKE_CODEX_CAPTURE_REQUESTS");
    let failure_marker = tmp.path().join("fail-next-start");
    unsafe {
        std::env::set_var("FAKE_CODEX_FAIL_THREAD_START_MARKER", &failure_marker);
    }
    let _failure_env = EnvGuard("FAKE_CODEX_FAIL_THREAD_START_MARKER");
    let (state, repo, role_cache) = state_with_live_daemon(&tmp).await;
    let track = seed_track(&repo).await;
    state
        .track_area_cache
        .insert(track.id.clone(), track.area_id.clone());
    let card_id = new_id();
    seed_assistant_card(&repo, &role_cache, &track, &card_id).await;
    let mut tx = repo.pool().begin().await.unwrap();
    card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "track-report".into(),
            sort: None,
            payload: serde_json::to_value(calm_server::track_report::TrackReportPayload::initial())
                .unwrap(),
        },
        CardRole::ReportCard,
        false,
        &role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    // The new wire carries required identity; no fixture token or historical executor row.
    let mut payload = serde_json::to_value(terminal_approval::start_payload(
        &track,
        &card_id,
        HarnessProfile::Assistant,
        true,
        None,
    ))
    .unwrap();
    payload["version"] = json!("assistant-v1");
    payload["provider"] = json!("codex");
    let op = state
        .operation_runtime
        .submit("planner-harness-start", key(), payload.clone())
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &op).await,
        OperationOutcome::Succeeded { .. }
    ));
    let old = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.kind, WorkerSessionKind::ManagedAssistant);
    let rows = wait_for_requests(&capture_file, 2).await;
    let native_start = rows.iter().find(|r| r["method"] == "thread/start").unwrap();
    let token = thread_start_token(native_start).to_string();
    let hash = auth::hash_token(&token);
    assert_eq!(
        assert_card_session_mcp_hash_parity(&repo, &card_id, &old.id).await,
        hash
    );
    let socket_dir = calm_test_sockets::socket_dir("assistant-rollback");
    let socket = calm_test_sockets::socket_path(socket_dir.path(), "kernel.sock");
    let server = McpServer::spawn(
        repo.clone(),
        state.events.clone(),
        state.write().clone(),
        socket.clone(),
        PathBuf::from("/nonexistent-shim"),
        build_default_registry(),
        None,
        Arc::new(tokio::sync::OnceCell::new()),
        Arc::new(tokio::sync::OnceCell::new()),
        tmp.path().join("gates"),
    )
    .await
    .unwrap();
    let before = mcp::call_tool_card_bound(&socket, &token, "neige_report_read", json!({})).await;
    assert!(before.get("error").is_none(), "{before}");
    // A dormant queue is moved by prepare and must be returned by compensation, with its ID.
    state.harness.shutdown_and_remove(&old.id).await.unwrap();
    let mut snapshot = HarnessSnapshot::from_value_strict(old.handle_state_json.clone().unwrap());
    snapshot.set_pending_entries(vec![QueueEntry::user_message(
        "owed message".into(),
        None,
        vec![],
    )]);
    let mut tx = repo.pool().begin().await.unwrap();
    session_set_handle_state_tx(
        &mut tx,
        &old.id,
        Some(serde_json::to_value(&snapshot).unwrap()),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    std::fs::write(&failure_marker, "fail").unwrap();
    let op = state
        .operation_runtime
        .submit("planner-harness-start", key(), payload.clone())
        .await
        .unwrap();
    match wait_op(&state, &op).await {
        OperationOutcome::Failed {
            from_phase,
            last_error,
            ..
        } => {
            assert_eq!(from_phase, PhaseTag::AppServerInteract);
            assert!(
                last_error.contains("forced thread/start failure"),
                "{last_error}"
            );
        }
        other => panic!("{other:?}"),
    }
    let restored = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored.id, old.id);
    assert_eq!(restored.thread_id, old.thread_id);
    assert_eq!(restored.status, old.status);
    assert_eq!(
        card_session_id(&repo, &card_id).await.as_deref(),
        Some(old.id.as_str())
    );
    let mut restored_snapshot = restored.handle_state_json.unwrap();
    let mut expected_snapshot = serde_json::to_value(&snapshot).unwrap();
    // Existing harvest reconstructs queued_at_ms; durable message identity/content must survive.
    for value in [&mut restored_snapshot, &mut expected_snapshot] {
        for meta in value["pending_entry_meta"].as_array_mut().unwrap() {
            meta.as_object_mut().unwrap().remove("queued_at_ms");
        }
    }
    for key in [
        "pending_queue",
        "pending_envelope_ids",
        "pending_entry_meta",
        "pending_message_ids",
    ] {
        assert_eq!(restored_snapshot[key], expected_snapshot[key], "{key}");
    }
    assert!(
        track_root_session_id(&repo, track.id.as_str())
            .await
            .is_none()
    );
    let successor_hashes: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT mcp_token_hash FROM worker_sessions WHERE card_id=?1 AND id<>?2",
    )
    .bind(&card_id)
    .bind(&old.id)
    .fetch_all(repo.pool())
    .await
    .unwrap();
    assert!(!successor_hashes.is_empty());
    assert!(successor_hashes.iter().all(Option::is_none));
    // A fresh connection uses the token already installed in the loaded native thread.
    let read = mcp::call_tool_card_bound(&socket, &token, "neige_report_read", json!({})).await;
    assert!(read.get("error").is_none(), "{read}");
    let prose = mcp::call_tool_card_bound(&socket, &token, "neige_report_commit", json!({
        "message":"restored assistant", "ops":[{"op":"upsert","kind":"prose","markdown":"restored native credential"}]
    })).await;
    assert!(prose.get("error").is_none(), "{prose}");
    let rows = wait_for_requests(&capture_file, 3).await;
    assert_eq!(
        rows.iter()
            .filter(|row| row["method"] == "initialize")
            .count(),
        1,
        "restoration did not cold-respawn the daemon or replace the native thread config"
    );
    // Replay the real kernel journal as if the restore committed before its completion marker.
    let operations = Arc::new(SqlxOperationRepo::new(repo.db().clone()));
    let failed_op = operations.get_operation(&op).await.unwrap().unwrap();
    let journal: CompensationStateVersioned =
        serde_json::from_value(failed_op.compensation_state.clone().unwrap()).unwrap();
    let mut restore = journal
        .steps
        .into_iter()
        .find(|step| step.op == "restore_old_runtime")
        .unwrap();
    restore.completed = false;
    let wiring = state.planner_wiring();
    let adapter = PlannerHarnessStartAdapter::new(
        repo.clone(),
        state.shared_codex_appserver.clone(),
        state.thread_seals().clone(),
        state.harness.clone(),
        state.plugin.clone(),
        state.card_role_cache.clone(),
        state.track_area_cache.clone(),
        None,
        wiring.claude,
        wiring.acp,
    );
    let ctx = SpawnCtx::new(
        repo.clone(),
        operations,
        state.daemon.clone(),
        state.terminal_renderer.clone(),
        state.events.clone(),
        OperationCompletionBus::new(),
    );
    adapter
        .compensate_step(
            &restore,
            failed_op.tx_output.as_ref().unwrap(),
            &failed_op,
            &ctx,
        )
        .await
        .unwrap();
    // Revocation after undo is not another permission to resurrect the journalled token.
    let mut tx = repo.pool().begin().await.unwrap();
    sqlx::query("UPDATE worker_sessions SET mcp_token_hash=NULL WHERE id=?1")
        .bind(&old.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        adapter
            .compensate_step(
                &restore,
                failed_op.tx_output.as_ref().unwrap(),
                &failed_op,
                &ctx
            )
            .await
            .is_err()
    );
    assert!(
        calm_server::mcp_server::handshake::handle_initialize(
            repo.as_ref(),
            None,
            &json!({"_meta":{"dev.neige/auth":{"token":token}}}),
            "2024-11-05"
        )
        .await
        .is_err()
    );
    // A successful replacement must still revoke the predecessor; the old token never transfers.
    let op = state
        .operation_runtime
        .submit("planner-harness-start", key(), payload)
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &op).await,
        OperationOutcome::Succeeded { .. }
    ));
    assert!(
        calm_server::mcp_server::handshake::handle_initialize(
            repo.as_ref(),
            None,
            &json!({"_meta":{"dev.neige/auth":{"token":token}}}),
            "2024-11-05"
        )
        .await
        .is_err()
    );
    let active = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(active.id, old.id);
    assert!(
        adapter
            .compensate_step(
                &restore,
                failed_op.tx_output.as_ref().unwrap(),
                &failed_op,
                &ctx
            )
            .await
            .is_err()
    );
    assert_eq!(
        card_session_id(&repo, &card_id).await.as_deref(),
        Some(active.id.as_str()),
        "a stale undo must not displace the successor"
    );
    let active_hash: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
            .bind(&active.id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_ne!(active_hash.as_deref(), Some(hash.as_str()));
    server.stop_listener_for_test().await;
    state
        .harness
        .shutdown_track(
            &track.id,
            state.shared_codex_appserver.thread_seals().clone(),
        )
        .await
        .unwrap();
}
