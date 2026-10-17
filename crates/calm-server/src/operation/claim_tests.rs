use super::*;

async fn claimed_operation() -> (crate::db::sqlite::SqlxRepo, SqlxOperationRepo, Operation) {
    let db = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let repo = SqlxOperationRepo::new(db.pool().clone());
    let id = repo
        .insert_operation(
            "claim-test",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(new_id()),
                payload_hash: "original-hash".into(),
            },
            json!({"input": "original"}),
        )
        .await
        .unwrap();
    let output = TxOutput::new("unknown", None, json!({"original": true}));
    sqlx::query("UPDATE operations SET phase='app_server_interact', phase_detail_json=?1, tx_output_json=?2 WHERE id=?3")
        .bind(serde_json::to_string(&json!({"kind":"mint_and_await", "thread_id":null})).unwrap())
        .bind(serde_json::to_string(&output).unwrap())
        .bind(&id)
        .execute(db.pool())
        .await
        .unwrap();
    let op = repo.claim_drive_batch(1).await.unwrap().pop().unwrap();
    (db, repo, op)
}

async fn expire(db: &crate::db::sqlite::SqlxRepo, op: &Operation) {
    sqlx::query("UPDATE operations SET lease_until_ms=?1 WHERE id=?2")
        .bind(now_ms() - 1)
        .bind(&op.id)
        .execute(db.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn expired_claim_cannot_set_phase_without_reclaim() {
    let (db, repo, op) = claimed_operation().await;
    expire(&db, &op).await;
    assert!(
        repo.set_phase(&op, Phase::Succeeded)
            .await
            .unwrap()
            .is_none(),
        "an unchanged owner token does not authorize settlement after its lease expires"
    );
    let stored = repo.get_operation(&op.id).await.unwrap().unwrap();
    assert_eq!(stored.phase.tag(), PhaseTag::AppServerInteract);
    assert_eq!(stored.lease_owner, op.lease_owner);
}

#[tokio::test]
async fn expired_claim_cannot_replace_output_without_reclaim() {
    let (db, repo, op) = claimed_operation().await;
    expire(&db, &op).await;
    let output = TxOutput::new("unknown", None, json!({"stale": true}));
    assert!(
        repo.set_phase_and_tx_output(&op, Phase::SpawnStarted, &output)
            .await
            .unwrap()
            .is_none(),
        "an expired claim cannot replace the committed domain checkpoint"
    );
    let stored = repo.get_operation(&op.id).await.unwrap().unwrap();
    assert_eq!(stored.phase.tag(), PhaseTag::AppServerInteract);
    assert_eq!(stored.tx_output.unwrap().result, json!({"original": true}));
}
