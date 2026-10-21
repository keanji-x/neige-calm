//! Observer ownership is process-local; exercise real sweep and observer entry points.
use super::c3_execution::*;
use super::*;
use calm_server::operation::forge_action_adapter::observer_count_for_test;
use calm_server::test_seams::{PausePoint, install_pause_for_test};

#[tokio::test]
async fn c3_observer_repeated_sweeps_stay_bounded() {
    let boot = TestBoot::new().await;
    let id = c3_running_probe(&boot, "bounded-observer").await;
    let claim = pause_before_claim(&id).await;
    sqlx::query("UPDATE operations SET parked_deadline_ms=0 WHERE id=?")
        .bind(&id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    for _ in 0..8 {
        // Force a successful sweep claim rather than a skipped leased row.
        sqlx::query("UPDATE operations SET lease_owner=NULL,lease_until_ms=NULL WHERE id=?")
            .bind(&id)
            .execute(boot.repo.pool())
            .await
            .unwrap();
        boot.runtime.sweep_parked().await.unwrap();
        tokio::time::sleep(Duration::from_millis(70)).await;
        assert_eq!(
            observer_count_for_test(&id),
            1,
            "sweeps must reuse the observer"
        );
        assert_eq!(c3_op(&boot, &id).await.phase, Phase::Parked);
    }
    fs::write(boot.temp_path("probe-finish"), "").unwrap();
    claim.release.notify_one();
    assert!(matches!(
        result_without_sweep(&boot, &id).await.outcome,
        OperationOutcome::Succeeded { .. }
    ));
    c3_wait(async || observer_count_for_test(&id) == 0).await;
    c3_all_retired(&boot, &id).await;
}

#[tokio::test]
async fn c3_observer_yield_restart_has_no_lost_wakeup() {
    let boot = TestBoot::new().await;
    let (id, _, observer) = c3_held(&boot, "observer-exit-race").await;
    sqlx::query("UPDATE operations SET parked_deadline_ms=0 WHERE id=?")
        .bind(&id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let exit = PausePoint::default();
    install_pause_for_test("forge-observer-exit", &id, exit.clone());
    let observer = tokio::spawn(observer);
    tokio::time::timeout(Duration::from_secs(10), exit.entered.notified())
        .await
        .unwrap();
    boot.runtime.sweep_parked().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        observer_count_for_test(&id),
        1,
        "restart must not spawn during exit"
    );
    assert_eq!(c3_op(&boot, &id).await.phase, Phase::Parked);
    exit.release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), observer)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result_without_sweep(&boot, &id).await.outcome,
        OperationOutcome::Failed { last_error_class: Some(ref class), .. } if class == "gate-infra"));
    c3_wait(async || observer_count_for_test(&id) == 0).await;
    c3_all_retired(&boot, &id).await;
}

#[tokio::test]
async fn c3_observer_unknown_executions_stay_bounded() {
    for probe in [false, true] {
        let mut boot = TestBoot::new().await;
        let id = if probe {
            c3_running_probe(&boot, "unknown-observer-probe").await
        } else {
            let input = c3_input(
                &boot,
                "unknown-observer-action",
                &format!("touch '{}'; sleep 30", boot.temp_path("started").display(),),
            );
            let id = boot
                .runtime
                .submit(FORGE_ACTION_KIND, op_key("unknown-observer-action"), input)
                .await
                .unwrap();
            wait_for_file(&boot.temp_path("started")).await;
            id
        };
        let claim = pause_before_claim(&id).await;
        boot._execution.restart().await;
        sqlx::query("UPDATE operations SET parked_deadline_ms=0 WHERE id=?")
            .bind(&id)
            .execute(boot.repo.pool())
            .await
            .unwrap();
        let plan = boot.runtime.recover_on_boot().await.unwrap();
        boot.runtime.apply_recovery(plan).await.unwrap();
        for _ in 0..8 {
            sqlx::query("UPDATE operations SET lease_owner=NULL,lease_until_ms=NULL WHERE id=?")
                .bind(&id)
                .execute(boot.repo.pool())
                .await
                .unwrap();
            boot.runtime.sweep_parked().await.unwrap();
            tokio::time::sleep(Duration::from_millis(70)).await;
            assert_eq!(
                observer_count_for_test(&id),
                1,
                "Unknown must not accumulate observers"
            );
            assert_eq!(c3_op(&boot, &id).await.phase, Phase::Parked);
        }
        claim.release.notify_one();
        // The accepted same-boot Unknown limit deliberately prevents completion.
        assert!(
            boot.operation_repo
                .operation_result(&id)
                .await
                .unwrap()
                .is_none()
        );
    }
}

async fn pause_before_claim(id: &str) -> PausePoint {
    let claim = PausePoint::default();
    install_pause_for_test("forge-observer-before-claim", id, claim.clone());
    tokio::time::timeout(Duration::from_secs(10), claim.entered.notified())
        .await
        .unwrap();
    claim
}

pub(super) async fn result_without_sweep(boot: &TestBoot, id: &str) -> OperationResult {
    // OperationRuntime::wait enforces deadlines itself; it would hide a lost restart.
    c3_wait(async || {
        boot.operation_repo
            .operation_result(id)
            .await
            .unwrap()
            .is_some()
    })
    .await;
    boot.operation_repo
        .operation_result(id)
        .await
        .unwrap()
        .unwrap()
}

async fn held_observer(boot: &TestBoot, name: &str, probe: bool) -> (String, PausePoint) {
    let (id, observer) = if probe {
        // Stage a settled action without starting its observer.
        (
            c3_probe_ready(boot, name, "echo probe-ran > probe-ran").await,
            None,
        )
    } else {
        let (id, _, observer) = c3_held(boot, name).await;
        (id, Some(observer))
    };
    let pause = PausePoint::default();
    install_pause_for_test("forge-observer-before-read", &id, pause.clone());
    if let Some(observer) = observer {
        tokio::spawn(observer);
    } else {
        let op = boot
            .operation_repo
            .claim_parked(&id)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            ForgeActionAdapter::new()
                .recover_owned_parked(&op, RecoveryMode::PreDeadlineProbe, &boot.spawn_ctx)
                .await
                .unwrap(),
            calm_server::operation::ParkedRecovery::LeaveParked
        ));
        // The production admission commits Held and attaches a fresh observer with
        // no pending restart that could mask an erroneous exit on the injected fault.
        let record = c3_record(&c3_op(boot, &id).await);
        c3_rewrite(boot, &id, &record).await;
    }
    tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
        .await
        .unwrap();
    let record = c3_record(&c3_op(boot, &id).await);
    let release = if probe {
        &record["probes"]["verdict"]["release"]
    } else {
        &record["release"]
    };
    assert_eq!(release, "Held");
    (id, pause)
}

async fn repository_fault_recovers(method: &str, probe: bool) {
    let boot = TestBoot::new().await;
    let (id, pause) = held_observer(&boot, method, probe).await;
    let fired = calm_server::test_seams::install_operation_fault_for_test(method, &id);
    pause.release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), fired.notified())
        .await
        .unwrap();
    // No boot, sweep, runtime.wait, or clock/deadline change can repair this observer.
    assert!(matches!(
        result_without_sweep(&boot, &id).await.outcome,
        OperationOutcome::Succeeded { .. }
    ));
    let record = c3_record(&c3_op(&boot, &id).await);
    let release = if probe {
        &record["probes"]["verdict"]["release"]
    } else {
        &record["release"]
    };
    assert!(release.get("Released").is_some());
    assert!(
        boot.temp_path(if probe { "probe-ran" } else { "ran" })
            .exists()
    );
    c3_all_retired(&boot, &id).await;
}

#[tokio::test]
async fn c3_observer_get_operation_failure_retries_held_action() {
    repository_fault_recovers("get_operation", false).await;
}
#[tokio::test]
async fn c3_observer_get_operation_failure_retries_held_probe() {
    repository_fault_recovers("get_operation", true).await;
}
#[tokio::test]
async fn c3_observer_claim_parked_failure_retries_held_action() {
    repository_fault_recovers("claim_parked", false).await;
}
#[tokio::test]
async fn c3_observer_claim_parked_failure_retries_held_probe() {
    repository_fault_recovers("claim_parked", true).await;
}
#[tokio::test]
async fn c3_observer_pre_deadline_sweep_reattaches_absent_observer() {
    let boot = TestBoot::new().await;
    let (id, _, observer) = c3_held(&boot, "sweep-reattach").await;
    drop(observer);
    assert_eq!(observer_count_for_test(&id), 0);
    assert!(c3_op(&boot, &id).await.parked_deadline_ms.unwrap() > now_ms());
    boot.runtime.sweep_parked().await.unwrap();
    assert!(matches!(
        result_without_sweep(&boot, &id).await.outcome,
        OperationOutcome::Succeeded { .. }
    ));
    assert!(
        c3_record(&c3_op(&boot, &id).await)["release"]
            .get("Released")
            .is_some()
    );
    assert!(boot.temp_path("ran").exists());
    c3_all_retired(&boot, &id).await;
}

#[tokio::test]
async fn c4_recovery_tick_redrives_forge_retire_after_store_error() {
    let boot = TestBoot::new().await;
    let flag = boot.temp_path("finish-action");
    let input = c3_input(
        &boot,
        "tick-retire",
        &format!("until [ -f '{}' ]; do sleep .01; done", flag.display()),
    );
    let id = boot
        .runtime
        .submit(FORGE_ACTION_KIND, op_key("tick-retire"), input)
        .await
        .unwrap();
    c3_wait(async || {
        c3_record(&c3_op(&boot, &id).await)["release"]
            .get("Released")
            .is_some()
    })
    .await;
    let pause = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(
        "forge-observer-before-read",
        &id,
        pause.clone(),
    );
    tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
        .await
        .unwrap();
    std::fs::write(flag, "").unwrap();
    c3_wait(async || sqlx::query_scalar::<_, bool>("SELECT settlement_json IS NOT NULL FROM workspace_executions WHERE json_extract(intent_json,'$.owner')=?").bind(&id).fetch_one(boot.repo.pool()).await.unwrap()).await;
    boot._execution.faults.arm("finish");
    boot.runtime.sweep_parked().await.unwrap();
    assert!(matches!(
        c3_result(&boot, &id).await.outcome,
        OperationOutcome::Succeeded { .. }
    ));
    c3_all_retired(&boot, &id).await;
    pause.release.notify_one();
    assert_eq!(
        boot._execution
            .faults
            .failures
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    let execution =
        serde_json::from_value(c3_record(&c3_op(&boot, &id).await)["execution_id"].clone())
            .unwrap();
    assert!(matches!(
        boot.spawn_ctx
            .execution
            .engine()
            .unwrap()
            .output(&execution, None)
            .await
            .unwrap(),
        workspace_runtime::execution::ExecutionReply::Retired(_)
    ));
}
