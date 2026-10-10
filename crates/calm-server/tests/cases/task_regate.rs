//! #2405: `neige_task_regate` re-runs a failed gate on the same candidate. The attempt's own row
//! goes back to `verifying`; the scheduler's gate drive runs the frozen gate against the attempt's
//! candidate again as the attempt after a reserved, never-run number. Fixtures come from
//! `git_delivery.rs`: a real kernel delivery in a fixture repository, the Planner calling the tool
//! through the registry, and the scheduler driving the gate.
use std::path::Path;
use std::time::Duration;

use super::git_delivery::*;
use crate::mcp_track_report::{call_tool, planner_identity, upsert_block};
use crate::task_recovery::{current, declare};
use calm_server::event::Event;
use calm_server::ids::ActorId;
use calm_server::mcp_server::registry::ToolCallIdentity;
use calm_server::model::{Task, TaskStatus};
use calm_server::operation::OperationKey;
use calm_server::operation::task_verify_adapter::{
    TASK_VERIFY_KIND, TaskGateResult, TaskVerifyOperationPayload,
};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::session_projection_repo::AgentProvider;
use calm_server::test_seams::KernelWorkspaceLease;
use calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR;
use calm_types::verify_target::{MismatchReason, VerifyTarget, VerifyTargetEvidence};
use serde_json::{Value, json};

const REGATE: &str = "neige_task_regate";

/// A gate whose one step passes once `green` exists (outside the checkout, so the step never
/// dirties it) and then waits for `release` when one is given.
fn gate_on(green: &Path, release: Option<&Path>) -> Value {
    let wait = release.map_or(String::new(), |release| {
        format!(
            " && until [ -f '{}' ]; do sleep 0.1; done",
            release.display()
        )
    });
    json!({
        "gate": {"steps": [{"name": "env", "cmd": format!("test -f '{}'{wait}", green.display())}],
                 "timeout_secs": 60},
        "no_gate_reason": null,
    })
}

fn outside(fx: &Fx, name: &str) -> std::path::PathBuf {
    fx.track_root.parent().unwrap().join(name)
}

async fn regate_as(fx: &Fx, identity: ToolCallIdentity, attempt: &str) -> Result<Value, RpcError> {
    call_tool(
        &fx.boot,
        REGATE,
        identity,
        json!({"attempt_id": attempt, "message": "the gate environment is fixed"}),
    )
    .await
}

async fn regate(fx: &Fx, attempt: &str) -> Result<Value, RpcError> {
    regate_as(fx, planner_identity(&fx.boot), attempt).await
}

/// The refusal of a regate the caller expects to be refused, with its message.
async fn refused(fx: &Fx, attempt: &str, code: i64, needle: &str) {
    let err = regate(fx, attempt)
        .await
        .expect_err("the regate is refused");
    assert_eq!(err.code, code, "{err:?}");
    assert!(err.message.contains(needle), "want {needle:?} in {err:?}");
}

/// Every `task.gate_result` of `attempt`, oldest first.
async fn verdicts(fx: &Fx, attempt: &str) -> Vec<TaskGateResult> {
    fx.events_for(GATE_RESULT_KIND, attempt)
        .await
        .into_iter()
        .map(|row| match row.event {
            Event::TaskGateResult {
                passed,
                failing_step,
                exit_code,
                log_tail,
                log_path,
                attempt,
                status_detail,
                target,
                ..
            } => TaskGateResult {
                verdict: calm_server::operation::task_verify_adapter::GateVerdict {
                    passed,
                    status_detail,
                    failing_step,
                    exit_code,
                    log_tail,
                    log_path,
                    attempt,
                },
                cwd: None,
                target: target.expect("a candidate-bound verdict carries its target"),
            },
            other => panic!("not a gate result: {other:?}"),
        })
        .collect()
}

fn attempts(verdicts: &[TaskGateResult]) -> Vec<i64> {
    verdicts.iter().map(|v| v.verdict.attempt).collect()
}

/// `(attempt, phase)` of every task-verify op of `task_id`, by attempt.
async fn gate_ops(fx: &Fx, task_id: &str) -> Vec<(String, String)> {
    let mut ops: Vec<(String, String)> = sqlx::query_as(
        "SELECT idempotency_key, phase FROM operations WHERE kind = ?1 \
         AND substr(idempotency_key, 1, length(?2)) = ?2",
    )
    .bind(TASK_VERIFY_KIND)
    .bind(format!("{task_id}#g"))
    .fetch_all(&fx.pool())
    .await
    .unwrap();
    ops.sort();
    ops.into_iter()
        .map(|(key, phase)| (key.rsplit_once("#g").unwrap().1.to_string(), phase))
        .collect()
}

fn candidate_of(verdict: &TaskGateResult) -> (&str, &str, &VerifyTargetEvidence) {
    match &verdict.target {
        VerifyTarget::Candidate {
            candidate_id,
            commit_sha,
            evidence,
            ..
        } => (candidate_id, commit_sha, evidence),
        other => panic!("not a candidate target: {other:?}"),
    }
}

/// The report's forge Operation to terminal, then the settlement step by hand.
async fn settle_by_hand(fx: &Fx, task_id: &str) -> DeliveryRowView {
    fx.wait_forge_op(task_id).await;
    let row = fx.delivery_row(task_id).await.expect("delivery row");
    fx.scheduler()
        .settle_git_delivery_for_test(&row.delivery_id)
        .await
        .unwrap();
    fx.delivery_row(task_id).await.unwrap()
}

/// A candidate-bound gated attempt of a new worker, reported and settled by hand (the caller has
/// stopped the live listener). `change` is written into the checkout first; `None` delivers no
/// change.
async fn settled(
    fx: &Fx,
    key: &str,
    gate: Value,
    change: Option<&str>,
) -> (Task, KernelWorkspaceLease, CandidateRowView) {
    let worker = fx.new_worker(key, AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&worker.card_id, key).await;
    let task = fx.running_task(key, "codex", &worker.card_id, gate).await;
    if let Some(change) = change {
        std::fs::write(lease.path.join(format!("{key}.txt")), change).unwrap();
    }
    fx.complete(&worker, &task.id).await;
    let row = settle_by_hand(fx, &task.id).await;
    assert_eq!(row.settlement.as_deref(), Some("candidate"), "{row:?}");
    let candidate = fx.candidate_row(&task.id).await.expect("candidate");
    assert_eq!(current(&fx.boot, key).await.status, TaskStatus::Verifying);
    (task, lease, candidate)
}

/// One gate drive of `key`'s current row, by the scheduler's own drive.
async fn drive(fx: &Fx, key: &str) {
    let task = current(&fx.boot, key).await;
    fx.scheduler().drive_gate_for_test(task).await.unwrap();
}

/// A settled attempt whose first gate run went red at attempt 1.
async fn red(fx: &Fx, key: &str, gate: Value) -> (Task, KernelWorkspaceLease, CandidateRowView) {
    let (task, lease, candidate) = settled(fx, key, gate, Some("change\n")).await;
    drive(fx, key).await;
    let row = current(&fx.boot, key).await;
    assert_eq!(row.status, TaskStatus::Failed, "{row:?}");
    assert_eq!(row.status_detail.as_deref(), Some("gate-red"));
    assert_eq!(attempts(&verdicts(fx, &task.id).await), vec![1]);
    (task, lease, candidate)
}

/// The rows a re-run must not add: cards, leases, deliveries, candidates of this Track.
async fn execution_rows(fx: &Fx) -> [i64; 4] {
    [
        fx.table_count("cards").await,
        fx.table_count("workspace_leases").await,
        fx.table_count("task_git_deliveries").await,
        fx.table_count("task_candidates").await,
    ]
}

async fn regate_events(fx: &Fx) -> Vec<calm_server::db::TrackEvent> {
    fx.boot
        .repo
        .events_for_track(fx.track(), &["task.regate_requested"], None)
        .await
        .unwrap()
}

// ---------------------------------------------------------------------------
// T1: the live path — the regate event pokes the scheduler, whose pass drives the gate again.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn regate_reruns_gate_on_same_candidate() {
    let fx = fixture().await;
    let green = outside(&fx, "green");
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id, "rerun").await;
    let task = fx
        .running_task("rerun", "codex", &worker.card_id, gate_on(&green, None))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "rerun\n").unwrap();
    fx.complete(&worker, &task.id).await;
    wait_gate_result(&fx, &task.id).await;
    let first = verdicts(&fx, &task.id).await;
    assert_eq!(attempts(&first), vec![1]);
    assert_eq!(first[0].verdict.status_detail.as_deref(), Some("gate-red"));
    assert_eq!(current(&fx.boot, "rerun").await.status, TaskStatus::Failed);
    let before = execution_rows(&fx).await;

    std::fs::write(&green, b"").unwrap();
    let receipt = regate(&fx, &task.id).await.unwrap();
    assert_eq!(
        receipt,
        json!({"status": "verifying", "key": "rerun", "previous_gate_run": 1, "next_gate_run": 3})
    );

    let all = tokio::time::timeout(WAIT, async {
        loop {
            let all = verdicts(&fx, &task.id).await;
            if all.len() >= 2 {
                break all;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the re-run's verdict");
    assert_eq!(attempts(&all), vec![1, 3]);
    assert!(all[1].verdict.passed, "{:?}", all[1]);
    let (first_candidate, first_sha, _) = candidate_of(&all[0]);
    let (candidate, sha, evidence) = candidate_of(&all[1]);
    assert_eq!((candidate, sha), (first_candidate, first_sha));
    let VerifyTargetEvidence::Verified { after, reasons, .. } = evidence else {
        panic!("{evidence:?}");
    };
    assert!(reasons.is_empty());
    assert_eq!(after.head, sha);
    let row = current(&fx.boot, "rerun").await;
    assert_eq!((row.status, row.gate_attempt), (TaskStatus::Done, 3));
    assert_eq!(
        execution_rows(&fx).await,
        before,
        "no card, lease or delivery"
    );
    assert_eq!(gate_ops(&fx, &task.id).await.len(), 2);

    let events = regate_events(&fx).await;
    assert_eq!(events.len(), 1, "{events:?}");
    match &events[0].event {
        Event::TaskRegateRequested {
            attempt_id,
            key,
            previous_gate_attempt,
            reserved_gate_attempt,
            agent_message,
        } => {
            assert_eq!(
                (attempt_id.as_str(), key.as_str()),
                (task.id.as_str(), "rerun")
            );
            assert_eq!((*previous_gate_attempt, *reserved_gate_attempt), (1, 2));
            assert_eq!(agent_message, "the gate environment is fixed");
        }
        other => panic!("{other:?}"),
    }
}

// ---------------------------------------------------------------------------
// T2 + T8: a drive right after the regate submits only the new attempt and never copies the
// old verdict back; `verification` reads not_admitted, then running, then the new verdict.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn regate_never_recopies_the_old_verdict() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let (green, release) = (outside(&fx, "green"), outside(&fx, "release"));
    let (task, _, _) = red(&fx, "recopy", gate_on(&green, Some(&release))).await;

    regate(&fx, &task.id).await.unwrap();
    refused(&fx, &task.id, -32409, "task recopy is verifying").await;
    let verification = |entry: Value| entry["candidate"]["verification"].clone();
    assert_eq!(
        verification(fx.plan_entry("recopy").await),
        json!({"state": "not_admitted", "gate_attempt": 2})
    );

    std::fs::write(&green, b"").unwrap();
    let scheduler = fx.scheduler();
    let row = current(&fx.boot, "recopy").await;
    let run = tokio::spawn(async move { scheduler.drive_gate_for_test(row).await.unwrap() });
    tokio::time::timeout(WAIT, async {
        while !gate_ops(&fx, &task.id)
            .await
            .iter()
            .any(|(attempt, phase)| attempt == "3" && phase == "parked")
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("#g3 parked");
    assert_eq!(
        verification(fx.plan_entry("recopy").await),
        json!({"state": "running", "gate_attempt": 3})
    );
    std::fs::write(&release, b"").unwrap();
    run.await.unwrap();

    assert_eq!(
        gate_ops(&fx, &task.id).await,
        vec![
            ("1".to_string(), "succeeded".to_string()),
            ("3".to_string(), "succeeded".to_string())
        ],
        "only the new attempt was submitted"
    );
    let all = verdicts(&fx, &task.id).await;
    assert_eq!(
        attempts(&all),
        vec![1, 3],
        "the attempt-1 verdict is not written twice"
    );
    assert!(all[1].verdict.passed);
    let row = current(&fx.boot, "recopy").await;
    assert_eq!((row.status, row.gate_attempt), (TaskStatus::Done, 3));
    let v = verification(fx.plan_entry("recopy").await);
    assert_eq!(v["state"], "passed", "{v}");
    assert_eq!(v["gate_log"], format!("runs/{}/gates/3.log", task.id));
}

// ---------------------------------------------------------------------------
// T4: an orphan op `#g2` (submitted after the row failed, refused in prepare) is skipped.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn regate_skips_an_orphan_gate_op() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let green = outside(&fx, "green");
    let (task, _, _) = red(&fx, "orphan", gate_on(&green, None)).await;
    // A drive holding a stale `verifying@1` snapshot submits `#g2`; prepare refuses the failed row.
    let payload = serde_json::to_value(TaskVerifyOperationPayload {
        actor: ActorId::KernelDispatcher,
        track_id: task.track_id.clone(),
        task_id: task.id.clone(),
        attempt: 2,
    })
    .unwrap();
    let op_id = fx
        .runtime
        .submit(
            TASK_VERIFY_KIND,
            OperationKey {
                operation_key: calm_server::model::new_id(),
                idempotency_key: Some(format!("{}#g2", task.id)),
                payload_hash: calm_server::routes::idempotency_key::stable_payload_hash(&payload)
                    .unwrap(),
            },
            payload,
        )
        .await
        .unwrap();
    tokio::time::timeout(WAIT, fx.runtime.wait(&op_id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        gate_ops(&fx, &task.id).await[1],
        ("2".into(), "failed".into())
    );

    std::fs::write(&green, b"").unwrap();
    let receipt = regate(&fx, &task.id).await.unwrap();
    assert_eq!(receipt["next_gate_run"], 4, "{receipt}");
    drive(&fx, "orphan").await;
    let ops: Vec<String> = gate_ops(&fx, &task.id)
        .await
        .into_iter()
        .map(|o| o.0)
        .collect();
    assert_eq!(ops, ["1", "2", "4"]);
    let all = verdicts(&fx, &task.id).await;
    assert_eq!(attempts(&all), vec![1, 4]);
    assert!(all[1].verdict.passed, "{:?}", all[1]);
    let row = current(&fx.boot, "orphan").await;
    assert_eq!((row.status, row.gate_attempt), (TaskStatus::Done, 4));
}

// ---------------------------------------------------------------------------
// T5: a `no_change` delivery re-runs against its base commit.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn regate_reruns_a_no_change_candidate_at_its_base() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let green = outside(&fx, "green");
    let (task, _, candidate) = settled(&fx, "unchanged", gate_on(&green, None), None).await;
    assert_eq!(candidate.commit_sha, candidate.base_sha, "no_change");
    drive(&fx, "unchanged").await;
    assert_eq!(
        current(&fx.boot, "unchanged").await.status,
        TaskStatus::Failed
    );

    std::fs::write(&green, b"").unwrap();
    regate(&fx, &task.id).await.unwrap();
    drive(&fx, "unchanged").await;
    let all = verdicts(&fx, &task.id).await;
    assert_eq!(attempts(&all), vec![1, 3]);
    assert!(all[1].verdict.passed, "{:?}", all[1]);
    let (_, sha, evidence) = candidate_of(&all[1]);
    assert_eq!(sha, candidate.base_sha);
    let VerifyTargetEvidence::Verified { before, .. } = evidence else {
        panic!("{evidence:?}");
    };
    assert_eq!(before.head, candidate.base_sha);
    assert_eq!(
        current(&fx.boot, "unchanged").await.status,
        TaskStatus::Done
    );
}

// ---------------------------------------------------------------------------
// T6: a checkout moved since the delivery: the re-run is refused as a target mismatch.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn regate_after_the_checkout_moved_is_a_target_mismatch() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let green = outside(&fx, "green");
    let (task, lease, candidate) = red(&fx, "moved", gate_on(&green, None)).await;
    git(
        &lease.path,
        &["commit", "-q", "--allow-empty", "-m", "moved by hand"],
    );
    std::fs::write(&green, b"").unwrap();

    regate(&fx, &task.id).await.unwrap();
    drive(&fx, "moved").await;
    let all = verdicts(&fx, &task.id).await;
    assert_eq!(attempts(&all), vec![1, 3]);
    assert_eq!(
        all[1].verdict.status_detail.as_deref(),
        Some("gate-target-mismatch")
    );
    let (id, _, evidence) = candidate_of(&all[1]);
    assert_eq!(id, candidate.candidate_id);
    let VerifyTargetEvidence::Refused { reasons, .. } = evidence else {
        panic!("{evidence:?}");
    };
    assert_eq!(reasons, &vec![MismatchReason::Head]);
    assert_eq!(current(&fx.boot, "moved").await.status, TaskStatus::Failed);
}

// ---------------------------------------------------------------------------
// Design §7: the re-run counts against the planner task ceiling again without admission; the
// projection keeps working and admits the next task once the re-run ends.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn regated_row_counts_against_the_ceiling_without_wedging_the_projection() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    sqlx::query("UPDATE tracks SET planner_task_ceiling = 1 WHERE id = ?1")
        .bind(fx.track())
        .execute(&fx.pool())
        .await
        .unwrap();
    let green = outside(&fx, "green");
    let (task, _, _) = red(&fx, "ceiling-a", gate_on(&green, None)).await;
    regate(&fx, &task.id).await.unwrap();

    let next = json!({
        "key": "ceiling-b", "kind": "codex", "goal": "after the re-run",
        "declared_by": PLANNER_DECLARATION_AUTHOR, "ready": true, "access": "read_only",
    });
    let (block_id, _) = declare(&fx.boot, next.clone()).await;
    let read = call_tool(
        &fx.boot,
        "neige_report_read",
        planner_identity(&fx.boot),
        json!({}),
    )
    .await
    .unwrap();
    let verdict = read["task_diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["key"] == "ceiling-b")
        .cloned()
        .expect("ceiling-b verdict");
    assert_eq!(verdict["schedulable"], false, "{verdict}");
    assert!(
        verdict["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "planner_task_ceiling"),
        "{verdict}"
    );

    std::fs::write(&green, b"").unwrap();
    drive(&fx, "ceiling-a").await;
    assert_eq!(
        current(&fx.boot, "ceiling-a").await.status,
        TaskStatus::Done
    );
    upsert_block(
        &fx.boot,
        planner_identity(&fx.boot),
        json!({"id": block_id, "kind": "task", "payload": next}),
    )
    .await
    .unwrap();
    assert_eq!(
        current(&fx.boot, "ceiling-b").await.status,
        TaskStatus::Pending
    );
}

#[path = "task_regate/refusals.rs"]
mod refusals;
