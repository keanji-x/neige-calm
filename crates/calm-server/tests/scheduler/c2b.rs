use super::*;
use calm_server::operation::RecoveryMode;
use workspace_runtime::execution::{ExecutionId, ExecutionReply, ExecutionState};

pub(super) struct Verify {
    pub _boot: Boot,
    pub ctx: SpawnCtx,
    pub adapter: TaskVerifyAdapter,
    pub op: Operation,
    pub deadline: i64,
    pub observer: calm_server::operation::ParkedObserver,
    pub dir: tempfile::TempDir,
}

pub(super) async fn held(key: &str, cmd: &str, timeout: i64) -> Verify {
    let boot = boot().await;
    let dir = tempfile::tempdir().unwrap();
    let gate = json!({"cwd":dir.path(),"steps":[{"name":"step","cmd":cmd}],"timeout_secs":timeout});
    let mut task = gate_task(&boot, key, &gate.to_string());
    task.gate_attempt = 1;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    let repo = Arc::new(SqlxOperationRepo::new(boot.repo.sqlite_db().unwrap()));
    let payload = serde_json::to_value(TaskVerifyOperationPayload {
        actor: ActorId::KernelDispatcher,
        track_id: boot.track_id.to_string(),
        task_id: task_id.clone(),
        attempt: 1,
    })
    .unwrap();
    let id = repo
        .insert_operation(
            "task-verify",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(format!("{task_id}#g1")),
                payload_hash: stable_payload_hash(&payload).unwrap(),
            },
            payload,
        )
        .await
        .unwrap();
    repo.claim_drive_batch(1).await.unwrap();
    let mut output = TxOutput::new("task", Some(task_id.clone()), json!({}));
    output.data = json!({"task_id":task_id,"track_id":boot.track_id,"area_id":boot.area_id,"key":key,"attempt":1,"cwd":dir.path(),"gate":gate});
    sqlx::query("UPDATE operations SET phase='spawn_started',tx_output_json=?1 WHERE id=?2")
        .bind(serde_json::to_string(&output).unwrap())
        .bind(&id)
        .execute(boot.repo.sqlite_db().unwrap().writer_pool())
        .await
        .unwrap();
    let op = repo.get_operation(&id).await.unwrap().unwrap();
    let ctx = SpawnCtx::new(
        boot.repo.clone(),
        repo.clone(),
        Arc::new(DaemonClient {
            data_dir: dir.path().into(),
            proc_supervisor_sock: None,
        }),
        TerminalRendererRegistry::new(),
        boot.events.clone(),
        OperationCompletionBus::new(),
    )
    .with_execution(boot.execution.lifecycle.clone());
    let adapter = TaskVerifyAdapter::new(dir.path().into());
    let SpawnOutcome::Parked {
        deadline_ms,
        observer,
    } = adapter.spawn_side_effect(&output, &op, &ctx).await.unwrap()
    else {
        panic!("park")
    };
    let op = repo.get_operation(&id).await.unwrap().unwrap();
    Verify {
        _boot: boot,
        ctx,
        adapter,
        op,
        deadline: deadline_ms,
        observer,
        dir,
    }
}

pub(super) fn record(op: &Operation) -> &Value {
    &op.tx_output.as_ref().unwrap().data["gate_execution"]
}
pub(super) fn id(op: &Operation) -> ExecutionId {
    serde_json::from_value(record(op)["execution_id"].clone()).unwrap()
}
pub(super) async fn park(v: &Verify) {
    v.ctx
        .operation_repo
        .set_parked(&v.op, v.deadline, Some("gate_execution"))
        .await
        .unwrap()
        .unwrap();
}
pub(super) async fn wait_op(
    ctx: &SpawnCtx,
    id: &str,
    predicate: impl Fn(&Operation) -> bool,
) -> Operation {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let op = ctx.operation_repo.get_operation(id).await.unwrap().unwrap();
            if predicate(&op) {
                break op;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("operation reached expected state")
}
pub(super) async fn boot_recover(ctx: &SpawnCtx, adapter: &TaskVerifyAdapter, id: &str) {
    let op = ctx
        .operation_repo
        .claim_parked_for_boot(id)
        .await
        .unwrap()
        .unwrap();
    adapter
        .recover_owned_parked(&op, RecoveryMode::Boot, ctx)
        .await
        .unwrap();
    // Use the production runtime reconciliation and original claimed row for actual completion.
    sqlx::query("UPDATE operations SET lease_owner=NULL,lease_until_ms=NULL WHERE id=?1")
        .bind(id)
        .execute(ctx.operation_repo.sqlite_db().writer_pool())
        .await
        .unwrap();
    let runtime = OperationRuntime::new_unchecked(
        ctx.operation_repo.clone(),
        vec![Arc::new(adapter.clone())],
        ctx.events.clone(),
        ctx.completion.clone(),
        ctx.clone(),
    );
    runtime.sweep_parked().await.unwrap();
}

#[tokio::test]
async fn c2b_verify_releases_only_after_park() {
    let v = held("release", "echo once >> ran", 60).await;
    assert!(v.op.spawn_artifacts.is_none());
    assert_eq!(record(&v.op)["release"], "Held");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!v.dir.path().join("ran").exists());
    park(&v).await;
    let task = tokio::spawn(v.observer);
    let op = wait_op(&v.ctx, &v.op.id, |o| o.phase.tag() == PhaseTag::Succeeded).await;
    assert!(op.tx_output.unwrap().result["passed"].as_bool().unwrap());
    assert_eq!(
        std::fs::read_to_string(v.dir.path().join("ran")).unwrap(),
        "once\n"
    );
    task.await.unwrap();
}

#[tokio::test]
async fn c2b_crash_before_token_is_infra_not_relaunch() {
    held_boot_is_infra().await;
}

pub(super) async fn held_boot_is_infra() {
    let v = held("held-crash", "touch ran", 60).await;
    park(&v).await;
    drop(v.observer);
    boot_recover(&v.ctx, &v.adapter, &v.op.id).await;
    let op = wait_op(&v.ctx, &v.op.id, |o| o.phase.tag() == PhaseTag::Succeeded).await;
    assert_eq!(op.tx_output.unwrap().result["status_detail"], "gate-infra");
    assert!(!v.dir.path().join("ran").exists());
}

#[tokio::test]
async fn c2b_boot_released_retries_only_close_then_reattaches() {
    let v = held("released-boot", "echo once >> ran; sleep 600", 2).await;
    park(&v).await;
    let observer = tokio::spawn(v.observer);
    wait_op(&v.ctx, &v.op.id, |o| {
        record(o)["release"].get("Released").is_some()
    })
    .await;
    observer.abort();
    let _ = observer.await;
    let authorized_at = now_ms() - 1750;
    sqlx::query(
        "UPDATE operations SET tx_output_json=json_set(tx_output_json, \
         '$.data.gate_execution.release.Released.authorized_at_ms',?1) WHERE id=?2",
    )
    .bind(authorized_at)
    .bind(&v.op.id)
    .execute(v.ctx.operation_repo.sqlite_db().writer_pool())
    .await
    .unwrap();
    let started = std::time::Instant::now();
    boot_recover(&v.ctx, &v.adapter, &v.op.id).await;
    let op = wait_op(&v.ctx, &v.op.id, |o| o.phase.tag() == PhaseTag::Succeeded).await;
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "uses the remaining authorization time"
    );
    assert_eq!(
        record(&op)["release"]["Released"]["authorized_at_ms"],
        authorized_at
    );
    assert_eq!(
        op.tx_output.unwrap().result["status_detail"],
        "gate-timeout"
    );
    assert_eq!(
        std::fs::read_to_string(v.dir.path().join("ran")).unwrap(),
        "once\n"
    );
}

pub(super) async fn recovered_status(past_deadline: bool) {
    let v = held("recover-status", "sleep 1; exit 7", 60).await;
    park(&v).await;
    let observer = tokio::spawn(v.observer);
    wait_op(&v.ctx, &v.op.id, |o| {
        record(o)["release"].get("Released").is_some()
    })
    .await;
    observer.abort();
    let _ = observer.await;
    let engine = v.ctx.execution.engine().unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(
                engine.output(&id(&v.op), None).await,
                Ok(ExecutionReply::Observation {
                    state: ExecutionState::Drained { .. },
                    ..
                })
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let task_id = v.op.tx_output.as_ref().unwrap().data["task_id"]
        .as_str()
        .unwrap();
    std::fs::write(v.dir.path().join(format!("{task_id}-g1.exit")), "0\n").unwrap();
    if past_deadline {
        sqlx::query("UPDATE operations SET parked_deadline_ms=1 WHERE id=?1")
            .bind(&v.op.id)
            .execute(v.ctx.operation_repo.sqlite_db().writer_pool())
            .await
            .unwrap();
    }
    boot_recover(&v.ctx, &v.adapter, &v.op.id).await;
    let op = wait_op(&v.ctx, &v.op.id, |o| o.phase.tag() == PhaseTag::Succeeded).await;
    let result = &op.tx_output.unwrap().result;
    assert_eq!(result["exit_code"], 7);
    assert_eq!(result["status_detail"], "gate-red");
}
#[tokio::test]
async fn c2b_recovered_verdict_uses_executor_status() {
    recovered_status(false).await;
}
#[tokio::test]
async fn c2b_settled_verify_after_deadline_keeps_status() {
    recovered_status(true).await;
}

#[tokio::test]
async fn c2b_timeout_probe_preserves_durable_decision() {
    timeout_probe_preserves_decision().await;
}

pub(super) async fn released_boot_recovers() {
    let v = held("reattach", "echo once >> ran; sleep 1", 60).await;
    park(&v).await;
    let observer = tokio::spawn(v.observer);
    wait_op(&v.ctx, &v.op.id, |o| {
        record(o)["release"].get("Released").is_some()
    })
    .await;
    observer.abort();
    let _ = observer.await;
    assert!(matches!(
        v.ctx
            .execution
            .engine()
            .unwrap()
            .output(&id(&v.op), None)
            .await,
        Ok(ExecutionReply::Observation {
            state: ExecutionState::Spawned,
            ..
        })
    ));
    boot_recover(&v.ctx, &v.adapter, &v.op.id).await;
    let op = wait_op(&v.ctx, &v.op.id, |o| o.phase.tag() == PhaseTag::Succeeded).await;
    assert_eq!(op.tx_output.unwrap().result["passed"], true);
    assert_eq!(
        std::fs::read_to_string(v.dir.path().join("ran")).unwrap(),
        "once\n"
    );
}

pub(super) async fn prior_execution_must_settle() {
    let v = held("prior", "sleep 600", 60).await;
    park(&v).await;
    let mut output = v.op.tx_output.clone().unwrap();
    output
        .data
        .as_object_mut()
        .unwrap()
        .remove("gate_execution");
    output.data["attempt"] = json!(2);
    let task_id = output.data["task_id"].as_str().unwrap().to_string();
    let mut payload = v.op.payload.clone();
    payload["attempt"] = json!(2);
    let next = v
        .ctx
        .operation_repo
        .insert_operation(
            "task-verify",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(format!("{task_id}#g2")),
                payload_hash: stable_payload_hash(&payload).unwrap(),
            },
            payload,
        )
        .await
        .unwrap();
    v.ctx.operation_repo.claim_drive_batch(1).await.unwrap();
    sqlx::query("UPDATE operations SET phase='spawn_started',tx_output_json=?1 WHERE id=?2")
        .bind(serde_json::to_string(&output).unwrap())
        .bind(&next)
        .execute(v.ctx.operation_repo.sqlite_db().writer_pool())
        .await
        .unwrap();
    let op = v
        .ctx
        .operation_repo
        .get_operation(&next)
        .await
        .unwrap()
        .unwrap();
    let error = v
        .adapter
        .spawn_side_effect(&output, &op, &v.ctx)
        .await
        .err()
        .expect("prior execution blocks admission");
    assert!(
        error.to_string().contains("prior gate execution"),
        "{error}"
    );
    let next = v
        .ctx
        .operation_repo
        .get_operation(&next)
        .await
        .unwrap()
        .unwrap();
    assert!(record(&next).is_null());
    drop(v.observer);
    boot_recover(&v.ctx, &v.adapter, &v.op.id).await;
    wait_op(&v.ctx, &v.op.id, |o| o.phase.tag() == PhaseTag::Succeeded).await;
    // The prior execution is now settled; the same production spawn path can admit attempt 2.
    sqlx::query("UPDATE tasks SET status='verifying',gate_attempt=2 WHERE id=?1")
        .bind(&task_id)
        .execute(v.ctx.operation_repo.sqlite_db().writer_pool())
        .await
        .unwrap();
    let SpawnOutcome::Parked {
        deadline_ms,
        observer,
    } = v
        .adapter
        .spawn_side_effect(&output, &next, &v.ctx)
        .await
        .unwrap()
    else {
        panic!("park")
    };
    v.ctx
        .operation_repo
        .set_parked(&next, deadline_ms, Some("gate_execution"))
        .await
        .unwrap()
        .unwrap();
    drop(observer);
    boot_recover(&v.ctx, &v.adapter, &next.id).await;
    wait_op(&v.ctx, &next.id, |o| o.phase.tag() == PhaseTag::Succeeded).await;
}

#[tokio::test]
async fn c2b_new_attempt_requires_prior_execution_settled() {
    prior_execution_must_settle().await;
}

pub(super) async fn predeadline_without_verdict() {
    use workspace_runtime::execution::Desired;
    let v = held("pre-deadline", "touch ran", 60).await;
    park(&v).await;
    drop(v.observer);
    let pool = v.ctx.operation_repo.sqlite_db();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    let engine = v.ctx.execution.engine().unwrap();
    let ticket = engine
        .request(&mut tx, &id(&v.op), Desired::Stop)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    engine.drive(ticket.committed());
    tokio::time::timeout(Duration::from_secs(10), engine.wait_settled(&id(&v.op)))
        .await
        .unwrap()
        .unwrap();
    let runtime = OperationRuntime::new_unchecked(
        v.ctx.operation_repo.clone(),
        vec![Arc::new(v.adapter.clone())],
        v.ctx.events.clone(),
        v.ctx.completion.clone(),
        v.ctx.clone(),
    );
    runtime.sweep_parked().await.unwrap();
    runtime.sweep_parked().await.unwrap();
    let op = wait_op(&v.ctx, &v.op.id, |o| o.phase.tag() == PhaseTag::Succeeded).await;
    assert!(v.deadline > now_ms());
    assert_eq!(op.tx_output.unwrap().result["status_detail"], "gate-infra");
    assert!(!v.dir.path().join("ran").exists());
}
