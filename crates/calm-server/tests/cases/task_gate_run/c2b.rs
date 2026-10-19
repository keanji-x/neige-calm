//! Production tool -> Engine -> typed supervisor -> held shell invariants.
use super::*;
use calm_server::operation::{Operation, PhaseTag};

async fn op(fx: &Fx, task: &str) -> Operation {
    fx.runtime
        .find_by_kind_and_idempotency(TASK_GATE_RUN_KIND, &format!("{task}#r1"))
        .await
        .unwrap()
        .unwrap()
}
fn record(op: &Operation) -> Value {
    op.tx_output.as_ref().unwrap().data["gate_execution"].clone()
}
async fn execution_count(fx: &Fx, owner: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM workspace_executions WHERE json_extract(intent_json,'$.owner')=?",
    )
    .bind(owner)
    .fetch_one(&fx.pool())
    .await
    .unwrap()
}
async fn wait_until(mut ready: impl AsyncFnMut() -> bool) {
    tokio::time::timeout(WAIT, async {
        while !ready().await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("invariant becomes observable");
}
async fn rewrite(fx: &Fx, id: &str, value: &Value) {
    sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.gate_execution',json(?)) WHERE id=?")
        .bind(value.to_string()).bind(id).execute(&fx.pool()).await.unwrap();
}

#[tokio::test]
async fn c2b_gate_run_has_one_engine_construction() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "one", json!([{"name":"ok","cmd":"true"}])).await;
    gate_run(&fx, &a, Some("test: one execution"))
        .await
        .unwrap();
    assert!(run_result(&fx, &a.task.id, 1).await.verdict.passed);
    let op = op(&fx, &a.task.id).await;
    assert_eq!(execution_count(&fx, &op.id).await, 1);
    assert!(op.spawn_artifacts.is_none());
    assert_eq!(record(&op)["version"], 1);
    let intent: String =
        sqlx::query_scalar("SELECT intent_json FROM workspace_executions WHERE execution_id=?")
            .bind(record(&op)["execution_id"].as_str().unwrap())
            .fetch_one(&fx.pool())
            .await
            .unwrap();
    let intent: Value = serde_json::from_str(&intent).unwrap();
    assert_eq!(intent["consumer"], "gate");
    assert_eq!(intent["program"]["io_mode"], "Pipe");
}

#[tokio::test]
async fn c2b_release_authorizes_before_first_byte() {
    let (hook, entered, resume) = pause();
    let fx = world_with(LONG_IDLE, |b| hooked(b, |a| a.with_before_release(hook))).await;
    let marker = outside(&fx, "authorized-step");
    let a = running(
        &fx,
        "authorize",
        json!([{"name":"mark","cmd":format!("touch '{}'",marker.display())}]),
    )
    .await;
    // Admit through the tool with a short wait while the post-park observer is paused.
    let call = gate_run(&fx, &a, Some("test: authorize before bytes"));
    tokio::pin!(call);
    tokio::select! { _=entered.notified()=>{}, result=&mut call=>panic!("unexpected tool completion: {result:?}") }
    let held = op(&fx, &a.task.id).await;
    assert_eq!(held.phase.tag(), PhaseTag::Parked);
    assert_eq!(record(&held)["release"], "Held");
    assert!(!marker.exists());
    resume.notify_one();
    call.await.unwrap();
    assert!(marker.exists());
    let released = record(&op(&fx, &a.task.id).await);
    assert!(released["release"]["Released"]["authorized_at_ms"].is_i64());
    assert_eq!(released["decision"], "Observing");
}

#[tokio::test]
async fn c2b_refused_release_writes_no_byte() {
    let (hook, entered, resume) = pause();
    let fx = world_with(LONG_IDLE, |b| hooked(b, |a| a.with_before_release(hook))).await;
    let marker = outside(&fx, "refused-step");
    let a = running(
        &fx,
        "refuse",
        json!([{"name":"mark","cmd":format!("touch '{}'",marker.display())}]),
    )
    .await;
    let call = gate_run(&fx, &a, Some("test: refuse release"));
    tokio::pin!(call);
    tokio::select! { _=entered.notified()=>{}, result=&mut call=>panic!("unexpected completion: {result:?}") }
    sqlx::query("UPDATE tasks SET status='failed' WHERE id=?")
        .bind(&a.task.id)
        .execute(&fx.pool())
        .await
        .unwrap();
    resume.notify_one();
    call.await.unwrap();
    assert!(!marker.exists());
    let result = run_result(&fx, &a.task.id, 1).await;
    assert!(!result.verdict.passed);
    let record = record(&op(&fx, &a.task.id).await);
    assert_eq!(record["release"], "Held");
    assert_eq!(record["decision"]["Stopping"]["reason"], "ReleaseRefused");
}

#[tokio::test]
async fn c2b_typed_park_needs_no_spawn_artifacts() {
    let (fx, a, resume) = held("typed").await;
    let op = op(&fx, &a.task.id).await;
    assert_eq!(op.phase.tag(), PhaseTag::Parked);
    assert!(op.spawn_artifacts.is_none());
    assert!(record(&op)["execution_id"].is_string());
    assert_eq!(execution_count(&fx, &op.id).await, 1);
    resume.notify_one();
    run_result(&fx, &a.task.id, 1).await;
}

#[tokio::test]
async fn c2b_changed_decision_aborts_completion() {
    let (hook, entered, resume) = pause();
    let fx = world_with(LONG_IDLE, |b| hooked(b, |a| a.with_before_completion(hook))).await;
    let a = running(&fx, "decision", json!([{"name":"ok","cmd":"true"}])).await;
    let call = gate_run(&fx, &a, Some("test: decision fence"));
    tokio::pin!(call);
    tokio::select! { _=entered.notified()=>{}, result=&mut call=>panic!("unexpected completion: {result:?}") }
    let before = op(&fx, &a.task.id).await;
    let mut changed = record(&before);
    changed["decision"] = json!({"Stopping":{"reason":"Timeout"}});
    rewrite(&fx, &before.id, &changed).await;
    resume.notify_one();
    // The completion's claim stays as backoff after the in-transaction mismatch.
    wait_until(async || op(&fx, &a.task.id).await.last_error.is_some()).await;
    let retained = op(&fx, &a.task.id).await;
    assert_eq!(retained.phase.tag(), PhaseTag::Parked);
    assert_eq!(record(&retained), changed);
    assert!(retained.lease_owner.is_some());
    assert_eq!(execution_count(&fx, &retained.id).await, 1);
}

#[tokio::test]
async fn c2b_leader_exit_drains_descendants_before_verdict() {
    let fx = world(LONG_IDLE).await;
    let pidfile = outside(&fx, "descendant");
    let a=running(&fx,"drain",json!([{"name":"background","cmd":format!("sleep 120 & echo $! > '{}'",pidfile.display())}])).await;
    gate_run(&fx, &a, Some("test: drain descendants"))
        .await
        .unwrap();
    let result = run_result(&fx, &a.task.id, 1).await;
    assert!(result.verdict.passed, "{result:?}");
    let pid: i32 = std::fs::read_to_string(pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
    let op = op(&fx, &a.task.id).await;
    let settlement: String =
        sqlx::query_scalar("SELECT settlement_json FROM workspace_executions WHERE execution_id=?")
            .bind(record(&op)["execution_id"].as_str().unwrap())
            .fetch_one(&fx.pool())
            .await
            .unwrap();
    assert!(settlement.contains("Drained"), "{settlement}");
}

#[tokio::test]
async fn c2b_cancel_parked_leaves_typed_op_parked() {
    let (fx, a, resume) = held("cancel-owned").await;
    let before = op(&fx, &a.task.id).await;
    assert!(
        !fx.runtime
            .cancel_parked(&before.id, "cancel requested")
            .await
            .unwrap()
    );
    let after = op(&fx, &a.task.id).await;
    assert_eq!(after.phase.tag(), PhaseTag::Parked);
    assert_eq!(record(&after), record(&before));
    assert_eq!(
        after.lease_owner, before.lease_owner,
        "cancel takes no owned claim"
    );
    assert!(after.compensation_state.is_none());
    resume.notify_one();
    run_result(&fx, &a.task.id, 1).await;
}

#[tokio::test]
async fn c2b_timeout_retries_while_probe_holds_claim() {
    use calm_server::operation::{OperationRepo, SqlxOperationRepo};
    let fx = world(Duration::from_millis(100)).await;
    let flag = outside(&fx, "timeout-hold");
    let a = running(
        &fx,
        "timeout-retry",
        json!([{"name":"block","cmd":wait_for(&flag)}]),
    )
    .await;
    gate_run(&fx, &a, Some("test: retry timeout under probe claim"))
        .await
        .unwrap();
    wait_step(&fx, &a.task.id, 1, "block").await;
    let before = op(&fx, &a.task.id).await;
    let repo = SqlxOperationRepo::new(fx.db());
    let claim = tokio::time::timeout(WAIT, async {
        loop {
            if let Some(claim) = repo.claim_parked(&before.id).await.unwrap() {
                break claim;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut value = record(&claim);
    value["release"]["Released"]["authorized_at_ms"] = json!(0);
    rewrite(&fx, &claim.id, &value).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(record(&op(&fx, &a.task.id).await)["decision"], "Observing");
    sqlx::query(
        "UPDATE operations SET lease_owner=NULL,lease_until_ms=NULL WHERE id=? AND lease_owner=?",
    )
    .bind(&claim.id)
    .bind(claim.lease_owner)
    .execute(&fx.pool())
    .await
    .unwrap();
    let result = run_result(&fx, &a.task.id, 1).await;
    assert_eq!(
        result.verdict.status_detail.as_deref(),
        Some("gate-timeout")
    );
    let after = op(&fx, &a.task.id).await;
    assert_eq!(record(&after)["decision"]["Stopping"]["reason"], "Timeout");
    assert_eq!(execution_count(&fx, &after.id).await, 1);
    assert!(!flag.exists());
}

// Keep the original observer paused while recovery drives the same real execution.
async fn held(key: &str) -> (Fx, Attempt, Arc<tokio::sync::Notify>) {
    let (hook, entered, resume) = pause();
    let fx = world_with(Duration::from_millis(100), |b| {
        hooked(b, |a| a.with_before_release(hook))
    })
    .await;
    let a = running(&fx, key, json!([{"name":"unexpected","cmd":"exit 19"}])).await;
    assert_eq!(
        gate_run(&fx, &a, Some("test: held execution"))
            .await
            .unwrap()["state"],
        "running"
    );
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .unwrap();
    (fx, a, resume)
}
async fn boot(fx: &Fx) {
    let plan = fx.runtime.recover_on_boot().await.unwrap();
    fx.runtime.apply_recovery(plan).await.unwrap();
}
async fn expire(fx: &Fx, id: &str) {
    sqlx::query("UPDATE operations SET lease_owner=NULL,lease_until_ms=NULL WHERE id=?")
        .bind(id)
        .execute(&fx.pool())
        .await
        .unwrap();
}
async fn settled_infra(fx: &Fx, a: &Attempt) {
    wait_until(async || {
        fx.runtime.sweep_parked().await.unwrap();
        op(fx, &a.task.id).await.phase.tag() == PhaseTag::Succeeded
    })
    .await;
    let result = run_result(fx, &a.task.id, 1).await;
    assert_eq!(result.verdict.status_detail.as_deref(), Some("gate-infra"));
    let op = op(fx, &a.task.id).await;
    assert_eq!(execution_count(fx, &op.id).await, 1);
    let (desired, settlement): (String, Option<String>) = sqlx::query_as(
        "SELECT desired,settlement_json FROM workspace_executions WHERE execution_id=?",
    )
    .bind(record(&op)["execution_id"].as_str().unwrap())
    .fetch_one(&fx.pool())
    .await
    .unwrap();
    assert_eq!(desired, "retire");
    assert!(settlement.is_some());
}

#[tokio::test]
async fn c2b_boot_held_stops_without_token() {
    let (fx, a, _) = held("boot-held").await;
    boot(&fx).await;
    settled_infra(&fx, &a).await;
    let value = record(&op(&fx, &a.task.id).await);
    assert_eq!(value["release"], "Held");
    assert_eq!(value["decision"]["Stopping"]["reason"], "KernelRestarted");
    assert!(
        !fx.boot
            .ctx
            .gate_logs_dir
            .join(format!("{}-r1.step", a.task.id))
            .exists()
    );
}

#[tokio::test]
async fn c2b_boot_after_deadline_keeps_boot_origin() {
    let (fx, a, _) = held("boot-overdue").await;
    let before = op(&fx, &a.task.id).await;
    sqlx::query("UPDATE operations SET parked_deadline_ms=parked_at_ms WHERE id=?")
        .bind(&before.id)
        .execute(&fx.pool())
        .await
        .unwrap();
    boot(&fx).await;
    settled_infra(&fx, &a).await;
    assert_eq!(
        record(&op(&fx, &a.task.id).await)["decision"]["Stopping"]["reason"],
        "KernelRestarted"
    );
}

#[tokio::test]
async fn c2b_redrive_of_held_ends_infra_without_second_execution() {
    let (fx, a, resume) = held("redrive-held").await;
    let before = op(&fx, &a.task.id).await;
    sqlx::query("UPDATE operations SET phase='spawn_started',parked_at_ms=NULL,parked_deadline_ms=NULL WHERE id=?")
        .bind(&before.id).execute(&fx.pool()).await.unwrap();
    // The re-drive observer uses the same pause; wait until its park has committed.
    fx.runtime.drive().await.unwrap();
    assert_eq!(op(&fx, &a.task.id).await.phase.tag(), PhaseTag::Parked);
    // Boot owns recovery while both observers remain paused.
    boot(&fx).await;
    settled_infra(&fx, &a).await;
    assert_eq!(
        record(&op(&fx, &a.task.id).await)["execution_id"],
        record(&before)["execution_id"]
    );
    resume.notify_waiters();
}

#[tokio::test]
async fn c2b_gate_run_refuses_unavailable_executor() {
    let (fx, a, _) = held("unavailable").await;
    let before = op(&fx, &a.task.id).await;
    let offline = fx.execution.socket.with_extension("offline");
    std::fs::rename(&fx.execution.socket, &offline).unwrap();
    let engine = fx.execution.lifecycle.engine().unwrap();
    let id = serde_json::from_value(record(&before)["execution_id"].clone()).unwrap();
    assert!(engine.output(&id, None).await.is_err());
    wait_until(async || !engine.available()).await;
    let error = gate_run(&fx, &a, Some("test: unavailable executor"))
        .await
        .unwrap_err();
    assert!(error.message.contains("execution"), "{error:?}");
    assert_eq!(run_ops(&fx, &a.task.id).await.len(), 1);
    assert_eq!(execution_count(&fx, &before.id).await, 1);
    std::fs::rename(offline, &fx.execution.socket).unwrap();
}

#[tokio::test]
async fn c2b_pre_c2b_native_op_is_never_advanced() {
    let (fx, a, _) = held("native").await;
    let before = op(&fx, &a.task.id).await;
    sqlx::query("UPDATE operations SET \
         tx_output_json=json_remove(tx_output_json,'$.data.gate_execution'),spawn_artifacts_json=? WHERE \
         id=?")
        .bind(json!({"pid":2147483000,"pgid":2147483000,"start_time":1,"boot_id":"old","log_path":null,"extra":{}}).to_string())
        .bind(&before.id).execute(&fx.pool()).await.unwrap();
    boot(&fx).await;
    let retained = op(&fx, &a.task.id).await;
    assert_eq!(retained.phase.tag(), PhaseTag::Parked);
    assert!(
        retained
            .last_error
            .unwrap()
            .contains("manually stop pgid 2147483000")
    );
    assert!(retained.tx_output.unwrap().result.is_object());
    assert_eq!(execution_count(&fx, &before.id).await, 1);
}

#[tokio::test]
async fn c2b_phase_writes_preserve_gate_record() {
    use calm_server::operation::{OperationRepo, Phase, SqlxOperationRepo, TxOutput};
    let (fx, a, _) = held("phase-record").await;
    let before = op(&fx, &a.task.id).await;
    sqlx::query("UPDATE operations SET phase='spawn_started',parked_at_ms=NULL,parked_deadline_ms=NULL WHERE id=?")
        .bind(&before.id).execute(&fx.pool()).await.unwrap();
    let repo = SqlxOperationRepo::new(fx.db());
    let claimed = repo
        .claim_drive_batch(32)
        .await
        .unwrap()
        .into_iter()
        .find(|o| o.id == before.id)
        .unwrap();
    let mut output = TxOutput::new("task", Some(a.task.id.clone()), json!({}));
    output.data = json!({"checkpoint":"replacement"});
    repo.set_phase_and_tx_output(&claimed, Phase::SpawnStarted, &output)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record(&op(&fx, &a.task.id).await), record(&before));
}

#[tokio::test]
async fn c2b_stop_decision_survives_unavailable_and_restart() {
    let (fx, a, _) = held("stop-outage").await;
    let before = op(&fx, &a.task.id).await;
    let id = serde_json::from_value(record(&before)["execution_id"].clone()).unwrap();
    let offline = fx.execution.socket.with_extension("offline");
    std::fs::rename(&fx.execution.socket, &offline).unwrap();
    assert!(
        fx.execution
            .lifecycle
            .engine()
            .unwrap()
            .output(&id, None)
            .await
            .is_err()
    );
    sqlx::query("UPDATE operations SET parked_deadline_ms=parked_at_ms WHERE id=?")
        .bind(&before.id)
        .execute(&fx.pool())
        .await
        .unwrap();
    fx.runtime.sweep_parked().await.unwrap();
    let stopped = op(&fx, &a.task.id).await;
    assert_eq!(stopped.phase.tag(), PhaseTag::Parked);
    assert_eq!(
        record(&stopped)["decision"]["Stopping"]["reason"],
        "Timeout"
    );
    boot(&fx).await;
    assert_eq!(op(&fx, &a.task.id).await.phase.tag(), PhaseTag::Parked);
    std::fs::rename(offline, &fx.execution.socket).unwrap();
    wait_until(async || {
        fx.runtime.sweep_parked().await.unwrap();
        op(&fx, &a.task.id).await.phase.tag() == PhaseTag::Succeeded
    })
    .await;
    assert_eq!(
        run_result(&fx, &a.task.id, 1)
            .await
            .verdict
            .status_detail
            .as_deref(),
        Some("gate-timeout")
    );
    assert_eq!(
        record(&op(&fx, &a.task.id).await)["execution_id"],
        record(&before)["execution_id"]
    );
}

#[tokio::test]
async fn c2b_unknown_execution_never_completes_op() {
    use workspace_runtime::execution::{ExecutionReply, ExecutionState};
    let (mut fx, a, resume) = held("unknown").await;
    let before = op(&fx, &a.task.id).await;
    let id = serde_json::from_value(record(&before)["execution_id"].clone()).unwrap();
    // Ensure the child really crossed the fork barrier before killing the supervisor.
    wait_until(async || {
        matches!(
            fx.execution
                .lifecycle
                .engine()
                .unwrap()
                .output(&id, None)
                .await,
            Ok(ExecutionReply::Observation {
                state: ExecutionState::Spawned,
                ..
            })
        )
    })
    .await;
    fx.execution.restart().await;
    assert!(matches!(
        fx.execution
            .lifecycle
            .engine()
            .unwrap()
            .output(&id, None)
            .await
            .unwrap(),
        ExecutionReply::Observation {
            state: ExecutionState::Unknown { .. },
            ..
        }
    ));
    boot(&fx).await;
    resume.notify_waiters();
    for _ in 0..3 {
        fx.runtime.sweep_parked().await.unwrap();
        assert_eq!(op(&fx, &a.task.id).await.phase.tag(), PhaseTag::Parked);
    }
    assert_eq!(record(&op(&fx, &a.task.id).await)["release"], "Held");
    assert!(
        fx.runtime
            .operation_result(&before.id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(execution_count(&fx, &before.id).await, 1);
}

#[tokio::test]
async fn c2b_lost_close_ack_does_not_repeat_steps() {
    let fx = world(Duration::from_millis(100)).await;
    let counter = outside(&fx, "close-counter");
    let flag = outside(&fx, "close-release");
    let a = running(&fx, "close-ack", json!([{"name":"once","cmd":format!("echo once >> '{}'; {}",counter.display(),wait_for(&flag))}])).await;
    gate_run(&fx, &a, Some("test: close acknowledgement replay"))
        .await
        .unwrap();
    wait_step(&fx, &a.task.id, 1, "once").await;
    let before = op(&fx, &a.task.id).await;
    let id = serde_json::from_value(record(&before)["execution_id"].clone()).unwrap();
    let engine = fx.execution.lifecycle.engine().unwrap();
    // Discard the first ACK and repeat through Engine's fresh protocol connection.
    engine.close_stdin(&id).await.unwrap();
    engine.close_stdin(&id).await.unwrap();
    assert_eq!(std::fs::read_to_string(&counter).unwrap(), "once\n");
    touch(&flag);
    assert!(run_result(&fx, &a.task.id, 1).await.verdict.passed);
    assert_eq!(execution_count(&fx, &before.id).await, 1);
}

#[tokio::test]
async fn c2b_typed_gate_op_occupies_until_settled() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (pause, entered, _resume) = pause();
    let releases = Arc::new(AtomicUsize::new(0));
    let hook: Hook = Arc::new(move || {
        let pause = pause.clone();
        let n = releases.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if n > 0 {
                pause().await;
            }
        })
    });
    let fx = world_with(Duration::from_millis(100), |b| {
        hooked(b, |a| a.with_before_release(hook))
    })
    .await;
    let a = running(&fx, "occupancy", json!([{"name":"ok","cmd":"true"}])).await;
    std::fs::write(a.lease.path.join("candidate.txt"), "candidate\n").unwrap();
    gate_run(&fx, &a, Some("test: occupancy")).await.unwrap();
    let result = run_result(&fx, &a.task.id, 1).await;
    assert!(result.verdict.passed);
    let first = op(&fx, &a.task.id).await;
    gate_run(&fx, &a, None).await.unwrap();
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .unwrap();
    let second = fx
        .runtime
        .find_by_kind_and_idempotency(TASK_GATE_RUN_KIND, &format!("{}#r2", a.task.id))
        .await
        .unwrap()
        .unwrap();
    // A terminal phase cannot replace the authoritative executor settlement.
    sqlx::query("UPDATE operations SET phase='succeeded',tx_output_json=json_set(tx_output_json,'$.result',json(?)) WHERE id=?")
        .bind(serde_json::to_string(&result).unwrap()).bind(&second.id).execute(&fx.pool()).await.unwrap();
    let error = gate_run(&fx, &a, None).await.unwrap_err();
    assert!(
        error.message.contains("could not be proven stopped"),
        "{error:?}"
    );
    fx.complete(&a.worker, &a.task.id, None).await;
    fx.wait_forge_op(&a.task.id).await;
    let delivery = fx.delivery_row(&a.task.id).await.unwrap();
    fx.scheduler()
        .settle_git_delivery_for_test(&delivery.delivery_id)
        .await
        .unwrap();
    assert_eq!(
        fx.delivery_row(&a.task.id)
            .await
            .unwrap()
            .settlement
            .as_deref(),
        Some("candidate")
    );
    // Pin reuse with a positive control: only the typed settlement fact changes.
    let task = current(&fx.boot, "occupancy").await;
    assert!(!super::reuse::prepare_without_commit(&fx, &task).await);
    let mut clear = record(&second);
    clear["execution_id"] = record(&first)["execution_id"].clone();
    rewrite(&fx, &second.id, &clear).await;
    assert!(super::reuse::prepare_without_commit(&fx, &task).await);
    rewrite(&fx, &second.id, &record(&second)).await;
    fx.scheduler().drive_gate_for_test(task).await.unwrap();
    assert_eq!(reused_run(&verdict(&fx, "occupancy").await), None);
    sqlx::query("UPDATE tasks SET status='failed',status_detail='gate-red' WHERE id=?")
        .bind(&a.task.id)
        .execute(&fx.pool())
        .await
        .unwrap();
    let error = call_tool(
        &fx.boot,
        "neige_task_regate",
        planner_identity(&fx.boot),
        json!({"attempt_id":a.task.id,"message":"retry"}),
    )
    .await
    .unwrap_err();
    assert!(
        error.message.contains("could not be proven stopped"),
        "{error:?}"
    );
}

#[path = "c2b/retention.rs"]
mod retention;
