//! #1727 S4 slice 4 PR-B: the task gate verifies the pinned candidate — admission after
//! settlement (A10, A10c), the prepare-transaction target check and same-transaction refusal
//! (A10b, A11, A11c, A13b), detect-after `finalize` on every completion path (A12, A12b, A12c),
//! `TaskGateResult.target` on every producer (A12d, A12e, A14, A14b), the out-of-process
//! restart between prepare and spawn (A11b), the `verification` read surface (D8) and the
//! one-turn gated lifecycle. Fixtures come from `git_delivery.rs`.
//!
//! One process per test (nextest): `provenance_observation_failure_is_not_a_verdict` mutates
//! `PATH`.
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::git_delivery::*;
use crate::task_recovery::current;
use calm_server::event::Event;
use calm_server::harness::Observation;
use calm_server::ids::ActorId;
use calm_server::model::{Task, TaskStatus, now_ms};
use calm_server::operation::task_verify_adapter::{
    TASK_VERIFY_KIND, TaskGateResult, TaskVerifyOperationPayload,
};
use calm_server::operation::{Operation, OperationKey, PhaseTag};
use calm_server::scheduler::Scheduler;
use calm_server::session_projection_repo::AgentProvider;
use calm_types::git_candidate::DeliveryWakeReason;
use calm_types::verify_target::{
    MismatchReason, NoCandidateReason, SamplePhase, UnboundReason, VerifyTarget,
    VerifyTargetEvidence,
};
use serde_json::{Value, json};

/// A machine boot id that can never match the host's, so a parked gate reads as dead.
const STALE_BOOT_ID: &str = "00000000-0000-0000-0000-000000000000";

fn gated(cmd: &str) -> Value {
    json!({"gate": {"steps": [{"name": "t", "cmd": cmd}], "timeout_secs": 60}, "no_gate_reason": null})
}

/// A gate step that runs `first`, then blocks until `flag` exists.
fn gate_then_wait(first: &str, flag: &Path) -> Value {
    gated(&format!(
        "{first}; until [ -f '{}' ]; do sleep 0.1; done",
        flag.display()
    ))
}

/// The `task.gate_result` event as the tests read it.
#[derive(Debug, Clone)]
struct GateResult {
    task_id: String,
    passed: bool,
    status_detail: Option<String>,
    failing_step: Option<String>,
    exit_code: Option<i32>,
    log_tail: String,
    log_path: String,
    attempt: i64,
    target: Option<VerifyTarget>,
}

fn gate_result(row: &calm_server::db::TrackEvent) -> GateResult {
    match &row.event {
        Event::TaskGateResult {
            task_id,
            passed,
            failing_step,
            exit_code,
            log_tail,
            log_path,
            attempt,
            status_detail,
            target,
            ..
        } => GateResult {
            task_id: task_id.clone(),
            passed: *passed,
            status_detail: status_detail.clone(),
            failing_step: failing_step.clone(),
            exit_code: *exit_code,
            log_tail: log_tail.clone(),
            log_path: log_path.clone(),
            attempt: *attempt,
            target: target.clone(),
        },
        other => panic!("not a gate result: {other:?}"),
    }
}

impl GateResult {
    /// The wake text the Dispatcher renders for this event (the mapping is field for field).
    fn turn_text(&self, key: &str) -> String {
        Observation::TaskGateResult {
            idempotency_key: self.task_id.clone(),
            key: key.to_string(),
            passed: self.passed,
            failing_step: self.failing_step.clone(),
            exit_code: self.exit_code,
            log_tail: self.log_tail.clone(),
            attempt: self.attempt,
            status_detail: self.status_detail.clone(),
            target: self.target.clone().map(Box::new),
        }
        .to_turn_text()
    }

    fn candidate(&self) -> (&str, &str, &str, &VerifyTargetEvidence) {
        match self.target.as_ref().expect("target") {
            VerifyTarget::Candidate {
                candidate_id,
                commit_sha,
                lease_id,
                evidence,
            } => (candidate_id, commit_sha, lease_id, evidence),
            other => panic!("not a candidate target: {other:?}"),
        }
    }

    /// The second line onward of a mismatch `log_tail` parses as the target (5.1.7).
    fn log_tail_target(&self) -> VerifyTarget {
        let (_, json) = self
            .log_tail
            .split_once('\n')
            .expect("one line then the target JSON");
        serde_json::from_str(json).expect("target JSON")
    }
}

async fn gate_result_events(fx: &Fx, task_id: &str) -> Vec<GateResult> {
    fx.events_for(GATE_RESULT_KIND, task_id)
        .await
        .iter()
        .map(gate_result)
        .collect()
}

async fn gate_op(fx: &Fx, task_id: &str) -> Option<Operation> {
    fx.runtime
        .find_by_kind_and_idempotency(TASK_VERIFY_KIND, &format!("{task_id}#g1"))
        .await
        .unwrap()
}

/// Poll the attempt's `#g1` Operation until `ready` holds.
async fn wait_gate_op_until(
    fx: &Fx,
    task_id: &str,
    what: &str,
    ready: impl Fn(&Operation) -> bool,
) -> Operation {
    tokio::time::timeout(WAIT, async {
        loop {
            if let Some(op) = gate_op(fx, task_id).await
                && ready(&op)
            {
                break op;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("gate op of {task_id} never {what}"))
}

async fn task_verify_op_count(fx: &Fx) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE kind = ?1")
        .bind(TASK_VERIFY_KIND)
        .fetch_one(&fx.pool())
        .await
        .unwrap()
}

/// The report's forge Operation to terminal, then the settlement step by hand (no
/// `schedule_pass`): the row settled as the caller expects it.
async fn settle_by_hand(fx: &Fx, task_id: &str) -> DeliveryRowView {
    fx.wait_forge_op(task_id).await;
    let row = fx.delivery_row(task_id).await.expect("delivery row");
    fx.scheduler()
        .settle_git_delivery_for_test(&row.delivery_id)
        .await
        .unwrap();
    fx.delivery_row(task_id).await.unwrap()
}

/// A candidate-bound gated attempt whose delivery settled as a candidate, with the live
/// listener stopped so nothing drives its gate until the test does. `files` are written into
/// the lease worktree before the report, so the candidate commit carries them.
async fn settled_gated_task(
    fx: &Fx,
    name: &str,
    gate_json: Value,
) -> (
    calm_server::mcp_server::registry::ToolCallIdentity,
    Task,
    calm_server::test_seams::KernelWorkspaceLease,
    CandidateRowView,
) {
    settled_gated_task_with(fx, name, gate_json, &[]).await
}

async fn settled_gated_task_with(
    fx: &Fx,
    name: &str,
    gate_json: Value,
    files: &[(&str, &str)],
) -> (
    calm_server::mcp_server::registry::ToolCallIdentity,
    Task,
    calm_server::test_seams::KernelWorkspaceLease,
    CandidateRowView,
) {
    let worker = fx.new_worker(name, AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task(name, "codex", &worker.card_id, gate_json)
        .await;
    std::fs::write(lease.path.join("worker.txt"), format!("{name}\n")).unwrap();
    for (file, content) in files {
        std::fs::write(lease.path.join(file), content).unwrap();
    }
    fx.complete(&worker, &task.id).await;
    let row = settle_by_hand(fx, &task.id).await;
    assert_eq!(row.settlement.as_deref(), Some("candidate"), "{row:?}");
    assert_eq!(row.wake_reason.as_deref(), Some("deferred_to_gate"));
    let candidate = fx.candidate_row(&task.id).await.expect("candidate");
    let task = current(&fx.boot, name).await;
    assert_eq!(task.status, TaskStatus::Verifying);
    (worker, task, lease, candidate)
}

fn gate_log_path(fx: &Fx, task_id: &str) -> PathBuf {
    fx.boot.ctx.gate_logs_dir.join(format!("{task_id}-g1.log"))
}

fn gate_log(fx: &Fx, task_id: &str) -> Option<String> {
    std::fs::read_to_string(gate_log_path(fx, task_id)).ok()
}

/// Submit `#g1` the way the scheduler does, but without its admission.
async fn submit_gate_bypassing_admission(fx: &Fx, task: &Task) -> String {
    let payload = serde_json::to_value(TaskVerifyOperationPayload {
        actor: ActorId::KernelDispatcher,
        track_id: task.track_id.clone(),
        task_id: task.id.clone(),
        attempt: 1,
    })
    .unwrap();
    let payload_hash = calm_server::routes::terminal_cards::stable_payload_hash(&payload).unwrap();
    let op_id = fx
        .runtime
        .submit(
            TASK_VERIFY_KIND,
            OperationKey {
                operation_key: calm_server::model::new_id(),
                idempotency_key: Some(format!("{}#g1", task.id)),
                payload_hash,
            },
            payload,
        )
        .await
        .unwrap();
    tokio::time::timeout(WAIT, fx.runtime.wait(&op_id))
        .await
        .expect("gate op terminal")
        .unwrap();
    op_id
}

fn spawn_drive(scheduler: Arc<Scheduler>, task: Task) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        scheduler.drive_gate_for_test(task).await.unwrap();
    })
}

/// `#g1` parked with its artifacts recorded (the wrapper is running).
async fn wait_gate_parked(fx: &Fx, task_id: &str) -> Operation {
    wait_gate_op_until(fx, task_id, "parked", |op| {
        op.phase.tag() == PhaseTag::Parked && op.spawn_artifacts.is_some()
    })
    .await
}

fn exit_path_of(op: &Operation) -> PathBuf {
    PathBuf::from(
        op.spawn_artifacts.as_ref().unwrap().extra["exit_path"]
            .as_str()
            .unwrap(),
    )
}

/// The wrapper is alive but its recorded identity belongs to another boot: dead to recovery.
async fn stale_artifacts(fx: &Fx, op_id: &str) {
    sqlx::query(
        "UPDATE operations SET spawn_artifacts_json = json_set(spawn_artifacts_json, '$.boot_id', ?1) WHERE id = ?2",
    )
    .bind(STALE_BOOT_ID)
    .bind(op_id)
    .execute(&fx.pool())
    .await
    .unwrap();
}

fn wait_for_file(path: &Path) {
    let deadline = std::time::Instant::now() + WAIT;
    while !path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn unsampled_phase(evidence: &VerifyTargetEvidence) -> &SamplePhase {
    match evidence {
        VerifyTargetEvidence::Unsampled { phase } => phase,
        other => panic!("not unsampled: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// A10: the gate is not submitted while the delivery is pending; admitted once it settles.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gate_is_not_submitted_while_delivery_pending() {
    let fx = fixture().await;
    // No live pass: the gate drive below is the only gate driver, so the one `runtime.wait`
    // anything can enter is the admission's wait on the held delivery Operation (the report
    // handler submits it without waiting; `resume_git_deliveries` runs only from a pass).
    fx.dispatcher.abort_event_listener_for_test();
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let flag = fx.track_root.parent().unwrap().join("commit-may-proceed");
    install_pre_commit(&lease, &hook_waiting_for(&flag, 0));
    let task = fx
        .running_task("admit", "codex", &worker.card_id, gated("true"))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "gated\n").unwrap();
    fx.complete(&worker, &task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    assert!(row.settlement.is_none(), "{row:?}");
    assert!(
        fx.forge_op(&row.forge_idempotency_key).await.is_some(),
        "the report handler submitted the delivery"
    );

    // The barrier: the drive notifies once it has entered `runtime.wait` — the admission's
    // "Operation present, not terminal → wait" arm — and only then is the absence read.
    let entered = Arc::new(tokio::sync::Notify::new());
    fx.runtime
        .install_wait_entered_hook_for_test(entered.clone());
    let task = current(&fx.boot, "admit").await;
    assert_eq!(task.status, TaskStatus::Verifying);
    let mut drive = spawn_drive(fx.scheduler(), task.clone());
    tokio::select! {
        entered = tokio::time::timeout(WAIT, entered.notified()) => {
            entered.expect("the gate drive entered its wait on the delivery Operation");
        }
        finished = &mut drive => {
            finished.unwrap();
            panic!(
                "the gate drive returned without waiting on the pending delivery ({} task-verify op(s))",
                task_verify_op_count(&fx).await
            );
        }
    }
    assert_eq!(
        task_verify_op_count(&fx).await,
        0,
        "gate submitted while pending"
    );
    assert_eq!(
        current(&fx.boot, "admit").await.status,
        TaskStatus::Verifying
    );
    assert!(
        fx.delivery_row(&task.id)
            .await
            .unwrap()
            .settlement
            .is_none()
    );
    assert!(fx.candidate_row(&task.id).await.is_none());

    // Released: the waiter settles (`deferred_to_gate`), is admitted and submits `#g1`.
    std::fs::write(&flag, b"").unwrap();
    drive.await.unwrap();
    let settled = fx.wait_settled(&task.id).await;
    let (result, wake_reason) = settled_result(&settled);
    let (candidate_id, commit_sha, _, _) = candidate_of(result);
    assert_eq!(wake_reason, DeliveryWakeReason::DeferredToGate);
    let op = gate_op(&fx, &task.id).await.expect("#g1 submitted");
    assert_eq!(op.phase.tag(), PhaseTag::Succeeded, "{op:?}");
    assert!(op.spawn_artifacts.is_some(), "the gate ran: {op:?}");
    let gate = gate_result(&wait_gate_result(&fx, &task.id).await);
    assert!(gate.passed, "{gate:?}");
    assert_eq!(gate.status_detail, None);
    let (id, sha, _, evidence) = gate.candidate();
    assert_eq!(id, candidate_id);
    assert_eq!(sha, commit_sha);
    assert!(
        matches!(evidence, VerifyTargetEvidence::Verified { reasons, .. } if reasons.is_empty()),
        "{evidence:?}"
    );
    assert_eq!(current(&fx.boot, "admit").await.status, TaskStatus::Done);
    assert_eq!(task_verify_op_count(&fx).await, 1);
}

// ---------------------------------------------------------------------------
// A10c: the waiter settles a terminal, unsettled delivery itself; two executors agree.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gate_waiter_settles_terminal_unsettled_delivery() {
    let fx = fixture().await;
    // No live pass: the report handler submits the delivery, the runtime drives it to terminal,
    // nobody settles it.
    fx.dispatcher.abort_event_listener_for_test();
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("waiter", "codex", &worker.card_id, gated("true"))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "waiter\n").unwrap();
    fx.complete(&worker, &task.id).await;
    let forge = fx.wait_forge_op(&task.id).await;
    assert_eq!(forge.phase.tag(), PhaseTag::Succeeded, "{forge:?}");
    let row = fx.delivery_row(&task.id).await.unwrap();
    assert!(row.settlement.is_none(), "{row:?}");
    assert!(fx.candidate_row(&task.id).await.is_none());
    assert!(fx.settled_events_for(&task.id).await.is_empty());
    let task = current(&fx.boot, "waiter").await;
    assert_eq!(task.status, TaskStatus::Verifying);

    // One gate drive, nothing else: it settles, is admitted, submits `#g1`, reconciles.
    fx.scheduler()
        .drive_gate_for_test(task.clone())
        .await
        .unwrap();

    let candidate = fx
        .candidate_row(&task.id)
        .await
        .expect("the waiter settled");
    let row = fx.delivery_row(&task.id).await.unwrap();
    assert_eq!(row.settlement.as_deref(), Some("candidate"));
    assert_eq!(row.wake_reason.as_deref(), Some("deferred_to_gate"));
    let settled = fx.settled_events_for(&task.id).await;
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(
        settled_result(&settled[0]).1,
        DeliveryWakeReason::DeferredToGate
    );
    let op = gate_op(&fx, &task.id).await.expect("#g1 submitted");
    assert_eq!(op.phase.tag(), PhaseTag::Succeeded, "{op:?}");
    let gate = gate_result(&wait_gate_result(&fx, &task.id).await);
    assert!(gate.passed, "{gate:?}");
    assert_eq!(gate.candidate().0, candidate.candidate_id);
    assert_eq!(current(&fx.boot, "waiter").await.status, TaskStatus::Done);

    // Second fixture: the settlement step and the gate drive run concurrently — one event, one
    // candidate row, one `#g1`, both executors `Ok` (the second UPDATE is the SQL guard's 0
    // rows, never the once-write trigger).
    let worker = fx.new_worker("waiter-2", AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("waiter-2", "codex", &worker.card_id, gated("true"))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "waiter-2\n").unwrap();
    fx.complete(&worker, &task.id).await;
    fx.wait_forge_op(&task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    assert!(row.settlement.is_none());
    let task = current(&fx.boot, "waiter-2").await;
    let scheduler = fx.scheduler();
    let (settle, drive) = tokio::join!(
        scheduler.settle_git_delivery_for_test(&row.delivery_id),
        scheduler.drive_gate_for_test(task.clone())
    );
    settle.unwrap();
    drive.unwrap();
    assert_eq!(fx.settled_events_for(&task.id).await.len(), 1);
    assert!(fx.candidate_row(&task.id).await.is_some());
    assert_eq!(
        fx.delivery_row(&task.id)
            .await
            .unwrap()
            .wake_reason
            .as_deref(),
        Some("deferred_to_gate")
    );
    let ops: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM operations WHERE kind = ?1 AND idempotency_key LIKE ?2",
    )
    .bind(TASK_VERIFY_KIND)
    .bind(format!("{}#g%", task.id))
    .fetch_one(&fx.pool())
    .await
    .unwrap();
    assert_eq!(ops, 1);
    assert!(gate_result(&wait_gate_result(&fx, &task.id).await).passed);
    assert_eq!(current(&fx.boot, "waiter-2").await.status, TaskStatus::Done);
}

/// Submit `#g1` past admission and assert the P3 refusal: op `Succeeded` with no process,
/// one `gate-infra` result naming `no_candidate`, the task `failed`, the wake text spelled.
async fn assert_refused_without_candidate(fx: &Fx, task: &Task, key: &str) -> GateResult {
    submit_gate_bypassing_admission(fx, task).await;
    let op = gate_op(fx, &task.id).await.expect("#g1");
    assert_eq!(op.phase.tag(), PhaseTag::Succeeded, "{key}: {op:?}");
    assert!(op.spawn_artifacts.is_none(), "{key}: no process ran");
    let frozen = &op.tx_output.as_ref().unwrap().data;
    assert_eq!(frozen["target"]["kind"], "no_candidate", "{key}: {frozen}");
    let events = gate_result_events(fx, &task.id).await;
    assert_eq!(events.len(), 1, "{key}: {events:?}");
    let gate = events[0].clone();
    assert!(!gate.passed);
    assert_eq!(gate.status_detail.as_deref(), Some("gate-infra"));
    assert!(
        gate.log_tail.contains("gate admitted before settlement"),
        "{key}: {}",
        gate.log_tail
    );
    assert!(
        !gate_log(fx, &task.id)
            .unwrap_or_default()
            .contains("::gate-step"),
        "{key}: no step ran"
    );
    let text = gate.turn_text(key);
    assert!(text.contains("no candidate to verify"), "{key}: {text}");
    assert!(!text.contains("candidate_id"), "{key}: {text}");
    assert_eq!(current(&fx.boot, key).await.status, TaskStatus::Failed);
    // The op result is the same verdict.
    let result: TaskGateResult =
        serde_json::from_value(op.tx_output.as_ref().unwrap().result.clone()).unwrap();
    assert_eq!(result.target, gate.target.clone().unwrap());
    gate
}

// ---------------------------------------------------------------------------
// A10b: `prepare_tx` refuses a candidate-bound attempt without a candidate, in its own
// transaction, and the spawn is a no-op.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gate_prepare_refuses_unsettled_delivery() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let flag = fx.track_root.parent().unwrap().join("commit-may-proceed");

    // 1. The delivery is pending (held in its hook).
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    install_pre_commit(&lease, &hook_waiting_for(&flag, 0));
    let task = fx
        .running_task("pending", "codex", &worker.card_id, gated("true"))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "pending\n").unwrap();
    fx.complete(&worker, &task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    assert!(row.settlement.is_none());
    let gate_pending = assert_refused_without_candidate(&fx, &task, "pending").await;
    assert_eq!(
        gate_pending.target,
        Some(VerifyTarget::NoCandidate {
            reason: NoCandidateReason::DeliveryPending {
                delivery_id: row.delivery_id.clone()
            }
        })
    );
    std::fs::write(&flag, b"").unwrap();
    fx.wait_forge_op(&task.id).await;
    remove_pre_commit(&lease);
    std::fs::remove_file(&flag).unwrap();

    // 2. The delivery failed.
    let worker = fx.new_worker("failed-worker", AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&worker.card_id).await;
    install_pre_commit(&lease, HOOK_EXIT_1);
    let task = fx
        .running_task("failed", "codex", &worker.card_id, gated("true"))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "failed\n").unwrap();
    fx.complete(&worker, &task.id).await;
    let row = settle_by_hand(&fx, &task.id).await;
    assert_eq!(row.settlement.as_deref(), Some("failed"));
    remove_pre_commit(&lease);
    let gate_failed = assert_refused_without_candidate(&fx, &task, "failed").await;
    assert_eq!(
        gate_failed.target,
        Some(VerifyTarget::NoCandidate {
            reason: NoCandidateReason::DeliveryFailed {
                delivery_id: row.delivery_id.clone()
            }
        })
    );

    // 3. No delivery row at all (deleted while the hook holds the commit).
    let worker = fx.new_worker("rowless-worker", AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&worker.card_id).await;
    install_pre_commit(&lease, &hook_waiting_for(&flag, 0));
    let task = fx
        .running_task("rowless", "codex", &worker.card_id, gated("true"))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "rowless\n").unwrap();
    fx.complete(&worker, &task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    sqlx::query("DELETE FROM task_git_deliveries WHERE delivery_id = ?1")
        .bind(&row.delivery_id)
        .execute(&fx.pool())
        .await
        .unwrap();
    let gate_rowless = assert_refused_without_candidate(&fx, &task, "rowless").await;
    assert_eq!(
        gate_rowless.target,
        Some(VerifyTarget::NoCandidate {
            reason: NoCandidateReason::NoDeliveryRow
        })
    );
    std::fs::write(&flag, b"").unwrap();
    tokio::time::timeout(
        WAIT,
        fx.runtime
            .wait(&fx.forge_op(&row.forge_idempotency_key).await.unwrap().id),
    )
    .await
    .unwrap()
    .unwrap();
    remove_pre_commit(&lease);

    // Positive: settled first, prepare passes and the gate runs.
    let (_, task, _, candidate) = settled_gated_task(&fx, "settled", gated("true")).await;
    fx.scheduler()
        .drive_gate_for_test(task.clone())
        .await
        .unwrap();
    let gate = gate_result(&wait_gate_result(&fx, &task.id).await);
    assert!(gate.passed, "{gate:?}");
    let (id, _, _, evidence) = gate.candidate();
    assert_eq!(id, candidate.candidate_id);
    assert!(matches!(evidence, VerifyTargetEvidence::Verified { .. }));
    assert!(gate_log(&fx, &task.id).unwrap().contains("::gate-step"));
    assert_eq!(current(&fx.boot, "settled").await.status, TaskStatus::Done);
}

// ---------------------------------------------------------------------------
// A13b (D12 (h)): a frozen `gate.cwd` on a candidate-bound agent task is refused in prepare.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn frozen_gate_cwd_on_agent_task_is_refused_in_prepare() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();

    // The row: claimed and verifying, candidate settled with `marker` committed, and `gate.cwd`
    // frozen into `gate_json` as the lease worktree's canonical path (the shape claimed before
    // the projection diagnostic existed).
    let (_, task, lease, candidate) =
        settled_gated_task_with(&fx, "frozen-cwd", gated("true"), &[("marker", "marker\n")]).await;
    assert!(
        git(&lease.path, &["ls-files", "marker"]) == "marker",
        "marker is tracked in the candidate"
    );
    let canonical: String =
        sqlx::query_scalar("SELECT canonical_path FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease.lease_id)
            .fetch_one(&fx.pool())
            .await
            .unwrap();
    assert_eq!(
        std::fs::canonicalize(&lease.path)
            .unwrap()
            .to_str()
            .unwrap(),
        canonical
    );
    let gate_json = json!({"steps": [{"name": "t", "cmd": "test -f marker"}], "cwd": canonical});
    sqlx::query("UPDATE tasks SET gate_json = ?1 WHERE id = ?2")
        .bind(gate_json.to_string())
        .bind(&task.id)
        .execute(&fx.pool())
        .await
        .unwrap();
    let task = current(&fx.boot, "frozen-cwd").await;

    fx.scheduler()
        .drive_gate_for_test(task.clone())
        .await
        .unwrap();

    let op = gate_op(&fx, &task.id).await.expect("#g1");
    assert_eq!(op.phase.tag(), PhaseTag::Failed, "{op:?}");
    assert!(op.tx_output.is_none(), "prepare failed before its commit");
    assert!(op.spawn_artifacts.is_none());
    let events = gate_result_events(&fx, &task.id).await;
    assert_eq!(events.len(), 1, "{events:?}");
    let gate = &events[0];
    assert!(!gate.passed);
    assert_eq!(gate.status_detail.as_deref(), Some("gate-infra"));
    let (id, sha, lease_id, evidence) = gate.candidate();
    assert_eq!(id, candidate.candidate_id);
    assert_eq!(sha, candidate.commit_sha);
    assert_eq!(lease_id, lease.lease_id);
    let SamplePhase::Prepare { reason } = unsampled_phase(evidence) else {
        panic!("{evidence:?}");
    };
    assert!(reason.contains("gate.cwd"), "{reason}");
    assert!(
        serde_json::to_value(evidence).unwrap()["phase"]
            .get("cwd")
            .is_none()
    );
    assert!(
        gate.log_tail.contains("base:{attempt}"),
        "{}",
        gate.log_tail
    );
    assert!(gate_log(&fx, &task.id).is_none(), "no step ran");
    let columns = fx.task_columns(&task.id).await;
    assert_eq!(columns.status, TaskStatus::Failed);
    assert_eq!(
        current(&fx.boot, "frozen-cwd").await.gate_attempt,
        0,
        "pre-bump fallback"
    );
    let entry = fx.plan_entry("frozen-cwd").await;
    assert_eq!(
        entry["candidate"]["verification"]["state"], "infra",
        "{entry}"
    );
    assert_eq!(
        entry["candidate"]["verification"]["target"]["evidence"]["phase"]["kind"],
        "prepare"
    );

    // Positive: the same row without `gate.cwd` — the gate runs in the lease worktree, `marker`
    // is there, the after-sample still matches.
    let (_, task, _, candidate) =
        settled_gated_task_with(&fx, "plain-cwd", gated("true"), &[("marker", "marker\n")]).await;
    sqlx::query("UPDATE tasks SET gate_json = ?1 WHERE id = ?2")
        .bind(json!({"steps": [{"name": "t", "cmd": "test -f marker"}]}).to_string())
        .bind(&task.id)
        .execute(&fx.pool())
        .await
        .unwrap();
    let task = current(&fx.boot, "plain-cwd").await;
    fx.scheduler()
        .drive_gate_for_test(task.clone())
        .await
        .unwrap();
    let gate = gate_result(&wait_gate_result(&fx, &task.id).await);
    assert!(gate.passed, "{gate:?}");
    let (id, _, _, evidence) = gate.candidate();
    assert_eq!(id, candidate.candidate_id);
    assert!(
        matches!(evidence, VerifyTargetEvidence::Verified { reasons, .. } if reasons.is_empty())
    );
    assert!(gate_log(&fx, &task.id).unwrap().contains("::gate-step t"));
}

// ---------------------------------------------------------------------------
// A11: HEAD moved between the candidate and the gate — refused in prepare, nothing spawned.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gate_refuses_moved_head_in_prepare_without_spawning() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let (_, task, lease, candidate) = settled_gated_task(&fx, "moved", gated("true")).await;
    git(
        &lease.path,
        &["commit", "-q", "--allow-empty", "-m", "moved"],
    );
    let moved = git(&lease.path, &["rev-parse", "HEAD"]);
    assert_ne!(moved, candidate.commit_sha);

    fx.scheduler()
        .drive_gate_for_test(task.clone())
        .await
        .unwrap();

    let op = gate_op(&fx, &task.id).await.expect("#g1");
    assert_eq!(op.phase.tag(), PhaseTag::Succeeded, "{op:?}");
    assert!(op.spawn_artifacts.is_none(), "nothing spawned: {op:?}");
    let output = op.tx_output.as_ref().unwrap();
    assert_eq!(output.data["target"]["refused"], true, "{}", output.data);
    let events = gate_result_events(&fx, &task.id).await;
    assert_eq!(events.len(), 1, "{events:?}");
    let gate = &events[0];
    assert!(!gate.passed);
    assert_eq!(gate.status_detail.as_deref(), Some("gate-target-mismatch"));
    let (id, sha, lease_id, evidence) = gate.candidate();
    assert_eq!(id, candidate.candidate_id);
    assert_eq!(sha, candidate.commit_sha);
    assert_eq!(lease_id, lease.lease_id);
    let VerifyTargetEvidence::Refused {
        cwd,
        before,
        reasons,
    } = evidence
    else {
        panic!("{evidence:?}");
    };
    assert_eq!(cwd, lease.path.to_str().unwrap());
    assert_eq!(before.head, moved);
    assert_eq!(reasons, &[MismatchReason::Head]);
    assert!(before.dirty.is_empty(), "{before:?}");
    assert!(before.provenance.registered);
    let log = gate_log(&fx, &task.id).expect("prepare wrote the refusal line");
    assert!(!log.contains("::gate-step"), "{log}");
    assert!(log.contains("gate REFUSED"), "{log}");
    let text = gate.turn_text("moved");
    assert!(text.contains("no step ran"), "{text}");
    assert!(
        text.contains("verification target mismatch (head)"),
        "{text}"
    );
    assert_eq!(current(&fx.boot, "moved").await.status, TaskStatus::Failed);
    let result: TaskGateResult = serde_json::from_value(output.result.clone()).unwrap();
    assert_eq!(result.target, gate.target.clone().unwrap());
    assert_eq!(gate.log_tail_target(), gate.target.clone().unwrap());
}

// ---------------------------------------------------------------------------
// A11c: a checkout that is not the registered lease worktree is a `provenance` mismatch even
// with HEAD == candidate and a clean tree.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gate_refuses_symlinked_external_clone_as_provenance_mismatch() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();

    // 1. The lease directory renamed away, a symlink to an external clone at the candidate in
    //    its place: realpath, common dir and registration all differ.
    let (_, task, lease, candidate) = settled_gated_task(&fx, "linked", gated("true")).await;
    let clone = fx.track_root.parent().unwrap().join("external-clone");
    git(
        &fx.track_root,
        &[
            "clone",
            "-q",
            fx.track_root.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    git(
        &clone,
        &["checkout", "-q", "--detach", &candidate.commit_sha],
    );
    assert_eq!(git(&clone, &["rev-parse", "HEAD"]), candidate.commit_sha);
    assert_eq!(git(&clone, &["status", "--porcelain"]), "");
    let moved_aside = PathBuf::from(format!("{}.moved", lease.path.display()));
    std::fs::rename(&lease.path, &moved_aside).unwrap();
    std::os::unix::fs::symlink(&clone, &lease.path).unwrap();

    fx.scheduler()
        .drive_gate_for_test(task.clone())
        .await
        .unwrap();
    let op = gate_op(&fx, &task.id).await.unwrap();
    assert_eq!(op.phase.tag(), PhaseTag::Succeeded);
    assert!(op.spawn_artifacts.is_none(), "no step ran");
    let gate = gate_result(&wait_gate_result(&fx, &task.id).await);
    assert!(!gate.passed);
    assert_eq!(gate.status_detail.as_deref(), Some("gate-target-mismatch"));
    let (id, _, _, evidence) = gate.candidate();
    assert_eq!(id, candidate.candidate_id);
    let VerifyTargetEvidence::Refused {
        before, reasons, ..
    } = evidence
    else {
        panic!("{evidence:?}");
    };
    assert_eq!(reasons, &[MismatchReason::Provenance], "{before:?}");
    assert_eq!(
        before.provenance.realpath,
        std::fs::canonicalize(&clone).unwrap().to_str().unwrap()
    );
    assert!(!before.provenance.registered);
    assert_eq!(before.head, candidate.commit_sha);
    assert!(before.dirty.is_empty());
    let text = gate.turn_text("linked");
    assert!(
        text.contains("cwd is not the registered lease worktree"),
        "{text}"
    );
    assert!(text.contains("registered=0"), "{text}");
    std::fs::remove_file(&lease.path).unwrap();
    std::fs::rename(&moved_aside, &lease.path).unwrap();

    // 2. The worktree removed and a standalone repository initialised at the same path, at the
    //    candidate: realpath equal, registered (it is its own main worktree), only the common
    //    dir differs.
    let (_, task, lease, candidate) = settled_gated_task(&fx, "reinit", gated("true")).await;
    git(
        &fx.track_root,
        &[
            "worktree",
            "remove",
            "--force",
            lease.path.to_str().unwrap(),
        ],
    );
    assert!(!lease.path.exists());
    git(
        &fx.track_root,
        &["init", "-q", lease.path.to_str().unwrap()],
    );
    git(
        &lease.path,
        &[
            "fetch",
            "-q",
            fx.track_root.to_str().unwrap(),
            &candidate.commit_sha,
        ],
    );
    git(&lease.path, &["checkout", "-q", "--detach", "FETCH_HEAD"]);
    assert_eq!(
        git(&lease.path, &["rev-parse", "HEAD"]),
        candidate.commit_sha
    );
    assert_eq!(git(&lease.path, &["status", "--porcelain"]), "");
    fx.scheduler()
        .drive_gate_for_test(task.clone())
        .await
        .unwrap();
    let gate = gate_result(&wait_gate_result(&fx, &task.id).await);
    assert_eq!(gate.status_detail.as_deref(), Some("gate-target-mismatch"));
    let (_, _, _, evidence) = gate.candidate();
    let VerifyTargetEvidence::Refused {
        before, reasons, ..
    } = evidence
    else {
        panic!("{evidence:?}");
    };
    assert_eq!(reasons, &[MismatchReason::Provenance], "{before:?}");
    assert_eq!(
        before.provenance.realpath,
        std::fs::canonicalize(&lease.path)
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert!(before.provenance.registered, "its own main worktree");
    assert_eq!(
        before.provenance.common_dir,
        std::fs::canonicalize(lease.path.join(".git"))
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert_ne!(
        before.provenance.common_dir,
        lease.git_common_dir.to_str().unwrap()
    );

    // Positive: an untouched lease worktree passes prepare.
    let (_, task, _, candidate) = settled_gated_task(&fx, "intact", gated("true")).await;
    fx.scheduler()
        .drive_gate_for_test(task.clone())
        .await
        .unwrap();
    let gate = gate_result(&wait_gate_result(&fx, &task.id).await);
    assert!(gate.passed, "{gate:?}");
    assert_eq!(gate.candidate().0, candidate.candidate_id);
}

// ---------------------------------------------------------------------------
// A12 / A12c: a step that leaves a new file discards the result (live path), whatever the
// checkout's `status.showUntrackedFiles`.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gate_result_discarded_when_tree_changed_during_steps() {
    let fx = fixture().await;
    let planner = fx.planner().await;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task(
            "touches",
            "codex",
            &worker.card_id,
            gated("touch extra.txt"),
        )
        .await;
    std::fs::write(lease.path.join("worker.txt"), "touches\n").unwrap();
    fx.complete(&worker, &task.id).await;
    let settled = fx.wait_settled(&task.id).await;
    let (candidate_id, ..) = candidate_of(settled_result(&settled).0);

    let gate = gate_result(&wait_gate_result(&fx, &task.id).await);
    assert!(!gate.passed, "{gate:?}");
    assert_eq!(gate.status_detail.as_deref(), Some("gate-target-mismatch"));
    assert_eq!(gate.failing_step, None);
    assert_eq!(gate.exit_code, None);
    let (id, _, _, evidence) = gate.candidate();
    assert_eq!(id, candidate_id);
    let VerifyTargetEvidence::Verified {
        before,
        after,
        reasons,
        ..
    } = evidence
    else {
        panic!("{evidence:?}");
    };
    assert!(before.dirty.is_empty(), "{before:?}");
    assert_eq!(after.dirty, vec!["?? extra.txt".to_string()]);
    assert_eq!(after.head, before.head);
    assert_eq!(reasons, &[MismatchReason::Dirty]);
    let op = gate_op(&fx, &task.id).await.unwrap();
    assert_eq!(op.phase.tag(), PhaseTag::Succeeded);
    assert!(op.spawn_artifacts.is_some(), "the step ran");
    let log = gate_log(&fx, &task.id).unwrap();
    assert!(log.contains("::gate-step t"), "{log}");
    assert!(log.contains("gate RESULT DISCARDED"), "{log}");
    assert_eq!(
        current(&fx.boot, "touches").await.status,
        TaskStatus::Failed
    );
    assert_eq!(gate.log_tail_target(), gate.target.clone().unwrap());
    let pending = wait_observations(&planner, 1).await;
    let text = pending[0].to_turn_text();
    assert!(text.contains("RESULT DISCARDED"), "{text}");
    assert!(
        text.contains("dirty after: 1 paths: ?? extra.txt"),
        "{text}"
    );
    assert_observations_exactly(&planner, 1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gate_catches_untracked_under_suppressing_config() {
    let fx = fixture().await;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    // Repository-level config, so the lease worktree's default `git status` hides untracked files.
    git(&lease.path, &["config", "status.showUntrackedFiles", "no"]);
    let task = fx
        .running_task(
            "suppressed",
            "codex",
            &worker.card_id,
            gated("touch extra.txt"),
        )
        .await;
    std::fs::write(lease.path.join("worker.txt"), "suppressed\n").unwrap();
    fx.complete(&worker, &task.id).await;
    fx.wait_settled(&task.id).await;
    let gate = gate_result(&wait_gate_result(&fx, &task.id).await);
    assert!(!gate.passed, "{gate:?}");
    assert_eq!(gate.status_detail.as_deref(), Some("gate-target-mismatch"));
    let (_, _, _, evidence) = gate.candidate();
    let VerifyTargetEvidence::Verified { after, reasons, .. } = evidence else {
        panic!("{evidence:?}");
    };
    assert_eq!(after.dirty, vec!["?? extra.txt".to_string()]);
    assert_eq!(reasons, &[MismatchReason::Dirty]);
    assert_eq!(
        git(&lease.path, &["status", "--porcelain"]),
        "",
        "the default status hides it"
    );

    // Positive: without the config the same step is caught the same way.
    let worker = fx.new_worker("plain", AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&worker.card_id).await;
    git(
        &lease.path,
        &["config", "--unset", "status.showUntrackedFiles"],
    );
    let task = fx
        .running_task("plain", "codex", &worker.card_id, gated("touch extra.txt"))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "plain\n").unwrap();
    fx.complete(&worker, &task.id).await;
    fx.wait_settled(&task.id).await;
    let gate = gate_result(&wait_gate_result(&fx, &task.id).await);
    assert_eq!(gate.status_detail.as_deref(), Some("gate-target-mismatch"));
    let (_, _, _, evidence) = gate.candidate();
    let VerifyTargetEvidence::Verified { after, .. } = evidence else {
        panic!("{evidence:?}");
    };
    assert_eq!(after.dirty, vec!["?? extra.txt".to_string()]);
}

// ---------------------------------------------------------------------------
// A12b: the dead-process + exit-file completion path (`recover_parked`'s `!alive` arm)
// samples after too. Only that arm: the recorded identity belongs to another boot, so the
// wrapper — alive, held on its flag — reads as dead and nothing is re-attached. The
// boot-reattach arm (the wrapper alive at reboot) is
// `live_reattached_gate_result_discarded_when_tree_changed`, out of process.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reattached_gate_result_discarded_when_tree_changed() {
    let mut fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let flag = fx.track_root.parent().unwrap().join("gate-may-finish");
    let (_, task, lease, candidate) =
        settled_gated_task(&fx, "reattached", gate_then_wait("touch extra.txt", &flag)).await;
    let drive = spawn_drive(fx.scheduler(), task.clone());
    let op = wait_gate_parked(&fx, &task.id).await;
    wait_for_file(&lease.path.join("extra.txt"));
    // The kernel "died" after the wrapper finished: the recorded identity belongs to another
    // boot (so the group it names is not signalled either) and the exit file says 0.
    stale_artifacts(&fx, &op.id).await;
    std::fs::write(exit_path_of(&op), "0\n").unwrap();

    fx.reboot().await;

    let gate = gate_result(&wait_gate_result(&fx, &task.id).await);
    assert!(!gate.passed, "{gate:?}");
    assert_eq!(gate.status_detail.as_deref(), Some("gate-target-mismatch"));
    let (id, _, _, evidence) = gate.candidate();
    assert_eq!(id, candidate.candidate_id);
    let VerifyTargetEvidence::Verified { after, reasons, .. } = evidence else {
        panic!("{evidence:?}");
    };
    assert_eq!(after.dirty, vec!["?? extra.txt".to_string()]);
    assert_eq!(reasons, &[MismatchReason::Dirty]);
    assert_eq!(
        current(&fx.boot, "reattached").await.status,
        TaskStatus::Failed
    );
    let op = gate_op(&fx, &task.id).await.unwrap();
    assert_eq!(op.phase.tag(), PhaseTag::Succeeded, "{op:?}");

    std::fs::write(&flag, b"").unwrap();
    drive.await.unwrap();
    assert_eq!(gate_result_events(&fx, &task.id).await.len(), 1);
}

// ---------------------------------------------------------------------------
// Review round 1 (5a/5b): `finalize` compares HEAD, not only the porcelain status; an after
// sample that cannot be taken is `gate-infra` with `Unsampled { Finalize }`.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gate_result_discarded_when_head_moved_during_steps() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    // The step moves HEAD and leaves the tree clean: only the `head` check can catch it.
    let (_, task, lease, candidate) = settled_gated_task(
        &fx,
        "head-moved",
        gated("git commit -q --allow-empty -m moved-by-the-gate"),
    )
    .await;
    fx.scheduler()
        .drive_gate_for_test(task.clone())
        .await
        .unwrap();
    let moved = git(&lease.path, &["rev-parse", "HEAD"]);
    assert_ne!(moved, candidate.commit_sha, "the step moved HEAD");
    assert_eq!(git(&lease.path, &["status", "--porcelain"]), "");

    let gate = gate_result(&wait_gate_result(&fx, &task.id).await);
    assert!(!gate.passed, "{gate:?}");
    assert_eq!(gate.status_detail.as_deref(), Some("gate-target-mismatch"));
    let (id, sha, _, evidence) = gate.candidate();
    assert_eq!(id, candidate.candidate_id);
    assert_eq!(sha, candidate.commit_sha);
    let VerifyTargetEvidence::Verified {
        before,
        after,
        reasons,
        ..
    } = evidence
    else {
        panic!("{evidence:?}");
    };
    assert_eq!(before.head, candidate.commit_sha);
    assert_eq!(after.head, moved);
    assert!(after.dirty.is_empty(), "{after:?}");
    assert_eq!(reasons, &[MismatchReason::Head]);
    let op = gate_op(&fx, &task.id).await.unwrap();
    assert_eq!(op.phase.tag(), PhaseTag::Succeeded);
    assert!(op.spawn_artifacts.is_some(), "the step ran");
    assert!(gate_log(&fx, &task.id).unwrap().contains("::gate-step t"));
    assert_eq!(
        current(&fx.boot, "head-moved").await.status,
        TaskStatus::Failed
    );
    let text = gate.turn_text("head-moved");
    assert!(text.contains("RESULT DISCARDED"), "{text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn after_sample_failure_is_gate_infra_unsampled_finalize() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    // The step breaks the linked worktree's `.git` file (prepare sampled a good tree; the
    // after sample cannot be taken) and exits 0.
    let (_, task, lease, candidate) =
        settled_gated_task(&fx, "unsampled-after", gated("printf garbage > .git")).await;
    fx.scheduler()
        .drive_gate_for_test(task.clone())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(lease.path.join(".git")).unwrap(),
        "garbage"
    );

    let gate = gate_result(&wait_gate_result(&fx, &task.id).await);
    assert!(!gate.passed, "{gate:?}");
    assert_eq!(gate.status_detail.as_deref(), Some("gate-infra"));
    let (id, _, _, evidence) = gate.candidate();
    assert_eq!(id, candidate.candidate_id);
    let SamplePhase::Finalize { cwd, reason } = unsampled_phase(evidence) else {
        panic!("{evidence:?}");
    };
    assert_eq!(cwd, lease.path.to_str().unwrap());
    assert!(
        reason.contains("lease provenance observation failed"),
        "{reason}"
    );
    assert!(
        gate.log_tail.contains("unsampled after the gate"),
        "{}",
        gate.log_tail
    );
    let op = gate_op(&fx, &task.id).await.unwrap();
    assert_eq!(op.phase.tag(), PhaseTag::Succeeded, "{op:?}");
    assert!(op.spawn_artifacts.is_some(), "the step ran");
    let log = gate_log(&fx, &task.id).unwrap();
    assert!(log.contains("::gate-step t"), "{log}");
    assert!(log.contains("unsampled after the gate"), "{log}");
    assert_eq!(
        current(&fx.boot, "unsampled-after").await.status,
        TaskStatus::Failed
    );
    let entry = fx.plan_entry("unsampled-after").await;
    assert_eq!(
        entry["candidate"]["verification"]["state"], "infra",
        "{entry}"
    );
    assert_eq!(
        entry["candidate"]["verification"]["target"]["evidence"]["phase"]["kind"],
        "finalize"
    );
}

// ---------------------------------------------------------------------------
// Review round 1 (1): a sampling command a repository hook holds is bounded — the prepare
// transaction ends as Stuck / `Unsampled { Prepare }` and the kernel's write slot is released.
// ---------------------------------------------------------------------------

/// Live (non-zombie) processes whose command line names `needle`.
fn live_processes_naming(needle: &str) -> Vec<i32> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        if !String::from_utf8_lossy(&cmdline).contains(needle) {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        if calm_server::proc_identity::parse_proc_stat_fields(&stat)
            .is_some_and(|fields| fields.state != 'Z' && fields.state != 'X')
        {
            found.push(pid);
        }
    }
    found
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gate_prepare_times_out_as_stuck_not_hang() {
    use calm_server::db::sqlite::begin_immediate_tx;
    use calm_server::operation::task_verify_adapter::SAMPLE_TIMEOUT;

    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let (_, task, lease, candidate) = settled_gated_task(&fx, "hooked", gated("true")).await;
    // A slow `clean` filter that never returns: `git status` runs it to recompute a tracked file
    // whose stat cache is stale, and it survives `-c core.fsmonitor=false` (the sampler now disables
    // fsmonitor, so an fsmonitor hook would never fire). The filter command and `.gitattributes` live
    // in the shared repository any worker can write.
    let hook = fx
        .track_root
        .parent()
        .unwrap()
        .join(format!("slow-clean-filter-{}.sh", std::process::id()));
    write_executable(&hook, "#!/bin/sh\nsleep 60\n");
    git(
        &lease.path,
        &["config", "filter.slow.clean", hook.to_str().unwrap()],
    );
    std::fs::write(
        lease.path.join(".gitattributes"),
        "worker.txt filter=slow\n",
    )
    .unwrap();
    // Stale the tracked file's stat cache (rewrite the same bytes with a newer mtime) so `git
    // status` must run the clean filter on it.
    let worker_txt = lease.path.join("worker.txt");
    let content = std::fs::read(&worker_txt).unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    std::fs::write(&worker_txt, &content).unwrap();
    let needle = hook.display().to_string();

    let drive = spawn_drive(fx.scheduler(), task.clone());
    // The prepare transaction is sampling: the hook is running under the sampler's `git status`.
    tokio::time::timeout(WAIT, async {
        while live_processes_naming(&needle).is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the sampler reached the hook");

    // Another writer, started while the prepare transaction holds the write slot: it completes
    // once the sample bound fires and the transaction rolls back — never `database is locked`.
    let pool = fx.pool();
    let task_id = task.id.clone();
    let writer_started = std::time::Instant::now();
    let writer = tokio::spawn(async move {
        let mut tx = begin_immediate_tx(&pool).await?;
        sqlx::query("UPDATE tasks SET updated_at_ms = updated_at_ms WHERE id = ?1")
            .bind(&task_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok::<_, calm_server::error::CalmError>(writer_started.elapsed())
    });
    let writer_elapsed = tokio::time::timeout(Duration::from_secs(30), writer)
        .await
        .expect("the write slot was released by the sample bound (30 s)")
        .unwrap()
        .expect("the concurrent write committed");
    eprintln!(
        "concurrent write committed after {writer_elapsed:?} (sample bound {SAMPLE_TIMEOUT:?})"
    );
    assert!(
        writer_elapsed <= SAMPLE_TIMEOUT + Duration::from_secs(2),
        "write slot held for {writer_elapsed:?}"
    );
    tokio::time::timeout(Duration::from_secs(30), drive)
        .await
        .expect("the gate drive returned (30 s)")
        .unwrap();

    let op = gate_op(&fx, &task.id).await.unwrap();
    assert_eq!(op.phase.tag(), PhaseTag::Stuck, "{op:?}");
    assert!(op.tx_output.is_none(), "prepare rolled back");
    let events = gate_result_events(&fx, &task.id).await;
    assert_eq!(events.len(), 1, "{events:?}");
    let gate = &events[0];
    assert_eq!(gate.status_detail.as_deref(), Some("gate-infra"));
    let (id, _, _, evidence) = gate.candidate();
    assert_eq!(id, candidate.candidate_id);
    let SamplePhase::Prepare { reason } = unsampled_phase(evidence) else {
        panic!("{evidence:?}");
    };
    assert!(reason.contains("timed out"), "{reason}");
    assert!(reason.contains("git status"), "{reason}");
    assert_eq!(current(&fx.boot, "hooked").await.status, TaskStatus::Failed);
    assert_eq!(
        current(&fx.boot, "hooked").await.gate_attempt,
        0,
        "pre-bump fallback"
    );
    // The hook (a grandchild of the sampler's `git`) was killed with the command's group.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let live = live_processes_naming(&needle);
        if live.is_empty() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "hook processes survived the sample bound: {live:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    git(&lease.path, &["config", "--unset", "filter.slow.clean"]);
    let _ = std::fs::remove_file(lease.path.join(".gitattributes"));
}

// ---------------------------------------------------------------------------
// Review round 1 (2): an existing `#gN` is waited and reconciled before admission — the
// D12 (i) window (a pre-slice-4 gate terminal beside a failed delivery) still flips its row.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn existing_terminal_gate_op_is_reconciled_before_admission() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    // A failed delivery under a `verifying` row ...
    let (_, task, lease) = fx.hook_failing_task("upgraded", gated("true")).await;
    let row = settle_by_hand(&fx, &task.id).await;
    assert_eq!(row.settlement.as_deref(), Some("failed"), "{row:?}");
    remove_pre_commit(&lease);
    assert_eq!(
        current(&fx.boot, "upgraded").await.status,
        TaskStatus::Verifying
    );
    // ... beside a `#g1` a slice 2/3 build submitted, froze (no `target` key) and that died
    // parked, terminal-Failed and never reconciled; the row was bumped to attempt 1.
    let op_id = calm_server::model::new_id();
    let frozen = json!({
        "target_type": "task", "target_id": task.id, "result": {},
        "data": {
            "task_id": task.id, "track_id": task.track_id, "area_id": fx.boot.area_id,
            "key": "upgraded", "attempt": 1, "cwd": lease.path,
            "gate": {"steps": [{"name": "t", "cmd": "true"}]},
        },
    });
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO operations (
               id, operation_key, kind, idempotency_key, payload_hash,
               target_type, target_id, target_json, payload_json, tx_output_json,
               phase, phase_detail_json, last_error, created_at_ms, updated_at_ms, completed_at_ms
           )
           VALUES (?1, ?2, ?3, ?4, 'pre-slice-4', 'task', ?5, ?6, ?7, ?8,
                   'failed', ?9, ?10, ?11, ?11, ?11)"#,
    )
    .bind(&op_id)
    .bind(format!("gate-{}", task.id))
    .bind(TASK_VERIFY_KIND)
    .bind(format!("{}#g1", task.id))
    .bind(&task.id)
    .bind(json!({"type": "task", "id": task.id}).to_string())
    .bind(json!({"task_id": task.id, "attempt": 1}).to_string())
    .bind(frozen.to_string())
    .bind(json!({"from_phase": "parked", "last_error_class": "parked_dead"}).to_string())
    .bind("gate process dead with no recorded verdict; gate-infra")
    .bind(now)
    .execute(&fx.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE tasks SET gate_attempt = 1 WHERE id = ?1")
        .bind(&task.id)
        .execute(&fx.pool())
        .await
        .unwrap();
    let task = current(&fx.boot, "upgraded").await;
    assert_eq!(task.gate_attempt, 1);

    fx.scheduler()
        .drive_gate_for_test(task.clone())
        .await
        .unwrap();

    // Reconciled: the row flipped from the terminal op, one result, no `#g2`.
    let events = gate_result_events(&fx, &task.id).await;
    assert_eq!(events.len(), 1, "{events:?}");
    let gate = &events[0];
    assert!(!gate.passed);
    assert_eq!(gate.status_detail.as_deref(), Some("gate-infra"));
    assert_eq!(gate.attempt, 1);
    assert_eq!(
        gate.target,
        Some(VerifyTarget::Unbound {
            reason: UnboundReason::LegacyFrozen
        })
    );
    assert_eq!(
        gate.log_tail,
        "gate process dead with no recorded verdict; gate-infra"
    );
    let after = current(&fx.boot, "upgraded").await;
    assert_eq!(after.status, TaskStatus::Failed);
    assert_eq!(after.gate_attempt, 1);
    assert!(
        fx.runtime
            .find_by_kind_and_idempotency(TASK_VERIFY_KIND, &format!("{}#g2", task.id))
            .await
            .unwrap()
            .is_none(),
        "no second gate on a failed delivery"
    );
    assert_eq!(task_verify_op_count(&fx).await, 1);
}

// ---------------------------------------------------------------------------
// A12d: the compensation step, the reconcile arm and the stuck prepare each carry a target.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn compensated_gate_carries_unsampled_target() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let (_, task, lease, candidate) = settled_gated_task(&fx, "compensated", gated("true")).await;
    // Spawn fails after the freeze: this attempt's wrapper script path is a directory, so the
    // script cannot be written (the gate-logs directory itself is shared by every test process).
    let script = fx.boot.ctx.gate_logs_dir.join(format!("{}-g1.sh", task.id));
    std::fs::create_dir_all(&script).unwrap();

    fx.scheduler()
        .drive_gate_for_test(task.clone())
        .await
        .unwrap();

    let op = gate_op(&fx, &task.id).await.unwrap();
    assert_eq!(op.phase.tag(), PhaseTag::Failed, "{op:?}");
    assert!(op.tx_output.is_some(), "frozen before the spawn failed");
    let events = gate_result_events(&fx, &task.id).await;
    assert_eq!(events.len(), 1, "{events:?}");
    let gate = &events[0];
    assert_eq!(gate.status_detail.as_deref(), Some("gate-infra"));
    let (id, sha, _, evidence) = gate.candidate();
    assert_eq!(id, candidate.candidate_id);
    assert_eq!(sha, candidate.commit_sha);
    let SamplePhase::Compensation { cwd, reason } = unsampled_phase(evidence) else {
        panic!("{evidence:?}");
    };
    assert_eq!(cwd, lease.path.to_str().unwrap());
    assert!(!reason.is_empty());
    assert_eq!(
        current(&fx.boot, "compensated").await.status,
        TaskStatus::Failed
    );
    std::fs::remove_dir(&script).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconciled_gate_carries_unsampled_target() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let flag = fx.track_root.parent().unwrap().join("gate-may-finish");
    let (_, task, lease, candidate) =
        settled_gated_task(&fx, "reconciled", gate_then_wait("true", &flag)).await;
    let drive = spawn_drive(fx.scheduler(), task.clone());
    let op = wait_gate_parked(&fx, &task.id).await;
    // The op fails behind the waiter's back (the shape enforcement leaves).
    sqlx::query(
        "UPDATE operations SET phase = 'failed', phase_detail_json = ?1, last_error = ?2, \
         lease_owner = NULL, lease_until_ms = NULL, completed_at_ms = ?3, updated_at_ms = ?3 WHERE id = ?4",
    )
    .bind(json!({"from_phase": PhaseTag::Parked, "last_error_class": "parked_dead"}).to_string())
    .bind("gate process dead (fixture)")
    .bind(now_ms())
    .bind(&op.id)
    .execute(&fx.pool())
    .await
    .unwrap();
    drive.await.unwrap();

    let events = gate_result_events(&fx, &task.id).await;
    assert_eq!(events.len(), 1, "{events:?}");
    let gate = &events[0];
    assert_eq!(gate.status_detail.as_deref(), Some("gate-infra"));
    assert_eq!(gate.log_tail, "gate process dead (fixture)");
    let (id, _, _, evidence) = gate.candidate();
    assert_eq!(id, candidate.candidate_id);
    let SamplePhase::Reconciliation { cwd, last_error } = unsampled_phase(evidence) else {
        panic!("{evidence:?}");
    };
    assert_eq!(cwd, lease.path.to_str().unwrap());
    assert_eq!(last_error, "gate process dead (fixture)");
    assert_eq!(
        current(&fx.boot, "reconciled").await.status,
        TaskStatus::Failed
    );
    std::fs::write(&flag, b"").unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stuck_gate_carries_unsampled_prepare_target() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let (_, task, lease, candidate) = settled_gated_task(&fx, "stuck", gated("true")).await;
    // The linked worktree's `.git` file is garbage: `rev-parse` fails, nothing can be sampled.
    std::fs::write(lease.path.join(".git"), "garbage\n").unwrap();

    fx.scheduler()
        .drive_gate_for_test(task.clone())
        .await
        .unwrap();

    let op = gate_op(&fx, &task.id).await.unwrap();
    assert_eq!(op.phase.tag(), PhaseTag::Stuck, "{op:?}");
    assert!(op.tx_output.is_none(), "prepare rolled back");
    let events = gate_result_events(&fx, &task.id).await;
    assert_eq!(events.len(), 1, "{events:?}");
    let gate = &events[0];
    assert_eq!(gate.status_detail.as_deref(), Some("gate-infra"));
    let (id, sha, _, evidence) = gate.candidate();
    assert_eq!(id, candidate.candidate_id);
    assert_eq!(sha, candidate.commit_sha);
    let SamplePhase::Prepare { reason } = unsampled_phase(evidence) else {
        panic!("{evidence:?}");
    };
    assert!(reason.contains("provenance observation failed"), "{reason}");
    let phase = serde_json::to_value(evidence).unwrap()["phase"].clone();
    assert!(phase.get("cwd").is_none(), "{phase}");
    assert_eq!(current(&fx.boot, "stuck").await.status, TaskStatus::Failed);
    assert_eq!(
        current(&fx.boot, "stuck").await.gate_attempt,
        0,
        "pre-bump fallback"
    );
    let entry = fx.plan_entry("stuck").await;
    assert_eq!(
        entry["candidate"]["verification"]["state"], "infra",
        "{entry}"
    );
}

// ---------------------------------------------------------------------------
// A12e (P9b): a success result that does not parse carries the frozen target.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unparseable_gate_result_carries_frozen_target() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let flag = fx.track_root.parent().unwrap().join("gate-may-finish");
    let (_, task, lease, candidate) =
        settled_gated_task(&fx, "garbled", gate_then_wait("true", &flag)).await;
    let drive = spawn_drive(fx.scheduler(), task.clone());
    let op = wait_gate_parked(&fx, &task.id).await;
    assert_eq!(current(&fx.boot, "garbled").await.gate_attempt, 1);
    sqlx::query(
        "UPDATE operations SET phase = 'succeeded', tx_output_json = json_set(tx_output_json, '$.result', json('{\"garbage\":1}')), \
         lease_owner = NULL, lease_until_ms = NULL, completed_at_ms = ?1, updated_at_ms = ?1 WHERE id = ?2",
    )
    .bind(now_ms())
    .bind(&op.id)
    .execute(&fx.pool())
    .await
    .unwrap();
    drive.await.unwrap();

    let events = gate_result_events(&fx, &task.id).await;
    assert_eq!(events.len(), 1, "{events:?}");
    let gate = &events[0];
    assert_eq!(gate.status_detail.as_deref(), Some("gate-infra"));
    let (id, _, _, evidence) = gate.candidate();
    assert_eq!(id, candidate.candidate_id);
    let SamplePhase::Reconciliation { cwd, last_error } = unsampled_phase(evidence) else {
        panic!("{evidence:?}");
    };
    assert_eq!(cwd, lease.path.to_str().unwrap());
    assert!(last_error.contains("unparseable"), "{last_error}");
    let task = current(&fx.boot, "garbled").await;
    assert_eq!(task.status, TaskStatus::Failed);
    assert_eq!(task.gate_attempt, 1, "flipped at the bumped attempt");
    std::fs::write(&flag, b"").unwrap();
}

// ---------------------------------------------------------------------------
// A14: a legacy lease runs the gate as today, unbound and unsampled.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn legacy_lease_gate_runs_without_target_check() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let worker = fx.codex_worker();
    // The fixtures-only plain lease: the all-NULL base tuple, `delivery_policy` NULL, a plain
    // directory that is not a git checkout — sampling it would fail, so a passed gate proves
    // no sample was taken.
    let dir = fx.track_root.parent().unwrap().join("plain-lease");
    std::fs::create_dir_all(&dir).unwrap();
    calm_server::test_seams::acquire_workspace_lease_for_test(
        &fx.pool(),
        &worker.card_id,
        fx.track(),
        "legacy-owner",
        &dir,
    )
    .await
    .unwrap();
    let task = fx
        .running_task("legacy", "codex", &worker.card_id, gated("test -d ."))
        .await;
    sqlx::query("UPDATE tasks SET status = 'verifying' WHERE id = ?1")
        .bind(&task.id)
        .execute(&fx.pool())
        .await
        .unwrap();
    let task = current(&fx.boot, "legacy").await;

    fx.scheduler()
        .drive_gate_for_test(task.clone())
        .await
        .unwrap();

    let op = gate_op(&fx, &task.id).await.unwrap();
    assert_eq!(op.phase.tag(), PhaseTag::Succeeded, "{op:?}");
    assert_eq!(
        op.tx_output.as_ref().unwrap().data["target"],
        json!({"kind": "unbound", "reason": "legacy_lease"})
    );
    let gate = gate_result(&wait_gate_result(&fx, &task.id).await);
    assert!(gate.passed, "{gate:?}");
    assert_eq!(gate.status_detail, None);
    assert_eq!(
        gate.target,
        Some(VerifyTarget::Unbound {
            reason: UnboundReason::LegacyLease
        })
    );
    assert_eq!(current(&fx.boot, "legacy").await.status, TaskStatus::Done);
    let entry = fx.plan_entry("legacy").await;
    assert_eq!(entry["candidate"]["binding"], "unbound", "{entry}");
    assert_eq!(entry["gate_result"]["target"]["reason"], "legacy_lease");
}

// ---------------------------------------------------------------------------
// A14b: a freeze without `target` boots as `LegacyFrozen`; a verdict without `target` reads
// as `LegacyVerdict`.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pre_upgrade_frozen_gate_boots_as_unbound() {
    let mut fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let flag = fx.track_root.parent().unwrap().join("gate-may-finish");
    // A step that would be a mismatch under the target check: only a freeze read as unbound
    // lets it pass.
    let (_, task, lease, _) =
        settled_gated_task(&fx, "pre-upgrade", gate_then_wait("touch extra.txt", &flag)).await;
    let drive = spawn_drive(fx.scheduler(), task.clone());
    let op = wait_gate_parked(&fx, &task.id).await;
    wait_for_file(&lease.path.join("extra.txt"));
    assert_eq!(
        op.tx_output.as_ref().unwrap().data["target"]["kind"],
        "candidate"
    );
    // The pre-slice-4 freeze shape: no `target` key; dead to recovery, exit file 0.
    sqlx::query(
        "UPDATE operations SET tx_output_json = json_remove(tx_output_json, '$.data.target') WHERE id = ?1",
    )
    .bind(&op.id)
    .execute(&fx.pool())
    .await
    .unwrap();
    stale_artifacts(&fx, &op.id).await;
    std::fs::write(exit_path_of(&op), "0\n").unwrap();

    fx.reboot().await;

    let gate = gate_result(&wait_gate_result(&fx, &task.id).await);
    assert!(gate.passed, "{gate:?}");
    assert_eq!(
        gate.target,
        Some(VerifyTarget::Unbound {
            reason: UnboundReason::LegacyFrozen
        })
    );
    assert_eq!(
        current(&fx.boot, "pre-upgrade").await.status,
        TaskStatus::Done
    );
    let entry = fx.plan_entry("pre-upgrade").await;
    assert_eq!(
        entry["candidate"]["verification"]["state"], "unbound",
        "{entry}"
    );
    std::fs::write(&flag, b"").unwrap();
    drive.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn legacy_verdict_reads_as_unbound() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let (_, task, lease, _) = settled_gated_task(&fx, "legacy-verdict", gated("true")).await;
    // The verdict a slice 2/3 build recorded: the seven `GateVerdict` keys plus `cwd`.
    let recorded = json!({
        "passed": true, "status_detail": null, "failing_step": null, "exit_code": 0,
        "log_tail": "::gate-step t\n", "log_path": gate_log_path(&fx, &task.id), "attempt": 1,
        "cwd": lease.path,
    });
    sqlx::query(
        "UPDATE tasks SET status = 'done', gate_result_json = ?1, gate_attempt = 1, finished_at_ms = ?2 WHERE id = ?3",
    )
    .bind(recorded.to_string())
    .bind(now_ms())
    .bind(&task.id)
    .execute(&fx.pool())
    .await
    .unwrap();

    let parsed: TaskGateResult = serde_json::from_value(recorded.clone()).unwrap();
    assert_eq!(
        parsed.target,
        VerifyTarget::Unbound {
            reason: UnboundReason::LegacyVerdict
        }
    );
    assert!(parsed.verdict.passed);
    assert_eq!(parsed.verdict.exit_code, Some(0));
    assert_eq!(parsed.cwd.as_deref(), lease.path.to_str());
    let entry = fx.plan_entry("legacy-verdict").await;
    assert_eq!(entry["candidate"]["binding"], "bound", "{entry}");
    assert_eq!(entry["candidate"]["verification"]["state"], "unbound");
    assert_eq!(
        entry["candidate"]["verification"]["target"],
        json!({"kind": "unbound", "reason": "legacy_verdict"})
    );
    assert_eq!(entry["candidate"]["verification"]["gate_attempt"], 1);
    assert_eq!(entry["gate_result"]["passed"], true);
    let summary = fx.plan_summary_entry("legacy-verdict").await;
    assert_eq!(
        summary["candidate"]["verification"]["state"], "unbound",
        "{summary}"
    );
    assert_eq!(summary["candidate"]["verification"]["gate_attempt"], 1);
}

// ---------------------------------------------------------------------------
// The gated candidate-bound lifecycle wakes the Planner exactly once.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gated_candidate_happy_path_wakes_planner_once() {
    let fx = fixture().await;
    let planner = fx.planner().await;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("happy", "codex", &worker.card_id, gated("true"))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "happy\n").unwrap();
    fx.complete(&worker, &task.id).await;
    let settled = fx.wait_settled(&task.id).await;
    let (result, wake_reason) = settled_result(&settled);
    let (candidate_id, commit_sha, _, _) = candidate_of(result);
    assert_eq!(wake_reason, DeliveryWakeReason::DeferredToGate);
    let gate = gate_result(&wait_gate_result(&fx, &task.id).await);
    assert!(gate.passed, "{gate:?}");
    let (id, sha, lease_id, evidence) = gate.candidate();
    assert_eq!(id, candidate_id);
    assert_eq!(sha, commit_sha);
    assert_eq!(lease_id, lease.lease_id);
    let VerifyTargetEvidence::Verified {
        cwd,
        before,
        after,
        reasons,
    } = evidence
    else {
        panic!("{evidence:?}");
    };
    assert!(reasons.is_empty());
    assert_eq!(cwd, lease.path.to_str().unwrap());
    assert_eq!(before.head, commit_sha);
    assert_eq!(after.head, commit_sha);
    assert!(before.provenance.registered && after.provenance.registered);
    assert_eq!(current(&fx.boot, "happy").await.status, TaskStatus::Done);
    let pending = wait_observations(&planner, 1).await;
    assert!(
        matches!(
            &pending[0],
            Observation::TaskGateResult { passed: true, .. }
        ),
        "{pending:?}"
    );
    assert_observations_exactly(&planner, 1).await;
    assert_eq!(gate_result_events(&fx, &task.id).await.len(), 1);
    let entry = fx.plan_entry("happy").await;
    assert_eq!(
        entry["candidate"]["verification"]["state"], "passed",
        "{entry}"
    );
    assert_eq!(
        entry["candidate"]["verification"]["gate_log"],
        format!("runs/{}/gates/1.log", task.id)
    );
}

// ---------------------------------------------------------------------------
// D8: `plan.list.candidate.verification` over the states a Track can hold at once.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plan_list_reads_gate_verification() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let verification = |entry: &Value| entry["candidate"]["verification"].clone();

    // passed
    let (_, task, _, _) = settled_gated_task(&fx, "v-passed", gated("true")).await;
    let before = verification(&fx.plan_entry("v-passed").await);
    assert_eq!(before["state"], "not_admitted", "{before}");
    assert_eq!(before["gate_attempt"], 0);
    fx.scheduler().drive_gate_for_test(task).await.unwrap();
    let v = verification(&fx.plan_entry("v-passed").await);
    assert_eq!(v["state"], "passed", "{v}");
    assert_eq!(v["gate_attempt"], 1);
    assert_eq!(v["target"]["kind"], "candidate");
    assert_eq!(v["target"]["evidence"]["kind"], "verified");
    assert!(v["log_path"].as_str().unwrap().ends_with("-g1.log"));

    // target_mismatch
    let (_, task, lease, _) = settled_gated_task(&fx, "v-mismatch", gated("true")).await;
    git(
        &lease.path,
        &["commit", "-q", "--allow-empty", "-m", "moved"],
    );
    fx.scheduler().drive_gate_for_test(task).await.unwrap();
    let v = verification(&fx.plan_entry("v-mismatch").await);
    assert_eq!(v["state"], "target_mismatch", "{v}");
    assert_eq!(v["target"]["evidence"]["kind"], "refused");
    assert_eq!(v["target"]["evidence"]["reasons"], json!(["head"]));

    // infra (no candidate: admitted before settlement)
    let worker = fx.new_worker("v-infra-w", AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&worker.card_id).await;
    install_pre_commit(&lease, HOOK_EXIT_1);
    let task = fx
        .running_task("v-infra", "codex", &worker.card_id, gated("true"))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "x\n").unwrap();
    fx.complete(&worker, &task.id).await;
    settle_by_hand(&fx, &task.id).await;
    remove_pre_commit(&lease);
    submit_gate_bypassing_admission(&fx, &task).await;
    let v = verification(&fx.plan_entry("v-infra").await);
    assert_eq!(v["state"], "infra", "{v}");
    assert_eq!(v["target"]["kind"], "no_candidate");

    // running (a gate Operation exists, parked)
    let gate_flag = fx.track_root.parent().unwrap().join("gate-may-finish");
    let (_, task, _, _) =
        settled_gated_task(&fx, "v-running", gate_then_wait("true", &gate_flag)).await;
    let drive = spawn_drive(fx.scheduler(), task.clone());
    wait_gate_parked(&fx, &task.id).await;
    let v = verification(&fx.plan_entry("v-running").await);
    assert_eq!(v, json!({"state": "running", "gate_attempt": 1}), "{v}");

    // unbound (a legacy verdict)
    let (_, task, _, _) = settled_gated_task(&fx, "v-unbound", gated("true")).await;
    sqlx::query(
        "UPDATE tasks SET status = 'done', gate_attempt = 1, gate_result_json = ?1 WHERE id = ?2",
    )
    .bind(json!({"passed": true, "log_tail": "", "log_path": "/l", "attempt": 1}).to_string())
    .bind(&task.id)
    .execute(&fx.pool())
    .await
    .unwrap();
    let v = verification(&fx.plan_entry("v-unbound").await);
    assert_eq!(v["state"], "unbound", "{v}");
    assert_eq!(
        v["target"],
        json!({"kind": "unbound", "reason": "legacy_verdict"})
    );

    // not_started (running worker) and ungated
    let worker = fx.new_worker("v-started-w", AgentProvider::Codex).await;
    fx.kernel_lease(&worker.card_id).await;
    fx.running_task("v-not-started", "codex", &worker.card_id, gated("true"))
        .await;
    let v = verification(&fx.plan_entry("v-not-started").await);
    assert_eq!(v, json!({"state": "not_started", "gate_attempt": 0}), "{v}");
    let worker = fx.new_worker("v-ungated-w", AgentProvider::Codex).await;
    fx.kernel_lease(&worker.card_id).await;
    fx.running_task("v-ungated", "codex", &worker.card_id, json!({}))
        .await;
    let v = verification(&fx.plan_entry("v-ungated").await);
    assert_eq!(v, json!({"state": "ungated", "gate_attempt": 0}), "{v}");

    // The summary keeps the state and the attempt.
    let summary = fx.plan_summary_entry("v-passed").await;
    assert_eq!(
        summary["candidate"]["verification"],
        json!({"state": "passed", "gate_attempt": 1}),
        "{summary}"
    );

    // not_admitted (pending delivery, no gate Operation). Last: the blocking hook lives in the
    // repository's common dir and would hold every later delivery too.
    let flag = fx.track_root.parent().unwrap().join("commit-may-proceed");
    let worker = fx.new_worker("v-pending-w", AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&worker.card_id).await;
    install_pre_commit(&lease, &hook_waiting_for(&flag, 0));
    let task = fx
        .running_task("v-pending", "codex", &worker.card_id, gated("true"))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "x\n").unwrap();
    fx.complete(&worker, &task.id).await;
    let entry = fx.plan_entry("v-pending").await;
    assert_eq!(
        entry["candidate"]["delivery"]["state"], "pending",
        "{entry}"
    );
    let v = verification(&entry);
    assert_eq!(
        v,
        json!({"state": "not_admitted", "gate_attempt": 0}),
        "{v}"
    );

    std::fs::write(&flag, b"").unwrap();
    std::fs::write(&gate_flag, b"").unwrap();
    drive.await.unwrap();
}

// ---------------------------------------------------------------------------
// 5.1.7: the mismatch `log_tail` carries the target as JSON and the log file has the line.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gate_target_mismatch_log_tail_carries_target_json() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();

    // The prepare-time refusal.
    let (_, task, lease, _) = settled_gated_task(&fx, "tail-refused", gated("true")).await;
    git(
        &lease.path,
        &["commit", "-q", "--allow-empty", "-m", "moved"],
    );
    fx.scheduler()
        .drive_gate_for_test(task.clone())
        .await
        .unwrap();
    let gate = gate_result(&wait_gate_result(&fx, &task.id).await);
    assert_eq!(gate.status_detail.as_deref(), Some("gate-target-mismatch"));
    let target = gate.log_tail_target();
    assert_eq!(target, gate.target.clone().unwrap());
    assert!(matches!(
        target,
        VerifyTarget::Candidate {
            evidence: VerifyTargetEvidence::Refused { .. },
            ..
        }
    ));
    let first_line = gate.log_tail.lines().next().unwrap();
    let log = std::fs::read_to_string(&gate.log_path).expect("log file exists");
    assert!(log.contains(first_line), "{log}");

    // The after-sample discard.
    let (_, task, _, _) = settled_gated_task(&fx, "tail-discarded", gated("touch extra.txt")).await;
    fx.scheduler()
        .drive_gate_for_test(task.clone())
        .await
        .unwrap();
    let gate = gate_result(&wait_gate_result(&fx, &task.id).await);
    assert_eq!(gate.status_detail.as_deref(), Some("gate-target-mismatch"));
    let target = gate.log_tail_target();
    assert_eq!(target, gate.target.clone().unwrap());
    assert!(matches!(
        target,
        VerifyTarget::Candidate {
            evidence: VerifyTargetEvidence::Verified { .. },
            ..
        }
    ));
    let first_line = gate.log_tail.lines().next().unwrap();
    let log = std::fs::read_to_string(&gate.log_path).expect("log file exists");
    assert!(log.contains("::gate-step t"), "{log}");
    assert!(log.contains(first_line), "{log}");
}

// ---------------------------------------------------------------------------
// A30, fifth fixture: an observation failure inside the provenance script is `Unsampled`, not
// a `provenance` verdict — the gate op is Stuck, `gate-infra`.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provenance_observation_failure_is_not_a_verdict() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    // The candidate is delivered by the real git; the gate samples under a `git` whose
    // `worktree list` prints its matching lines, then exits 128.
    let (_, task, _, candidate) = settled_gated_task(&fx, "observed", gated("true")).await;
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("git on PATH");
    let bin = fx.track_root.parent().unwrap().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_executable(
        &bin.join("git"),
        &format!(
            "#!/bin/sh\nREAL='{}'\nif [ \"$1\" = worktree ] && [ \"$2\" = list ]; then \"$REAL\" \"$@\"; exit 128; fi\nexec \"$REAL\" \"$@\"\n",
            real_git.display()
        ),
    );
    let original_path = std::env::var_os("PATH").unwrap();
    let mut dirs = vec![bin];
    dirs.extend(std::env::split_paths(&original_path));
    // The sampler resolves `git` from this process's PATH; one process per test under nextest.
    unsafe { std::env::set_var("PATH", std::env::join_paths(dirs).unwrap()) };
    let drive = fx.scheduler().drive_gate_for_test(task.clone()).await;
    unsafe { std::env::set_var("PATH", original_path) };
    drive.unwrap();

    let op = gate_op(&fx, &task.id).await.unwrap();
    assert_eq!(op.phase.tag(), PhaseTag::Stuck, "{op:?}");
    let events = gate_result_events(&fx, &task.id).await;
    assert_eq!(events.len(), 1, "{events:?}");
    let gate = &events[0];
    assert_eq!(gate.status_detail.as_deref(), Some("gate-infra"));
    let (id, _, _, evidence) = gate.candidate();
    assert_eq!(id, candidate.candidate_id);
    let SamplePhase::Prepare { reason } = unsampled_phase(evidence) else {
        panic!("{evidence:?}");
    };
    assert!(reason.contains("exit 1"), "{reason}");
    assert!(!reason.contains("provenance realpath="), "{reason}");
    assert_eq!(
        current(&fx.boot, "observed").await.status,
        TaskStatus::Failed
    );
}

// ---------------------------------------------------------------------------
// A11b: the kernel dies between the prepare commit and the spawn; the refused gate survives.
// ---------------------------------------------------------------------------

/// Claim `key`'s pending attempt the way dispatch does — the frozen context closure written
/// with the claim — then run it on `worker`. A row claimed by a bare status UPDATE has no
/// closure and the kernel's boot context sweep marks it stale (`refuse_if_context_stale`).
async fn claim_with_closure(fx: &Fx, key: &str, worker: &str) -> Task {
    let task = current(&fx.boot, key).await;
    assert_eq!(task.status, TaskStatus::Pending);
    let closure = calm_server::task_context::TaskContextMonitor::new(
        fx.boot.repo.clone(),
        fx.boot.ctx.events.clone(),
        fx.boot.ctx.write.clone(),
    )
    .resolve_task_closure(fx.track(), key)
    .await
    .unwrap();
    let pool = fx.pool();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    assert_eq!(
        calm_server::db::sqlite::task_claim_pending_tx(
            &mut tx,
            &task.id,
            now_ms(),
            &closure.refs,
            closure.closure_truncated,
        )
        .await
        .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    sqlx::query(
        "UPDATE tasks SET status = 'running', worker_card_id = ?1, updated_at_ms = ?3 WHERE id = ?2",
    )
    .bind(worker)
    .bind(&task.id)
    .bind(now_ms())
    .execute(&pool)
    .await
    .unwrap();
    current(&fx.boot, key).await
}

/// The world of an out-of-process test, on a file-backed database the kernel binary is launched
/// against: the in-process kernel stays passive (its live listener stopped, its sweeps
/// boot-gated) and only seeds rows through the production paths.
struct FileWorld {
    fx: Fx,
    tmp_path: PathBuf,
    db_path: PathBuf,
    _tmp: tempfile::TempDir,
}

async fn file_world() -> FileWorld {
    use crate::mcp_track_report::boot_at;
    let tmp = tempfile::tempdir().expect("tempdir");
    let tmp_path = tmp.path().to_path_buf();
    let db_path = tmp_path.join("calm.db");
    let db_str = db_path.to_string_lossy().to_string();
    assert!(!db_str.contains("/.local/share/neige-calm"));
    let db_url = format!("sqlite://{db_str}?mode=rwc");
    let fx = fixture_on(boot_at(&db_url).await, |tmp| {
        let repo = tmp.join("repo");
        init_repo(&repo);
        repo
    })
    .await;
    fx.dispatcher.abort_event_listener_for_test();
    FileWorld {
        fx,
        tmp_path,
        db_path,
        _tmp: tmp,
    }
}

/// A gated attempt declared, claimed with its context closure (so the kernel's boot context
/// sweep keeps it), delivered and settled as a candidate: `verifying@0` with a candidate row,
/// the shape the launched kernel's boot sweep admits.
async fn file_world_candidate(
    world: &FileWorld,
    key: &str,
    gate: Value,
) -> (
    Task,
    calm_server::test_seams::KernelWorkspaceLease,
    CandidateRowView,
) {
    use crate::task_recovery::declare;
    let fx = &world.fx;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    declare(
        &fx.boot,
        json!({
            "key": key, "kind": "codex", "goal": format!("deliver {key}"),
            "declared_by": calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,
            "ready": true, "gate": gate,
        }),
    )
    .await;
    let task = claim_with_closure(fx, key, &worker.card_id).await;
    std::fs::write(lease.path.join("worker.txt"), "delivered\n").unwrap();
    fx.complete(&worker, &task.id).await;
    let row = settle_by_hand(fx, &task.id).await;
    assert_eq!(row.settlement.as_deref(), Some("candidate"));
    let candidate = fx.candidate_row(&task.id).await.unwrap();
    let task = current(&fx.boot, key).await;
    assert_eq!(task.status, TaskStatus::Verifying);
    (task, lease, candidate)
}

/// Poll `#g1` of `task_id` on the file database until `ready` holds (the launched kernel
/// drives it).
async fn wait_file_gate_op(
    world: &FileWorld,
    task_id: &str,
    what: &str,
    ready: impl Fn(&Operation) -> bool,
) -> Operation {
    wait_gate_op_until(&world.fx, task_id, what, ready).await
}

fn parked_artifacts(op: &Operation) -> calm_server::operation::SpawnArtifacts {
    op.spawn_artifacts.clone().expect("parked with artifacts")
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_gate_survives_restart_between_prepare_and_spawn() {
    use crate::support::kernel_proc::{
        ChildGuard, free_port_or_skip, launch_kernel_to, spawn_kernel_to, wait_exit_with_timeout,
    };

    let Some(port) = free_port_or_skip("gate-restart") else {
        return; // SKIP was printed: sandbox denied loopback bind (hosted CI runners)
    };
    // The world, through the production paths, on the file-backed database the kernel binary
    // is launched against next: a gated attempt declared, claimed with its context closure,
    // delivered and settled as a candidate; then HEAD moved past the candidate.
    let world = file_world().await;
    let fx = &world.fx;
    let (task, lease, candidate) = file_world_candidate(
        &world,
        "gated",
        json!({"steps": [{"name": "t", "cmd": "true"}]}),
    )
    .await;
    git(
        &lease.path,
        &["commit", "-q", "--allow-empty", "-m", "moved"],
    );
    let moved = git(&lease.path, &["rev-parse", "HEAD"]);
    assert_ne!(moved, candidate.commit_sha);
    let task_id = task.id.clone();
    let key = format!("{task_id}#g1");
    let pool = fx.pool();

    // boot#1: the boot sweep drives the gate; prepare refuses and commits; the seam aborts
    // before the spawn. The abort can land before or after the listener binds, so boot#1 is
    // waited on for its exit, not for readiness. Its stderr is kept: the seam names itself.
    let crash_env: Vec<(&str, OsString)> = vec![(
        "CALM_TEST_CRASH_AT",
        OsString::from("task-verify-post-prepare"),
    )];
    let boot1_log = world.tmp_path.join("boot1.log");
    let mut boot1 = ChildGuard {
        child: spawn_kernel_to(
            &world.tmp_path,
            &world.db_path,
            port,
            &crash_env,
            Some(&boot1_log),
        ),
        port,
    };
    let status = wait_exit_with_timeout(&mut boot1, Duration::from_secs(60));
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(libc::SIGABRT),
        "boot#1 must die by the CALM_TEST_CRASH_AT abort seam, got {status:?}"
    );
    let boot1_output = std::fs::read_to_string(&boot1_log).unwrap();
    assert!(
        boot1_output.contains("CALM_TEST_CRASH_AT=task-verify-post-prepare: aborting"),
        "boot#1 died at the named seam:\n{boot1_output}"
    );
    let (op_id, phase, tx_output, artifacts): (String, String, Option<String>, Option<String>) =
        sqlx::query_as(
            "SELECT id, phase, tx_output_json, spawn_artifacts_json FROM operations \
             WHERE kind = 'task-verify' AND idempotency_key = ?1",
        )
        .bind(&key)
        .fetch_one(&pool)
        .await
        .unwrap();
    // The driver persists `SpawnStarted` before it calls `spawn_side_effect`, whose first
    // statement is the seam: this is exactly the phase the abort leaves behind.
    assert_eq!(phase, "spawn_started", "the crash window's phase");
    let frozen: Value =
        serde_json::from_str(tx_output.as_deref().expect("prepare committed")).unwrap();
    assert_eq!(frozen["data"]["target"]["refused"], true, "{frozen}");
    assert!(artifacts.is_none());
    let events = gate_result_events(fx, &task_id).await;
    assert_eq!(
        events.len(),
        1,
        "the refusal was written in the prepare transaction: {events:?}"
    );
    assert_eq!(current(&fx.boot, "gated").await.status, TaskStatus::Failed);

    // boot#2: recovery drives the op from its committed freeze; the spawn is a no-op. Its
    // log carries the recovery plan the kernel prints before applying it.
    let boot2_log = world.tmp_path.join("boot2.log");
    let Some(mut boot2) = launch_kernel_to(
        &world.tmp_path,
        &world.db_path,
        "boot-2",
        &[],
        Some(&boot2_log),
    ) else {
        return; // SKIP was printed
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let phase = loop {
        let phase: String = sqlx::query_scalar(
            "SELECT phase FROM operations WHERE kind = 'task-verify' AND idempotency_key = ?1",
        )
        .bind(&key)
        .fetch_one(&pool)
        .await
        .unwrap();
        if phase == "succeeded" || phase == "failed" || phase == "stuck" {
            break phase;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "boot#2 never completed the gate op: {phase}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    boot2.sigkill_and_reap();
    assert_eq!(phase, "succeeded");
    let boot2_output = std::fs::read_to_string(&boot2_log).unwrap();
    let plan_line = boot2_output
        .lines()
        .find(|line| line.contains("operation recovery plan item") && line.contains(&op_id))
        .unwrap_or_else(|| {
            panic!("boot#2 logged no recovery plan item for {op_id}:\n{boot2_output}")
        });
    assert!(
        plan_line.contains("drive from spawn_started"),
        "recovery resumed the op from spawn_started: {plan_line}"
    );
    let op = gate_op(fx, &task_id).await.unwrap();
    assert!(
        op.spawn_artifacts.is_none(),
        "no process was ever spawned: {op:?}"
    );
    let task = current(&fx.boot, "gated").await;
    assert_eq!(task.status, TaskStatus::Failed);
    assert_eq!(task.gate_attempt, 1);
    let events = gate_result_events(fx, &task_id).await;
    assert_eq!(
        events.len(),
        1,
        "exactly one gate result across abort + reboot: {events:?}"
    );
    let gate = &events[0];
    assert_eq!(gate.status_detail.as_deref(), Some("gate-target-mismatch"));
    let (id, _, _, evidence) = gate.candidate();
    assert_eq!(id, candidate.candidate_id);
    let VerifyTargetEvidence::Refused {
        before, reasons, ..
    } = evidence
    else {
        panic!("{evidence:?}");
    };
    assert_eq!(before.head, moved);
    assert_eq!(reasons, &[MismatchReason::Head]);
}

// ---------------------------------------------------------------------------
// Review round 1 (3): the dead-process recovery path stops the recorded group before its
// after-sample — a step's backgrounded child that outlived the wrapper is killed, not left
// to write after the sample. Out of process: the kernel that spawned the gate is SIGKILLed
// while the wrapper waits, so no live observer of that boot survives to kill the group.
// ---------------------------------------------------------------------------

/// Kills a recorded gate group's still-marked members on drop — on unwind (panic) and on every
/// early `return` — so a wrapper/straggler `until [ -f <flag> ]; do sleep 0.1; done` loop cannot
/// spin forever once its tempdir (holding the flag) is gone. Authenticated by the same inherited
/// `NEIGE_GATE_OP` marker production's recovery sweep uses, so it never signals a foreign process
/// that recycled the pgid.
#[cfg(target_os = "linux")]
struct GateGroupCleanup {
    pgid: i32,
    marker: String,
}
#[cfg(target_os = "linux")]
impl Drop for GateGroupCleanup {
    fn drop(&mut self) {
        let members = calm_server::proc_identity::group_members_with_env_marker(
            self.pgid,
            "NEIGE_GATE_OP",
            &self.marker,
        );
        let _ = calm_server::proc_identity::sigkill_verified_members(&members);
    }
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovered_gate_stops_the_group_before_sampling() {
    use crate::support::kernel_proc::{free_port_or_skip, launch_kernel};
    use calm_server::proc_identity::{
        parse_proc_stat_fields, read_proc_start_time, scan_process_group_members, verify_owned_pid,
    };

    if free_port_or_skip("gate-stragglers").is_none() {
        return; // SKIP was printed: sandbox denied loopback bind (hosted CI runners)
    }
    let world = file_world().await;
    let fx = &world.fx;
    let straggler_flag = world.tmp_path.join("straggler-may-finish");
    let leader_flag = world.tmp_path.join("gate-may-finish");
    let pidfile = world.tmp_path.join("straggler.pid");
    // The step backgrounds a child in the wrapper's group (no job control: same pgid) that
    // waits for its own flag and then writes into the checkout; the wrapper itself waits.
    let step = format!(
        "( until [ -f '{}' ]; do sleep 0.1; done; touch late.txt ) & echo $! > '{}'; until [ -f '{}' ]; do sleep 0.1; done",
        straggler_flag.display(),
        pidfile.display(),
        leader_flag.display()
    );
    let (task, lease, candidate) = file_world_candidate(
        &world,
        "stragglers",
        json!({"steps": [{"name": "t", "cmd": step}], "timeout_secs": 600}),
    )
    .await;

    // boot#1 admits and runs the gate: `#g1` parked, the straggler recorded.
    let Some(mut boot1) = launch_kernel(&world.tmp_path, &world.db_path, "boot-1", &[]) else {
        return; // SKIP was printed
    };
    let op = wait_file_gate_op(&world, &task.id, "parked", |op| {
        op.phase.tag() == PhaseTag::Parked && op.spawn_artifacts.is_some()
    })
    .await;
    let artifacts = parked_artifacts(&op);
    assert_eq!(artifacts.pgid, artifacts.pid, "the wrapper leads its group");
    // Installed the moment the group is observed: on panic or any early return this kills the
    // still-marked wrapper/straggler so their flag-poll loops cannot outlive the vanished tempdir.
    let _gate_cleanup = GateGroupCleanup {
        pgid: artifacts.pgid,
        marker: format!("{}#g1", task.id),
    };
    wait_for_file(&pidfile);
    let straggler: i32 = tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(raw) = std::fs::read_to_string(&pidfile)
                && raw.ends_with('\n')
                && let Ok(pid) = raw.trim().parse::<i32>()
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the step recorded its background child");
    let straggler_start = read_proc_start_time(straggler).expect("the straggler is alive");
    let stat = std::fs::read_to_string(format!("/proc/{straggler}/stat")).unwrap();
    assert_eq!(
        parse_proc_stat_fields(&stat).unwrap().pgrp,
        artifacts.pgid,
        "the straggler is in the recorded group"
    );

    // The kernel dies with the wrapper still waiting; then the wrapper finishes on its own —
    // exit 0, exit file written, its leader reaped by init — and the straggler outlives it.
    boot1.sigkill_and_reap();
    assert!(verify_owned_pid(
        artifacts.pid,
        artifacts.start_time,
        &artifacts.boot_id
    ));
    std::fs::write(&leader_flag, b"").unwrap();
    let exit_path = exit_path_of(&op);
    wait_for_file(&exit_path);
    assert_eq!(std::fs::read_to_string(&exit_path).unwrap().trim(), "0");
    tokio::time::timeout(WAIT, async {
        while verify_owned_pid(artifacts.pid, artifacts.start_time, &artifacts.boot_id) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the wrapper's leader is reaped");
    assert_eq!(
        read_proc_start_time(straggler),
        Some(straggler_start),
        "the straggler outlived the wrapper"
    );
    assert!(!lease.path.join("late.txt").exists());

    // boot#2: the dead-process recovery reads the exit file, stops the group, samples clean.
    let Some(mut boot2) = launch_kernel(&world.tmp_path, &world.db_path, "boot-2", &[]) else {
        return; // SKIP was printed
    };
    let op = wait_file_gate_op(&world, &task.id, "succeeded", |op| {
        op.phase.tag() == PhaseTag::Succeeded
    })
    .await;
    let result: TaskGateResult =
        serde_json::from_value(op.tx_output.as_ref().unwrap().result.clone()).unwrap();
    assert!(result.verdict.passed, "{result:?}");
    // The straggler was stopped before the after-sample. A SIGKILLed straggler lingers as a zombie
    // (its start_time unchanged) until init reaps it, so the bounded check accepts gone, recycled
    // (start_time changed) OR Z/X — the same predicate production's `group_stopped` uses.
    tokio::time::timeout(WAIT, async {
        loop {
            let dead = match std::fs::read_to_string(format!("/proc/{straggler}/stat"))
                .ok()
                .and_then(|stat| parse_proc_stat_fields(&stat))
            {
                None => true,
                Some(fields) => {
                    fields.start_time != straggler_start
                        || fields.state == 'Z'
                        || fields.state == 'X'
                }
            };
            if dead {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the recovery path stopped the recorded group before sampling");
    assert!(
        scan_process_group_members(artifacts.pgid)
            .iter()
            .all(|member| member.is_zombie),
        "{:?}",
        scan_process_group_members(artifacts.pgid)
    );
    let gate = gate_result(&wait_gate_result(fx, &task.id).await);
    boot2.sigkill_and_reap();
    assert!(gate.passed, "{gate:?}");
    assert_eq!(gate.status_detail, None);
    let (id, _, _, evidence) = gate.candidate();
    assert_eq!(id, candidate.candidate_id);
    let VerifyTargetEvidence::Verified { after, reasons, .. } = evidence else {
        panic!("{evidence:?}");
    };
    assert!(reasons.is_empty(), "{evidence:?}");
    assert!(after.dirty.is_empty(), "{after:?}");
    assert_eq!(
        current(&fx.boot, "stragglers").await.status,
        TaskStatus::Done
    );
    assert_eq!(gate_result_events(fx, &task.id).await.len(), 1);
    // The straggler is dead, so releasing it changes nothing: the verdict stood on a checkout
    // no descendant could still write to.
    std::fs::write(&straggler_flag, b"").unwrap();
    assert!(!lease.path.join("late.txt").exists());
}

// ---------------------------------------------------------------------------
// Review round 1 (5c): the boot-reattach observer (the wrapper alive at reboot) samples after
// too. Out of process, as above. The row is terminal before boot#2 (the D12 (i) shape: a
// Planner flipped it while the gate was parked), so boot#2's scheduler drives no gate — its
// 25 ms `wait` loop would otherwise run the driver's dead-work probe, which completes a dead
// leader through `recover_parked`'s `!alive` arm ahead of the 2 s re-attach poll (A12b pins
// that arm). With no waiter, the re-attached observer is the one completion path, and the op
// result carries its `finalize`.
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_reattached_gate_result_discarded_when_tree_changed() {
    use crate::support::kernel_proc::{free_port_or_skip, launch_kernel};
    use calm_server::proc_identity::verify_owned_pid;

    if free_port_or_skip("gate-reattach").is_none() {
        return; // SKIP was printed: sandbox denied loopback bind (hosted CI runners)
    }
    let world = file_world().await;
    let fx = &world.fx;
    let leader_flag = world.tmp_path.join("gate-may-finish");
    let step = format!(
        "touch extra.txt; until [ -f '{}' ]; do sleep 0.1; done",
        leader_flag.display()
    );
    let (task, lease, candidate) = file_world_candidate(
        &world,
        "reattach-live",
        json!({"steps": [{"name": "t", "cmd": step}], "timeout_secs": 600}),
    )
    .await;

    // boot#1 runs the gate to parked; the step has left its file; the kernel dies.
    let Some(mut boot1) = launch_kernel(&world.tmp_path, &world.db_path, "boot-1", &[]) else {
        return; // SKIP was printed
    };
    let op = wait_file_gate_op(&world, &task.id, "parked", |op| {
        op.phase.tag() == PhaseTag::Parked && op.spawn_artifacts.is_some()
    })
    .await;
    let artifacts = parked_artifacts(&op);
    // Kills the still-marked wrapper on panic or early return (see `GateGroupCleanup`).
    let _gate_cleanup = GateGroupCleanup {
        pgid: artifacts.pgid,
        marker: format!("{}#g1", task.id),
    };
    wait_for_file(&lease.path.join("extra.txt"));
    boot1.sigkill_and_reap();
    assert!(verify_owned_pid(
        artifacts.pid,
        artifacts.start_time,
        &artifacts.boot_id
    ));
    // The row is flipped by hand while the gate is parked (see the header): boot#2 has no
    // `verifying` row to drive, so nothing waits on `#g1` and nothing probes it.
    sqlx::query(
        "UPDATE tasks SET status = 'failed', status_detail = 'delivery-abandoned', finished_at_ms = ?1 WHERE id = ?2",
    )
    .bind(now_ms())
    .bind(&task.id)
    .execute(&fx.pool())
    .await
    .unwrap();

    // boot#2 finds the wrapper alive: the Boot arm leaves the op parked and re-attaches an
    // observer (the listener binds only after recovery, so readiness proves the arm ran).
    let Some(mut boot2) = launch_kernel(&world.tmp_path, &world.db_path, "boot-2", &[]) else {
        return; // SKIP was printed
    };
    let op = gate_op(fx, &task.id).await.unwrap();
    assert_eq!(op.phase.tag(), PhaseTag::Parked, "{op:?}");
    assert!(
        op.lease_owner.is_none(),
        "the parked lease was cleared for boot"
    );
    assert!(verify_owned_pid(
        artifacts.pid,
        artifacts.start_time,
        &artifacts.boot_id
    ));

    // The wrapper finishes (exit 0); the re-attached observer reads the exit file, stops the
    // group and samples: the tree changed during the steps, the result is discarded. The op
    // result is the observer's verdict (the terminal row takes no result: guard miss, no
    // event).
    std::fs::write(&leader_flag, b"").unwrap();
    let op = wait_file_gate_op(&world, &task.id, "succeeded", |op| {
        op.phase.tag() == PhaseTag::Succeeded
    })
    .await;
    boot2.sigkill_and_reap();
    let result: TaskGateResult =
        serde_json::from_value(op.tx_output.as_ref().unwrap().result.clone()).unwrap();
    assert!(!result.verdict.passed, "{result:?}");
    assert_eq!(
        result.verdict.status_detail.as_deref(),
        Some("gate-target-mismatch")
    );
    assert_eq!(result.cwd.as_deref(), lease.path.to_str());
    let VerifyTarget::Candidate {
        candidate_id,
        evidence,
        ..
    } = &result.target
    else {
        panic!("{result:?}");
    };
    assert_eq!(candidate_id, &candidate.candidate_id);
    let VerifyTargetEvidence::Verified { after, reasons, .. } = evidence else {
        panic!("{evidence:?}");
    };
    assert_eq!(after.dirty, vec!["?? extra.txt".to_string()]);
    assert_eq!(reasons, &[MismatchReason::Dirty]);
    let log = std::fs::read_to_string(&result.verdict.log_path).unwrap();
    assert!(log.contains("::gate-step t"), "{log}");
    assert!(log.contains("gate RESULT DISCARDED"), "{log}");
    assert!(gate_result_events(fx, &task.id).await.is_empty());
    assert_eq!(
        current(&fx.boot, "reattach-live").await.status,
        TaskStatus::Failed
    );
}
