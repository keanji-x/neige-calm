//! Retryable preparation must roll back writes and yield only its own claim.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

struct DeferredPreparation(AtomicBool);
#[async_trait]
impl ProviderAdapter for DeferredPreparation {
    fn kind(&self) -> &'static str {
        "prepare-admission-test"
    }
    fn phases(&self) -> &'static [PhaseTag] {
        &[PhaseTag::Pending, PhaseTag::TxCommitted]
    }
    async fn validate(&self, _: &Value) -> Result<()> {
        Ok(())
    }
    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        _: &Value,
        _: &Operation,
    ) -> Result<TxOutput> {
        sqlx::query("INSERT INTO preparation_writes(id) VALUES(1)")
            .execute(&mut **tx)
            .await?;
        if self.0.swap(false, Ordering::SeqCst) {
            return Err(CalmError::OperationDeferred("resource occupied".into()));
        }
        Ok(TxOutput::new("unknown", None, json!({})))
    }
    async fn app_server_interact(
        &self,
        _: &mut TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        unreachable!("this test exercises preparation only")
    }
    async fn spawn_side_effect(
        &self,
        _: &TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        unreachable!("deferred preparation cannot spawn")
    }
    async fn plan_compensation(
        &self,
        _: PhaseTag,
        _: &str,
        _: &TxOutput,
        _: &Operation,
    ) -> Result<CompensationStateVersioned> {
        unreachable!("deferred preparation cannot compensate")
    }
    async fn compensate_step(
        &self,
        _: &CompensationStep,
        _: &TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<()> {
        unreachable!("deferred preparation cannot compensate")
    }
}

#[tokio::test]
async fn deferred_prepare_rolls_back_and_reuses_the_pending_operation() {
    let db = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("CREATE TABLE preparation_writes(id INTEGER PRIMARY KEY)")
        .execute(db.pool())
        .await
        .unwrap();
    let repo = SqlxOperationRepo::new(db.pool().clone());
    let op_id = repo
        .insert_operation(
            "prepare-admission-test",
            OperationKey {
                operation_key: "prepare-key".into(),
                idempotency_key: Some("prepare-idem".into()),
                payload_hash: "prepare-hash".into(),
            },
            json!({}),
        )
        .await
        .unwrap();
    let op = repo.claim_drive_batch(1).await.unwrap().pop().unwrap();
    let adapter = DeferredPreparation(AtomicBool::new(true));
    assert!(matches!(
        repo.prepare_tx_and_advance(&op, &adapter).await,
        Err(CalmError::OperationDeferred(_))
    ));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM preparation_writes")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "deferral must not leak partial preparation writes"
    );
    let deferred = repo.get_operation(&op_id).await.unwrap().unwrap();
    assert!(matches!(deferred.phase, Phase::Pending));
    assert!(
        deferred.lease_owner.is_none(),
        "yield the claim rather than wait for its long expiry"
    );
    let (until, updated): (Option<i64>, i64) =
        sqlx::query_as("SELECT lease_until_ms,updated_at_ms FROM operations WHERE id=?1")
            .bind(&op_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(
        until.is_some_and(|until| until > updated),
        "persist a cooldown so the driver does not spin"
    );
    let resumed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(op) = repo.claim_drive_batch(1).await.unwrap().pop() {
                break op;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(resumed.id, op_id);
    let (prepared, _) = repo
        .prepare_tx_and_advance(&resumed, &adapter)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(prepared.phase, Phase::TxCommitted));
    assert_eq!(prepared.operation_key, "prepare-key");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM preparation_writes")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        count, 1,
        "the successful retry commits exactly its own preparation"
    );
}
