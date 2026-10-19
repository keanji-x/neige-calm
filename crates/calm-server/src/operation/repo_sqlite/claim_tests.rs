use super::super::{AppServerInteractKind, Tx};
use super::*;

async fn claimed_operation() -> (crate::db::sqlite::SqlxRepo, SqlxOperationRepo, Operation) {
    let db = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let repo = SqlxOperationRepo::new(db.db().clone());
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
        repo.set_phase(&op, Phase::Succeeded, None)
            .await
            .unwrap()
            .applied()
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

fn compensation() -> CompensationStateVersioned {
    CompensationStateVersioned {
        version: 1,
        from_phase: PhaseTag::AppServerInteract,
        reason: "test failure".into(),
        steps: vec![],
    }
}

fn interaction(thread: &str) -> AppServerInteractKind {
    AppServerInteractKind::MintAndAwait {
        thread_id: Some(thread.into()),
    }
}

#[tokio::test]
async fn expired_claim_rejects_compensation_and_failure_writers() {
    let (db, repo, op) = claimed_operation().await;
    expire(&db, &op).await;
    assert!(
        repo.set_compensating(&op, &compensation(), None)
            .await
            .unwrap()
            .applied()
            .is_none()
    );
    assert!(
        repo.update_compensation_state(&op, &compensation())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo.mark_failed(&op, "stale".into(), op.phase.tag(), None, None)
            .await
            .unwrap()
            .applied()
            .is_none()
    );
    assert!(
        repo.mark_stuck(&op, "stale".into(), op.phase.tag(), None)
            .await
            .unwrap()
            .applied()
            .is_none()
    );
    let stored = repo.get_operation(&op.id).await.unwrap().unwrap();
    assert_eq!(stored.phase.tag(), PhaseTag::AppServerInteract);
    assert!(stored.compensation_state.is_none());
    assert!(stored.last_error.is_none());
}

#[tokio::test]
async fn expired_claim_rejects_provider_checkpoint() {
    let (db, repo, op) = claimed_operation().await;
    expire(&db, &op).await;
    let mut tx = begin_immediate_tx(db.db()).await.unwrap();
    assert!(
        checkpoint_app_server_interact_tx(
            &mut tx,
            &op,
            interaction("stale"),
            op.tx_output.as_ref().unwrap(),
            &TxOutput::new("unknown", None, json!({"stale": true})),
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    let stored = repo.get_operation(&op.id).await.unwrap().unwrap();
    assert_eq!(stored.tx_output.unwrap().result, json!({"original": true}));
    assert_eq!(stored.phase, op.phase);
}

#[tokio::test]
async fn expired_claim_rejects_spawn_artifacts_and_parking() {
    let (db, repo, op) = claimed_operation().await;
    sqlx::query("UPDATE operations SET phase='spawn_started', phase_detail_json=NULL WHERE id=?1")
        .bind(&op.id)
        .execute(db.pool())
        .await
        .unwrap();
    let op = repo.get_operation(&op.id).await.unwrap().unwrap();
    let artifacts = SpawnArtifacts {
        pid: 1,
        pgid: 1,
        start_time: 1,
        boot_id: "fixture".into(),
        log_path: None,
        extra: Value::Null,
    };
    repo.record_spawn_artifacts(&op, &artifacts).await.unwrap();
    expire(&db, &op).await;
    let mut replacement = artifacts.clone();
    replacement.pid = 2;
    assert!(
        repo.record_spawn_artifacts(&op, &replacement)
            .await
            .is_err()
    );
    assert!(
        repo.set_parked(&op, now_ms() + 10_000, None)
            .await
            .unwrap()
            .is_none()
    );
    let stored = repo.get_operation(&op.id).await.unwrap().unwrap();
    assert_eq!(stored.phase, Phase::SpawnStarted);
    assert_eq!(stored.spawn_artifacts, Some(artifacts));
}

#[tokio::test]
async fn original_identity_phase_owner_and_completion_are_claim_fences() {
    for changed in [
        "operation_key='another-key'",
        "kind='another-kind'",
        "idempotency_key='another-idempotency-key'",
        "payload_hash='another-hash'",
        "payload_json='{}'",
        "phase='spawn_started'",
        "lease_owner='another-owner'",
        "completed_at_ms=1",
    ] {
        let (db, repo, op) = claimed_operation().await;
        sqlx::query(&format!("UPDATE operations SET {changed} WHERE id=?1"))
            .bind(&op.id)
            .execute(db.pool())
            .await
            .unwrap();
        assert!(
            repo.set_phase(&op, Phase::Succeeded, None)
                .await
                .unwrap()
                .applied()
                .is_none(),
            "{changed}"
        );
        assert!(
            repo.set_phase_and_tx_output(&op, Phase::SpawnStarted, op.tx_output.as_ref().unwrap())
                .await
                .unwrap()
                .is_none(),
            "{changed}"
        );
        let mut tx = begin_immediate_tx(db.db()).await.unwrap();
        assert!(
            checkpoint_app_server_interact_tx(
                &mut tx,
                &op,
                interaction("stale"),
                op.tx_output.as_ref().unwrap(),
                op.tx_output.as_ref().unwrap()
            )
            .await
            .is_err(),
            "{changed}"
        );
        tx.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn sequential_provider_checkpoints_use_explicit_previous_output() {
    let (db, repo, op) = claimed_operation().await;
    let original = op.tx_output.as_ref().unwrap();
    let first = TxOutput::new("unknown", None, json!({"checkpoint": 1}));
    let second = TxOutput::new("unknown", None, json!({"checkpoint": 2}));
    for (expected, output, thread) in [(original, &first, "first"), (&first, &second, "second")] {
        let mut tx = begin_immediate_tx(db.db()).await.unwrap();
        checkpoint_app_server_interact_tx(&mut tx, &op, interaction(thread), expected, output)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    let stored = repo.get_operation(&op.id).await.unwrap().unwrap();
    assert_eq!(
        stored.phase,
        Phase::AppServerInteract {
            kind: interaction("second")
        }
    );
    assert_eq!(stored.tx_output.unwrap().result, second.result);
    // The driver retains its original phase detail; it accepts the callback's
    // own final committed output without freezing a whole Operation snapshot.
    let next = repo
        .set_phase_and_tx_output(&op, Phase::SpawnStarted, &second)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.phase, Phase::SpawnStarted);
    assert_eq!(next.tx_output.unwrap().result, second.result);
}

#[tokio::test]
async fn stale_same_claim_output_cannot_overwrite_a_committed_checkpoint() {
    let (db, repo, op) = claimed_operation().await;
    let original = op.tx_output.as_ref().unwrap();
    let first = TxOutput::new("unknown", None, json!({"checkpoint": 1}));
    let stale = TxOutput::new("unknown", None, json!({"checkpoint": "stale"}));
    let mut tx = begin_immediate_tx(db.db()).await.unwrap();
    checkpoint_app_server_interact_tx(&mut tx, &op, interaction("first"), original, &first)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = begin_immediate_tx(db.db()).await.unwrap();
    assert!(
        checkpoint_app_server_interact_tx(&mut tx, &op, interaction("stale"), original, &stale)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    assert!(
        repo.set_phase_and_tx_output(&op, Phase::SpawnStarted, &stale)
            .await
            .unwrap()
            .is_none()
    );
    let stored = repo.get_operation(&op.id).await.unwrap().unwrap();
    assert_eq!(
        stored.phase,
        Phase::AppServerInteract {
            kind: interaction("first")
        }
    );
    assert_eq!(stored.tx_output.unwrap().result, first.result);
    assert_eq!(stored.lease_owner, op.lease_owner);
}

#[tokio::test]
async fn phase_only_settlement_preserves_a_provider_checkpoint() {
    let (db, repo, op) = claimed_operation().await;
    let output = TxOutput::new("unknown", None, json!({"checkpoint": "owned"}));
    let mut tx = begin_immediate_tx(db.db()).await.unwrap();
    checkpoint_app_server_interact_tx(
        &mut tx,
        &op,
        interaction("owned"),
        op.tx_output.as_ref().unwrap(),
        &output,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let settled = repo
        .set_phase(&op, Phase::Succeeded, None)
        .await
        .unwrap()
        .applied()
        .unwrap();
    assert_eq!(settled.tx_output.unwrap().result, output.result);
}

#[tokio::test]
async fn failed_later_checkpoint_can_compensate_without_replacing_committed_output() {
    let (db, repo, op) = claimed_operation().await;
    let first = TxOutput::new("unknown", None, json!({"checkpoint": 1}));
    let mut tx = begin_immediate_tx(db.db()).await.unwrap();
    checkpoint_app_server_interact_tx(
        &mut tx,
        &op,
        interaction("first"),
        op.tx_output.as_ref().unwrap(),
        &first,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // A native effect may already have happened before its later checkpoint
    // fails. The failed writer must still enter compensation using the last
    // committed output, while its plan retains the actual cleanup arguments.
    let mut later = first.clone();
    later.result = json!({"checkpoint": 2});
    later.data = json!({"cleanup_ticket": "already-issued-effect"});
    sqlx::query(
        "CREATE TRIGGER reject_later_checkpoint \
        BEFORE UPDATE OF tx_output_json ON operations \
        WHEN json_extract(NEW.tx_output_json,'$.result.checkpoint')=2 \
        BEGIN SELECT RAISE(ABORT,'forced later checkpoint failure'); END",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let mut tx = begin_immediate_tx(db.db()).await.unwrap();
    let error =
        checkpoint_app_server_interact_tx(&mut tx, &op, interaction("later"), &first, &later)
            .await
            .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("forced later checkpoint failure"),
        "{error}"
    );
    tx.rollback().await.unwrap();
    let mut state = compensation();
    state.steps.push(super::super::CompensationStep::new(
        "cleanup_issued_effect",
        json!({"ticket": later.data["cleanup_ticket"]}),
    ));
    let compensating = repo
        .set_compensating(&op, &state, None)
        .await
        .unwrap()
        .applied()
        .expect("failed checkpoint must not strand a still-owned operation before compensation");
    assert_eq!(compensating.phase, Phase::Compensating);
    assert_eq!(compensating.tx_output.unwrap().result, first.result);
    assert_eq!(
        compensating.compensation_state.unwrap()["steps"][0]["args"]["ticket"],
        "already-issued-effect"
    );
}

#[tokio::test]
async fn settlement_retains_released_terminal_launch_owner_bytes() {
    let (db, repo, op) = claimed_operation().await;
    let state = json!({"version":1,"state":"requested","terminal_id":"original-terminal","supervisor_sock":"/original.sock"});
    sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data',json(?1)) WHERE id=?2")
        .bind(serde_json::to_string(&json!({"terminal_launch":state})).unwrap())
        .bind(&op.id).execute(db.pool()).await.unwrap();
    let op = repo.get_operation(&op.id).await.unwrap().unwrap();
    let settled = repo
        .set_compensating(&op, &compensation(), None)
        .await
        .unwrap()
        .applied()
        .unwrap();
    assert_eq!(settled.tx_output.unwrap().data["terminal_launch"], state);
}

struct TxOnlyClaimAdapter;

#[async_trait]
impl super::super::TxOnlyAdapter for TxOnlyClaimAdapter {
    fn kind(&self) -> &'static str {
        "claim-test-tx-only"
    }
    async fn validate(&self, _input: &Value) -> Result<()> {
        Ok(())
    }
    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        _input: &Value,
        op: &Operation,
    ) -> Result<TxOutput> {
        sqlx::query("INSERT INTO claim_effects (op_id) VALUES (?1)")
            .bind(&op.id)
            .execute(&mut **tx)
            .await?;
        Ok(TxOutput::new("unknown", None, json!({"created": true})))
    }
}

#[tokio::test]
async fn keyed_commit_settles_its_own_unleased_insert_and_replays_once() {
    use super::super::tx_only::{KeyedCommit, commit_tx_only};
    let db = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("CREATE TABLE claim_effects (op_id TEXT PRIMARY KEY)")
        .execute(db.pool())
        .await
        .unwrap();
    let key = OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(new_id()),
        payload_hash: "same".into(),
    };
    let first = commit_tx_only(
        db.db(),
        &TxOnlyClaimAdapter,
        key.clone(),
        json!({"input":"accepted"}),
    )
    .await
    .unwrap();
    let KeyedCommit::Committed { result, .. } = first else {
        panic!("first request must commit")
    };
    let second = commit_tx_only(
        db.db(),
        &TxOnlyClaimAdapter,
        key,
        json!({"input":"accepted"}),
    )
    .await
    .unwrap();
    assert!(matches!(second, KeyedCommit::Replay(id) if id == result.op_id));
    let stored = SqlxOperationRepo::new(db.db().clone())
        .get_operation(&result.op_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.phase, Phase::Succeeded);
    assert!(stored.lease_owner.is_none());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM claim_effects")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn expired_claim_cannot_use_the_inserted_transaction_branch() {
    let (db, repo, op) = claimed_operation().await;
    expire(&db, &op).await;
    let mut tx = begin_immediate_tx(db.db()).await.unwrap();
    assert!(
        write_phase_and_tx_output(
            &mut tx,
            PhaseWrite::Inserted(&op),
            &Phase::Succeeded,
            op.tx_output.as_ref().unwrap()
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        repo.get_operation(&op.id).await.unwrap().unwrap().phase,
        op.phase
    );
}

#[tokio::test]
async fn expired_claim_cannot_enter_prepare_callback() {
    let (db, repo, op) = claimed_operation().await;
    expire(&db, &op).await;
    // This table intentionally does not exist. Entering the real prepare_tx
    // callback would error, even if a later settlement rolled it back.
    assert!(
        repo.prepare_tx_and_advance(&op, &TxOnlyClaimAdapter, None)
            .await
            .unwrap()
            .committed()
            .is_none()
    );
    let stored = repo.get_operation(&op.id).await.unwrap().unwrap();
    assert_eq!(stored.phase, op.phase);
    assert_eq!(stored.tx_output.unwrap().result, json!({"original":true}));
}

struct PoisonPreparedEvidence;
#[async_trait]
impl crate::operation::PreparedTx for PoisonPreparedEvidence {
    async fn apply(
        self: Box<Self>,
        _: &mut Tx<'_>,
        _: &Operation,
    ) -> Result<crate::operation::PreparedTxResult> {
        panic!("expired claim consumed external evidence");
    }
}

#[tokio::test]
async fn expired_claim_cannot_consume_prepared_evidence() {
    let (db, repo, op) = claimed_operation().await;
    expire(&db, &op).await;
    let result = repo
        .prepare_tx_and_advance(
            &op,
            &TxOnlyClaimAdapter,
            Some(Box::new(PoisonPreparedEvidence)),
        )
        .await
        .unwrap();
    assert!(matches!(
        result,
        crate::operation::PrepareAdvance::ClaimLost
    ));
    assert_eq!(
        repo.get_operation(&op.id).await.unwrap().unwrap().phase,
        op.phase
    );
}

#[tokio::test]
async fn owned_key_retains_terminal_writes() {
    for record in [json!({"version": 1}), json!(null), json!("malformed")] {
        let (db, repo, stale_snapshot) = claimed_operation().await;
        repo.set_phase(&stale_snapshot, Phase::SpawnStarted, None)
            .await
            .unwrap()
            .applied()
            .unwrap();
        let stale_snapshot = repo.claim_drive_batch(1).await.unwrap().pop().unwrap();
        // The claim snapshot predates the owner's checkpoint: read the transactional row.
        sqlx::query("UPDATE operations SET \
            tx_output_json=json_set(tx_output_json,'$.data',json_object('terminal_launch',json(?1))) \
            WHERE id=?2")
            .bind(record.to_string()).bind(&stale_snapshot.id).execute(db.pool()).await.unwrap();
        let before: String = sqlx::query_scalar("SELECT \
            json_object('phase',phase,'detail',phase_detail_json,'output',tx_output_json,'owner',lease_owner,'until',lease_until_ms,'updated',updated_at_ms,'completed',completed_at_ms,'compensation',compensation_state) \
            FROM operations WHERE id=?1")
            .bind(&stale_snapshot.id).fetch_one(db.pool()).await.unwrap();
        assert_eq!(
            repo.get_operation(&stale_snapshot.id)
                .await
                .unwrap()
                .unwrap()
                .tx_output
                .unwrap()
                .data["terminal_launch"],
            record
        );
        let key = Some("terminal_launch");
        let outcomes = [
            repo.set_phase(&stale_snapshot, Phase::Succeeded, key)
                .await
                .unwrap(),
            repo.set_compensating(&stale_snapshot, &compensation(), key)
                .await
                .unwrap(),
        ];
        assert!(
            outcomes
                .iter()
                .all(|v| matches!(v, super::super::LeaveWrite::Retained))
        );
        assert!(matches!(
            repo.mark_failed(
                &stale_snapshot,
                "failure".into(),
                PhaseTag::SpawnStarted,
                None,
                key
            )
            .await
            .unwrap(),
            super::super::LeaveWrite::Retained
        ));
        assert!(matches!(
            repo.mark_stuck(&stale_snapshot, "stuck".into(), PhaseTag::SpawnStarted, key)
                .await
                .unwrap(),
            super::super::LeaveWrite::Retained
        ));
        let after: String = sqlx::query_scalar("SELECT \
            json_object('phase',phase,'detail',phase_detail_json,'output',tx_output_json,'owner',lease_owner,'until',lease_until_ms,'updated',updated_at_ms,'completed',completed_at_ms,'compensation',compensation_state) \
            FROM operations WHERE id=?1")
            .bind(&stale_snapshot.id).fetch_one(db.pool()).await.unwrap();
        assert_eq!(after, before);
        let stored = repo
            .get_operation(&stale_snapshot.id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            stored
                .last_error
                .unwrap()
                .starts_with("owned execution retained:")
        );
        assert!(repo.claim_drive_batch(1).await.unwrap().is_empty());
    }
}
