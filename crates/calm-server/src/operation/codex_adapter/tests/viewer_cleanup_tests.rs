use super::*;
use crate::decision_sink::{CardDecisionSink, DeliveryMessage, WorkerTaskReport};
use crate::mcp_server::{AppContext, ToolCallIdentity};
use crate::operation::{OperationCompletionBus, OperationRuntime, Phase};
use crate::state::{DaemonClient, WriteContext};
use crate::terminal_renderer::TerminalRendererRegistry;
use calm_session::control::{ControlMsg, ControlReply};
use calm_session::{read_frame, write_frame};
use calm_truth::db::RepoRead;
use calm_truth::session_projection_repo::WorkerSessionProjectionRepo;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[tokio::test]
async fn codex_viewer_completion_preserves_business() {
    exercise_viewer_report(crate::model::TaskStatus::Done, false).await;
}
#[tokio::test]
async fn codex_viewer_failure_preserves_business() {
    exercise_viewer_report(crate::model::TaskStatus::Failed, false).await;
}
#[tokio::test]
async fn codex_viewer_verifying_preserves_business() {
    exercise_viewer_report(crate::model::TaskStatus::Verifying, false).await;
}

#[tokio::test]
async fn codex_rejected_viewer_restart_preserves_started_business() {
    exercise_viewer_report(crate::model::TaskStatus::Verifying, true).await;
}

async fn exercise_viewer_report(expected: crate::model::TaskStatus, restart: bool) {
    let harness = worker_lease_harness_with_disk(restart).await;
    harness
        .repo
        .seed_track_area_cache(&harness.adapter.track_area_cache)
        .await
        .unwrap();
    let write = WriteContext::new(
        harness.adapter.card_role_cache.clone(),
        harness.adapter.track_area_cache.clone(),
    );
    let mut declaration = json!({"key":"launch", "kind":"codex", "goal":"preserve completed notes", "ready":true, "declared_by":"user"});
    if expected == crate::model::TaskStatus::Verifying {
        declaration["gate"] = json!({"steps":[{"name":"check","cmd":"true"}]});
    } else {
        declaration["no_gate_reason"] = json!("viewer race fixture");
    }
    let task = crate::operation::launch_cleanup_test_support::claimed_task(
        harness.repo.clone(),
        harness.events.clone(),
        write.clone(),
        &harness.track_id,
        declaration,
    )
    .await;
    let shared = SharedCodexAppServer::new_fake_running_with_pending(harness.repo.clone(), None);
    let dir = calm_test_sockets::socket_dir("viewer");
    let sock = dir.path().join("viewer.sock");
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    let starts = Arc::new(AtomicUsize::new(0));
    let starts_for_endpoint = starts.clone();
    // Controlled endpoint NEVER executes the requested Codex viewer command.
    let endpoint = tokio::spawn(async move {
        let mut clients = tokio::task::JoinSet::new();
        loop {
            let (mut connection, _) = listener.accept().await.unwrap();
            let starts = starts_for_endpoint.clone();
            clients.spawn(async move {
                if matches!(
                    read_frame::<ControlMsg, _>(&mut connection).await,
                    Ok(ControlMsg::EnsureProc(_))
                ) {
                    starts.fetch_add(1, Ordering::SeqCst);
                    let _ = write_frame(
                        &mut connection,
                        &ControlReply::SpawnFailed {
                            disposition:
                                calm_session::control::SpawnFailedDisposition::NoChildCreated,
                            error: "fixture optional viewer unavailable".into(),
                            child_already_reaped: true,
                        },
                    )
                    .await;
                }
            });
        }
    });
    struct Abort(tokio::task::AbortHandle);
    impl Drop for Abort {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _endpoint_abort = Abort(endpoint.abort_handle());
    let server = McpServer::new_for_test(crate::mcp_server::McpShimConfig {
        shim_bin: dir.path().join("shim"),
        socket_path: dir.path().join("mcp.sock"),
    });
    let mut adapter = CodexWorkerAdapter::new(
        harness.repo.clone(),
        Arc::new(CodexClient::new_stub()),
        shared.clone(),
        Some(server),
        harness.adapter.card_role_cache.clone(),
        harness.adapter.track_area_cache.clone(),
        harness.repo_root.path().into(),
    );
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let entered_hook = entered.clone();
    let resume_hook = resume.clone();
    adapter.viewer_preparation_hook = Some(Arc::new(move || {
        let entered = entered_hook.clone();
        let resume = resume_hook.clone();
        Box::pin(async move {
            entered.notify_one();
            resume.notified().await;
        })
    }));
    let op_repo = Arc::new(SqlxOperationRepo::new(harness.repo.pool().clone()));
    let mut daemon = DaemonClient::new_stub();
    daemon.proc_supervisor_sock = Some(sock);
    let daemon = Arc::new(daemon);
    let completion = OperationCompletionBus::new();
    let runtime = Arc::new(
        OperationRuntime::new(
            op_repo.clone(),
            vec![Arc::new(adapter)],
            harness.events.clone(),
            completion.clone(),
            SpawnCtx::new(
                harness.repo.clone(),
                op_repo.clone(),
                daemon.clone(),
                TerminalRendererRegistry::new_with_repo(harness.repo.clone()),
                harness.events.clone(),
                completion,
            ),
        )
        .await
        .unwrap(),
    );
    let (kind, payload) = crate::scheduler::build_worker_payload(&task).unwrap();
    let key = OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(task.id.clone()),
        payload_hash: crate::routes::idempotency_key::stable_payload_hash(&payload).unwrap(),
    };
    if restart {
        sqlx::query("CREATE TRIGGER interrupt_launch BEFORE UPDATE OF phase ON operations WHEN NEW.phase IN \
            ('spawn_succeeded','compensating','stuck') BEGIN SELECT RAISE(ABORT,'viewer ack interruption'); END")
            .execute(harness.repo.pool()).await.unwrap();
    }
    let submitted_key = key.clone();
    let run = tokio::spawn(async move { runtime.submit(kind, submitted_key, payload).await });
    let _run_abort = Abort(run.abort_handle());
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    assert_eq!(shared.started_turns_for_test().len(), 1);
    let op = op_repo
        .find_by_idempotency_key(kind, &key)
        .await
        .unwrap()
        .unwrap();
    let output = op.tx_output.as_ref().unwrap();
    let card_id = output.output_string("card_id", "test").unwrap();
    let session_id = output.output_string("runtime_id", "test").unwrap();
    let cwd = std::path::PathBuf::from(output.output_string("cwd", "test").unwrap());
    assert!(
        cwd.is_dir(),
        "production provisioning must finish before viewer preparation"
    );
    std::fs::write(
        cwd.join("completed-notes.txt"),
        b"preserve this completed work\n",
    )
    .unwrap();
    if restart {
        resume.notify_one();
        let _ = tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .unwrap()
            .unwrap();
        let op = op_repo.get_operation(&op.id).await.unwrap().unwrap();
        assert_eq!(op.phase, Phase::SpawnStarted);
        assert_eq!(
            op.tx_output.as_ref().unwrap().data["terminal_launch"]["state"],
            "rejected"
        );
        let session = harness
            .repo
            .session_projection_by_id(&session_id)
            .await
            .unwrap()
            .unwrap();
        assert!(session.thread_id.is_some());
        assert!(session.active_turn_id.is_some());
        sqlx::query("DROP TRIGGER interrupt_launch")
            .execute(harness.repo.pool())
            .await
            .unwrap();
        let reboot_repo = Arc::new(
            crate::db::sqlite::SqlxRepo::open(&format!(
                "sqlite:{}",
                harness.repo_root.path().join(".git/test.sqlite").display()
            ))
            .await
            .unwrap(),
        );
        let reboot_ops = Arc::new(SqlxOperationRepo::new(reboot_repo.pool().clone()));
        let events = crate::event::EventBus::new();
        let state = crate::state::AppState::from_parts(
            reboot_repo.clone(),
            events.clone(),
            daemon.clone(),
            Arc::new(crate::plugin_host::PluginHost::new_full(
                Arc::new(crate::plugin_host::PluginRegistry::empty()),
                reboot_repo.clone(),
                std::path::PathBuf::new(),
                dir.path().join("plugins"),
                vec![],
                events.clone(),
                write,
            )),
            Arc::new(CodexClient::new_stub()),
            None,
            None,
        );
        crate::reconcile_supervisor_on_boot(&state).await;
        let reboot_adapter = CodexWorkerAdapter::new(
            reboot_repo.clone(),
            Arc::new(CodexClient::new_stub()),
            shared.clone(),
            None,
            harness.adapter.card_role_cache.clone(),
            harness.adapter.track_area_cache.clone(),
            harness.repo_root.path().into(),
        );
        let bus = OperationCompletionBus::new();
        let reboot = OperationRuntime::new(
            reboot_ops.clone(),
            vec![Arc::new(reboot_adapter)],
            events.clone(),
            bus.clone(),
            SpawnCtx::new(
                reboot_repo.clone(),
                reboot_ops.clone(),
                daemon,
                TerminalRendererRegistry::new_with_repo(reboot_repo.clone()),
                events,
                bus,
            ),
        )
        .await
        .unwrap();
        for _ in 0..2 {
            reboot
                .apply_recovery(reboot.recover_on_boot().await.unwrap())
                .await
                .unwrap();
            reboot.drive().await.unwrap();
        }
        assert_eq!(
            shared.interrupted_turns_for_test().len(),
            0,
            "viewer rejection must not interrupt business"
        );
        assert_eq!(
            shared.started_turns_for_test().len(),
            1,
            "recovery must not duplicate turn/start"
        );
        assert_eq!(
            starts.load(Ordering::SeqCst),
            1,
            "recovery must not repeat viewer Ensure"
        );
        assert_eq!(
            reboot_ops
                .get_operation(&op.id)
                .await
                .unwrap()
                .unwrap()
                .phase,
            Phase::Succeeded
        );
        assert!(reboot_repo.card_get(&card_id).await.unwrap().is_some());
        assert!(
            reboot_repo
                .session_projection_by_id(&session_id)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            reboot_repo
                .terminal_get(&output.output_string("terminal_id", "test").unwrap())
                .await
                .unwrap()
                .is_some()
        );
        let recovered_session = reboot_repo
            .session_projection_by_id(&session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recovered_session.thread_id, session.thread_id);
        assert_eq!(recovered_session.active_turn_id, session.active_turn_id);
        let lease_state: String =
            sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id=?1")
                .bind(output.output_string("lease_id", "test").unwrap())
                .fetch_one(reboot_repo.pool())
                .await
                .unwrap();
        assert_eq!(lease_state, "held");
        assert!(cwd.join("completed-notes.txt").is_file());
        return;
    }
    let session = harness
        .repo
        .session_projection_by_id(&session_id)
        .await
        .unwrap()
        .unwrap();
    let track = harness
        .repo
        .track_get(&harness.track_id)
        .await
        .unwrap()
        .unwrap();
    let identity = ToolCallIdentity {
        card_id: card_id.clone(),
        role: crate::model::CardRole::Worker,
        provider: crate::session_projection_repo::AgentProvider::Codex,
        session_id,
        track_id: Some(harness.track_id.clone()),
        area_id: track.area_id.to_string(),
        thread_id: session.thread_id.unwrap(),
    };
    let context = Arc::new(AppContext {
        terminal_interaction: Arc::new(tokio::sync::OnceCell::new()),
        repo: harness.repo.clone(),
        track_vcs: None,
        events: harness.events.clone(),
        write,
        daemon_token_hash: None,
        gate_logs_dir: dir.path().join("gates"),
        plugin_host: Arc::new(tokio::sync::OnceCell::new()),
        operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
        track_creator: Arc::new(tokio::sync::OnceCell::new()),
        scheduler_poke: Arc::new(tokio::sync::OnceCell::new()),
        series_resolver: Arc::new(crate::report_series::SeriesResolver::new_unstarted(None)),
        plugin_results: Arc::new(crate::plugin_results::PluginResults::new()),
        read_ledger: Arc::new(crate::report_read_ledger::ReadLedger::new()),
        preview: Arc::new(crate::preview::PreviewRegistry::disabled()),
        sqlite_pool: crate::db::Repo::sqlite_pool(harness.repo.as_ref()),
        gate_run_wait: crate::operation::task_gate_run::GateRunWait::DEFAULT,
    });
    let report = if expected == crate::model::TaskStatus::Failed {
        WorkerTaskReport::Failed {
            attempt_id: task.id.clone(),
            reason: "useful failed notes retained".into(),
        }
    } else {
        WorkerTaskReport::Completed {
            attempt_id: task.id.clone(),
            result: json!({"notes":"completed-notes.txt"}),
            artifacts: vec![],
            commit_message: DeliveryMessage::Kernel,
        }
    };
    CardDecisionSink::from_app_context(&context)
        .commit_worker_task_report(&identity, report)
        .await
        .unwrap();
    assert_eq!(
        harness
            .repo
            .task_get(&task.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        expected
    );
    resume.notify_one();
    let op_id = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let op = op_repo.get_operation(&op_id).await.unwrap().unwrap();
    let card_exists = harness.repo.card_get(&card_id).await.unwrap().is_some();
    assert!(
        cwd.join("completed-notes.txt").is_file(),
        "optional viewer discarded already-reported work: expected={expected:?}, card_exists={card_exists}, phase={:?}",
        op.phase
    );
    assert!(card_exists);
    assert_eq!(
        std::fs::read(cwd.join("completed-notes.txt")).unwrap(),
        b"preserve this completed work\n"
    );
    assert_eq!(
        harness
            .repo
            .task_get(&task.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        expected
    );
    assert_eq!(
        starts.load(Ordering::SeqCst),
        usize::from(expected == crate::model::TaskStatus::Verifying)
    );
    assert_eq!(op.phase, Phase::Succeeded);
}
