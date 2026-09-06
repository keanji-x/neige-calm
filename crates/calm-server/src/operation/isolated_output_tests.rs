use super::*;

#[tokio::test]
async fn isolated_driver_phase_preserves_newer_private_checkpoint() {
    preserves_checkpoint(false).await;
}

#[tokio::test]
async fn isolated_driver_compensation_preserves_newer_private_checkpoint() {
    preserves_checkpoint(true).await;
}

async fn preserves_checkpoint(compensate: bool) {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let repo = SqlxOperationRepo::new(sqlx_repo.pool().clone());
    let op_id = repo
        .insert_operation(
            "codex-isolated-worker",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some("attempt-a".into()),
                payload_hash: "fixture".into(),
            },
            json!({"task_id":"attempt-a"}),
        )
        .await
        .unwrap();
    let mut stale = TxOutput::new("card", Some("worker-card".into()), json!({}));
    stale.data = json!({"isolated_execution":{"version":"isolated-execution-v1","admission":"open","provider":"before-ack"}});
    let mut current = stale.clone();
    current.data["isolated_execution"] =
        json!({"version":"isolated-execution-v1","admission":"closed","provider":"quiesced"});
    current.data["launch_admission"] = json!({"task_id":"attempt-a"});
    sqlx::query("UPDATE operations SET phase='spawn_started',lease_owner='owner',tx_output_json=?1 WHERE id=?2")
        .bind(serde_json::to_string(&current).unwrap()).bind(&op_id)
        .execute(sqlx_repo.pool()).await.unwrap();
    let op = repo.get_operation(&op_id).await.unwrap().unwrap();
    let after = if compensate {
        repo.set_compensating(
            &op,
            &CompensationStateVersioned {
                version: 1,
                from_phase: PhaseTag::SpawnStarted,
                reason: "lost reply".into(),
                steps: vec![],
            },
            &stale,
        )
        .await
        .unwrap()
        .unwrap()
    } else {
        repo.set_phase_and_tx_output(&op, Phase::SpawnSucceeded, &stale)
            .await
            .unwrap()
            .unwrap()
    };
    let stored = after.tx_output.unwrap();
    assert_eq!(
        stored.data["isolated_execution"], current.data["isolated_execution"],
        "outer driver must preserve separately committed observations and stop evidence"
    );
    assert_eq!(
        stored.data["launch_admission"], current.data["launch_admission"],
        "actual final admission cannot be erased by an older output clone"
    );
}
