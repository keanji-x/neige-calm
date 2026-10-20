use super::c3_execution::*;
use super::*;

#[tokio::test]
async fn c3_phase_writes_preserve_forge_record() {
    let boot = TestBoot::new().await;
    let input = c3_input(&boot, "phase-preserve", "true");
    let (id, op, output) = claimed_spawn_started_forge_op(&boot, "phase-preserve", input)
        .await
        .unwrap();
    let SpawnOutcome::Parked { observer, .. } = ForgeActionAdapter::new()
        .spawn_side_effect(&output, &op, &boot.spawn_ctx)
        .await
        .unwrap()
    else {
        panic!()
    };
    drop(observer);
    let before = c3_record(&c3_op(&boot, &id).await);
    boot.operation_repo
        .set_phase_and_tx_output(&op, Phase::SpawnStarted, &output)
        .await
        .unwrap();
    assert_eq!(c3_record(&c3_op(&boot, &id).await), before);
}

#[tokio::test]
async fn c3_completion_rechecks_record_and_retires_in_same_tx() {
    let boot = TestBoot::new().await;
    let id = c3_probe_ready(&boot, "completion-cas", "true").await;
    let stale = boot
        .operation_repo
        .claim_parked(&id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        ForgeActionAdapter::new()
            .recover_owned_parked(&stale, RecoveryMode::PreDeadlineProbe, &boot.spawn_ctx)
            .await
            .unwrap(),
        ParkedRecovery::LeaveParked
    ));
    let mut tx = boot.repo.pool().begin().await.unwrap();
    assert!(
        ForgeActionAdapter::new()
            .complete_owned_parked_tx(&mut tx, &stale)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let desired: String =
        sqlx::query_scalar("SELECT desired FROM workspace_executions WHERE execution_id=?")
            .bind(c3_record(&stale)["execution_id"].as_str().unwrap())
            .fetch_one(boot.repo.pool())
            .await
            .unwrap();
    assert_ne!(desired, "retire");
    use workspace_runtime::execution::{Desired, ExecutionId};
    let latest = c3_record(&c3_op(&boot, &id).await);
    let probe: ExecutionId =
        serde_json::from_value(latest["probes"]["verdict"]["execution_id"].clone()).unwrap();
    let engine = boot.spawn_ctx.execution.engine().unwrap();
    let mut tx = boot.repo.pool().begin().await.unwrap();
    let ticket = engine
        .request(&mut tx, &probe, Desired::Stop)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    engine.drive(ticket.committed());
    tokio::time::timeout(Duration::from_secs(10), engine.wait_settled(&probe))
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE operations SET lease_owner=NULL,lease_until_ms=NULL WHERE id=?")
        .bind(&id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    boot.runtime.sweep_parked().await.unwrap();
    assert!(matches!(
        c3_result(&boot, &id).await.outcome,
        OperationOutcome::Failed { .. }
    ));
    c3_all_retired(&boot, &id).await;
    assert_eq!(c3_count(&boot, &id).await, 2);
}

#[tokio::test]
async fn c3_stale_admission_never_removes_execution_evidence() {
    let boot = TestBoot::new().await;
    let input = c3_input(&boot, "stale-evidence", "printf newer-evidence");
    let (id, stale, old_output) = claimed_spawn_started_forge_op(&boot, "stale-evidence", input)
        .await
        .unwrap();
    let winner = boot
        .operation_repo
        .claim_operation_for_recovery(&id)
        .await
        .unwrap()
        .unwrap();
    let SpawnOutcome::Parked {
        deadline_ms,
        observer,
    } = ForgeActionAdapter::new()
        .spawn_side_effect(winner.tx_output.as_ref().unwrap(), &winner, &boot.spawn_ctx)
        .await
        .unwrap()
    else {
        panic!()
    };
    boot.operation_repo
        .set_parked(&winner, deadline_ms, Some("forge_execution"))
        .await
        .unwrap()
        .unwrap();
    tokio::spawn(observer);
    assert!(matches!(
        c3_result(&boot, &id).await.outcome,
        OperationOutcome::Succeeded { .. }
    ));
    let code = boot.temp_path("stale-evidence.code");
    let stdout = boot.temp_path("stale-evidence.stdout");
    let before = (fs::read(&code).unwrap(), fs::read(&stdout).unwrap());
    assert!(
        ForgeActionAdapter::new()
            .spawn_side_effect(&old_output, &stale, &boot.spawn_ctx)
            .await
            .is_err()
    );
    assert_eq!(fs::read(&code).unwrap(), before.0);
    assert_eq!(fs::read(&stdout).unwrap(), before.1);
    assert_eq!(c3_count(&boot, &id).await, 1);
}
