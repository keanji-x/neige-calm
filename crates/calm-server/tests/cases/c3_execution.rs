use super::*;

pub(super) fn c3_record(op: &Operation) -> Value {
    op.tx_output.as_ref().unwrap().data["forge_execution"].clone()
}
pub(super) async fn c3_op(boot: &TestBoot, id: &str) -> Operation {
    boot.operation_repo
        .get_operation(id)
        .await
        .unwrap()
        .unwrap()
}
pub(super) async fn c3_wait(mut predicate: impl AsyncFnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !predicate().await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("C3 invariant observed");
}
pub(super) async fn c3_rewrite(boot: &TestBoot, id: &str, record: &Value) {
    sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.forge_execution',json(?2)), \
        lease_owner=NULL,lease_until_ms=NULL WHERE id=?1")
        .bind(id).bind(record.to_string()).execute(boot.repo.pool()).await.unwrap();
}
pub(super) async fn c3_count(boot: &TestBoot, id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM workspace_executions WHERE json_extract(intent_json,'$.owner')=?1",
    )
    .bind(id)
    .fetch_one(boot.repo.pool())
    .await
    .unwrap()
}
pub(super) async fn c3_result(boot: &TestBoot, id: &str) -> OperationResult {
    tokio::time::timeout(Duration::from_secs(10), boot.runtime.wait(&id.to_owned()))
        .await
        .unwrap_or_else(|_| panic!("C3 completion timeout: {:?}", id))
        .unwrap()
}
pub(super) fn c3_merge_input(boot: &TestBoot, name: &str, script: &str) -> Value {
    payload(
        boot,
        name,
        vec!["/bin/sh".into(), "-c".into(), script.into()],
        boot.temp_path(name),
    )
}
pub(super) fn c3_input(boot: &TestBoot, name: &str, script: &str) -> Value {
    let mut input = c3_merge_input(boot, name, script);
    input["event_spec"] = Value::Null;
    input["subject"] = Value::Null;
    input
}
pub(super) async fn c3_held(
    boot: &TestBoot,
    name: &str,
) -> (String, Operation, calm_server::operation::ParkedObserver) {
    let input = c3_input(boot, name, "echo ran > ran");
    let (id, op, output) = claimed_spawn_started_forge_op(boot, name, input)
        .await
        .unwrap();
    let SpawnOutcome::Parked {
        deadline_ms,
        observer,
    } = ForgeActionAdapter::new()
        .spawn_side_effect(&output, &op, &boot.spawn_ctx)
        .await
        .unwrap()
    else {
        panic!("owned park")
    };
    boot.operation_repo
        .set_parked(&op, deadline_ms, Some("forge_execution"))
        .await
        .unwrap()
        .unwrap();
    (id, op, observer)
}
#[tokio::test]
async fn c3_action_has_one_engine_construction() {
    let boot = TestBoot::new().await;
    let mut input = c3_merge_input(&boot, "one-construction", "exit 7");
    input["probe"] = json!({"probe_argv":["/bin/true"],"output_probe_argv":["/bin/sh","-c","echo '{\"oid\":\"abcdef\",\"headRefOid\":\"123456\"}'"]});
    let id = boot
        .runtime
        .submit(FORGE_ACTION_KIND, op_key("one-construction"), input)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(12), boot.runtime.wait(&id)).await;
    if result.is_err() {
        let op = c3_op(&boot, &id).await;
        let rows: Vec<(String, String, Option<String>)> =
            sqlx::query_as("SELECT execution_id,desired,settlement_json FROM workspace_executions")
                .fetch_all(boot.repo.pool())
                .await
                .unwrap();
        panic!("op={op:?} executions={rows:?}");
    }
    assert!(matches!(
        result.unwrap().unwrap().outcome,
        OperationOutcome::Succeeded { .. }
    ));
    assert_eq!(c3_count(&boot, &id).await, 3);
    let op = c3_op(&boot, &id).await;
    assert!(op.spawn_artifacts.is_none());
    let rows: Vec<String> = sqlx::query_scalar("SELECT intent_json FROM workspace_executions")
        .fetch_all(boot.repo.pool())
        .await
        .unwrap();
    for raw in rows {
        let intent: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(intent["consumer"], "forge");
        assert_eq!(intent["program"]["io_mode"], "Pipe");
        assert_eq!(intent["program"]["program"], "/bin/sh");
    }
}
#[tokio::test]
async fn c3_action_releases_only_after_park() {
    let boot = TestBoot::new().await;
    let input = c3_input(&boot, "park-order", "echo ran > ran");
    let (id, op, output) = claimed_spawn_started_forge_op(&boot, "park-order", input)
        .await
        .unwrap();
    let SpawnOutcome::Parked {
        deadline_ms,
        observer,
    } = ForgeActionAdapter::new()
        .spawn_side_effect(&output, &op, &boot.spawn_ctx)
        .await
        .unwrap()
    else {
        panic!()
    };
    c3_wait(async || c3_count(&boot, &id).await == 1).await;
    assert_absent_briefly(&boot.temp_path("ran")).await;
    assert_eq!(c3_record(&c3_op(&boot, &id).await)["release"], "Held");
    boot.operation_repo
        .set_parked(&op, deadline_ms, Some("forge_execution"))
        .await
        .unwrap()
        .unwrap();
    tokio::spawn(observer);
    assert!(matches!(
        c3_result(&boot, &id).await.outcome,
        OperationOutcome::Succeeded { .. }
    ));
    assert!(boot.temp_path("ran").exists());
}
#[tokio::test]
async fn c3_action_refuses_unavailable_executor() {
    let boot = TestBoot::new().await;
    let input = c3_input(&boot, "unavailable", "echo ran > ran");
    let (id, op, output) = claimed_spawn_started_forge_op(&boot, "unavailable", input)
        .await
        .unwrap();
    let mut ctx = boot.spawn_ctx.clone();
    ctx.execution = Default::default();
    assert!(matches!(
        ForgeActionAdapter::new()
            .spawn_side_effect(&output, &op, &ctx)
            .await,
        Err(CalmError::ExecutionUnavailable)
    ));
    assert_eq!(c3_count(&boot, &id).await, 0);
    assert!(!boot.temp_path("ran").exists());
}
#[tokio::test]
async fn c3_boot_held_stops_without_token() {
    let boot = TestBoot::new().await;
    let (id, _, observer) = c3_held(&boot, "boot-held").await;
    drop(observer);
    let plan = boot.runtime.recover_on_boot().await.unwrap();
    boot.runtime.apply_recovery(plan).await.unwrap();
    let result = c3_result(&boot, &id).await;
    assert!(
        matches!(result.outcome,OperationOutcome::Failed { last_error_class:Some(ref class),.. } if class=="gate-infra")
    );
    assert!(!boot.temp_path("ran").exists());
    assert_eq!(c3_count(&boot, &id).await, 1);
    assert_eq!(
        c3_record(&c3_op(&boot, &id).await)["decision"],
        json!({"Stopping":{"reason":"KernelRestarted"}})
    );
}
#[tokio::test]
async fn c3_boot_releasing_never_repeats_token() {
    let boot = TestBoot::new().await;
    let (id, _, observer) = c3_held(&boot, "boot-releasing").await;
    drop(observer);
    let mut record = c3_record(&c3_op(&boot, &id).await);
    record["release"] = json!({"Releasing":{"authorized_at_ms":now_ms()}});
    c3_rewrite(&boot, &id, &record).await;
    let plan = boot.runtime.recover_on_boot().await.unwrap();
    boot.runtime.apply_recovery(plan).await.unwrap();
    assert!(matches!(
        c3_result(&boot, &id).await.outcome,
        OperationOutcome::Failed { .. }
    ));
    assert!(!boot.temp_path("ran").exists());
    assert_eq!(c3_count(&boot, &id).await, 1);
    assert_eq!(
        c3_record(&c3_op(&boot, &id).await)["decision"],
        json!({"Stopping":{"reason":"ReleaseRefused"}})
    );
}

pub(super) async fn c3_settle_held(boot: &TestBoot, id: &str) {
    use workspace_runtime::execution::{Desired, ExecutionId};
    let mut record = c3_record(&c3_op(boot, id).await);
    let execution: ExecutionId = serde_json::from_value(record["execution_id"].clone()).unwrap();
    let engine = boot.spawn_ctx.execution.engine().unwrap();
    let mut tx = boot.repo.pool().begin().await.unwrap();
    let ticket = engine
        .request(&mut tx, &execution, Desired::Stop)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    engine.drive(ticket.committed());
    tokio::time::timeout(Duration::from_secs(10), engine.wait_settled(&execution))
        .await
        .unwrap()
        .unwrap();
    record["release"] = json!({"Released":{"authorized_at_ms":now_ms()}});
    c3_rewrite(boot, id, &record).await;
}
pub(super) async fn c3_probe_ready(boot: &TestBoot, name: &str, probe: &str) -> String {
    let (id, observer) = c3_probe_ready_with_observer(boot, name, probe).await;
    drop(observer);
    id
}
pub(super) async fn c3_probe_ready_with_observer(
    boot: &TestBoot,
    name: &str,
    probe: &str,
) -> (String, calm_server::operation::ParkedObserver) {
    let mut input = c3_input(boot, name, "exit 7");
    input["probe"] = json!({"probe_argv":["/bin/sh","-c",probe],"output_probe_argv":null});
    let (id, op, output) = claimed_spawn_started_forge_op(boot, name, input)
        .await
        .unwrap();
    let SpawnOutcome::Parked {
        deadline_ms,
        observer,
    } = ForgeActionAdapter::new()
        .spawn_side_effect(&output, &op, &boot.spawn_ctx)
        .await
        .unwrap()
    else {
        panic!()
    };
    boot.operation_repo
        .set_parked(&op, deadline_ms, Some("forge_execution"))
        .await
        .unwrap()
        .unwrap();
    c3_settle_held(boot, &id).await;
    (id, observer)
}
#[tokio::test]
async fn c3_probe_admission_is_durably_exclusive() {
    for output in [false, true] {
        for steal_boot in [false, true] {
            let boot = TestBoot::new().await;
            let (id, pause) = if output {
                super::c3_probe::output_admission_ready(&boot).await
            } else {
                // Dropping this cold observer does not start it. The first winner
                // recovery below attaches one, so arm its pause before admission.
                let id = c3_probe_ready(&boot, "exclusive", "exit 0").await;
                let pause = calm_server::test_seams::PausePoint::default();
                calm_server::test_seams::install_pause_for_test(
                    "forge-observer-before-read",
                    &id,
                    pause.clone(),
                );
                (id, pause)
            };
            let baseline = if output { 2 } else { 1 };
            let stale = boot
                .operation_repo
                .claim_parked(&id)
                .await
                .unwrap()
                .unwrap();
            if !steal_boot {
                sqlx::query("UPDATE operations SET lease_until_ms=0 WHERE id=?1")
                    .bind(&id)
                    .execute(boot.repo.pool())
                    .await
                    .unwrap();
            }
            let winner = if steal_boot {
                boot.operation_repo
                    .claim_parked_for_boot(&id)
                    .await
                    .unwrap()
                    .unwrap()
            } else {
                boot.operation_repo
                    .claim_parked(&id)
                    .await
                    .unwrap()
                    .unwrap()
            };
            let adapter = ForgeActionAdapter::new();
            assert!(
                adapter
                    .recover_owned_parked(&stale, RecoveryMode::PreDeadlineProbe, &boot.spawn_ctx)
                    .await
                    .is_err()
            );
            assert_eq!(
                c3_count(&boot, &id).await,
                baseline,
                "stale admission must roll back execution insert"
            );
            assert!(matches!(
                adapter
                    .recover_owned_parked(&winner, RecoveryMode::PreDeadlineProbe, &boot.spawn_ctx)
                    .await
                    .unwrap(),
                ParkedRecovery::LeaveParked
            ));
            if !output {
                tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
                    .await
                    .unwrap();
            }
            assert_eq!(c3_count(&boot, &id).await, baseline + 1);
            assert!(
                adapter
                    .recover_owned_parked(&winner, RecoveryMode::PreDeadlineProbe, &boot.spawn_ctx)
                    .await
                    .is_err()
            );
            assert_eq!(c3_count(&boot, &id).await, baseline + 1);
            use workspace_runtime::execution::*;
            let alias = format!("forge:{id}:{}", if output { "output" } else { "verdict" });
            let raw: String =
                sqlx::query_scalar("SELECT intent_json FROM workspace_executions WHERE alias=?")
                    .bind(alias)
                    .fetch_one(boot.repo.pool())
                    .await
                    .unwrap();
            let mut metadata: ExecutionMetadata = serde_json::from_str(&raw).unwrap();
            metadata.id = ExecutionId::fresh().unwrap();
            let intent = ExecutionIntent {
                metadata,
                secrets: ExecutionSecrets {
                    ticket: CapabilityToken::fresh().unwrap(),
                    tokens: ExecutionControlTokens::fresh().unwrap(),
                },
            };
            let mut tx = boot.repo.pool().begin().await.unwrap();
            assert!(
                boot.spawn_ctx
                    .execution
                    .engine()
                    .unwrap()
                    .admit(&mut tx, intent)
                    .await
                    .is_err()
            );
            tx.rollback().await.unwrap();
            // Keep the accelerator out of every admission/alias assertion, then
            // let it finish after relinquishing the test's winning claim.
            c3_rewrite(&boot, &id, &c3_record(&c3_op(&boot, &id).await)).await;
            pause.release.notify_one();
            c3_result(&boot, &id).await;
        }
    }
}
#[tokio::test]
async fn c3_redrive_of_held_ends_infra_without_second_execution() {
    let boot = TestBoot::new().await;
    let input = c3_input(&boot, "redrive-held", "touch ran");
    let (id, op, output) = claimed_spawn_started_forge_op(&boot, "redrive-held", input)
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
    let plan = boot.runtime.recover_on_boot().await.unwrap();
    boot.runtime.apply_recovery(plan).await.unwrap();
    let result = c3_result(&boot, &id).await;
    assert!(
        matches!(result.outcome, OperationOutcome::Failed { .. }),
        "result={result:?} op={:?}",
        c3_op(&boot, &id).await
    );
    assert_eq!(c3_count(&boot, &id).await, 1);
    assert!(!boot.temp_path("ran").exists());
}
#[tokio::test]
async fn c3_probe_waits_for_available_executor() {
    let boot = TestBoot::new().await;
    let id = c3_probe_ready(&boot, "probe-unavailable", "true").await;
    let mut ctx = boot.spawn_ctx.clone();
    ctx.execution = Default::default();
    let runtime = OperationRuntime::new_unchecked(
        boot.operation_repo.clone(),
        vec![Arc::new(ForgeActionAdapter::new())],
        ctx.events.clone(),
        ctx.completion.clone(),
        ctx.clone(),
    );
    runtime.sweep_parked().await.unwrap();
    let retained = c3_op(&boot, &id).await;
    assert_eq!(retained.phase, Phase::Parked);
    assert!(
        retained
            .last_error
            .as_deref()
            .unwrap()
            .contains("owned execution retained")
    );
    assert!(retained.lease_owner.is_some());
    assert!(retained.lease_until_ms.unwrap() > now_ms());
    assert_eq!(c3_count(&boot, &id).await, 1);
    ctx.execution
        .bind(Arc::new(boot.spawn_ctx.execution.engine().unwrap().clone()));
    runtime.sweep_parked().await.unwrap();
    assert_eq!(c3_count(&boot, &id).await, 1, "lease backs off retry");
    sqlx::query("UPDATE operations SET lease_until_ms=0 WHERE id=?")
        .bind(&id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    runtime.sweep_parked().await.unwrap();
    assert_eq!(c3_count(&boot, &id).await, 2);
    // The sweep attaches the observer; only observe its completion, without a
    // boot claimant stealing its lease during the probe's release handshake.
    assert!(matches!(
        super::c3_observer::result_without_sweep(&boot, &id)
            .await
            .outcome,
        OperationOutcome::Succeeded { .. }
    ));
}
#[tokio::test]
async fn c3_pre_c3_native_forge_op_is_never_advanced() {
    let boot = TestBoot::new().await;
    let id = seed_native_parked_forge_op_with_deadline(
        &boot,
        "legacy-c3",
        c3_input(&boot, "legacy-c3", "touch ran"),
        dead_artifacts(),
        now_ms() + 30_000,
    )
    .await
    .unwrap();
    let before = c3_op(&boot, &id).await;
    let plan = boot.runtime.recover_on_boot().await.unwrap();
    boot.runtime.apply_recovery(plan).await.unwrap();
    let after = c3_op(&boot, &id).await;
    assert_eq!(after.phase, Phase::Parked);
    assert_eq!(
        serde_json::to_value(after.spawn_artifacts).unwrap(),
        serde_json::to_value(before.spawn_artifacts).unwrap()
    );
    assert!(after.last_error.unwrap().contains("old kernel"));
    assert_eq!(c3_count(&boot, &id).await, 0);
    assert!(!boot.temp_path("ran").exists());
}

pub(super) async fn c3_stop_execution(boot: &TestBoot, id: &str) {
    use workspace_runtime::execution::{Desired, ExecutionId};
    let record = c3_record(&c3_op(boot, id).await);
    let execution: ExecutionId = serde_json::from_value(record["execution_id"].clone()).unwrap();
    let engine = boot.spawn_ctx.execution.engine().unwrap();
    let mut tx = boot.repo.pool().begin().await.unwrap();
    let ticket = engine
        .request(&mut tx, &execution, Desired::Stop)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    engine.drive(ticket.committed());
}

pub(super) async fn c3_running_probe(boot: &TestBoot, name: &str) -> String {
    let mut input = c3_input(boot, name, "exit 7");
    input["probe"] = json!({"probe_argv":["/bin/sh","-c",format!("touch '{}'; i=0; while [ ! -f '{}' ] && [ \"$i\" -lt 1500 ]; do i=$((i+1)); sleep .02; done",boot.temp_path("probe-started").display(),boot.temp_path("probe-finish").display())],"output_probe_argv":null});
    let id = boot
        .runtime
        .submit(FORGE_ACTION_KIND, op_key(name), input)
        .await
        .unwrap();
    wait_for_file(&boot.temp_path("probe-started")).await;
    c3_wait(async || {
        c3_record(&c3_op(boot, &id).await)["probes"]["verdict"]["release"]
            .get("Released")
            .is_some()
    })
    .await;
    id
}
pub(super) async fn c3_all_retired(boot: &TestBoot, id: &str) {
    c3_wait(async || {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM workspace_executions WHERE json_extract(intent_json,'$.owner')=? \
        AND (desired<>'retire' OR secrets_json IS NOT NULL)",
        )
        .bind(id)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap()
            == 0
    })
    .await;
}
#[tokio::test]
async fn c3_no_completion_while_probe_unsettled() {
    let boot = TestBoot::new().await;
    let id = c3_running_probe(&boot, "unsettled-probe").await;
    sqlx::query("UPDATE operations SET parked_deadline_ms=0 WHERE id=?")
        .bind(&id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let claimed = boot
        .operation_repo
        .claim_parked_for_boot(&id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        ForgeActionAdapter::new()
            .recover_owned_parked(&claimed, RecoveryMode::PreDeadlineProbe, &boot.spawn_ctx)
            .await
            .unwrap(),
        ParkedRecovery::LeaveParked
    ));
    sqlx::query("UPDATE operations SET lease_owner=NULL,lease_until_ms=NULL WHERE id=?")
        .bind(&id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    boot.runtime.sweep_parked().await.unwrap();
    let plan = boot.runtime.recover_on_boot().await.unwrap();
    boot.runtime.apply_recovery(plan).await.unwrap();
    assert_eq!(phase(&boot.repo, &id).await, "parked");
    assert!(
        boot.operation_repo
            .operation_result(&id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(c3_count(&boot, &id).await, 2);
    fs::write(boot.temp_path("probe-finish"), "").unwrap();
    assert!(matches!(
        c3_result(&boot, &id).await.outcome,
        OperationOutcome::Succeeded { .. }
    ));
    c3_all_retired(&boot, &id).await;
}
#[tokio::test]
async fn c3_probe_timeout_stops_through_engine() {
    let boot = TestBoot::new().await;
    let id = c3_running_probe(&boot, "probe-timeout").await;
    let mut record = c3_record(&c3_op(&boot, &id).await);
    record["probes"]["verdict"]["release"] =
        json!({"Released":{"authorized_at_ms":now_ms()-61_000}});
    let execution = record["probes"]["verdict"]["execution_id"].clone();
    let probe_path = boot.temp_path(&format!(
        "probe-timeout.probe.{}",
        execution.as_str().unwrap()
    ));
    stage_result_files(&probe_path, "0", "");
    c3_rewrite(&boot, &id, &record).await;
    boot.runtime.sweep_parked().await.unwrap();
    let result = c3_result(&boot, &id).await;
    assert!(
        matches!(result.outcome,OperationOutcome::Failed {last_error_class:Some(ref class),..} if class=="gate-infra")
    );
    let record = c3_record(&c3_op(&boot, &id).await);
    assert_eq!(
        record["probes"]["verdict"]["decision"],
        json!({"Stopping":{"reason":"Timeout"}})
    );
    assert_eq!(record["probes"]["verdict"]["execution_id"], execution);
    assert_eq!(c3_count(&boot, &id).await, 2);
    c3_all_retired(&boot, &id).await;
}
#[tokio::test]
async fn c3_timeout_stops_then_completes_after_settlement() {
    for probe in [false, true] {
        let boot = TestBoot::new().await;
        let mut input = c3_merge_input(
            &boot,
            "timeout-action",
            &format!("touch '{}'; sleep 30", boot.temp_path("started").display()),
        );
        if probe {
            input["probe"] = json!({"probe_argv":["/bin/true"],"output_probe_argv":["/bin/sh","-c","echo '{\"oid\":\"abcdef\",\"headRefOid\":\"123456\"}'"]});
        }
        let (id, op, output) = claimed_spawn_started_forge_op(&boot, "timeout-action", input)
            .await
            .unwrap();
        let SpawnOutcome::Parked {
            deadline_ms,
            observer,
        } = ForgeActionAdapter::new()
            .spawn_side_effect(&output, &op, &boot.spawn_ctx)
            .await
            .unwrap()
        else {
            panic!("owned park")
        };
        boot.operation_repo
            .set_parked(&op, deadline_ms, Some("forge_execution"))
            .await
            .unwrap()
            .unwrap();
        let observer = tokio::spawn(observer);
        wait_for_file(&boot.temp_path("started")).await;
        sqlx::query("UPDATE operations SET parked_deadline_ms=0 WHERE id=?")
            .bind(&id)
            .execute(boot.repo.pool())
            .await
            .unwrap();
        // Let the active observer reach the expired deadline before the first sweep.
        tokio::time::timeout(Duration::from_secs(1), observer)
            .await
            .expect("expired observer must stop claiming before the sweep")
            .unwrap();
        let before = c3_op(&boot, &id).await;
        assert_eq!(before.phase, Phase::Parked);
        assert_eq!(c3_record(&before)["decision"], "Observing");
        assert_eq!(forge_event_count(&boot.repo).await, 0);
        assert!(
            before.lease_owner.is_none(),
            "expired observer must yield its claim"
        );
        boot.runtime.sweep_parked().await.unwrap();
        assert_eq!(
            c3_record(&c3_op(&boot, &id).await)["decision"],
            json!({"Stopping":{"reason":"Timeout"}}),
            "the first sweep must apply the timeout"
        );
        let result = c3_result(&boot, &id).await;
        if probe {
            assert!(
                matches!(result.outcome, OperationOutcome::Succeeded { .. }),
                "{result:?}"
            );
            let OperationOutcome::Succeeded { result } = &result.outcome else {
                unreachable!()
            };
            assert_eq!(result["event"]["head_sha"], "123456");
            assert_eq!(result["event"]["merge_sha"], "abcdef");
            assert_eq!(c3_count(&boot, &id).await, 3);
        } else {
            assert!(
                matches!(result.outcome,OperationOutcome::Failed {last_error_class:Some(ref class),..} if class=="action-timeout")
            );
            assert_eq!(c3_count(&boot, &id).await, 1);
        }
        assert_eq!(
            c3_record(&c3_op(&boot, &id).await)["decision"],
            json!({"Stopping":{"reason":"Timeout"}})
        );
        c3_all_retired(&boot, &id).await;
    }
}
#[tokio::test]
async fn c3_unknown_execution_never_completes_op() {
    for probe in [false, true] {
        let mut boot = TestBoot::new().await;
        let id = if probe {
            c3_running_probe(&boot, "unknown-probe").await
        } else {
            let input = c3_input(
                &boot,
                "unknown-action",
                format!("touch '{}'; sleep 30", boot.temp_path("started").display()).as_str(),
            );
            let id = boot
                .runtime
                .submit(FORGE_ACTION_KIND, op_key("unknown-action"), input)
                .await
                .unwrap();
            wait_for_file(&boot.temp_path("started")).await;
            // Process start can precede the observer's durable release acknowledgement.
            c3_wait(async || {
                c3_record(&c3_op(&boot, &id).await)["release"]
                    .get("Released")
                    .is_some()
            })
            .await;
            id
        };
        boot._execution.restart().await;
        let plan = boot.runtime.recover_on_boot().await.unwrap();
        boot.runtime.apply_recovery(plan).await.unwrap();
        boot.runtime.sweep_parked().await.unwrap();
        assert_eq!(phase(&boot.repo, &id).await, "parked");
        let unsettled: i64=sqlx::query_scalar("SELECT count(*) FROM workspace_executions WHERE json_extract(intent_json,'$.owner')=? AND settlement_json IS NULL").bind(&id).fetch_one(boot.repo.pool()).await.unwrap();
        assert_eq!(unsettled, 1);
        assert!(
            boot.operation_repo
                .operation_result(&id)
                .await
                .unwrap()
                .is_none()
        );
    }
}
#[tokio::test]
async fn c3_every_terminal_outcome_drives_retire() {
    for code in [0, 1, 2] {
        let boot = TestBoot::new().await;
        let mut input = c3_input(&boot, "terminal", "exit 7");
        input["probe"] =
            json!({"probe_argv":["/bin/sh","-c",format!("exit {code}")],"output_probe_argv":null});
        let id = boot
            .runtime
            .submit(FORGE_ACTION_KIND, op_key("terminal"), input)
            .await
            .unwrap();
        let result = c3_result(&boot, &id).await;
        assert_eq!(
            matches!(result.outcome, OperationOutcome::Succeeded { .. }),
            code == 0
        );
        c3_all_retired(&boot, &id).await;
    }
}
#[tokio::test]
async fn c3_boot_releasing_probe_is_refused_not_resent() {
    let boot = TestBoot::new().await;
    let id = c3_probe_ready(&boot, "probe-releasing", "touch probe-ran").await;
    // Admission attaches an observer; keep it from releasing before boot refusal.
    let pause = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(
        "forge-observer-before-read",
        &id,
        pause.clone(),
    );
    let op = boot
        .operation_repo
        .claim_parked(&id)
        .await
        .unwrap()
        .unwrap();
    ForgeActionAdapter::new()
        .recover_owned_parked(&op, RecoveryMode::PreDeadlineProbe, &boot.spawn_ctx)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
        .await
        .unwrap();
    let mut record = c3_record(&c3_op(&boot, &id).await);
    record["probes"]["verdict"]["release"] = json!({"Releasing":{"authorized_at_ms":now_ms()}});
    c3_rewrite(&boot, &id, &record).await;
    let plan = boot.runtime.recover_on_boot().await.unwrap();
    boot.runtime.apply_recovery(plan).await.unwrap();
    assert_eq!(
        c3_record(&c3_op(&boot, &id).await)["probes"]["verdict"]["decision"],
        json!({"Stopping":{"reason":"ReleaseRefused"}})
    );
    pause.release.notify_one();
    assert!(matches!(
        c3_result(&boot, &id).await.outcome,
        OperationOutcome::Failed { .. }
    ));
    assert!(!boot.temp_path("probe-ran").exists());
    assert_eq!(c3_count(&boot, &id).await, 2);
    c3_all_retired(&boot, &id).await;
}
#[tokio::test]
async fn c3_boot_released_retries_only_close_then_reattaches() {
    assert_boot_live_reattach_completes_from_result_files_without_probe()
        .await
        .unwrap();
}
