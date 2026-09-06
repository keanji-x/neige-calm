use super::*;
use crate::operation::OperationCompletionBus;
use crate::state::{DaemonClient, WriteContext};
use crate::terminal_renderer::TerminalRendererRegistry;

#[tokio::test]
async fn task_recovery_codex_withdrawn_during_preparation_never_starts_turn() {
    let harness = worker_lease_harness().await;
    let write = WriteContext::new(
        harness.adapter.card_role_cache.clone(),
        harness.adapter.track_area_cache.clone(),
    );
    let fixture=crate::task_recovery::launch_test_support::recovered_claimed_task(harness.repo.clone(),harness.events.clone(),write,&harness.track_id,
        json!({"key":"launch","kind":"codex","goal":"bounded launch fixture","ready":true,"declared_by":"user","no_gate_reason":"launch admission test"})).await;
    let shared = SharedCodexAppServer::new_fake_running_with_pending(harness.repo.clone(), None);
    let socket_dir = tempfile::tempdir().unwrap();
    let server = McpServer::new_for_test(crate::mcp_server::McpShimConfig {
        shim_bin: socket_dir.path().join("shim"),
        socket_path: socket_dir.path().join("mcp.sock"),
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
    adapter.preparation_hook = Some(Arc::new(move || {
        let entered = entered_hook.clone();
        let resume = resume_hook.clone();
        Box::pin(async move {
            entered.notify_one();
            resume.notified().await;
        })
    }));
    let adapter = Arc::new(adapter);
    let repo = Arc::new(SqlxOperationRepo::new(harness.repo.pool().clone()));
    let (kind, payload) = crate::scheduler::build_worker_payload(&fixture.task).unwrap();
    let id = repo
        .insert_operation(
            kind,
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(fixture.task.id.clone()),
                payload_hash: crate::routes::terminal_cards::stable_payload_hash(&payload).unwrap(),
            },
            payload,
        )
        .await
        .unwrap();
    let op = repo
        .claim_drive_batch(1)
        .await
        .unwrap()
        .into_iter()
        .find(|op| op.id == id)
        .unwrap();
    repo.prepare_tx_and_advance(&op, adapter.as_ref())
        .await
        .unwrap()
        .unwrap();
    let op = repo
        .claim_drive_batch(1)
        .await
        .unwrap()
        .into_iter()
        .find(|op| op.id == id)
        .unwrap();
    repo.set_phase(&op, Phase::SpawnStarted)
        .await
        .unwrap()
        .unwrap();
    let op = repo
        .claim_drive_batch(1)
        .await
        .unwrap()
        .into_iter()
        .find(|op| op.id == id)
        .unwrap();
    let output = op.tx_output.clone().unwrap();
    // Even a deliberately broken launch-fence mutation must never reach a real
    // shared-host Codex/supervisor; only the fake app-server turn is observable.
    let mut daemon = DaemonClient::new_stub();
    daemon.proc_supervisor_sock = Some(socket_dir.path().join("unused-supervisor.sock"));
    let ctx = SpawnCtx::new(
        harness.repo.clone(),
        repo,
        Arc::new(daemon),
        TerminalRendererRegistry::new(),
        harness.events.clone(),
        OperationCompletionBus::new(),
    );
    let run = tokio::spawn(async move { adapter.spawn_side_effect(&output, &op, &ctx).await });
    struct AbortOnDrop(tokio::task::AbortHandle);
    impl Drop for AbortOnDrop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _abort = AbortOnDrop(run.abort_handle());
    tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    fixture.withdraw().await;
    resume.notify_one();
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), run)
        .await
        .unwrap()
        .unwrap();
    assert!(
        shared.started_turns_for_test().is_empty(),
        "no business turn may start after withdrawal committed during preparation"
    );
    assert!(result.is_err());
}
