//! WA2 turn issuance: an operation's preparation CAS and provider admission
//! share the caller's writer transaction, and only its COMMIT drives.
use super::tests::Owner;
use super::*;
use crate::operation::{
    OperationKey, OperationRepo, PhaseTag, SqlxOperationRepo, TxCommittedAdvance, TxOutput,
    advance_to_tx_committed_tx,
};

async fn phase(ops: &SqlxOperationRepo, id: &str) -> PhaseTag {
    ops.get_operation(id).await.unwrap().unwrap().phase.tag()
}

#[tokio::test]
async fn w_turn_admission_rolls_back_with_phase() {
    let dir = tempfile::tempdir().unwrap();
    let binding = crate::harness::backend::dispatch_fixture::seed(
        dir.path(),
        crate::session_projection_repo::AgentProvider::Claude,
    )
    .await;
    let repo = &binding.repo;
    // Recovery ticks never start this execution, so a spawn proves COMMIT drove it.
    let fixture = crate::test_support::execution::Fixture::new_with_interval(
        repo.db().clone(),
        Duration::from_secs(3600),
    )
    .await;
    let owner = ProviderExecution {
        owner: Arc::new(Owner),
        drivers: Default::default(),
    };
    let ops = SqlxOperationRepo::new(repo.db().clone());
    let op_id = ops
        .insert_operation(
            "w-turn-admission",
            OperationKey {
                operation_key: crate::model::new_id(),
                idempotency_key: Some(format!("w-turn-admission:{}:first", binding.worker)),
                payload_hash: "turn".into(),
            },
            serde_json::json!({}),
        )
        .await
        .unwrap();
    let stale = ops.claim_drive_operation(&op_id).await.unwrap().unwrap();
    // Lease expiry lets a second claimant supersede `stale`, whose phase CAS then fails.
    sqlx::query("UPDATE operations SET lease_until_ms = 0 WHERE id = ?1")
        .bind(&op_id)
        .execute(repo.pool())
        .await
        .unwrap();
    let live = ops.claim_drive_operation(&op_id).await.unwrap().unwrap();
    let locks = crate::per_card_lock::new_keyed_locks();
    let track_free = || crate::per_card_lock::try_lock_key(&locks, &binding.track).is_some();
    for (op, commit) in [(&stale, false), (&live, true)] {
        let mut intent = workspace_runtime::execution::test_support::intent(dir.path().into());
        intent.metadata.consumer = Owner.consumer().into();
        intent.metadata.track.0 = binding.track.clone();
        intent.metadata.alias.0 = Owner.alias(&binding.worker, intent.metadata.id.as_str());
        intent.metadata.program.program = "/bin/sh".into();
        intent.metadata.program.args = vec!["-c".into(), "exec sleep 120".into()];
        let id = intent.metadata.id.clone();
        let mut output = TxOutput::new(
            "execution",
            Some(id.as_str().into()),
            serde_json::Value::Null,
        );
        output.data = serde_json::json!({ "execution_id": id.as_str() });
        let handoff = TrackFenceHandoff::new(
            crate::harness::run_loop::issuance::track_fence_in_for_test(&binding.track, &locks)
                .await,
        );
        let tx = calm_truth::db::sqlite::begin_immediate_tx(repo.db())
            .await
            .unwrap();
        // Authorization and admission run first, while the op claim may still be live
        // (design §3.3); the phase CAS follows in the same transaction.
        let mut admitted = owner
            .admit_tx(
                tx,
                &fixture.engine,
                &binding.worker,
                PreparedIntent(intent),
                Purpose::RewindCheck,
                OccupancyFence::ReleaseOnCommit(&handoff),
            )
            .await
            .unwrap();
        assert!(
            owner.drivers.contains(&id),
            "driver registered before COMMIT"
        );
        assert!(
            handoff.0.lock().unwrap().is_none(),
            "fence moved into admission"
        );
        assert!(!track_free(), "admission holds the Track fence");
        let advanced = advance_to_tx_committed_tx(admitted.tx_mut(), op, &output)
            .await
            .unwrap();
        if !commit {
            assert_eq!(advanced, TxCommittedAdvance::ClaimLost);
            drop(admitted);
            assert!(track_free(), "rollback releases the Track fence");
            assert!(
                owner.drivers.is_empty_for_test(),
                "rollback releases the driver"
            );
            // The writer is free again, and nothing the admission wrote committed.
            let mut next = calm_truth::db::sqlite::begin_immediate_tx(repo.db())
                .await
                .unwrap();
            let tag: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id = ?1")
                .bind(&op_id)
                .fetch_one(&mut **next)
                .await
                .unwrap();
            assert_eq!(tag, PhaseTag::Pending.as_str());
            assert!(
                sql::load(&mut **next, &binding.worker)
                    .await
                    .unwrap()
                    .is_none()
            );
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM workspace_executions")
                .fetch_one(&mut **next)
                .await
                .unwrap();
            assert_eq!(count, 0, "no intent row survives the rollback");
            next.rollback().await.unwrap();
            continue;
        }
        assert_eq!(advanced, TxCommittedAdvance::Advanced);
        let (claim, guard) = admitted.commit().await.unwrap();
        assert!(track_free(), "COMMIT releases the Track fence");
        assert_eq!(phase(&ops, &op_id).await, PhaseTag::TxCommitted);
        let stored = ops.get_operation(&op_id).await.unwrap().unwrap();
        assert_eq!(stored.tx_output.unwrap().data["execution_id"], id.as_str());
        assert_eq!(claim.record().execution_id, id.as_str());
        assert_eq!(
            sql::load(repo.pool(), &binding.worker)
                .await
                .unwrap()
                .unwrap()
                .record()
                .execution_id,
            id.as_str()
        );
        release::wait_spawned(&fixture.engine, &id).await.unwrap();
        owner
            .stop(
                repo.db(),
                Some(&fixture.engine),
                &binding.worker,
                &claim,
                StopReason::Finished,
            )
            .await
            .unwrap();
        wait_settled(&fixture.engine, &id, Duration::from_secs(10))
            .await
            .unwrap();
        drop(guard);
        owner
            .finish(
                repo.db(),
                Some(&fixture.engine),
                &binding.worker,
                &claim,
                None,
            )
            .await
            .unwrap();
    }
}
