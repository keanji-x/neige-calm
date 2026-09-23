//! `ProviderAdapter::before_insert` (#1777): `OperationRuntime::submit` runs
//! it after `validate`, before the op row exists, and outside every
//! transaction — the worker adapters fetch the attached repository's upstream
//! there, and nothing may hold the kernel's one write transaction on it.

use std::sync::Mutex as StdMutex;
use std::time::Duration;

use super::*;
use crate::terminal_renderer::TerminalRendererRegistry;

const KIND: &str = "before-insert-test";

struct OrderingAdapter {
    pool: SqlitePool,
    log: Arc<StdMutex<Vec<String>>>,
}

#[async_trait]
impl ProviderAdapter for OrderingAdapter {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn phases(&self) -> &'static [PhaseTag] {
        &[
            PhaseTag::Pending,
            PhaseTag::TxCommitted,
            PhaseTag::Succeeded,
        ]
    }

    async fn validate(&self, _input: &Value) -> Result<()> {
        self.log.lock().unwrap().push("validate".into());
        Ok(())
    }

    async fn before_insert(&self, _input: &Value) {
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE kind = ?1")
            .bind(KIND)
            .fetch_one(&self.pool)
            .await
            .unwrap();
        // The kernel's write transaction is free: taking it here would wait
        // (and time out) if the caller held one. Dropped at once (rollback).
        let write = tokio::time::timeout(Duration::from_secs(2), begin_immediate_tx(&self.pool))
            .await
            .map(|tx| tx.is_ok());
        self.log
            .lock()
            .unwrap()
            .push(format!("before_insert rows={rows} write_tx_free={write:?}"));
    }

    async fn prepare_tx<'tx>(
        &self,
        _tx: &mut Tx<'tx>,
        _input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        self.log.lock().unwrap().push("prepare_tx".into());
        Ok(TxOutput::new("unknown", None, json!({})))
    }

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }

    async fn spawn_side_effect(
        &self,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        _output: &TxOutput,
        _op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.into(),
            steps: Vec::new(),
        })
    }

    async fn compensate_step(
        &self,
        _step: &CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn submit_runs_before_insert_before_the_row_and_outside_any_transaction() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let pool = sqlx_repo.pool().clone();
    let operation_repo = Arc::new(SqlxOperationRepo::new(pool.clone()));
    let log = Arc::new(StdMutex::new(Vec::new()));
    let adapter: Arc<dyn ProviderAdapter> = Arc::new(OrderingAdapter {
        pool: pool.clone(),
        log: log.clone(),
    });
    let events = EventBus::new();
    let completion = OperationCompletionBus::new();
    let route_repo: Arc<dyn crate::db::RouteRepo> = Arc::new(sqlx_repo);
    let runtime = OperationRuntime::new_unchecked(
        operation_repo.clone(),
        vec![adapter],
        events.clone(),
        completion.clone(),
        SpawnCtx::new(
            route_repo.clone(),
            operation_repo,
            Arc::new(DaemonClient::new_stub()),
            TerminalRendererRegistry::new_with_repo(route_repo),
            events,
            completion,
        ),
    );

    let key = || OperationKey {
        operation_key: new_id(),
        idempotency_key: Some("before-insert".into()),
        payload_hash: "hash".into(),
    };
    runtime.submit(KIND, key(), json!({})).await.unwrap();

    assert_eq!(
        *log.lock().unwrap(),
        [
            "validate",
            "before_insert rows=0 write_tx_free=Ok(true)",
            "prepare_tx",
        ]
    );

    // A retried submission of the existing op short-circuits before both.
    log.lock().unwrap().clear();
    runtime.submit(KIND, key(), json!({})).await.unwrap();
    assert!(log.lock().unwrap().is_empty(), "{:?}", log.lock().unwrap());
}
