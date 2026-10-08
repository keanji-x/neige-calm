//! #2464 slice 2: the first gate of an attempt reuses the attempt's last run when it passed on the
//! delivered commit and the remote-tracking refs and tags did not move (§4); otherwise the gate
//! runs as before. Each test drives the real tool, the real delivery and the scheduler's gate drive.
use std::os::unix::fs::MetadataExt;

use super::*;
use calm_server::db::sqlite::begin_immediate_tx;
use calm_server::ids::ActorId;
use calm_server::operation::task_verify_adapter::{
    TASK_VERIFY_KIND, TaskVerifyAdapter, TaskVerifyOperationPayload,
};
use calm_server::operation::{Operation, Phase};

/// Report `a` done and settle its delivery: the forge Operation to terminal, then the settlement
/// step the stopped listener would have run.
async fn done_and_settled(fx: &Fx, a: &Attempt) {
    fx.complete(&a.worker, &a.task.id).await;
    fx.wait_forge_op(&a.task.id).await;
    let row = fx.delivery_row(&a.task.id).await.expect("delivery row");
    fx.scheduler()
        .settle_git_delivery_for_test(&row.delivery_id)
        .await
        .unwrap();
    let row = fx.delivery_row(&a.task.id).await.unwrap();
    assert_eq!(row.settlement.as_deref(), Some("candidate"), "{row:?}");
}

/// [`done_and_settled`], then the scheduler's gate drive once.
async fn done_and_gated(fx: &Fx, a: &Attempt) -> TaskGateResult {
    done_and_settled(fx, a).await;
    let task = current(&fx.boot, &a.task.key).await;
    assert_eq!(task.status, TaskStatus::Verifying, "{task:?}");
    fx.scheduler().drive_gate_for_test(task).await.unwrap();
    verdict(fx, &a.task.key).await
}

fn gate_file(fx: &Fx, task_id: &str, stem: &str) -> PathBuf {
    fx.boot.ctx.gate_logs_dir.join(format!("{task_id}-{stem}"))
}

/// The gate's held wrapper script was written: a gate process was spawned for gate attempt `n`.
fn gate_spawned(fx: &Fx, task_id: &str, n: i64) -> bool {
    gate_file(fx, task_id, &format!("g{n}.sh")).exists()
}

fn assert_gate_ran(fx: &Fx, a: &Attempt, verdict: &TaskGateResult, n: i64) {
    assert_eq!(reused_run(verdict), None, "{verdict:?}");
    assert!(
        matches!(
            &verdict.target,
            VerifyTarget::Candidate {
                evidence: VerifyTargetEvidence::Verified { .. },
                ..
            }
        ),
        "{verdict:?}"
    );
    assert!(gate_spawned(fx, &a.task.id, n), "#g{n} spawned a process");
}

fn update_ref(a: &Attempt, name: &str, target: &str) {
    git(&a.lease.path, &["update-ref", name, target]);
}

const ORIGIN_MAIN: &str = "refs/remotes/origin/main";

// ---------------------------------------------------------------------------
// U1: an unchanged candidate reuses the passing run; its log keeps the gate's address.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unchanged_candidate_reuses_the_passing_run() {
    let fx = world(LONG_IDLE).await;
    let a = running(
        &fx,
        "u1",
        json!([{"name": "say", "cmd": "echo u1-run-output"}]),
    )
    .await;
    std::fs::write(a.lease.path.join("u1.txt"), "u1\n").unwrap();
    let commit = passed(&gate_run(&fx, &a, Some("Deliver u1")).await.unwrap()).to_string();

    let verdict = done_and_gated(&fx, &a).await;

    assert_eq!(current(&fx.boot, "u1").await.status, TaskStatus::Done);
    assert_eq!(
        reused_run(&verdict),
        Some(format!("{}#r1", a.task.id).as_str()),
        "{verdict:?}"
    );
    let VerifyTarget::Candidate { commit_sha, .. } = &verdict.target else {
        panic!("{verdict:?}");
    };
    assert_eq!(commit_sha, &commit);
    assert!(
        verdict.verdict.passed && verdict.verdict.attempt == 1,
        "{verdict:?}"
    );
    assert!(
        !gate_spawned(&fx, &a.task.id, 1),
        "no #g1 wrapper script was written"
    );
    assert_eq!(
        Path::new(&verdict.verdict.log_path),
        gate_file(&fx, &a.task.id, "g1.log")
    );
    let run_log = std::fs::read_to_string(gate_file(&fx, &a.task.id, "r1.log")).unwrap();
    assert!(run_log.contains("u1-run-output"), "{run_log}");
    let cat = call_tool(
        &fx.boot,
        "neige_track_cat",
        planner_identity(&fx.boot),
        json!({"path": format!("runs/{}/gates/1.log", a.task.id)}),
    )
    .await
    .unwrap();
    assert_eq!(cat["content"], json!(run_log), "{cat}");
}

// ---------------------------------------------------------------------------
// U2: a change after the run makes a new commit, so the gate runs.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_change_after_the_run_runs_the_gate() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "u2", json!([{"name": "fine", "cmd": "true"}])).await;
    std::fs::write(a.lease.path.join("u2.txt"), "u2\n").unwrap();
    let commit = passed(&gate_run(&fx, &a, Some("Deliver u2")).await.unwrap()).to_string();
    std::fs::write(a.lease.path.join("u2-later.txt"), "after the run\n").unwrap();

    let verdict = done_and_gated(&fx, &a).await;

    assert_ne!(
        fx.candidate_row(&a.task.id).await.unwrap().commit_sha,
        commit,
        "the delivery committed the later change"
    );
    assert_gate_ran(&fx, &a, &verdict, 1);
    assert_eq!(current(&fx.boot, "u2").await.status, TaskStatus::Done);
}

// ---------------------------------------------------------------------------
// U3: only the last run counts: a red run after a passing one on the same commit is not reused.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_red_last_run_is_not_reused() {
    let fx = world(LONG_IDLE).await;
    let counter = outside(&fx, "u3-counter");
    let count = format!(
        "n=$(cat '{c}' 2>/dev/null || echo 0); n=$((n+1)); echo $n > '{c}'; test $n -ne 2",
        c = counter.display()
    );
    let a = running(&fx, "u3", json!([{"name": "count", "cmd": count}])).await;
    // A clean tree: both runs are on the lease base, the commit the delivery then pins.
    let first = passed(&gate_run(&fx, &a, None).await.unwrap()).to_string();
    let second = gate_run(&fx, &a, None).await.unwrap();
    assert_eq!(second["status_detail"], "gate-red", "{second}");
    assert_eq!(second["commit"], json!(first), "{second}");
    let entry = fx.plan_entry("u3").await;
    assert_eq!(
        entry["candidate"]["verification"]["gate_runs"],
        json!({"used": 2, "max": 5, "last": {"run": 2, "commit": first, "passed": false,
               "status_detail": "gate-red", "failing_step": "count"}}),
        "{entry}"
    );

    let verdict = done_and_gated(&fx, &a).await;

    assert_eq!(
        fx.candidate_row(&a.task.id).await.unwrap().commit_sha,
        first
    );
    assert_gate_ran(&fx, &a, &verdict, 1);
    assert_eq!(std::fs::read_to_string(&counter).unwrap().trim(), "3");
    assert_eq!(current(&fx.boot, "u3").await.status, TaskStatus::Done);
}

// ---------------------------------------------------------------------------
// U4: a remote-tracking ref that moved after the run makes the gate run.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_moved_remote_ref_runs_the_gate() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "u4", json!([{"name": "fine", "cmd": "true"}])).await;
    update_ref(&a, ORIGIN_MAIN, &a.lease.base_sha);
    std::fs::write(a.lease.path.join("u4.txt"), "u4\n").unwrap();
    let commit = passed(&gate_run(&fx, &a, Some("Deliver u4")).await.unwrap()).to_string();
    update_ref(&a, ORIGIN_MAIN, &commit);

    let verdict = done_and_gated(&fx, &a).await;

    assert_eq!(
        fx.candidate_row(&a.task.id).await.unwrap().commit_sha,
        commit
    );
    assert_gate_ran(&fx, &a, &verdict, 1);
}

// ---------------------------------------------------------------------------
// U5: a regate never reuses, even when the run would match again.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_regate_never_reuses() {
    let fx = world(LONG_IDLE).await;
    let worker = fx.new_worker("u5", AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&worker.card_id).await;
    let base = lease.base_sha.clone();
    let step = format!("test \"$(git rev-parse {ORIGIN_MAIN})\" = {base}");
    let task = fx
        .running_task(
            "u5",
            "codex",
            &worker.card_id,
            json!({"gate": {"steps": [{"name": "ref", "cmd": step}], "timeout_secs": 120},
                   "no_gate_reason": null}),
        )
        .await;
    let a = Attempt {
        worker,
        lease,
        task,
    };
    update_ref(&a, ORIGIN_MAIN, &base);
    std::fs::write(a.lease.path.join("u5.txt"), "u5\n").unwrap();
    let commit = passed(&gate_run(&fx, &a, Some("Deliver u5")).await.unwrap()).to_string();
    update_ref(&a, ORIGIN_MAIN, &commit);

    let first = done_and_gated(&fx, &a).await;

    assert_gate_ran(&fx, &a, &first, 1);
    let row = current(&fx.boot, "u5").await;
    assert_eq!(
        (row.status, row.status_detail.as_deref()),
        (TaskStatus::Failed, Some("gate-red"))
    );
    update_ref(&a, ORIGIN_MAIN, &base);
    call_tool(
        &fx.boot,
        "neige_task_regate",
        planner_identity(&fx.boot),
        json!({"attempt_id": a.task.id, "message": "the ref is back"}),
    )
    .await
    .unwrap();
    let task = current(&fx.boot, "u5").await;
    fx.scheduler().drive_gate_for_test(task).await.unwrap();

    let again = verdict(&fx, "u5").await;
    assert_eq!(again.verdict.attempt, 3, "{again:?}");
    assert_gate_ran(&fx, &a, &again, 3);
    assert_eq!(current(&fx.boot, "u5").await.status, TaskStatus::Done);
}

// ---------------------------------------------------------------------------
// U6: `origin/HEAD` retargeted between two refs at one commit makes the gate run.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retargeted_origin_head_runs_the_gate() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "u6", json!([{"name": "fine", "cmd": "true"}])).await;
    update_ref(&a, ORIGIN_MAIN, &a.lease.base_sha);
    update_ref(&a, "refs/remotes/origin/other", &a.lease.base_sha);
    let origin_head = ["symbolic-ref", "refs/remotes/origin/HEAD"];
    git(
        &a.lease.path,
        &[origin_head[0], origin_head[1], ORIGIN_MAIN],
    );
    std::fs::write(a.lease.path.join("u6.txt"), "u6\n").unwrap();
    passed(&gate_run(&fx, &a, Some("Deliver u6")).await.unwrap());
    git(
        &a.lease.path,
        &[origin_head[0], origin_head[1], "refs/remotes/origin/other"],
    );

    let verdict = done_and_gated(&fx, &a).await;

    assert_gate_ran(&fx, &a, &verdict, 1);
}

// ---------------------------------------------------------------------------
// U7: a re-prepared reuse neither fails nor truncates the run's log.
// ---------------------------------------------------------------------------

/// A `#g1` prepare through the production adapter whose transaction then rolls back, as a prepare
/// that did not commit leaves it; returns whether it reused.
async fn prepare_without_commit(fx: &Fx, task: &Task) -> bool {
    let adapter = TaskVerifyAdapter::new(fx.boot.ctx.gate_logs_dir.clone());
    let payload = serde_json::to_value(TaskVerifyOperationPayload {
        actor: ActorId::KernelDispatcher,
        track_id: task.track_id.clone(),
        task_id: task.id.clone(),
        attempt: 1,
    })
    .unwrap();
    let op = Operation {
        id: "u7-uncommitted-prepare".into(),
        operation_key: "u7-uncommitted-prepare".into(),
        kind: TASK_VERIFY_KIND.into(),
        idempotency_key: Some(format!("{}#g1", task.id)),
        payload_hash: String::new(),
        target_type: "track".into(),
        target_id: Some(task.track_id.clone()),
        target: json!({}),
        payload: payload.clone(),
        tx_output: None,
        phase: Phase::Pending,
        phase_detail: None,
        attempt: 0,
        last_error: None,
        compensation_state: None,
        lease_owner: None,
        lease_until_ms: None,
        spawn_artifacts: None,
        parked_at_ms: None,
        parked_deadline_ms: None,
    };
    let pool = fx.pool();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    let output = adapter.prepare_tx(&mut tx, &payload, &op).await.unwrap();
    tx.rollback().await.unwrap();
    output.data["target"]["kind"] == "reused"
}

/// A passing run on a delivered, settled attempt of `key`, its gate not driven yet.
async fn delivered_after_a_passing_run(fx: &Fx, key: &str) -> Attempt {
    let a = running(
        fx,
        key,
        json!([{"name": "say", "cmd": "echo run-log-line"}]),
    )
    .await;
    std::fs::write(a.lease.path.join(format!("{key}.txt")), "x\n").unwrap();
    passed(&gate_run(fx, &a, Some("Deliver it")).await.unwrap());
    done_and_settled(fx, &a).await;
    a
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reprepared_reuse_neither_fails_nor_truncates_the_run_log() {
    // Part 1: a leftover link from the uncommitted prepare; the re-driven prepare still reuses.
    let fx = world(LONG_IDLE).await;
    let a = delivered_after_a_passing_run(&fx, "u7a").await;
    let (run_log, g1_log) = (
        gate_file(&fx, &a.task.id, "r1.log"),
        gate_file(&fx, &a.task.id, "g1.log"),
    );
    assert!(prepare_without_commit(&fx, &a.task).await, "it reused");
    assert!(g1_log.exists(), "the uncommitted prepare left its link");

    let task = current(&fx.boot, "u7a").await;
    fx.scheduler().drive_gate_for_test(task).await.unwrap();

    let first = verdict(&fx, "u7a").await;
    assert_eq!(
        reused_run(&first),
        Some(format!("{}#r1", a.task.id).as_str()),
        "{first:?}"
    );
    assert_eq!(
        std::fs::metadata(&g1_log).unwrap().ino(),
        std::fs::metadata(&run_log).unwrap().ino(),
        "the gate's log is the run's log"
    );

    // Part 2: a leftover link again, then the remote ref moves: the gate runs, into its own file.
    let fx = world(LONG_IDLE).await;
    let a = delivered_after_a_passing_run(&fx, "u7b").await;
    let (run_log, g1_log) = (
        gate_file(&fx, &a.task.id, "r1.log"),
        gate_file(&fx, &a.task.id, "g1.log"),
    );
    let run_bytes = std::fs::read(&run_log).unwrap();
    assert!(prepare_without_commit(&fx, &a.task).await, "it reused");
    update_ref(&a, ORIGIN_MAIN, &a.lease.base_sha);

    let task = current(&fx.boot, "u7b").await;
    fx.scheduler().drive_gate_for_test(task).await.unwrap();

    let second = verdict(&fx, "u7b").await;
    assert_gate_ran(&fx, &a, &second, 1);
    assert_eq!(
        std::fs::read(&run_log).unwrap(),
        run_bytes,
        "the run's log is unchanged"
    );
    assert_ne!(
        std::fs::metadata(&g1_log).unwrap().ino(),
        std::fs::metadata(&run_log).unwrap().ino()
    );
    let gate_log = std::fs::read_to_string(&g1_log).unwrap();
    assert!(gate_log.contains("::gate-step say"), "{gate_log}");
}
