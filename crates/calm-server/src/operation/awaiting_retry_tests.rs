//! Explicit-retry interaction receipts must remain nonterminal and undriven.
use super::*;

#[test]
fn awaiting_retry_roundtrips_the_saved_interaction_without_a_terminal_result() {
    let detail = json!({"kind": "mint_and_await", "thread_id": "original"});
    let phase = Phase::deserialize_join("awaiting_retry", Some(&detail)).unwrap();
    let (tag, saved) = phase.serialize_split();
    assert_eq!(tag.as_str(), "awaiting_retry");
    assert_eq!(saved, Some(detail));
}

#[tokio::test]
async fn awaiting_retry_schema_accepts_a_durable_receipt_without_spawn_artifacts() {
    let truth = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let repo = SqlxOperationRepo::new(truth.pool().clone());
    let id = repo
        .insert_operation(
            "confirmation-test",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some("intent".into()),
                payload_hash: "hash".into(),
            },
            json!({"intent": "keep"}),
        )
        .await
        .unwrap();
    let output = TxOutput::new("unknown", None, json!({"confirmed": false}));
    sqlx::query("UPDATE operations SET phase='awaiting_retry', phase_detail_json=?1, tx_output_json=?2 WHERE id=?3")
        .bind(json!({"kind":"mint_and_await","thread_id":null}).to_string())
        .bind(serde_json::to_string(&output).unwrap()).bind(&id).execute(truth.pool()).await.unwrap();
    assert!(repo.operation_result(&id).await.unwrap().is_none());
    assert!(repo.claim_drive_batch(32).await.unwrap().is_empty());
    assert!(
        repo.abandoned_running_operations_on_boot()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repo.abandoned_running_operations_steady_state()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repo.claim_operation_for_recovery(&id)
            .await
            .unwrap()
            .is_none()
    );
    let stored = repo.get_operation(&id).await.unwrap().unwrap();
    assert_eq!(stored.payload, json!({"intent":"keep"}));
    assert_eq!(stored.idempotency_key.as_deref(), Some("intent"));
    assert!(stored.spawn_artifacts.is_none());
    assert!(
        sqlx::query("DELETE FROM operations WHERE id=?1")
            .bind(&id)
            .execute(truth.pool())
            .await
            .is_err(),
        "the retry receipt must keep its permanent deduplication key"
    );
}

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

struct ConfirmationAdapter {
    confirmed: AtomicBool,
    interactions: AtomicUsize,
    spawns: AtomicUsize,
}

#[async_trait]
impl ProviderAdapter for ConfirmationAdapter {
    fn kind(&self) -> &'static str {
        "confirmation-test"
    }
    fn phases(&self) -> &'static [PhaseTag] {
        &[
            PhaseTag::Pending,
            PhaseTag::TxCommitted,
            PhaseTag::AppServerInteract,
            PhaseTag::AwaitingRetry,
            PhaseTag::SpawnStarted,
            PhaseTag::SpawnSucceeded,
            PhaseTag::Succeeded,
        ]
    }
    fn app_server_interact_kind(
        &self,
        _: &TxOutput,
        _: &Operation,
    ) -> Result<AppServerInteractKind> {
        Ok(AppServerInteractKind::MintAndAwait { thread_id: None })
    }
    async fn validate(&self, _: &Value) -> Result<()> {
        Ok(())
    }
    async fn prepare_tx<'tx>(&self, _: &mut Tx<'tx>, _: &Value, _: &Operation) -> Result<TxOutput> {
        Ok(TxOutput::new("unknown", None, json!({"ready":false})))
    }
    async fn app_server_interact(
        &self,
        output: &mut TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        self.interactions.fetch_add(1, Ordering::SeqCst);
        if self.confirmed.load(Ordering::SeqCst) {
            output.result = json!({"ready":true});
            Ok(AppServerInteractOutcome::NotApplicable)
        } else {
            output.data = json!({"provider_proof":"unconfirmed"});
            Ok(AppServerInteractOutcome::AwaitingRetry)
        }
    }
    async fn spawn_side_effect(
        &self,
        _: &TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        self.spawns.fetch_add(1, Ordering::SeqCst);
        Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
    }
    async fn plan_compensation(
        &self,
        _: PhaseTag,
        _: &str,
        _: &TxOutput,
        _: &Operation,
    ) -> Result<CompensationStateVersioned> {
        panic!("deferred interaction must not compensate")
    }
    async fn compensate_step(
        &self,
        _: &CompensationStep,
        _: &TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<()> {
        panic!("deferred interaction must not compensate")
    }
}

async fn runtime() -> (
    OperationRuntime,
    Arc<SqlxOperationRepo>,
    Arc<ConfirmationAdapter>,
) {
    let truth = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let repo = Arc::new(SqlxOperationRepo::new(truth.pool().clone()));
    let adapter = Arc::new(ConfirmationAdapter {
        confirmed: AtomicBool::new(false),
        interactions: AtomicUsize::new(0),
        spawns: AtomicUsize::new(0),
    });
    let runtime = super::tests::test_runtime(truth, repo.clone(), vec![adapter.clone()]);
    (runtime, repo, adapter)
}

fn key(hash: &str) -> OperationKey {
    OperationKey {
        operation_key: new_id(),
        idempotency_key: Some("intent".into()),
        payload_hash: hash.into(),
    }
}

#[tokio::test]
async fn awaiting_retry_read_boot_and_same_key_replay_never_drive_or_compensate() {
    let (runtime, repo, adapter) = runtime().await;
    let id = runtime
        .submit("confirmation-test", key("hash"), json!({"request":"kept"}))
        .await
        .unwrap();
    for _ in 0..2 {
        let receipt = runtime
            .wait_for_receipt(&id, Duration::from_millis(50))
            .await
            .unwrap();
        let OperationReceipt::AwaitingRetry {
            operation_id,
            attempt,
            output,
        } = receipt
        else {
            panic!("missing deferred receipt")
        };
        assert_eq!(operation_id, id);
        assert_eq!(attempt, 0);
        assert_eq!(output.data, json!({"provider_proof":"unconfirmed"}));
        assert_eq!(output.result, json!({"ready":false}));
        let plan = runtime.recover_on_boot().await.unwrap();
        assert!(plan.items.is_empty());
        runtime.apply_recovery(plan).await.unwrap();
        runtime.drive().await.unwrap();
        assert_eq!(
            runtime
                .submit("confirmation-test", key("hash"), json!({"request":"kept"}))
                .await
                .unwrap(),
            id
        );
    }
    assert_eq!(adapter.interactions.load(Ordering::SeqCst), 1);
    assert_eq!(adapter.spawns.load(Ordering::SeqCst), 0);
    assert!(repo.operation_result(&id).await.unwrap().is_none());
    assert!(matches!(
        runtime.wait(&id).await,
        Err(CalmError::ServiceUnavailable(_))
    ));
    assert!(matches!(
        runtime
            .retry_interaction("confirmation-test", &key("different"), 0)
            .await,
        Err(CalmError::IdempotencyKeyReused(_))
    ));
    assert_eq!(adapter.interactions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn awaiting_retry_generation_prevents_concurrent_and_stale_rearms() {
    let (runtime, repo, adapter) = runtime().await;
    let id = runtime
        .submit("confirmation-test", key("hash"), json!({}))
        .await
        .unwrap();
    let first_key = key("hash");
    let second_key = key("hash");
    let (a, b) = tokio::join!(
        runtime.retry_interaction("confirmation-test", &first_key, 0),
        runtime.retry_interaction("confirmation-test", &second_key, 0)
    );
    for receipt in [a.unwrap(), b.unwrap()] {
        assert!(matches!(
            receipt,
            OperationReceipt::AwaitingRetry { attempt: 1, .. }
        ));
    }
    assert_eq!(
        adapter.interactions.load(Ordering::SeqCst),
        2,
        "one continuation for the observed generation"
    );
    runtime
        .retry_interaction("confirmation-test", &key("hash"), 0)
        .await
        .unwrap();
    assert_eq!(
        adapter.interactions.load(Ordering::SeqCst),
        2,
        "a lost-response retry cannot rearm a later unknown"
    );
    adapter.confirmed.store(true, Ordering::SeqCst);
    let receipt = runtime
        .retry_interaction("confirmation-test", &key("hash"), 1)
        .await
        .unwrap();
    let OperationReceipt::Settled(result) = receipt else {
        panic!("retry did not finish")
    };
    assert_eq!(result.op_id, id);
    assert!(
        matches!(result.outcome, OperationOutcome::Succeeded { result } if result == json!({"ready":true}))
    );
    runtime
        .retry_interaction("confirmation-test", &key("hash"), 1)
        .await
        .unwrap();
    assert_eq!(adapter.spawns.load(Ordering::SeqCst), 1);
    assert_eq!(adapter.interactions.load(Ordering::SeqCst), 3);
    assert_eq!(repo.get_operation(&id).await.unwrap().unwrap().attempt, 2);
}

#[tokio::test]
async fn awaiting_retry_expired_or_replaced_lease_cannot_checkpoint_a_receipt() {
    let (_, repo, _) = runtime().await;
    let id = repo
        .insert_operation("confirmation-test", key("hash"), json!({}))
        .await
        .unwrap();
    let claimed = repo.claim_drive_batch(1).await.unwrap().pop().unwrap();
    let phase = Phase::AppServerInteract {
        kind: AppServerInteractKind::MintAndAwait { thread_id: None },
    };
    repo.set_phase_and_tx_output(
        &claimed,
        phase,
        &TxOutput::new("unknown", None, json!({"original":true})),
    )
    .await
    .unwrap()
    .unwrap();
    let claimed = repo.claim_drive_batch(1).await.unwrap().pop().unwrap();
    sqlx::query("UPDATE operations SET lease_until_ms=?1 WHERE id=?2")
        .bind(now_ms() - 1)
        .bind(&id)
        .execute(&repo.pool)
        .await
        .unwrap();
    let deferred = Phase::AwaitingRetry {
        kind: AppServerInteractKind::MintAndAwait { thread_id: None },
    };
    let changed = TxOutput::new("unknown", None, json!({"wrong":true}));
    assert!(
        repo.set_phase_and_tx_output(&claimed, deferred.clone(), &changed)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo.set_phase(&claimed, deferred.clone())
            .await
            .unwrap()
            .is_none()
    );
    let newer = repo.claim_drive_batch(1).await.unwrap().pop().unwrap();
    assert_ne!(newer.lease_owner, claimed.lease_owner);
    assert!(
        repo.set_phase_and_tx_output(&claimed, deferred.clone(), &changed)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        repo.get_operation(&id).await.unwrap().unwrap().phase,
        Phase::AppServerInteract { .. }
    ));
    assert_eq!(
        repo.get_operation(&id)
            .await
            .unwrap()
            .unwrap()
            .tx_output
            .unwrap()
            .result,
        json!({"original":true})
    );
    assert!(
        repo.set_phase_and_tx_output(&newer, deferred, &changed)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn awaiting_retry_bounded_receipt_wait_is_read_only_while_operation_runs() {
    let (runtime, repo, adapter) = runtime().await;
    let id = repo
        .insert_operation("confirmation-test", key("hash"), json!({}))
        .await
        .unwrap();
    assert!(matches!(
        runtime
            .wait_for_receipt(&id, Duration::from_millis(30))
            .await
            .unwrap(),
        OperationReceipt::Running { attempt: 0, .. }
    ));
    assert_eq!(adapter.interactions.load(Ordering::SeqCst), 0);
    assert_eq!(adapter.spawns.load(Ordering::SeqCst), 0);
    assert!(matches!(
        repo.get_operation(&id).await.unwrap().unwrap().phase,
        Phase::Pending
    ));
}

#[tokio::test]
async fn awaiting_retry_unkeyed_submission_is_refused_before_admission_and_effects() {
    let (runtime, repo, adapter) = runtime().await;
    for intent_key in [None, Some(String::new())] {
        let key = OperationKey {
            operation_key: new_id(),
            idempotency_key: intent_key,
            payload_hash: "hash".into(),
        };
        assert!(matches!(
            runtime.submit("confirmation-test", key, json!({})).await,
            Err(CalmError::BadRequest(_))
        ));
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM operations")
        .fetch_one(&repo.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(adapter.interactions.load(Ordering::SeqCst), 0);
    assert_eq!(adapter.spawns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn awaiting_retry_unregistered_adapter_cannot_claim_a_saved_intent() {
    let (runtime, repo, _) = runtime().await;
    let id = repo
        .insert_operation("removed-adapter", key("hash"), json!({}))
        .await
        .unwrap();
    let output = TxOutput::new("unknown", None, json!({"unconfirmed":true}));
    sqlx::query("UPDATE operations SET phase='awaiting_retry', phase_detail_json=?1, tx_output_json=?2 WHERE id=?3")
        .bind(json!({"kind":"mint_and_await","thread_id":null}).to_string())
        .bind(serde_json::to_string(&output).unwrap()).bind(&id).execute(&repo.pool).await.unwrap();
    assert!(matches!(
        runtime
            .retry_interaction("removed-adapter", &key("hash"), 0)
            .await,
        Err(CalmError::BadRequest(_))
    ));
    let op = repo.get_operation(&id).await.unwrap().unwrap();
    assert!(matches!(op.phase, Phase::AwaitingRetry { .. }));
    assert!(op.lease_owner.is_none());
    assert_eq!(op.attempt, 0);
}
