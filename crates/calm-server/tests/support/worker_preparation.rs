//! Drive a persisted pending worker through its real claim, external observation and commit.
use calm_server::db::sqlite::SqlxRepo;
use calm_server::operation::{
    Operation, OperationCompletionBus, OperationRepo, ProviderAdapter, SpawnCtx, SqlxOperationRepo,
    TxOutput,
};
use calm_server::state::AppState;
use std::sync::Arc;

pub async fn prepare(
    repo: Arc<SqlxRepo>,
    state: &AppState,
    adapter: &dyn ProviderAdapter,
    original: &Operation,
) -> (TxOutput, Operation) {
    let operations = Arc::new(SqlxOperationRepo::new(repo.db().clone()));
    let claimed = operations
        .claim_drive_batch(32)
        .await
        .unwrap()
        .into_iter()
        .find(|op| op.id == original.id)
        .expect("pending worker is claimed");
    let ctx = SpawnCtx::new(
        state.repo.clone(),
        operations.clone(),
        state.daemon.clone(),
        state.terminal_renderer.clone(),
        state.events.clone(),
        OperationCompletionBus::new(),
    );
    let evidence = adapter
        .prepare_external(&claimed.payload, &claimed, &ctx)
        .await
        .unwrap();
    let (prepared, _) = operations
        .prepare_tx_and_advance(&claimed, adapter, evidence)
        .await
        .unwrap()
        .committed()
        .expect("worker preparation commits");
    (prepared.tx_output.clone().unwrap(), prepared)
}
