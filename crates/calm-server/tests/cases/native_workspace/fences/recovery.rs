use super::*;
use calm_server::event::EventBus;
use calm_server::operation::{
    OperationCompletionBus, OperationRuntime, RecoveryPlan, SpawnCtx, SqlxOperationRepo,
};
use calm_server::state::DaemonClient;
use calm_server::terminal_renderer::TerminalRendererRegistry;

#[tokio::test]
async fn native_runtime_boot_settles_execution_without_a_business_operation_or_scheduler() {
    let (root, repo, daemon, thread, turn, lease, nonce) = managed_turn().await;
    let facts = json!({"thread":{"id":thread,"cwd":root.path(),"status":{"type":"idle"},
        "turns":[{"id":turn,"status":"completed","items":[{"type":"userMessage","clientId":nonce}]}]}});
    std::fs::write(
        root.path().join("run/codex-appserver.thread-read"),
        serde_json::to_vec(&facts).unwrap(),
    )
    .unwrap();
    let operation_repo = Arc::new(SqlxOperationRepo::new(repo.pool().clone()));
    let events = EventBus::new();
    let completion = OperationCompletionBus::new();
    let spawn = SpawnCtx::new(
        repo.clone(),
        operation_repo.clone(),
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new(),
        events.clone(),
        completion.clone(),
    )
    .with_shared_codex_appserver(daemon);
    let runtime =
        OperationRuntime::new_unchecked(operation_repo, vec![], events, completion, spawn);
    runtime
        .apply_recovery(RecoveryPlan { items: vec![] })
        .await
        .unwrap();
    let state: String = sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id=?1")
        .bind(&lease)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(
        state, "released",
        "startup must reconcile native executions independently of abandoned business operations"
    );
}
