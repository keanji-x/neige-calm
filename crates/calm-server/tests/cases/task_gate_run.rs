//! #2464 slice 1: `neige_task_gate`. A gated worker asks; the kernel commits its checkout as the
//! attempt's one commit above the lease base, with the delivery's own script, and runs the task's
//! gate on it, in the lease checkout. Fixtures come from `git_delivery.rs`: real git, the real held
//! wrapper, a test-played worker calling the tool through its `ToolCallIdentity`, and a
//! fixtures-only wait bound (`AppContext::gate_run_wait`). Every test but R4 reads its result from
//! one call whose bound is longer than its run, or from the op row.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::git_delivery::*;
use crate::mcp_track_report::{Boot, boot, call_tool, planner_identity};
use crate::task_recovery::{current, declare};
use calm_server::ids::TrackId;
use calm_server::mcp_server::registry::ToolCallIdentity;
use calm_server::model::{Task, TaskStatus};
use calm_server::operation::ProviderAdapter;
use calm_server::operation::task_gate_run::{
    GateRunResult, GateRunWait, TASK_GATE_RUN_KIND, TaskGateRunAdapter,
};
use calm_server::operation::task_verify_adapter::TaskGateResult;
use calm_server::plugin_host::mcp::RpcError;
use calm_server::session_projection_repo::AgentProvider;
use calm_server::test_seams::KernelWorkspaceLease;
use calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR;
use calm_types::verify_target::{MismatchReason, VerifyTarget, VerifyTargetEvidence};
use futures::future::BoxFuture;
use serde_json::{Value, json};

const GATE: &str = "neige_task_gate";

/// An idle window whose wait (60 s) outlasts every run below.
const LONG_IDLE: Duration = Duration::from_secs(120);

type Hook = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

/// The git-delivery fixture with the wait of one call set from `idle`, its live listener stopped.
pub(super) async fn world(idle: Duration) -> Fx {
    world_with(idle, |_| Vec::new()).await
}

/// [`world`] with adapters that replace the defaults of their kind (a hooked gate-run adapter).
pub(super) async fn world_with(
    idle: Duration,
    extra: impl FnOnce(&Boot) -> Vec<Arc<dyn ProviderAdapter>>,
) -> Fx {
    let mut boot = boot().await;
    Arc::get_mut(&mut boot.ctx)
        .expect("the context is not shared yet")
        .gate_run_wait = GateRunWait::for_idle(idle);
    let fx = fixture_on_with_adapters(
        boot,
        |tmp| {
            let repo = tmp.join("repo");
            init_repo(&repo);
            repo
        },
        extra,
    )
    .await;
    fx.dispatcher.abort_event_listener_for_test();
    fx
}

fn hooked(
    boot: &Boot,
    hook: impl FnOnce(TaskGateRunAdapter) -> TaskGateRunAdapter,
) -> Vec<Arc<dyn ProviderAdapter>> {
    vec![Arc::new(hook(TaskGateRunAdapter::new(
        boot.ctx.gate_logs_dir.clone(),
    ))) as Arc<dyn ProviderAdapter>]
}

/// A one-shot pause: the hook signals `entered` and waits for `resume`.
pub(super) fn pause() -> (Hook, Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let (e, r) = (entered.clone(), resume.clone());
    let hook: Hook = Arc::new(move || {
        let (e, r) = (e.clone(), r.clone());
        Box::pin(async move {
            e.notify_one();
            r.notified().await;
        })
    });
    (hook, entered, resume)
}

pub(super) fn outside(fx: &Fx, name: &str) -> PathBuf {
    fx.track_root.parent().unwrap().join(name)
}

/// A step command that waits until `flag` exists (outside the checkout, so it never dirties it).
pub(super) fn wait_for(flag: &Path) -> String {
    format!("until [ -f '{}' ]; do sleep 0.05; done", flag.display())
}

pub(super) fn touch(path: &Path) {
    std::fs::write(path, b"").unwrap();
}

pub(super) struct Attempt {
    pub(super) worker: ToolCallIdentity,
    pub(super) lease: KernelWorkspaceLease,
    pub(super) task: Task,
}

/// A running codex attempt of `key` in the track's checkout under a kernel-delivery lease, gated by
/// `steps`.
pub(super) async fn running(fx: &Fx, key: &str, steps: Value) -> Attempt {
    let worker = fx.new_worker(key, AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&worker.card_id, key).await;
    let task = fx
        .running_task(
            key,
            "codex",
            &worker.card_id,
            json!({"gate": {"steps": steps, "timeout_secs": 120}, "no_gate_reason": null}),
        )
        .await;
    Attempt {
        worker,
        lease,
        task,
    }
}

pub(super) async fn gate_run(
    fx: &Fx,
    attempt: &Attempt,
    message: Option<&str>,
) -> Result<Value, RpcError> {
    let mut args = json!({"attempt_id": attempt.task.id});
    if let Some(message) = message {
        args["commit_message"] = json!(message);
    }
    call_tool(&fx.boot, GATE, attempt.worker.clone(), args).await
}

/// `(key, phase)` of every run op of `task_id`.
pub(super) async fn run_ops(fx: &Fx, task_id: &str) -> Vec<(String, String)> {
    let mut ops: Vec<(String, String)> = sqlx::query_as(
        "SELECT idempotency_key, phase FROM operations WHERE kind = ?1 \
         AND substr(idempotency_key, 1, length(?2)) = ?2",
    )
    .bind(TASK_GATE_RUN_KIND)
    .bind(format!("{task_id}#r"))
    .fetch_all(&fx.pool())
    .await
    .unwrap();
    ops.sort();
    ops
}

/// Run `run`'s result from its op row, once the op is terminal.
pub(super) async fn run_result(fx: &Fx, task_id: &str, run: i64) -> GateRunResult {
    let key = format!("{task_id}#r{run}");
    let op = fx
        .runtime
        .find_by_kind_and_idempotency(TASK_GATE_RUN_KIND, &key)
        .await
        .unwrap()
        .expect("the run's op row");
    tokio::time::timeout(WAIT, async {
        while fx.runtime.operation_result(&op.id).await.unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the run ends");
    let op = fx
        .runtime
        .find_by_kind_and_idempotency(TASK_GATE_RUN_KIND, &key)
        .await
        .unwrap()
        .unwrap();
    serde_json::from_value(op.tx_output.expect("tx_output").result)
        .unwrap_or_else(|error| panic!("{key}: not a succeeded run result: {error}"))
}

/// Wait until run `run`'s wrapper started the step `name`.
pub(super) async fn wait_step(fx: &Fx, task_id: &str, run: i64, name: &str) {
    let log = fx
        .boot
        .ctx
        .gate_logs_dir
        .join(format!("{task_id}-r{run}.log"));
    let line = format!("::gate-step {name}");
    tokio::time::timeout(WAIT, async {
        while !std::fs::read_to_string(&log).is_ok_and(|text| text.contains(&line)) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("run {run} never started step {name}"));
}

/// The persisted verdict of `key`'s current attempt.
pub(super) async fn verdict(fx: &Fx, key: &str) -> TaskGateResult {
    let raw = current(&fx.boot, key)
        .await
        .gate_result_json
        .unwrap_or_else(|| panic!("{key} has no verdict"));
    serde_json::from_str(&raw).unwrap()
}

/// The run a verdict reused (#2464 slice 2), or `None` when its gate ran.
pub(super) fn reused_run(verdict: &TaskGateResult) -> Option<&str> {
    match &verdict.target {
        VerifyTarget::Candidate {
            evidence: VerifyTargetEvidence::Reused { run, .. },
            ..
        } => Some(run),
        _ => None,
    }
}

pub(super) fn passed(answer: &Value) -> &str {
    assert_eq!(answer["state"], "finished", "{answer}");
    assert_eq!(answer["passed"], true, "{answer}");
    answer["commit"]
        .as_str()
        .expect("a passed run names its commit")
}

// ---------------------------------------------------------------------------
// R1: the checkpoint commits the checkout before the declared steps run.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_gate_run_commits_new_files_before_its_steps_run() {
    let fx = world(LONG_IDLE).await;
    let a = running(
        &fx,
        "r1",
        json!([{"name": "tracked",
                "cmd": "git ls-files --error-unmatch new.txt && test -z \"$(git status --porcelain)\""}]),
    )
    .await;
    std::fs::write(a.lease.path.join("new.txt"), "new\n").unwrap();
    let message = "Add new.txt\n\nThe run's checkpoint commits it.\n\nRefs: #2464";

    let answer = gate_run(&fx, &a, Some(message)).await.unwrap();

    let commit = passed(&answer).to_string();
    assert_eq!(
        (&answer["run"], &answer["runs_used"], &answer["runs_max"]),
        (&json!(1), &json!(1), &json!(5)),
        "{answer}"
    );
    let path = &a.lease.path;
    assert_eq!(git(path, &["rev-parse", "HEAD"]), commit);
    assert_eq!(
        git(
            path,
            &["rev-parse", &format!("refs/heads/{}", fx.worker_branch())]
        ),
        commit,
        "the branch is at the run's commit"
    );
    assert_eq!(git(path, &["rev-parse", "HEAD^"]), a.lease.base_sha);
    assert_eq!(git(path, &["log", "-1", "--format=%B"]), message);
    assert!(git(path, &["status", "--porcelain"]).is_empty());
}

// ---------------------------------------------------------------------------
// R2: a red step answers with its name and tail; the attempt keeps running.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_red_step_returns_its_name_and_tail() {
    let fx = world(LONG_IDLE).await;
    let a = running(
        &fx,
        "r2",
        json!([{"name": "fine", "cmd": "true"},
               {"name": "lint", "cmd": "echo lint-broke-here; exit 3"}]),
    )
    .await;

    let answer = gate_run(&fx, &a, None).await.unwrap();

    assert_eq!(answer["state"], "finished", "{answer}");
    assert_eq!(answer["passed"], false, "{answer}");
    assert_eq!(answer["status_detail"], "gate-red", "{answer}");
    assert_eq!(answer["failing_step"], "lint", "{answer}");
    assert_eq!(answer["exit_code"], 3, "{answer}");
    assert!(
        answer["log_tail"]
            .as_str()
            .unwrap()
            .contains("lint-broke-here"),
        "{answer}"
    );
    assert_eq!(
        answer["commit"], a.lease.base_sha,
        "an empty change set makes no commit"
    );
    assert_eq!(current(&fx.boot, "r2").await.status, TaskStatus::Running);
    assert!(fx.events_for(GATE_RESULT_KIND, &a.task.id).await.is_empty());
}

// ---------------------------------------------------------------------------
// R3: the checkout changed while a step ran: the result is discarded.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_edit_during_the_run_discards_its_result() {
    let fx = world(LONG_IDLE).await;
    let flag = outside(&fx, "r3-release");
    let a = running(
        &fx,
        "r3",
        json!([{"name": "block", "cmd": wait_for(&flag)}]),
    )
    .await;

    let (answer, ()) = tokio::join!(gate_run(&fx, &a, None), async {
        wait_step(&fx, &a.task.id, 1, "block").await;
        std::fs::write(a.lease.path.join("edited.txt"), "during the run\n").unwrap();
        touch(&flag);
    });

    let answer = answer.unwrap();
    assert_eq!(answer["status_detail"], "gate-target-mismatch", "{answer}");
    let result = run_result(&fx, &a.task.id, 1).await;
    let Some(VerifyTargetEvidence::Verified { after, reasons, .. }) = &result.evidence else {
        panic!("{result:?}");
    };
    assert_eq!(reasons, &vec![MismatchReason::Dirty]);
    assert_eq!(after.dirty, vec!["?? edited.txt".to_string()]);
}

// ---------------------------------------------------------------------------
// R4: past the wait bound the call answers `running`; the next call joins the same run.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_past_the_wait_bound_returns_running_and_the_next_call_joins() {
    let fx = world(Duration::from_secs(10)).await;
    let flag = outside(&fx, "r4-release");
    let a = running(
        &fx,
        "r4",
        json!([{"name": "block", "cmd": wait_for(&flag)}]),
    )
    .await;
    std::fs::write(a.lease.path.join("r4.txt"), "r4\n").unwrap();

    let first = gate_run(&fx, &a, Some("the first call's message"))
        .await
        .unwrap();
    assert_eq!(first["state"], "running", "{first}");
    assert_eq!(
        (&first["run"], &first["step"]),
        (&json!(1), &json!("block"))
    );

    // The second call is admitted while the step still blocks, so it can only join r1; the step
    // is released once that short admission is long done, well inside the call's 5 s wait.
    let (second, ()) = tokio::join!(gate_run(&fx, &a, Some("a joining call's message")), async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(
            run_ops(&fx, &a.task.id).await.len(),
            1,
            "the call joined r1"
        );
        touch(&flag);
    });
    let second = second.unwrap();
    passed(&second);
    assert_eq!(second["run"], 1, "{second}");
    assert_eq!(run_ops(&fx, &a.task.id).await.len(), 1, "one op row");
    assert_eq!(
        git(&a.lease.path, &["log", "-1", "--format=%B"]),
        "the first call's message"
    );
}

// ---------------------------------------------------------------------------
// R4b: a checkpoint blocked in a clean filter still answers within the bound.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blocked_checkpoint_still_answers_within_the_wait_bound() {
    let fx = world(Duration::from_secs(4)).await;
    let flag = outside(&fx, "r4b-filter-may-run");
    let a = running(&fx, "r4b", json!([{"name": "fine", "cmd": "true"}])).await;
    let path = &a.lease.path;
    git(
        path,
        &[
            "config",
            "filter.block.clean",
            &format!("{}; cat", wait_for(&flag)),
        ],
    );
    std::fs::write(path.join(".gitattributes"), "*.blk filter=block\n").unwrap();
    std::fs::write(path.join("x.blk"), "held by the filter\n").unwrap();

    let started = Instant::now();
    let answer = gate_run(&fx, &a, None).await.unwrap();
    let elapsed = started.elapsed();

    assert_eq!(answer["state"], "running", "{answer}");
    assert_eq!(answer["step"], "neige-checkpoint", "{answer}");
    assert!(elapsed < Duration::from_millis(3500), "{elapsed:?}");
    touch(&flag);
    let result = run_result(&fx, &a.task.id, 1).await;
    assert!(result.verdict.passed, "{result:?}");
}

// ---------------------------------------------------------------------------
// R4c: a slow operation drive never holds the call.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_drive_does_not_hold_the_call() {
    let slow: Hook = Arc::new(|| Box::pin(tokio::time::sleep(Duration::from_secs(4))));
    let fx = world_with(Duration::from_secs(2), |boot| {
        hooked(boot, |adapter| adapter.with_before_spawn(slow))
    })
    .await;
    let a = running(&fx, "r4c", json!([{"name": "fine", "cmd": "true"}])).await;

    let started = Instant::now();
    let answer = gate_run(&fx, &a, None).await.unwrap();
    let elapsed = started.elapsed();

    assert_eq!(answer["state"], "running", "{answer}");
    assert!(elapsed < Duration::from_millis(2500), "{elapsed:?}");
    let result = run_result(&fx, &a.task.id, 1).await;
    assert!(
        result.verdict.passed,
        "the run completes after the call returned: {result:?}"
    );
}

// ---------------------------------------------------------------------------
// R5: an attempt gets five runs.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_sixth_run_is_refused_with_the_cap() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "r5", json!([{"name": "fine", "cmd": "true"}])).await;
    for run in 1..=5 {
        let answer = gate_run(&fx, &a, None).await.unwrap();
        passed(&answer);
        assert_eq!(
            (&answer["run"], &answer["runs_used"]),
            (&json!(run), &json!(run))
        );
    }

    let err = gate_run(&fx, &a, None).await.unwrap_err();

    assert_eq!(err.code, -32409, "{err:?}");
    assert!(
        err.message
            .contains("attempt used 5 of 5 gate runs; report done and the kernel's gate decides"),
        "{err:?}"
    );
    assert_eq!(run_ops(&fx, &a.task.id).await.len(), 5);
}

// ---------------------------------------------------------------------------
// R6 (D8, H1): done during a run delivers after the run, whose leader stays unreaped until its
// completion commits; the passing run on the delivered commit is the gate's verdict (slice 2).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn done_during_a_run_delivers_after_the_run() {
    let (hold, entered, resume) = pause();
    let fx = world_with(LONG_IDLE, |boot| {
        hooked(boot, |adapter| adapter.with_before_completion(hold))
    })
    .await;
    let flag = outside(&fx, "r6-release");
    let a = running(
        &fx,
        "r6",
        json!([{"name": "block", "cmd": wait_for(&flag)}]),
    )
    .await;
    std::fs::write(a.lease.path.join("r6.txt"), "r6\n").unwrap();

    let (answer, ()) = tokio::join!(gate_run(&fx, &a, Some("Deliver r6")), async {
        wait_step(&fx, &a.task.id, 1, "block").await;
        fx.complete(&a.worker, &a.task.id).await;
        assert_eq!(
            fx.forge_op_count().await,
            0,
            "no delivery while the step runs"
        );
        let row = fx
            .delivery_row(&a.task.id)
            .await
            .expect("the report wrote the row");
        assert!(row.settlement.is_none(), "{row:?}");

        touch(&flag);
        entered.notified().await;
        // The verdict is in, its completion not: a parked probe must still see a live leader.
        fx.runtime.sweep_parked().await.unwrap();
        fx.scheduler()
            .schedule_track(TrackId::from(fx.track().to_string()))
            .await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        fx.runtime.sweep_parked().await.unwrap();
        resume.notify_one();
    });

    let commit = passed(&answer.unwrap()).to_string();
    fx.wait_settled(&a.task.id).await;
    let candidate = fx.candidate_row(&a.task.id).await.expect("candidate");
    assert_eq!(
        candidate.commit_sha, commit,
        "the delivery made no second commit"
    );
    let task = current(&fx.boot, "r6").await;
    assert_eq!(task.status, TaskStatus::Verifying);
    fx.scheduler().drive_gate_for_test(task).await.unwrap();
    assert_eq!(current(&fx.boot, "r6").await.status, TaskStatus::Done);
    let gates = fx.events_for(GATE_RESULT_KIND, &a.task.id).await;
    assert_eq!(gates.len(), 1, "one verdict: {gates:?}");
    let verdict = verdict(&fx, "r6").await;
    assert_eq!(
        reused_run(&verdict),
        Some(format!("{}#r1", a.task.id).as_str()),
        "{verdict:?}"
    );
}

// ---------------------------------------------------------------------------
// R7 (D8): a cancel during a run keeps the checkout busy until the run ends.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_during_a_run_keeps_the_checkout_busy_until_it_ends() {
    let fx = world(LONG_IDLE).await;
    let flag = outside(&fx, "r7-release");
    let a = running(
        &fx,
        "r7",
        json!([{"name": "block", "cmd": wait_for(&flag)}]),
    )
    .await;
    declare(
        &fx.boot,
        json!({"key": "r7-next", "kind": "codex", "goal": "after the run",
               "declared_by": PLANNER_DECLARATION_AUTHOR, "ready": true,
               "no_gate_reason": "fixture"}),
    )
    .await;
    let scheduler = fx.scheduler();
    scheduler.mark_boot_sweep_complete();
    scheduler.mark_context_sweep_boot_complete();

    let (answer, ()) = tokio::join!(gate_run(&fx, &a, None), async {
        wait_step(&fx, &a.task.id, 1, "block").await;
        call_tool(
            &fx.boot,
            "neige_task_cancel",
            planner_identity(&fx.boot),
            json!({"key": "r7", "message": "stop"}),
        )
        .await
        .unwrap();
        scheduler.sweep_all().await;
        assert_eq!(current(&fx.boot, "r7").await.status, TaskStatus::Canceled);
        assert!(
            fx.delivery_row(&a.task.id).await.is_some(),
            "the cancel wrote the delivery row"
        );
        scheduler
            .schedule_track(TrackId::from(fx.track().to_string()))
            .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            fx.forge_op_count().await,
            0,
            "nothing delivered during the run"
        );
        assert_eq!(
            current(&fx.boot, "r7-next").await.status,
            TaskStatus::Pending,
            "the checkout stays busy while the run runs"
        );
        touch(&flag);
    });

    answer.unwrap();
    fx.wait_settled(&a.task.id).await;
    tokio::time::timeout(WAIT, async {
        loop {
            scheduler
                .schedule_track(TrackId::from(fx.track().to_string()))
                .await;
            if current(&fx.boot, "r7-next").await.status != TaskStatus::Pending {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the next task is claimed once the run ended and the delivery settled");
}

#[path = "task_gate_run/more.rs"]
mod more;
#[path = "task_gate_run/restart.rs"]
mod restart;
#[path = "task_gate_run/reuse.rs"]
mod reuse;
