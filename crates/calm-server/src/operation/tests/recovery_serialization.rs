use std::sync::OnceLock;
use std::time::Duration;

use tokio::sync::Notify;

use super::*;

#[derive(Default)]
struct PreparationFenceAdapter {
    fence: OnceLock<Arc<Mutex<()>>>,
    recovery_entered: Notify,
    release_recovery: Notify,
    calls: std::sync::Mutex<Vec<(Value, bool)>>,
}

#[async_trait]
impl ProviderAdapter for PreparationFenceAdapter {
    fn kind(&self) -> &'static str {
        "preparation-fence-test"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        &[
            PhaseTag::Pending,
            PhaseTag::TxCommitted,
            PhaseTag::Succeeded,
        ]
    }

    async fn validate(&self, _input: &Value) -> Result<()> {
        Ok(())
    }

    async fn prepare_external(&self, input: &Value, _ctx: &SpawnCtx) -> Result<()> {
        // Observe the existing serialization fence at the actual effect callback, before
        // the ordinary driver starts: another caller cannot make an unlocked recovery pass.
        let fenced = self.fence.get().unwrap().try_lock().is_err();
        self.calls.lock().unwrap().push((input.clone(), fenced));
        if input["source"] == "recovery" {
            self.recovery_entered.notify_one();
            self.release_recovery.notified().await;
        }
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        _tx: &mut Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        Ok(TxOutput::new("unknown", None, input.clone()))
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

fn key(source: &str) -> OperationKey {
    OperationKey {
        operation_key: format!("preparation-{source}"),
        idempotency_key: Some(format!("preparation-{source}")),
        payload_hash: format!("hash-{source}"),
    }
}

#[tokio::test]
async fn recovery_and_ordinary_drive_share_the_effect_serialization_fence() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let repo = Arc::new(SqlxOperationRepo::new(sqlx_repo.pool().clone()));
    let adapter = Arc::new(PreparationFenceAdapter::default());
    let runtime = Arc::new(test_runtime(sqlx_repo, repo.clone(), vec![adapter.clone()]));
    adapter.fence.set(runtime.drive_mutex.clone()).unwrap();
    let recovery_payload = json!({ "source": "recovery" });
    let recovery_id = repo
        .insert_operation(adapter.kind(), key("recovery"), recovery_payload.clone())
        .await
        .unwrap();
    let plan = runtime.recover_on_boot().await.unwrap();
    assert!(matches!(
        plan.items.as_slice(),
        [RecoveryItem::Recover { op_id, from_phase: Phase::Pending, .. }] if op_id == &recovery_id
    ));

    let recovery = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.apply_recovery(plan).await }
    });
    tokio::time::timeout(Duration::from_secs(10), adapter.recovery_entered.notified())
        .await
        .expect("recovery reaches the real preparation callback");
    let ordinary_payload = json!({ "source": "ordinary" });
    let ordinary_id = repo
        .insert_operation(adapter.kind(), key("ordinary"), ordinary_payload.clone())
        .await
        .unwrap();

    // Poll the public ordinary driver while recovery is paused in its effect. With the
    // shared fence this poll must wait at the mutex, before claiming the accepted row.
    let ordinary_drive = runtime.drive();
    tokio::pin!(ordinary_drive);
    let ordinary_poll = futures::poll!(ordinary_drive.as_mut());
    let ordinary_waited = ordinary_poll.is_pending();
    let calls_while_recovering = adapter.calls.lock().unwrap().clone();
    adapter.release_recovery.notify_one();
    let (recovered, driven) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(recovery, async {
            match ordinary_poll {
                std::task::Poll::Ready(result) => result,
                std::task::Poll::Pending => ordinary_drive.await,
            }
        })
    })
    .await
    .expect("both drivers finish after the preparation barrier opens");
    recovered.unwrap().unwrap();
    driven.unwrap();

    // Finish both callers before asserting the missing-fence regression, so a red test
    // leaves no effect callback suspended and also checks that neither acceptance is lost.
    assert_ne!(recovery_id, ordinary_id);
    for (id, source, payload) in [
        (&recovery_id, "recovery", &recovery_payload),
        (&ordinary_id, "ordinary", &ordinary_payload),
    ] {
        let stored = repo.get_operation(id).await.unwrap().unwrap();
        assert_eq!(stored.id, *id);
        assert_eq!(stored.payload_hash, key(source).payload_hash);
        assert_eq!(&stored.payload, payload);
        assert_eq!(stored.phase, Phase::Succeeded);
        assert!(matches!(
            repo.operation_result(id).await.unwrap(),
            Some(OperationResult {
                outcome: OperationOutcome::Succeeded { .. },
                ..
            })
        ));
    }
    let calls = adapter.calls.lock().unwrap().clone();
    assert_eq!(
        calls.iter().map(|(input, _)| input).collect::<Vec<_>>(),
        vec![&recovery_payload, &ordinary_payload],
        "each distinct accepted operation prepares exactly once"
    );
    assert!(ordinary_waited, "ordinary drive waits during recovery");
    assert_eq!(calls_while_recovering.len(), 1, "effects must not overlap");
    assert!(
        calls.iter().all(|(_, fenced)| *fenced),
        "recovery and ordinary effects must both hold the existing drive mutex: {calls:?}"
    );
}
