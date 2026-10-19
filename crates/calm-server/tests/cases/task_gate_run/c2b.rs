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
    let (hook, entered, resume) = pause();
    let fx = world_with(LONG_IDLE, |b| hooked(b, |a| a.with_before_release(hook))).await;
    let a = running(&fx, "typed", json!([{"name":"ok","cmd":"true"}])).await;
    let call = gate_run(&fx, &a, Some("test: typed park"));
    tokio::pin!(call);
    tokio::select! { _=entered.notified()=>{}, result=&mut call=>panic!("unexpected completion: {result:?}") }
    let op = op(&fx, &a.task.id).await;
    assert_eq!(op.phase.tag(), PhaseTag::Parked);
    assert!(op.spawn_artifacts.is_none());
    assert!(record(&op)["execution_id"].is_string());
    assert_eq!(execution_count(&fx, &op.id).await, 1);
    resume.notify_one();
    call.await.unwrap();
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
    // Dropping the request cancels no execution or operation.
    drop(call);
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
    let (hook, entered, resume) = pause();
    let fx = world_with(LONG_IDLE, |b| hooked(b, |a| a.with_before_release(hook))).await;
    let a = running(&fx, "cancel-owned", json!([{"name":"ok","cmd":"true"}])).await;
    let call = gate_run(&fx, &a, Some("test: retain owned cancellation"));
    tokio::pin!(call);
    tokio::select! { _=entered.notified()=>{}, result=&mut call=>panic!("unexpected completion: {result:?}") }
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
    call.await.unwrap();
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

#[tokio::test]
async fn c2b_malformed_record_is_retained_with_backoff() {
    let (hook, entered, resume) = pause();
    let fx = world_with(LONG_IDLE, |b| hooked(b, |a| a.with_before_release(hook))).await;
    let a = running(&fx, "malformed", json!([{"name":"ok","cmd":"true"}])).await;
    let call = gate_run(&fx, &a, Some("test: retain malformed gate record"));
    tokio::pin!(call);
    tokio::select! { _=entered.notified()=>{}, result=&mut call=>panic!("unexpected completion: {result:?}") }
    let before = op(&fx, &a.task.id).await;
    rewrite(&fx, &before.id, &json!({"version":1,"execution_id":false})).await;
    resume.notify_one();
    wait_until(async || op(&fx, &a.task.id).await.last_error.is_some()).await;
    let retained = op(&fx, &a.task.id).await;
    assert_eq!(retained.phase.tag(), PhaseTag::Parked);
    assert!(retained.lease_owner.is_some());
    assert!(retained.lease_until_ms.unwrap() > calm_server::model::now_ms());
    assert!(
        retained
            .last_error
            .as_deref()
            .unwrap()
            .contains("owned execution retained")
    );
    assert_eq!(execution_count(&fx, &retained.id).await, 1);
    tokio::time::sleep(Duration::from_millis(150)).await;
    let later = op(&fx, &a.task.id).await;
    assert_eq!(
        later.lease_owner, retained.lease_owner,
        "no hot-loop reclaim"
    );
    assert_eq!(later.lease_until_ms, retained.lease_until_ms);
    drop(call);
}
