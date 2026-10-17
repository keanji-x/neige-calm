//! One-shot evidence may be discarded without permanently failing its Pending operation.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

struct ChangingFacts {
    collections: AtomicUsize,
}
struct Observation {
    stale: bool,
}
#[async_trait]
impl PreparedTx for Observation {
    async fn apply(self: Box<Self>, _: &mut Tx<'_>, _: &Operation) -> Result<PreparedTxResult> {
        Ok(if self.stale {
            PreparedTxResult::Stale
        } else {
            PreparedTxResult::Ready(TxOutput::new("unknown", None, json!({"fresh": true})))
        })
    }
}
#[async_trait]
impl ProviderAdapter for ChangingFacts {
    fn kind(&self) -> &'static str {
        "changing-facts-test"
    }
    fn phases(&self) -> &'static [PhaseTag] {
        &[
            PhaseTag::Pending,
            PhaseTag::TxCommitted,
            PhaseTag::SpawnStarted,
            PhaseTag::SpawnSucceeded,
            PhaseTag::Succeeded,
        ]
    }
    async fn validate(&self, _: &Value) -> Result<()> {
        Ok(())
    }
    async fn prepare_external(
        &self,
        _: &Value,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<Option<Box<dyn PreparedTx>>> {
        let collection = self.collections.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Box::new(Observation {
            stale: collection < 3,
        })))
    }
    async fn prepare_tx<'tx>(&self, _: &mut Tx<'tx>, _: &Value, _: &Operation) -> Result<TxOutput> {
        panic!("the operation must consume its one-shot observation")
    }
    async fn app_server_interact(
        &self,
        _: &mut TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }
    async fn spawn_side_effect(
        &self,
        _: &TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
    }
    async fn plan_compensation(
        &self,
        _: PhaseTag,
        _: &str,
        _: &TxOutput,
        _: &Operation,
    ) -> Result<CompensationStateVersioned> {
        panic!("ordinary metadata changes must not compensate")
    }
    async fn compensate_step(
        &self,
        _: &CompensationStep,
        _: &TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<()> {
        panic!("ordinary metadata changes must not compensate")
    }
}

#[tokio::test]
async fn stale_preparation_is_bounded_and_wait_reclaims_pending_after_lease_expiry() {
    let truth = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let repo = Arc::new(SqlxOperationRepo::new(truth.pool().clone()));
    let adapter = Arc::new(ChangingFacts {
        collections: AtomicUsize::new(0),
    });
    let runtime = super::tests::test_runtime(truth, repo.clone(), vec![adapter.clone()]);
    let id = runtime
        .submit(
            adapter.kind(),
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some("one-operation".into()),
                payload_hash: "accepted-input".into(),
            },
            json!({"request":"accepted"}),
        )
        .await
        .unwrap();
    runtime.drive().await.unwrap();
    assert_eq!(adapter.collections.load(Ordering::SeqCst), 3);
    let pending = repo.get_operation(&id).await.unwrap().unwrap();
    assert_eq!(pending.phase, Phase::Pending);
    assert!(pending.tx_output.is_none());
    assert!(pending.last_error.is_none());
    assert!(pending.lease_owner.is_some());
    assert!(pending.lease_until_ms.unwrap() > now_ms());
    assert!(repo.operation_result(&id).await.unwrap().is_none());
    runtime.drive().await.unwrap();
    assert_eq!(
        adapter.collections.load(Ordering::SeqCst),
        3,
        "no immediate reclaim loop"
    );
    sqlx::query("UPDATE operations SET lease_until_ms=?1 WHERE id=?2")
        .bind(now_ms() - 1)
        .bind(&id)
        .execute(&repo.sqlite_pool())
        .await
        .unwrap();
    // This exercises the actual wait -> 25ms drive -> claim_drive_batch path, without a daemon
    // assumption or waiting sixty seconds for the original lease.
    let result = tokio::time::timeout(Duration::from_secs(5), runtime.wait(&id))
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(result.outcome, OperationOutcome::Succeeded { result } if result == json!({"fresh":true}))
    );
    assert_eq!(adapter.collections.load(Ordering::SeqCst), 4);
}
