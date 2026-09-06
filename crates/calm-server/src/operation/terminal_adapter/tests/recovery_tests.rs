use super::*;
use crate::operation::{OperationCompletionBus, Phase};
use crate::state::{DaemonClient, WriteContext};
use crate::terminal_renderer::TerminalRendererRegistry;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn task_recovery_terminal_withdrawn_during_preparation_never_launches() {
    let harness = terminal_worker_harness().await;
    let events = crate::event::EventBus::new();
    let write = WriteContext::new(
        harness.adapter.card_role_cache.clone(),
        harness.adapter.track_area_cache.clone(),
    );
    let fixture=crate::task_recovery::launch_test_support::recovered_claimed_task(harness.repo.clone(),events.clone(),write,&harness.track_id,
        json!({"key":"launch","kind":"terminal","command":"true","ready":true,"declared_by":"user"})).await;
    let launches = Arc::new(AtomicUsize::new(0));
    let counter = launches.clone();
    let hook: SpawnHook = Arc::new(move |_, _, _, _| {
        let counter = counter.clone();
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(SpawnHandle::NoOp)
        })
    });
    let mut adapter = TerminalWorkerAdapter::new_with_spawn_hook(
        harness.repo.clone(),
        harness.adapter.card_role_cache.clone(),
        harness.adapter.track_area_cache.clone(),
        hook,
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
    let ctx = SpawnCtx::new(
        harness.repo.clone(),
        repo,
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new(),
        events,
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
    assert_eq!(launches.load(Ordering::SeqCst), 0);
    assert!(result.is_err());
}
