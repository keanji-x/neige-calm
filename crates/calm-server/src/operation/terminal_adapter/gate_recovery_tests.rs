use super::*;
use crate::operation::task_verify_adapter::{TaskVerifyAdapter, gate_attempt_key};
use crate::operation::{OperationCompletionBus, Phase, SpawnOutcome};
use crate::state::{DaemonClient, WriteContext};
use crate::terminal_renderer::TerminalRendererRegistry;

#[tokio::test]
async fn task_recovery_gate_withdrawn_after_prepare_never_releases_steps() {
    for withdrawn in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let harness = terminal_worker_harness_with_workspace(dir.path().to_str().unwrap()).await;
        let events = crate::event::EventBus::new();
        let write = WriteContext::new(
            harness.adapter.card_role_cache.clone(),
            harness.adapter.track_area_cache.clone(),
        );
        let fixture = crate::task_recovery::launch_test_support::recovered_claimed_task(
            harness.repo.clone(), events.clone(), write, &harness.track_id,
            json!({"key":"launch", "kind":"terminal", "command":"true", "ready":true, "declared_by":"user", "gate":{"steps":[{"name":"witness", "cmd":"printf released > gate-released"}]}}),
        ).await;
        let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
        assert_eq!(
            crate::db::sqlite::task_start_verifying_from_worker_tx(
                &mut tx,
                &fixture.task.id,
                &harness.track_id,
                crate::db::sqlite::TaskReporter::Kernel,
                crate::model::now_ms(),
            )
            .await
            .unwrap(),
            1
        );
        tx.commit().await.unwrap();
        let mut adapter = TaskVerifyAdapter::new(dir.path().join("logs"));
        let fixture = Arc::new(fixture);
        if withdrawn {
            let fixture = fixture.clone();
            adapter.before_release = Some(Arc::new(move || {
                let fixture = fixture.clone();
                Box::pin(async move { fixture.withdraw().await })
            }));
        }
        let repo = Arc::new(SqlxOperationRepo::new(harness.repo.pool().clone()));
        let payload = serde_json::to_value(
            crate::operation::task_verify_adapter::TaskVerifyOperationPayload {
                actor: crate::ids::ActorId::KernelDispatcher,
                track_id: harness.track_id.clone(),
                task_id: fixture.task.id.clone(),
                attempt: 1,
            },
        )
        .unwrap();
        let id = repo
            .insert_operation(
                "task-verify",
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: Some(gate_attempt_key(&fixture.task.id, 1)),
                    payload_hash: crate::routes::terminal_cards::stable_payload_hash(&payload)
                        .unwrap(),
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
        repo.prepare_tx_and_advance(&op, &adapter)
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
            repo.clone(),
            Arc::new(DaemonClient::new_stub()),
            TerminalRendererRegistry::new_with_repo(harness.repo.clone()),
            events,
            OperationCompletionBus::new(),
        );
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            adapter.spawn_side_effect(&output, &op, &ctx),
        )
        .await
        .unwrap();
        let refused = result.is_err();
        // Reap every successfully released finite test shell, including when a
        // deliberate mutation incorrectly allows the withdrawn branch.
        if let Ok(SpawnOutcome::Parked { observer, .. }) = result {
            tokio::time::timeout(std::time::Duration::from_secs(10), observer)
                .await
                .unwrap();
        }
        assert_eq!(
            refused, withdrawn,
            "withdrawn={withdrawn}: actual verifier admission"
        );
        assert_eq!(dir.path().join("gate-released").exists(), !withdrawn);
        let recorded = repo.get_operation(&id).await.unwrap().unwrap();
        assert!(
            recorded.spawn_artifacts.is_some(),
            "held wrapper is recorded before release decision"
        );
        assert_eq!(
            recorded
                .tx_output
                .unwrap()
                .data
                .get("launch_admission")
                .is_some(),
            !withdrawn
        );
    }
}
