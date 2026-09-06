use super::*;
use crate::operation::launch_cleanup_test_support::{
    AckProxy, install_commit_fault, probe_running, spawn_sibling,
};
use crate::operation::{OperationRuntime, Phase};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

#[tokio::test]
async fn recovery_launch_claude_commit_failure_retains_workspace_and_settings() {
    let harness = claude_worker_harness().await;
    let write = WriteContext::new(
        harness.adapter.card_role_cache.clone(),
        harness.adapter.track_area_cache.clone(),
    );
    let fixture = crate::task_recovery::launch_test_support::recovered_claimed_task(harness.repo.clone(), harness.events.clone(), write, &harness.track_id,
        json!({"key":"launch","kind":"claude","goal":"keep useful notes","ready":true,"declared_by":"user","no_gate_reason":"owned fixture"})).await;
    let dir = calm_test_sockets::socket_dir("claude");
    let witness = dir.path().join("launched");
    let binary = dir.path().join("fixture-claude");
    std::fs::write(
        &binary,
        format!(
            "#!/bin/sh\nprintf useful-notes > retained-notes\nprintf running > {}\nsleep 120\n",
            shell_single_quote(witness.to_str().unwrap())
        ),
    )
    .unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut client = CodexClient::new_stub();
    client.claude_bin = binary.to_str().unwrap().into();
    client.claude_settings_dir = dir.path().join("settings");
    let mcp = McpServer::new_for_test(crate::mcp_server::McpShimConfig {
        shim_bin: dir.path().join("shim"),
        socket_path: dir.path().join("mcp.sock"),
    });
    let adapter = ClaudeWorkerAdapter::new(
        harness.repo.clone(),
        Arc::new(client),
        Some(mcp),
        harness.adapter.card_role_cache.clone(),
        harness.adapter.track_area_cache.clone(),
        harness.workspace.path().into(),
    );
    let supervisor = calm_proc_supervisor::test_support::InProcessProcSupervisor::start()
        .await
        .unwrap();
    let sibling = spawn_sibling(supervisor.sock(), dir.path()).await;
    let proxy = AckProxy::start(supervisor.sock(), witness, false).await;
    install_commit_fault(harness.repo.pool()).await;
    let op_repo = Arc::new(SqlxOperationRepo::new(harness.repo.pool().clone()));
    let mut daemon = DaemonClient::new_stub();
    daemon.proc_supervisor_sock = Some(proxy.sock.clone());
    let renderer = TerminalRendererRegistry::new_with_repo(harness.repo.clone());
    let completion = OperationCompletionBus::new();
    let runtime = OperationRuntime::new(
        op_repo.clone(),
        vec![Arc::new(adapter)],
        harness.events.clone(),
        completion.clone(),
        SpawnCtx::new(
            harness.repo.clone(),
            op_repo.clone(),
            Arc::new(daemon),
            renderer,
            harness.events.clone(),
            completion,
        ),
    )
    .await
    .unwrap();
    let (kind, payload) = crate::scheduler::build_worker_payload(&fixture.task).unwrap();
    let id = tokio::time::timeout(
        Duration::from_secs(15),
        runtime.submit(
            kind,
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(fixture.task.id),
                payload_hash: crate::routes::terminal_cards::stable_payload_hash(&payload).unwrap(),
            },
            payload,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let op = op_repo.get_operation(&id).await.unwrap().unwrap();
    assert!(
        op.compensation_state.as_ref().unwrap()["reason"]
            .as_str()
            .unwrap()
            .contains("FOREIGN KEY")
    );
    let output = op.tx_output.as_ref().unwrap();
    let cwd = std::path::PathBuf::from(output.output_string("cwd", "test").unwrap());
    let settings = std::path::PathBuf::from(output.output_string("settings_path", "test").unwrap());
    assert_eq!(
        std::fs::read(cwd.join("retained-notes")).unwrap(),
        b"useful-notes"
    );
    assert!(
        settings.is_file(),
        "first compensation step cannot discard owned settings"
    );
    assert!(
        harness
            .repo
            .card_get(&output.output_string("card_id", "test").unwrap())
            .await
            .unwrap()
            .is_some()
    );
    let term = harness
        .repo
        .terminal_get(&output.output_string("terminal_id", "test").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(term.pid.is_some());
    assert!(matches!(op.phase, Phase::Stuck { .. }));
    assert!(probe_running(supervisor.sock(), &sibling).await);
}
