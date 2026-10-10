//! #2464 slice 1, out of process (D9): a run across a kernel restart. The world is a file database
//! the shipped kernel binary is launched against; the in-process kernel stays passive (its live
//! listener stopped, its sweeps boot-gated) and only seeds rows through production paths. The run
//! is admitted by the production admission transaction and driven by the launched kernel's boot
//! recovery.
#![cfg(target_os = "linux")]
use std::ffi::OsString;

use super::*;
use crate::gate_binding::{FileWorld, claim_with_closure, file_world};
use crate::support::kernel_proc::{
    ChildGuard, free_port_or_skip, launch_kernel, spawn_kernel_to, wait_exit_with_timeout,
};
use calm_server::operation::{Operation, PhaseTag};

/// Creates `flag` on drop (panic or early return included), so a step waiting on it ends before
/// its tempdir goes.
struct Release(PathBuf);

impl Drop for Release {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.0, b"");
    }
}

/// A running attempt of `key`, claimed with its context closure (so the launched kernel's boot
/// context sweep keeps it), in the track's checkout under a kernel-delivery lease, and its first
/// run admitted, not driven.
async fn admitted(world: &FileWorld, key: &str, steps: Value) -> (Attempt, String) {
    let fx = &world.fx;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id, key).await;
    declare(
        &fx.boot,
        json!({"key": key, "kind": "codex", "goal": format!("run {key}"),
               "declared_by": PLANNER_DECLARATION_AUTHOR, "ready": true,
               "gate": {"steps": steps, "timeout_secs": 600}}),
    )
    .await;
    let task = claim_with_closure(fx, key, &worker.card_id).await;
    let run = calm_server::test_seams::admit_gate_run_for_test(
        &fx.pool(),
        &task.id,
        &worker.card_id,
        &worker.session_id,
        fx.track(),
    )
    .await
    .unwrap();
    assert_eq!(run, format!("{}#r1", task.id));
    (
        Attempt {
            worker,
            lease,
            task,
        },
        run,
    )
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
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let op = run_op(fx, key).await;
            if ready(&op) {
                break op;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{key} never reached {what}"))
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
        child: spawn_kernel_to(
            &world.tmp_path,
            &world.db_path,
            port,
            &[("CALM_TEST_CRASH_AT", OsString::from(seam))],
            Some(&log),
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
// R8a (D9, P10): a restart before the park re-runs nothing; the re-drive replaces the
// never-released wrapper and the run completes once.
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
    assert!(op.spawn_artifacts.is_some(), "the wrapper was recorded");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!ran.exists(), "nothing was released before the park");

    let Some(mut boot2) = launch_kernel(&world.tmp_path, &world.db_path, "boot-2", &[]) else {
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
    assert!(result.verdict.passed, "{result:?}");
    assert_eq!(
        std::fs::read_to_string(&ran).unwrap(),
        "ran\n",
        "the step ran once"
    );
    assert_eq!(current(&fx.boot, "r8a").await.status, TaskStatus::Running);
}

// ---------------------------------------------------------------------------
// R8b (D9, P11): a restart after the park, before the release: the held wrapper exits at stdin
// EOF having run nothing, and the run ends as infra.
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

    let Some(mut boot2) = launch_kernel(&world.tmp_path, &world.db_path, "boot-2", &[]) else {
        return; // SKIP was printed
    };
    let op = wait_run_op(fx, &key, "failed", |op| op.phase.tag() == PhaseTag::Failed).await;
    boot2.sigkill_and_reap();
    assert!(
        op.last_error
            .as_deref()
            .is_some_and(|error| error.starts_with("gate-infra")),
        "{op:?}"
    );
    let log = log_of(&world, &a.task.id);
    assert!(!log.contains("::gate-step"), "no step ran:\n{log}");
    assert_eq!(git(&a.lease.path, &["rev-parse", "HEAD"]), a.lease.base_sha);
    assert_eq!(
        git(&a.lease.path, &["status", "--porcelain"]),
        "?? r8b.txt",
        "HEAD unmoved, nothing staged"
    );
    let next = calm_server::test_seams::admit_gate_run_for_test(
        &fx.pool(),
        &a.task.id,
        &a.worker.card_id,
        &a.worker.session_id,
        fx.track(),
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
    let (a, key) = admitted(
        &world,
        "r8c",
        json!([{"name": "block", "cmd": wait_for(&flag)}]),
    )
    .await;

    let Some(mut boot1) = launch_kernel(&world.tmp_path, &world.db_path, "boot-1", &[]) else {
        return; // SKIP was printed
    };
    let op = wait_run_op(fx, &key, "parked", |op| {
        op.phase.tag() == PhaseTag::Parked && op.spawn_artifacts.is_some()
    })
    .await;
    let artifacts = op.spawn_artifacts.clone().unwrap();
    tokio::time::timeout(WAIT, async {
        while !log_of(&world, &a.task.id).contains("::gate-step block") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the step started");
    boot1.sigkill_and_reap();
    assert!(
        calm_server::proc_identity::verify_owned_pid(
            artifacts.pid,
            artifacts.start_time,
            &artifacts.boot_id
        ),
        "the step keeps running across the restart"
    );

    let Some(mut boot2) = launch_kernel(&world.tmp_path, &world.db_path, "boot-2", &[]) else {
        return; // SKIP was printed
    };
    let op = wait_run_op(fx, &key, "failed", |op| op.phase.tag() == PhaseTag::Failed).await;
    boot2.sigkill_and_reap();
    assert_eq!(
        op.last_error.as_deref(),
        Some("gate-infra: the kernel restarted during the run"),
        "{op:?}"
    );
    tokio::time::timeout(WAIT, async {
        while calm_server::proc_identity::verify_owned_pid(
            artifacts.pid,
            artifacts.start_time,
            &artifacts.boot_id,
        ) || !calm_server::proc_identity::group_members_with_env_marker(
            artifacts.pgid,
            "NEIGE_GATE_OP",
            &key,
        )
        .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the restarted kernel stopped the run's group");
    assert!(!flag.exists(), "the step was never released");
}
