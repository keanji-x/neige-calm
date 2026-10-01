use super::*;

#[tokio::test]
async fn readonly_worker_preparation_declares_native_only() {
    let harness = worker_lease_harness().await;
    let context = json!({"neige_workspace":{"access":"read_only"}});
    let (output, _, _) = prepare_worker_with_context(&harness, "reader", "reader", context).await;
    let card = harness
        .repo
        .card_get(&output.output_string("card_id", "test").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        card.payload["worker_presentation"],
        json!({"kind":"native_only"})
    );
    let mut projected = card;
    crate::session_projection_lookup::project_runtime_into_card_payload(
        harness.repo.as_ref(),
        &mut projected,
    )
    .await
    .unwrap();
    assert_eq!(
        projected.payload["worker_snapshot"],
        json!({
            "task_id":format!("{}:reader",harness.track_id), "goal":"test", "status":"dispatched",
            "report":{"kind":"pending"},
        })
    );
}

#[tokio::test]
async fn writable_worker_preparation_reserves_managed_session() {
    let harness = worker_lease_harness().await;
    let (output, _, _) = prepare_worker_and_op(&harness, "writer", "writer").await;
    let terminal = output.output_string("terminal_id", "test").unwrap();
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workspace_leases WHERE holder_kind='terminal' AND holder_id=?1 AND state='held'"
    ).bind(terminal).fetch_one(harness.repo.pool()).await.unwrap();
    assert_eq!(
        held, 1,
        "interactive native client must retain its own session writer"
    );
}

#[tokio::test]
async fn readonly_native_session_rejects_late_terminal_attachment() {
    let harness = worker_lease_harness().await;
    let op = pending_worker_with_context(
        &harness,
        "reader-attach",
        "reader-attach",
        json!({"neige_workspace":{"access":"read_only"}}),
    )
    .await;
    let operations = SqlxOperationRepo::new(harness.repo.pool().clone());
    let (prepared, _) = operations
        .prepare_tx_and_advance(&op, &harness.adapter)
        .await
        .unwrap()
        .unwrap();
    let terminal = prepared
        .tx_output
        .unwrap()
        .output_string("terminal_id", "test")
        .unwrap();
    let error = crate::operation::terminal_launch::resolve(
        harness.repo.as_ref(),
        &terminal,
        Path::new("/tmp/never-contacted-native-attach.sock"),
        None,
    )
    .await
    .err()
    .expect("read-only worker metadata must never grant an interactive channel");
    assert!(error.to_string().contains("no managed write authority"));
}

#[tokio::test]
async fn writable_native_session_ui_cannot_issue_fresh_process() {
    let harness = worker_lease_harness().await;
    let op =
        pending_worker_with_context(&harness, "writer-attach", "writer-attach", Value::Null).await;
    let operations = SqlxOperationRepo::new(harness.repo.pool().clone());
    let (prepared, _) = operations
        .prepare_tx_and_advance(&op, &harness.adapter)
        .await
        .unwrap()
        .unwrap();
    let terminal = prepared
        .tx_output
        .unwrap()
        .output_string("terminal_id", "test")
        .unwrap();
    let start = crate::operation::terminal_launch::resolve(
        harness.repo.as_ref(),
        &terminal,
        Path::new("/tmp/never-contacted-native-attach.sock"),
        None,
    )
    .await
    .unwrap();
    assert!(matches!(
        start,
        crate::operation::terminal_launch::TerminalStart::AttachOnly(_)
    ));
}

#[tokio::test]
async fn native_session_retains_writer_until_exact_supervisor_stop() {
    use calm_session::control::{ControlMsg, ControlReply};
    use calm_session::{read_frame, write_frame};
    let harness = worker_lease_harness().await;
    let op = pending_worker_with_context(&harness, "stop-proof", "stop-proof", Value::Null).await;
    let operations = SqlxOperationRepo::new(harness.repo.pool().clone());
    let (prepared, _) = operations
        .prepare_tx_and_advance(&op, &harness.adapter)
        .await
        .unwrap()
        .unwrap();
    let terminal = prepared
        .tx_output
        .unwrap()
        .output_string("terminal_id", "test")
        .unwrap();
    let dir = calm_test_sockets::socket_dir("native-session-stop-proof");
    let socket = dir.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let expected = format!("term:{terminal}");
    let endpoint = tokio::spawn(async move {
        for confirmed in [false, true] {
            let (mut stream, _) = listener.accept().await.unwrap();
            match read_frame::<ControlMsg, _>(&mut stream).await.unwrap() {
                ControlMsg::Probe(request) => assert_eq!(request.proc_id, expected),
                message => panic!("unexpected probe: {message:?}"),
            }
            write_frame(
                &mut stream,
                &ControlReply::ProbeOk {
                    supervisor_version: calm_session::SUPERVISOR_CONTROL_VERSION,
                    proc_running: false,
                },
            )
            .await
            .unwrap();
            let (mut stream, _) = listener.accept().await.unwrap();
            match read_frame::<ControlMsg, _>(&mut stream).await.unwrap() {
                ControlMsg::StopAndConfirm { proc_id } => assert_eq!(proc_id, expected),
                message => panic!("unexpected stop: {message:?}"),
            }
            write_frame(
                &mut stream,
                &if confirmed {
                    ControlReply::Stopped
                } else {
                    ControlReply::CleanupOk
                },
            )
            .await
            .unwrap();
        }
    });
    assert!(
        crate::terminal_renderer::stop_and_release_terminal(
            harness.repo.as_ref(),
            &socket,
            &terminal
        )
        .await
        .is_err()
    );
    let held: i64 = sqlx::query_scalar("SELECT count(*) FROM workspace_leases WHERE holder_kind='terminal' AND holder_id=?1 AND state='held'")
        .bind(&terminal).fetch_one(harness.repo.pool()).await.unwrap();
    assert_eq!(
        held, 1,
        "probe idle and cleanup acknowledgements never confirm stop"
    );
    crate::terminal_renderer::stop_and_release_terminal(harness.repo.as_ref(), &socket, &terminal)
        .await
        .unwrap();
    let held: i64 = sqlx::query_scalar("SELECT count(*) FROM workspace_leases WHERE holder_kind='terminal' AND holder_id=?1 AND state='held'")
        .bind(&terminal).fetch_one(harness.repo.pool()).await.unwrap();
    assert_eq!(held, 0);
    let stored = operations
        .get_operation(&prepared.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.tx_output.unwrap().data["terminal_launch"]["state"],
        "stopped"
    );
    crate::terminal_renderer::stop_and_release_terminal(harness.repo.as_ref(), &socket, &terminal)
        .await
        .unwrap();
    endpoint.await.unwrap();
}

#[tokio::test]
async fn native_fence_claimed_session_cannot_release_from_old_not_requested_snapshot() {
    let harness = worker_lease_harness().await;
    let pending =
        pending_worker_with_context(&harness, "claimed-session", "claimed-session", Value::Null)
            .await;
    let operations = std::sync::Arc::new(SqlxOperationRepo::new(harness.repo.pool().clone()));
    let (prepared, _) = operations
        .prepare_tx_and_advance(&pending, &harness.adapter)
        .await
        .unwrap()
        .unwrap();
    let output = prepared.tx_output.clone().unwrap();
    let claimed = operations
        .claim_drive_batch(1)
        .await
        .unwrap()
        .pop()
        .unwrap();
    operations
        .set_phase_and_tx_output(&claimed, Phase::SpawnStarted, &output)
        .await
        .unwrap()
        .unwrap();
    let op = operations
        .claim_drive_batch(1)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let terminal = output.output_string("terminal_id", "fixture").unwrap();
    let execution:String=sqlx::query_scalar("SELECT lease_id FROM workspace_leases WHERE holder_kind='terminal' AND holder_id=?1 AND state='held'")
        .bind(&terminal).fetch_one(harness.repo.pool()).await.unwrap();
    crate::operation::execution_manager::tests::issue_session_fixture(
        harness.repo.pool(),
        &execution,
    )
    .await;
    let ctx = SpawnCtx::new(
        harness.repo.clone(),
        operations,
        std::sync::Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new_with_repo(harness.repo.clone()),
        harness.events.clone(),
        OperationCompletionBus::new(),
    );
    assert!(
        crate::operation::worker_cleanup::require_cleanup_safe(&ctx, &op, &output, false)
            .await
            .is_err(),
        "a capability-bearing managed session cannot use an older NotRequested caller snapshot as stop proof"
    );
    let state: String = sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id=?1")
        .bind(execution)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap();
    assert_eq!(state, "held");
}

#[tokio::test]
async fn native_fence_worker_delete_rechecks_live_native_reference() {
    let harness = worker_lease_harness().await;
    let (output, _, _) = prepare_worker_and_op(&harness, "late-reference", "late-reference").await;
    let card = output.output_string("card_id", "fixture").unwrap();
    let terminal = output.output_string("terminal_id", "fixture").unwrap();
    let shared = SharedCodexAppServer::new_fake_running_with_pending(harness.repo.clone(), None);
    let thread = shared
        .thread_start_mint_for_card(
            &card,
            SharedThreadStartParams {
                cwd: output.output_string("cwd", "fixture").unwrap(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
            },
        )
        .await
        .unwrap();
    shared
        .turn_start(
            &thread,
            vec![crate::codex_appserver::InputItem::text(
                "writer raced cleanup",
            )],
            &TurnModelSelection::inherit(),
            None,
        )
        .await
        .unwrap();
    let result = compensate_worker_rows(
        harness.repo.as_ref(),
        TerminalRendererRegistry::new().as_ref(),
        &harness.adapter.card_role_cache,
        &card,
        &terminal,
    )
    .await;
    assert_eq!(
        result,
        WorkerCleanupOutcome::Preserved,
        "projection deletion must check managed execution references in its final transaction"
    );
    assert!(harness.repo.card_get(&card).await.unwrap().is_some());
}
