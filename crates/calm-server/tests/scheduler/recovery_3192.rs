use super::*;

#[cfg(target_os = "linux")]
async fn interrupted_gate(timeout: bool) {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let mut boot = boot().await;
    let dir = unique_gate_dir("recovery-3192");
    let task_id = format!("{}:interrupted", boot.track_id);
    let gate = json!({"cwd":dir,"timeout_secs":if timeout {1} else {60},"steps":[{"name":"interrupted","cmd":"printf launch >> launches; while [ ! -e interrupt ]; do sleep 0.01; done; kill -9 0"}]}).to_string();
    seed_task(&boot, gate_task(&boot, "interrupted", &gate)).await;
    let (runtime, scheduler) = build_scheduler(
        &boot,
        vec![
            Arc::new(TaskVerifyAdapter::new(dir.clone())),
            Arc::new(CardSpawnAdapter {
                kind: "codex-worker",
                card_id: boot.worker_card_id.to_string(),
            }),
        ],
    );
    scheduler.schedule_track(boot.track_id.clone()).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(op) = runtime
                .find_by_kind_and_idempotency("task-verify", &format!("{task_id}#g1"))
                .await
                .unwrap()
                && op.tx_output.as_ref().is_some_and(|o| {
                    o.data["gate_execution"]["release"]
                        .get("Released")
                        .is_some()
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    std::fs::write(dir.join("interrupt"), "interrupt").unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(op) = runtime
                .find_by_kind_and_idempotency("task-verify", &format!("{task_id}#g1"))
                .await
                .unwrap()
                && let Some(record) = op
                    .tx_output
                    .as_ref()
                    .and_then(|o| o.data.get("gate_execution"))
            {
                let id = serde_json::from_value(record["execution_id"].clone()).unwrap();
                if matches!(
                    boot.execution
                        .lifecycle
                        .engine()
                        .unwrap()
                        .output(&id, None)
                        .await,
                    Ok(workspace_runtime::execution::ExecutionReply::Observation {
                        state: workspace_runtime::execution::ExecutionState::Unknown { .. },
                        ..
                    })
                ) {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    if timeout {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let op = runtime
                    .find_by_kind_and_idempotency("task-verify", &format!("{task_id}#g1"))
                    .await
                    .unwrap()
                    .unwrap();
                if op.tx_output.as_ref().unwrap().data["gate_execution"]["decision"]
                    == json!({"Stopping":{"reason":"Timeout"}})
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }
    boot.execution.restart().await;
    let row = wait_for_terminal_row(&boot, "interrupted", 15).await;
    assert_eq!(row.status, TaskStatus::Failed);
    assert_eq!(row.status_detail.as_deref(), Some("gate-infra"));
    let verdict: Value = serde_json::from_str(row.gate_result_json.as_deref().unwrap()).unwrap();
    assert_eq!(verdict["exit_code"], Value::Null);
    assert_eq!(event_rows(&boot, "task.gate_result").await.len(), 1);
    runtime.sweep_parked().await.unwrap();
    assert_eq!(event_rows(&boot, "task.gate_result").await.len(), 1);
    assert_eq!(
        std::fs::read_to_string(dir.join("launches")).unwrap(),
        "launch"
    );
    let op = runtime
        .find_by_kind_and_idempotency("task-verify", &format!("{task_id}#g1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op.phase.tag(), PhaseTag::Succeeded);
    let pool = boot.repo.sqlite_db().unwrap();
    let mut conn = pool.read_pool().acquire().await.unwrap();
    assert_eq!(
        calm_server::db::sqlite::checkout_occupancy(&mut conn, boot.track_id.as_str(), "")
            .await
            .unwrap(),
        calm_server::db::sqlite::CheckoutOccupancy::Free
    );
    drop(conn);
    seed_projected_task(
        &boot,
        plan_task(&boot.track_id, "next-writer", TaskKind::Codex, &[]),
    )
    .await;
    scheduler.schedule_track(boot.track_id.clone()).await;
    assert_eq!(
        task_row(&boot, "next-writer").await.status,
        TaskStatus::Running
    );
    runtime.sweep_parked().await.unwrap();
    assert_eq!(
        task_row(&boot, "next-writer").await.status,
        TaskStatus::Running
    );
    assert_eq!(event_rows(&boot, "task.gate_result").await.len(), 1);
}

#[tokio::test]
async fn recovery_3192_legacy_no_status_precedes_timeout() {
    use calm_server::operation::RecoveryMode;
    let v = c2b::held("legacy-no-status", "exit 7", 60).await;
    c2b::park(&v).await;
    // Released legacy settlements are immutable inputs to owner recovery, not executor proof.
    sqlx::query(
        "UPDATE workspace_executions SET settled_at_ms=1,settlement_json=?1 WHERE execution_id=?2",
    )
    .bind(r#"{"Journal":{"Drained":{"status":null}}}"#)
    .bind(c2b::id(&v.op).as_str())
    .execute(v.ctx.operation_repo.sqlite_db().writer_pool())
    .await
    .unwrap();
    sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.gate_execution.decision',json(?1)) WHERE id=?2")
        .bind(r#"{"Stopping":{"reason":"Timeout"}}"#).bind(&v.op.id)
        .execute(v.ctx.operation_repo.sqlite_db().writer_pool()).await.unwrap();
    let op = v
        .ctx
        .operation_repo
        .get_operation(&v.op.id)
        .await
        .unwrap()
        .unwrap();
    let recovered = v
        .adapter
        .recover_owned_parked(&op, RecoveryMode::PreDeadlineProbe, &v.ctx)
        .await
        .unwrap();
    let calm_server::operation::ParkedRecovery::Complete(result) = recovered else {
        panic!("must complete")
    };
    let (ParkedOutcome::Succeeded { result }, _) = *result else {
        panic!("must report verdict")
    };
    assert_eq!(result["status_detail"], "gate-infra");
    assert_eq!(result["exit_code"], Value::Null);
    // Both gate owners consume the same immutable legacy settlement.
    let mut run_op = op.clone();
    let data = &mut run_op.tx_output.as_mut().unwrap().data;
    for (key, value) in [
        ("card_id", json!(v._boot.worker_card_id)),
        ("run", json!(1)),
        ("branch", json!("test")),
        ("base_sha", json!("unused")),
        ("canonical_path", json!(v.dir.path())),
        ("git_common_dir", json!(v.dir.path())),
        ("ref_name", json!("refs/neige/test")),
        ("message", json!("test legacy recovery")),
    ] {
        data[key] = value;
    }
    let recovered = TaskGateRunAdapter::new(v.dir.path().to_owned())
        .recover_owned_parked(&run_op, RecoveryMode::PreDeadlineProbe, &v.ctx)
        .await
        .unwrap();
    let calm_server::operation::ParkedRecovery::Complete(result) = recovered else {
        panic!("run must complete")
    };
    let (ParkedOutcome::Succeeded { result }, _) = *result else {
        panic!("run must report verdict")
    };
    assert_eq!(result["verdict"]["status_detail"], "gate-infra");
    assert_eq!(result["verdict"]["exit_code"], Value::Null);
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn recovery_3192_gate_settles_infra_after_executor_restart() {
    interrupted_gate(false).await;
}
#[tokio::test]
#[cfg(target_os = "linux")]
async fn recovery_3192_interruption_precedes_recorded_timeout() {
    interrupted_gate(true).await;
}
