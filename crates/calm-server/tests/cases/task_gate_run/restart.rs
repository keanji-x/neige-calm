//! #2464 slice 1, out of process (D9): a run across a kernel restart. The world is a file database
//! the shipped kernel binary is launched against; the in-process kernel stays passive (its live
//! listener stopped, its sweeps boot-gated) and only seeds rows through production paths. The run
//! is admitted through production paths: boot recovery for pre-release crashes and live MCP
//! admission for the running-step crash.
#![cfg(target_os = "linux")]
use std::ffi::OsString;

use super::*;
use crate::gate_binding::{FileWorld, claim_with_closure, file_world};
use crate::support::kernel_proc::{
    ChildGuard, free_port_or_skip, spawn_kernel_to_with_args, wait_exit_with_timeout, wait_ready,
};
use calm_server::operation::{Operation, PhaseTag};

fn execution_args(world: &FileWorld) -> Vec<OsString> {
    vec![
        "--kernel-client-key-file".into(),
        world.fx.execution.key.clone().into(),
        "--proc-supervisor-sock".into(),
        world.fx.execution.socket.clone().into(),
    ]
}
fn launch(world: &FileWorld, name: &str) -> Option<ChildGuard> {
    let port = free_port_or_skip(name)?;
    let mut child = ChildGuard {
        child: spawn_kernel_to_with_args(
            &world.tmp_path,
            &world.db_path,
            port,
            &[],
            None,
            &execution_args(world),
        ),
        port,
    };
    assert!(
        wait_ready(&mut child.child, port).is_ok(),
        "kernel failed before ready"
    );
    Some(child)
}
fn typed_record(op: &Operation) -> Value {
    op.tx_output.as_ref().unwrap().data["gate_execution"].clone()
}
fn infra_result(op: &Operation) {
    let result: GateRunResult =
        serde_json::from_value(op.tx_output.as_ref().unwrap().result.clone()).unwrap();
    assert!(!result.verdict.passed, "{result:?}");
    assert_eq!(result.verdict.status_detail.as_deref(), Some("gate-infra"));
}

/// Creates `flag` on drop (panic or early return included), so a step waiting on it ends before
/// its tempdir goes.
struct Release(PathBuf);

impl Drop for Release {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.0, b"");
    }
}

/// A running attempt of `key`, claimed with its context closure (so the launched kernel's boot
/// context sweep keeps it), in the track's checkout under a kernel-delivery lease.
async fn attempt(world: &FileWorld, key: &str, steps: Value) -> Attempt {
    let fx = &world.fx;
    let worker = fx.new_worker(key, AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&worker.card_id).await;
    declare(
        &fx.boot,
        json!({"key": key, "kind": "codex", "goal": format!("run {key}"),
               "declared_by": PLANNER_DECLARATION_AUTHOR, "ready": true,
               "gate": {"steps": steps, "timeout_secs": 600}}),
    )
    .await;
    let task = claim_with_closure(fx, key, &worker.card_id).await;
    Attempt {
        worker,
        lease,
        task,
    }
}

async fn admitted(world: &FileWorld, key: &str, steps: Value) -> (Attempt, String) {
    let a = attempt(world, key, steps).await;
    let fx = &world.fx;
    let run = calm_server::test_seams::admit_gate_run_for_test(
        &fx.db(),
        &a.task.id,
        &a.worker.card_id,
        fx.track(),
        calm_server::decision_sink::CommitMessage::parse("test: checkpoint restarted worker")
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(run, format!("{}#r1", a.task.id));
    (a, run)
}

async fn run_op(fx: &Fx, key: &str) -> Operation {
    fx.runtime
        .find_by_kind_and_idempotency(TASK_GATE_RUN_KIND, key)
        .await
        .unwrap()
        .expect("the run's op row")
}

async fn wait_run_op(
    fx: &Fx,
    key: &str,
    what: &str,
    ready: impl Fn(&Operation) -> bool,
) -> Operation {
    let mut last = None;
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Some(op) = fx
                .runtime
                .find_by_kind_and_idempotency(TASK_GATE_RUN_KIND, key)
                .await
                .unwrap()
            {
                last = Some(op.clone());
                if ready(&op) {
                    break op;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{key} never reached {what}: {last:?}"))
}

fn log_of(world: &FileWorld, task_id: &str) -> String {
    std::fs::read_to_string(
        world
            .tmp_path
            .join("data/gate-logs")
            .join(format!("{task_id}-r1.log")),
    )
    .unwrap_or_default()
}

/// boot#1, made to abort at `seam`; returns once it died there.
fn crash_at(world: &FileWorld, port: u16, seam: &str) {
    let log = world.tmp_path.join(format!("{seam}.log"));
    let mut boot1 = ChildGuard {
        child: spawn_kernel_to_with_args(
            &world.tmp_path,
            &world.db_path,
            port,
            &[("CALM_TEST_CRASH_AT", OsString::from(seam))],
            Some(&log),
            &execution_args(world),
        ),
        port,
    };
    let status = wait_exit_with_timeout(&mut boot1, Duration::from_secs(60));
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(libc::SIGABRT),
        "boot#1 must die at the seam, got {status:?}"
    );
    let output = std::fs::read_to_string(&log).unwrap();
    assert!(
        output.contains(&format!("CALM_TEST_CRASH_AT={seam}: aborting")),
        "boot#1 died at {seam}:\n{output}"
    );
}

// ---------------------------------------------------------------------------
// R8a (D9, P10): a restart before the park re-runs nothing; the re-drive stops the
// same never-released execution and the run completes once as infra.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_before_the_park_reruns_nothing() {
    let Some(port) = free_port_or_skip("gate-run-pre-park") else {
        return; // SKIP was printed: sandbox denied loopback bind
    };
    let world = file_world().await;
    let fx = &world.fx;
    let ran = world.tmp_path.join("r8a-step-ran");
    let (_, key) = admitted(
        &world,
        "r8a",
        json!([{"name": "mark", "cmd": format!("echo ran >> '{}'", ran.display())}]),
    )
    .await;

    crash_at(&world, port, "task-gate-run-pre-park");
    let op = run_op(fx, &key).await;
    assert_eq!(op.phase.tag(), PhaseTag::SpawnStarted, "{op:?}");
    assert!(op.spawn_artifacts.is_none());
    assert_eq!(typed_record(&op)["release"], "Held");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!ran.exists(), "nothing was released before the park");

    let Some(mut boot2) = launch(&world, "boot-2") else {
        return; // SKIP was printed
    };
    let op = wait_run_op(fx, &key, "a terminal phase", |op| {
        matches!(
            op.phase.tag(),
            PhaseTag::Succeeded | PhaseTag::Failed | PhaseTag::Stuck
        )
    })
    .await;
    boot2.sigkill_and_reap();
    assert_eq!(op.phase.tag(), PhaseTag::Succeeded, "{op:?}");
    let result: GateRunResult = serde_json::from_value(op.tx_output.unwrap().result).unwrap();
    assert!(!result.verdict.passed, "{result:?}");
    assert_eq!(result.verdict.status_detail.as_deref(), Some("gate-infra"));
    assert!(
        !ran.exists(),
        "redrive never releases or repeats the wrapper"
    );
    assert_eq!(current(&fx.boot, "r8a").await.status, TaskStatus::Running);
}

// ---------------------------------------------------------------------------
// R8b (D9, P11): a restart after the park, before release: boot stops the held wrapper
// without a token, and the run ends as infra.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_after_the_park_ends_the_run_as_infra() {
    let Some(port) = free_port_or_skip("gate-run-post-park") else {
        return; // SKIP was printed: sandbox denied loopback bind
    };
    let world = file_world().await;
    let fx = &world.fx;
    let flag = world.tmp_path.join("r8b-release");
    let _release = Release(flag.clone());
    let (a, key) = admitted(
        &world,
        "r8b",
        json!([{"name": "block", "cmd": wait_for(&flag)}]),
    )
    .await;
    std::fs::write(a.lease.path.join("r8b.txt"), "never committed\n").unwrap();

    crash_at(&world, port, "task-gate-run-post-park");
    let op = run_op(fx, &key).await;
    assert_eq!(op.phase.tag(), PhaseTag::Parked, "{op:?}");
    tokio::time::sleep(Duration::from_secs(2)).await;

    let Some(mut boot2) = launch(&world, "boot-2") else {
        return; // SKIP was printed
    };
    let op = wait_run_op(fx, &key, "failed", |op| {
        op.phase.tag() == PhaseTag::Succeeded
    })
    .await;
    boot2.sigkill_and_reap();
    infra_result(&op);
    let log = log_of(&world, &a.task.id);
    assert!(!log.contains("::gate-step"), "no step ran:\n{log}");
    assert_eq!(git(&a.lease.path, &["rev-parse", "HEAD"]), a.lease.base_sha);
    assert_eq!(
        git(&a.lease.path, &["status", "--porcelain"]),
        "?? r8b.txt",
        "HEAD unmoved, nothing staged"
    );
    let next = calm_server::test_seams::admit_gate_run_for_test(
        &fx.db(),
        &a.task.id,
        &a.worker.card_id,
        fx.track(),
        calm_server::decision_sink::CommitMessage::parse("test: checkpoint restarted worker")
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(next, format!("{}#r2", a.task.id), "the next call is run 2");
}

// ---------------------------------------------------------------------------
// R8c (D9, P12): the kernel is killed while a step runs; the step keeps running across the
// restart, and the next boot stops its group and ends the run as infra.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_during_a_step_ends_it_as_infra() {
    if free_port_or_skip("gate-run-restart-during-step").is_none() {
        return; // SKIP was printed: sandbox denied loopback bind
    }
    let world = file_world().await;
    let fx = &world.fx;
    let flag = world.tmp_path.join("r8c-never-released");
    let _release = Release(flag.clone());
    let Some(mut boot1) = launch(&world, "boot-1") else {
        return; // SKIP was printed
    };
    // Create the live session after boot recovery has invalidated pre-boot sessions.
    let a = attempt(
        &world,
        "r8c",
        json!([{"name": "block", "cmd": wait_for(&flag)}]),
    )
    .await;

    let key = format!("{}#r1", a.task.id);
    let token = calm_server::mcp_server::auth::CardMcpToken::generate();
    let hash = calm_server::mcp_server::auth::hash_token(token.as_str());
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&fx.db())
        .await
        .unwrap();
    calm_server::db::sqlite::card_mcp_token_set_tx(&mut tx, &a.worker.card_id, &hash)
        .await
        .unwrap();
    calm_server::db::sqlite::session_mcp_token_set_tx(&mut tx, &a.worker.session_id, &hash)
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET session_id=? WHERE id=?")
        .bind(&a.worker.session_id)
        .bind(&a.worker.card_id)
        .execute(&mut **tx)
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET thread_id=? WHERE id=?")
        .bind(&a.worker.thread_id)
        .bind(&a.worker.session_id)
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let _request = crate::support::mcp::send_tool_call_without_reply(
        &world.tmp_path.join("data/mcp/kernel.sock"),
        token.as_str(),
        &a.worker.thread_id,
        2,
        GATE,
        json!({"attempt_id":a.task.id,"commit_message":"test: checkpoint restarted worker"}),
    )
    .await;
    let op = wait_run_op(fx, &key, "parked", |op| {
        op.phase.tag() == PhaseTag::Parked && typed_record(op)["execution_id"].is_string()
    })
    .await;
    let execution: workspace_runtime::execution::ExecutionId =
        serde_json::from_value(typed_record(&op)["execution_id"].clone()).unwrap();
    tokio::time::timeout(WAIT, async {
        while !log_of(&world, &a.task.id).contains("::gate-step block") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the step started");
    boot1.sigkill_and_reap();
    assert!(matches!(
        fx.execution
            .lifecycle
            .engine()
            .unwrap()
            .output(&execution, None)
            .await
            .unwrap(),
        workspace_runtime::execution::ExecutionReply::Observation {
            state: workspace_runtime::execution::ExecutionState::Spawned,
            ..
        }
    ));

    let Some(mut boot2) = launch(&world, "boot-2") else {
        return; // SKIP was printed
    };
    let op = wait_run_op(fx, &key, "failed", |op| {
        op.phase.tag() == PhaseTag::Succeeded
    })
    .await;
    boot2.sigkill_and_reap();
    infra_result(&op);
    let row: Option<String> =
        sqlx::query_scalar("SELECT settlement_json FROM workspace_executions WHERE execution_id=?")
            .bind(execution.as_str())
            .fetch_one(&fx.pool())
            .await
            .unwrap();
    assert!(row.is_some(), "the executor durably drained before verdict");
    assert!(!flag.exists(), "the step was never released");
}
