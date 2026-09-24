//! #1785 S2: `calm.task.replace` — admission (design §4.7, one test per row), the stop, the
//! appended successor block, the receipt's replay and rollback, and the §4.6 wake guarantee.
//! The carry itself (lease prepare, git) is in `task_replace_carry.rs`.
use std::path::Path;
use std::time::Duration;

use super::git_delivery::*;
use crate::mcp_track_report::{call_tool, planner_identity};
use crate::task_recovery::{current, declare};
use calm_server::dispatcher::task_event_pushes_planner_for_test;
use calm_server::event::Event;
use calm_server::ids::ActorId;
use calm_server::mcp_server::registry::ToolCallIdentity;
use calm_server::model::{Task, TaskStatus};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::session_projection_repo::AgentProvider;
use calm_server::test_seams::{
    KernelWorkspaceLease, take_kernel_workspace_lease_for_attempt_for_test,
};
use calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR;
use serde_json::{Value, json};

pub(super) const TOOL_TASK_REPLACE: &str = "calm.task.replace";

/// The git-delivery fixture on an attached Track whose scheduler claims nothing (budget 0) until
/// a test opens the budget.
pub(super) async fn replace_fixture() -> Fx {
    let fx = fixture().await;
    prepare_replace(&fx).await;
    fx
}

/// An attached Track holding claims, with the active Planner session the report write requires.
pub(super) async fn prepare_replace(fx: &Fx) {
    hold_claims(fx).await;
    crate::mcp_task_dispatch::bind_planner(&fx.boot, &planner_identity(&fx.boot).session_id, false)
        .await;
}

pub(super) async fn hold_claims(fx: &Fx) {
    let kind: String = sqlx::query_scalar("SELECT workspace_kind FROM tracks WHERE id = ?1")
        .bind(fx.track())
        .fetch_one(&fx.pool())
        .await
        .unwrap();
    assert_eq!(
        kind, "attached",
        "the git-delivery fixture's Track is attached"
    );
    sqlx::query("UPDATE tracks SET task_budget = 0 WHERE id = ?1")
        .bind(fx.track())
        .execute(&fx.pool())
        .await
        .unwrap();
}

pub(super) async fn open_claims(fx: &Fx) {
    sqlx::query("UPDATE tracks SET task_budget = 4 WHERE id = ?1")
        .bind(fx.track())
        .execute(&fx.pool())
        .await
        .unwrap();
}

pub(super) fn replace_args(task: &Task, request: &str) -> Value {
    json!({
        "key": task.key, "expected_attempt_id": task.id, "idempotency_key": request,
        "reason": "the review found blockers",
        "goal": format!("address the review of {}", task.key),
        "acceptance": "every blocking finding is fixed",
    })
}

pub(super) async fn replace(fx: &Fx, args: Value) -> Result<Value, RpcError> {
    call_tool(
        &fx.boot,
        TOOL_TASK_REPLACE,
        planner_identity(&fx.boot),
        args,
    )
    .await
}

pub(super) fn assert_refusal(result: &Result<Value, RpcError>, code: &str) {
    let error = result.as_ref().expect_err("replace must be refused");
    assert_eq!(error.code, -32409, "{error:?}");
    assert!(error.message.starts_with(code), "{code}: {}", error.message);
}

/// A reporting worker whose lease went through the production base resolution for `attempt`
/// (the carry branch for a replacement successor).
pub(super) async fn attempt_lease(
    fx: &Fx,
    name: &str,
    attempt: &str,
) -> (ToolCallIdentity, KernelWorkspaceLease) {
    let worker = fx.new_worker(name, AgentProvider::Codex).await;
    let lease = take_kernel_workspace_lease_for_attempt_for_test(
        &fx.pool(),
        fx.track(),
        &worker.card_id,
        &fx.workspace_root,
        attempt,
    )
    .await
    .unwrap();
    (worker, lease)
}

pub(super) fn write_files(root: &Path, files: &[(&str, &str)]) {
    for (file, content) in files {
        std::fs::write(root.join(file), content).unwrap();
    }
}

/// The worker reports, the kernel delivers, and the delivery settles as a candidate.
pub(super) async fn report_and_settle(
    fx: &Fx,
    worker: &ToolCallIdentity,
    attempt: &str,
) -> CandidateRowView {
    fx.complete(worker, attempt).await;
    fx.wait_forge_op(attempt).await;
    let row = fx.delivery_row(attempt).await.expect("delivery row");
    fx.scheduler()
        .settle_git_delivery_for_test(&row.delivery_id)
        .await
        .unwrap();
    fx.candidate_row(attempt).await.expect("candidate")
}

/// A declared producer that wrote `files` and settled a candidate. `extra` merges into the block.
pub(super) async fn produced(
    fx: &Fx,
    key: &str,
    files: &[(&str, &str)],
    extra: Value,
) -> (Task, CandidateRowView) {
    let worker = fx.new_worker(key, AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx.running_task(key, "codex", &worker.card_id, extra).await;
    write_files(&lease.path, files);
    let candidate = report_and_settle(fx, &worker, &task.id).await;
    (current(&fx.boot, key).await, candidate)
}

/// A replacement successor run as a worker would run it: its carry lease, `files`, a report.
pub(super) async fn produce_successor(
    fx: &Fx,
    successor: &Task,
    files: &[(&str, &str)],
) -> (KernelWorkspaceLease, CandidateRowView) {
    let (worker, lease) = attempt_lease(fx, &successor.key.replace('.', "-"), &successor.id).await;
    fx.claim_running(&successor.id, &worker.card_id).await;
    write_files(&lease.path, files);
    let candidate = report_and_settle(fx, &worker, &successor.id).await;
    (lease, candidate)
}

/// A declared task claimed `running` on its own worker card with a live session.
pub(super) async fn running(fx: &Fx, key: &str, extra: Value) -> (ToolCallIdentity, Task) {
    let worker = fx.new_worker(key, AgentProvider::Codex).await;
    let task = fx.running_task(key, "codex", &worker.card_id, extra).await;
    (worker, task)
}

pub(super) async fn report_blocks(fx: &Fx) -> Vec<calm_types::track_report::ReportBlock> {
    crate::mcp_task_dispatch::payload(&fx.boot)
        .await
        .blocks
        .unwrap_or_default()
}

pub(super) async fn block_of(fx: &Fx, key: &str) -> calm_types::track_report::ReportBlock {
    report_blocks(fx)
        .await
        .into_iter()
        .find(|b| b.payload["key"] == key)
        .unwrap_or_else(|| panic!("no block for {key}"))
}

pub(super) async fn receipt_count(fx: &Fx) -> i64 {
    fx.table_count("task_replacements").await
}

async fn event_count(fx: &Fx) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(&fx.pool())
        .await
        .unwrap()
}

/// Wait until `key`'s current attempt reaches `status` (a live gate or settlement runs it there).
pub(super) async fn wait_status(fx: &Fx, key: &str, status: TaskStatus) -> Task {
    tokio::time::timeout(WAIT, async {
        loop {
            let task = current(&fx.boot, key).await;
            if task.status == status {
                break task;
            }
            if task.status == TaskStatus::Verifying {
                let _ = fx.scheduler().drive_gate_for_test(task).await;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{key} never reached {status:?}"))
}

/// Records every cleanup marker reason written, whatever the reap does with the marker later.
async fn log_cleanup_markers(fx: &Fx) {
    let pool = fx.pool();
    sqlx::query("CREATE TABLE marker_log (card_id TEXT, reason TEXT)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER log_marker AFTER UPDATE OF handle_state_json ON worker_sessions \
         WHEN json_extract(NEW.handle_state_json, '$.timeout_cleanup.reason') IS NOT NULL \
         BEGIN INSERT INTO marker_log VALUES (NEW.card_id, \
         json_extract(NEW.handle_state_json, '$.timeout_cleanup.reason')); END",
    )
    .execute(&pool)
    .await
    .unwrap();
}

/// §4.7 `running`: the predecessor is canceled through the Planner cancel's CAS and marked for
/// the reap; the successor block is appended right after it with the inherited execution fields.
#[tokio::test]
async fn replace_running_predecessor_cancels_it_and_appends_successor() {
    let fx = replace_fixture().await;
    let gate = json!({"steps": [{"name": "t", "cmd": "true"}], "timeout_secs": 60});
    let extra = json!({"gate": gate, "no_gate_reason": null, "priority": 3,
        "context": {"base_sha": "0123", "apply": "git diff A B | git apply"}});
    let (worker, task) = running(&fx, "impl", extra).await;
    let before = block_of(&fx, "impl").await;
    log_cleanup_markers(&fx).await;

    let response = replace(&fx, replace_args(&task, "r1")).await.unwrap();

    assert_eq!(response["replayed"], false);
    assert_eq!(
        response["predecessor"],
        json!({"key": "impl", "attempt_id": task.id,
        "prior_status": "running", "stop": "canceled_now"})
    );
    let successor_id = format!("{}:impl.2", fx.track());
    assert_eq!(
        response["successor"],
        json!({"key": "impl.2", "attempt_id": successor_id})
    );
    assert_eq!(response["carry"], json!({"none": "no_candidate"}));
    let row = fx.task_columns(&task.id).await;
    assert_eq!(row.status, TaskStatus::Canceled);
    let predecessor = current(&fx.boot, "impl").await;
    assert_eq!(
        predecessor.status_detail.as_deref(),
        Some("superseded: impl.2")
    );
    let marked: Vec<(String, String)> =
        sqlx::query_as("SELECT DISTINCT card_id, reason FROM marker_log")
            .fetch_all(&fx.pool())
            .await
            .unwrap();
    assert_eq!(
        marked,
        vec![(worker.card_id.clone(), "planner_superseded".to_string())]
    );

    let successor = current(&fx.boot, "impl.2").await;
    assert_eq!(successor.id, successor_id);
    assert_eq!(successor.status, TaskStatus::Pending);
    let blocks = report_blocks(&fx).await;
    let at = blocks
        .iter()
        .position(|b| b.payload["key"] == "impl")
        .unwrap();
    assert_eq!(blocks[at], before, "the predecessor block is unchanged");
    let appended = &blocks[at + 1];
    assert_eq!(
        appended.payload["key"], "impl.2",
        "appended right after the predecessor"
    );
    for field in [
        "gate",
        "no_gate_reason",
        "kind",
        "depends_on",
        "priority",
        "spawn",
    ] {
        assert_eq!(
            appended.payload.get(field),
            before.payload.get(field),
            "{field}"
        );
    }
    assert_eq!(appended.payload["ready"], true);
    assert_eq!(appended.payload["declared_by"], PLANNER_DECLARATION_AUTHOR);
    assert_eq!(appended.payload["goal"], "address the review of impl");
    assert_eq!(
        appended.payload["acceptance"],
        "every blocking finding is fixed"
    );
    assert_eq!(
        appended.payload["context"],
        json!({}),
        "no predecessor context key survives"
    );
    assert!(appended.payload.get("released_by_user").is_none());
    assert_eq!(successor.gate_json, predecessor.gate_json);
    assert_eq!(receipt_count(&fx).await, 1);
    let updated = fx
        .boot
        .repo
        .events_for_track(fx.track(), &["plan.updated"], None)
        .await
        .unwrap();
    let Event::PlanUpdated { changed_keys, .. } = &updated.last().unwrap().event else {
        panic!()
    };
    assert_eq!(
        changed_keys,
        &vec!["impl".to_string(), "impl.2".to_string()]
    );
    assert!(fx.events_for(TASK_FAILED_KIND, &task.id).await.is_empty());
}

/// §4.7 `pending`: the pending CAS cancels it.
#[tokio::test]
async fn replace_pending_predecessor_cancels_it() {
    let fx = replace_fixture().await;
    declare(
        &fx.boot,
        json!({"key": "later", "kind": "codex", "goal": "do it",
        "declared_by": PLANNER_DECLARATION_AUTHOR, "ready": true, "no_gate_reason": "fixture"}),
    )
    .await;
    let task = current(&fx.boot, "later").await;
    assert_eq!(task.status, TaskStatus::Pending);

    let response = replace(&fx, replace_args(&task, "p1")).await.unwrap();

    assert_eq!(response["predecessor"]["prior_status"], "pending");
    assert_eq!(response["predecessor"]["stop"], "canceled_now");
    let predecessor = current(&fx.boot, "later").await;
    assert_eq!(predecessor.status, TaskStatus::Canceled);
    assert_eq!(
        predecessor.status_detail.as_deref(),
        Some("superseded: later.2")
    );
    assert_eq!(
        current(&fx.boot, "later.2").await.status,
        TaskStatus::Pending
    );
    // The canceled predecessor is never scheduled again once the budget opens.
    open_claims(&fx).await;
    fx.scheduler()
        .schedule_track(fx.boot.track_id.clone())
        .await;
    assert_eq!(
        current(&fx.boot, "later").await.status,
        TaskStatus::Canceled
    );
    assert_ne!(
        current(&fx.boot, "later.2").await.status,
        TaskStatus::Pending,
        "the successor is claimed"
    );
}

/// §4.7 first row: a finished producer with a settled candidate carries it; nothing is stopped.
#[tokio::test]
async fn replace_done_predecessor_carries_its_candidate() {
    let fx = replace_fixture().await;
    let (task, candidate) = produced(&fx, "prod", &[("a.txt", "A\n")], json!({})).await;
    assert_eq!(task.status, TaskStatus::Done);

    let response = replace(&fx, replace_args(&task, "d1")).await.unwrap();

    assert_eq!(response["predecessor"]["prior_status"], "done");
    assert_eq!(response["predecessor"]["stop"], "already_terminal");
    assert_eq!(
        response["carry"],
        json!({"source_attempt_id": task.id,
        "source_candidate_id": candidate.candidate_id, "candidate_sha": candidate.commit_sha})
    );
    assert_eq!(current(&fx.boot, "prod").await.status, TaskStatus::Done);
}

/// §4.7 terminal without a candidate: a worker-reported failure has none.
#[tokio::test]
async fn replace_terminal_predecessor_without_candidate_carries_none() {
    let fx = replace_fixture().await;
    let (worker, task) = running(&fx, "broken", json!({})).await;
    call_tool(
        &fx.boot,
        "calm.task.fail",
        worker,
        json!({"idempotency_key": task.id, "reason": "stuck"}),
    )
    .await
    .unwrap();
    assert_eq!(fx.task_columns(&task.id).await.status, TaskStatus::Failed);

    let response = replace(&fx, replace_args(&task, "t1")).await.unwrap();

    assert_eq!(response["predecessor"]["prior_status"], "failed");
    assert_eq!(response["predecessor"]["stop"], "already_terminal");
    assert_eq!(response["carry"], json!({"none": "no_candidate"}));
    let none = replace(&fx, json!({"key": "broken.2", "expected_attempt_id": current(&fx.boot, "broken.2").await.id,
        "idempotency_key": "t2", "reason": "again", "goal": "g", "acceptance": "a", "carry": "none"}))
        .await
        .unwrap();
    assert_eq!(none["carry"], json!({"none": "requested_none"}));
    assert_eq!(none["successor"]["key"], "broken.3");
}

/// A successor that ended without a candidate hands on the carry its own replacement had.
#[tokio::test]
async fn replace_of_a_successor_without_candidate_inherits_its_carry() {
    let fx = replace_fixture().await;
    let (task, candidate) = produced(&fx, "src", &[("a.txt", "A\n")], json!({})).await;
    replace(&fx, replace_args(&task, "i1")).await.unwrap();
    let successor = current(&fx.boot, "src.2").await;
    let worker = fx.new_worker("src-2", AgentProvider::Codex).await;
    fx.claim_running(&successor.id, &worker.card_id).await;
    call_tool(
        &fx.boot,
        "calm.task.fail",
        worker,
        json!({"idempotency_key": successor.id, "reason": "x"}),
    )
    .await
    .unwrap();

    let response = replace(&fx, replace_args(&current(&fx.boot, "src.2").await, "i2"))
        .await
        .unwrap();

    assert_eq!(response["successor"]["key"], "src.3");
    assert_eq!(response["carry"]["source_attempt_id"], task.id);
    assert_eq!(
        response["carry"]["source_candidate_id"],
        candidate.candidate_id.as_str()
    );
}

/// §4.7 stop CAS moved nothing: the whole request rolls back — and so does a failure after a stop
/// that did move the row.
#[tokio::test]
async fn replace_rollback_is_atomic() {
    let fx = replace_fixture().await;
    let (_worker, task) = running(&fx, "atomic", json!({})).await;
    let blocks = report_blocks(&fx).await;
    let events = event_count(&fx).await;
    let pool = fx.pool();
    sqlx::query(
        "CREATE TRIGGER lose_cancel BEFORE UPDATE OF status ON tasks \
         WHEN NEW.status = 'canceled' BEGIN SELECT RAISE(IGNORE); END",
    )
    .execute(&pool)
    .await
    .unwrap();

    let result = replace(&fx, replace_args(&task, "a1")).await;

    assert_refusal(&result, "predecessor_changed");
    assert!(result.unwrap_err().message.contains("running"));
    assert_eq!(report_blocks(&fx).await, blocks, "no block");
    assert_eq!(receipt_count(&fx).await, 0, "no receipt");
    assert_eq!(event_count(&fx).await, events, "no event");

    sqlx::query("DROP TRIGGER lose_cancel")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER deny_receipt BEFORE INSERT ON task_replacements \
         BEGIN SELECT RAISE(ABORT, 'injected receipt failure'); END",
    )
    .execute(&pool)
    .await
    .unwrap();

    assert!(replace(&fx, replace_args(&task, "a2")).await.is_err());

    assert_eq!(
        fx.task_columns(&task.id).await.status,
        TaskStatus::Running,
        "the stop rolled back"
    );
    assert_eq!(report_blocks(&fx).await, blocks);
    assert_eq!(event_count(&fx).await, events);
    assert!(
        fx.boot
            .repo
            .task_current_get(fx.track(), "atomic.2")
            .await
            .unwrap()
            .is_none()
    );
}

/// §4.7 the same request key: the same request replays, a different one conflicts.
#[tokio::test]
async fn replace_replays_same_request_and_conflicts_on_different_request() {
    let fx = replace_fixture().await;
    let (task, _) = produced(&fx, "again", &[("a.txt", "A\n")], json!({})).await;
    let first = replace(&fx, replace_args(&task, "k1")).await.unwrap();
    let blocks = report_blocks(&fx).await;
    let events = event_count(&fx).await;

    let replayed = replace(&fx, replace_args(&task, "k1")).await.unwrap();

    let mut expected = first.clone();
    expected["replayed"] = json!(true);
    assert_eq!(replayed, expected);
    assert_eq!(report_blocks(&fx).await, blocks);
    assert_eq!(event_count(&fx).await, events);
    let mut different = replace_args(&task, "k1");
    different["goal"] = json!("something else");
    assert_refusal(&replace(&fx, different).await, "idempotency_conflict");
}

/// The response is lost after the commit; the retry gets the original answer from the receipt,
/// though the predecessor row has moved on since.
#[tokio::test]
async fn response_loss_replay_returns_the_original_answer() {
    let fx = replace_fixture().await;
    let (_worker, task) = running(&fx, "lossy", json!({})).await;
    let first = replace(&fx, replace_args(&task, "l1")).await.unwrap();
    assert_eq!(fx.task_columns(&task.id).await.status, TaskStatus::Canceled);

    let retried = replace(&fx, replace_args(&task, "l1")).await.unwrap();

    assert_eq!(retried["replayed"], true);
    for field in ["receipt_id", "predecessor", "successor", "carry"] {
        assert_eq!(retried[field], first[field], "{field}");
    }
    assert_eq!(retried["predecessor"]["prior_status"], "running");
    assert_eq!(retried["predecessor"]["stop"], "canceled_now");
}

/// Review blocks that name the predecessor keep validating: a finished review of the old round
/// still depends on the old key, whose block stays.
#[tokio::test]
async fn replace_keeps_finished_review_dependencies_valid() {
    let fx = replace_fixture().await;
    let (task, _) = produced(&fx, "code", &[("a.txt", "A\n")], json!({})).await;
    let reviewer = fx.new_worker("review-code", AgentProvider::Codex).await;
    let review = fx
        .running_task(
            "review-code",
            "codex",
            &reviewer.card_id,
            json!({"depends_on": ["code"]}),
        )
        .await;
    fx.complete(&reviewer, &review.id).await;
    assert_eq!(
        current(&fx.boot, "review-code").await.status,
        TaskStatus::Done
    );
    let read = || {
        call_tool(
            &fx.boot,
            "calm.report.read",
            planner_identity(&fx.boot),
            json!({}),
        )
    };
    let diagnostics = |report: Value| {
        report["taskDiagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["key"] == "review-code")
            .map(|d| d["diagnostics"].clone())
            .collect::<Vec<_>>()
    };
    let before = diagnostics(read().await.unwrap());

    replace(&fx, replace_args(&task, "rv1")).await.unwrap();

    assert_eq!(
        diagnostics(read().await.unwrap()),
        before,
        "no new diagnostic"
    );
    assert_eq!(
        block_of(&fx, "review-code").await.payload["depends_on"],
        json!(["code"])
    );
}

/// §4.6: every exit of a successor execution wakes the Planner, except the Planner's own
/// cancel/replace. An enumeration over the events those exits emit, on a gated successor row.
#[tokio::test]
async fn successor_exits_wake_the_planner_except_the_planners_own() {
    let fx = replace_fixture().await;
    let gate = json!({"gate": {"steps": [{"name": "t", "cmd": "true"}], "timeout_secs": 60}, "no_gate_reason": null});
    let (task, _) = produced(&fx, "woken", &[("a.txt", "A\n")], gate).await;
    let task = wait_status(&fx, &task.key, TaskStatus::Done).await;
    let (_, _) = produced(&fx, "plain", &[("b.txt", "B\n")], json!({})).await;
    let before: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM events")
        .fetch_one(&fx.pool())
        .await
        .unwrap();
    replace(&fx, replace_args(&task, "wk1")).await.unwrap();
    let successor = current(&fx.boot, "woken.2").await;
    assert!(
        successor.gate_json.is_some(),
        "the successor is gated like its predecessor"
    );
    let pushes = |actor: ActorId, event: Event| {
        let repo = fx.boot.repo.clone();
        let write = fx.boot.ctx.write.clone();
        async move { task_event_pushes_planner_for_test(repo.as_ref(), &write, &event, &actor).await }
    };
    let failed = |reason: &str| Event::TaskFailed {
        idempotency_key: successor.id.clone(),
        reason: reason.into(),
        details: None,
        agent_message: None,
    };
    // 12a / 13b / 13c: the kernel fails the successor before its gate; then a worker's own report.
    let exits = [
        (
            "spawn-failed: refused: carry-conflict: a.txt",
            ActorId::KernelDispatcher,
        ),
        ("worker-turn-ended", ActorId::KernelDispatcher),
        ("worker-timeout", ActorId::KernelDispatcher),
        ("worker-reported: stuck", fx.codex_worker().to_actor_id()),
    ];
    for (detail, actor) in exits {
        sqlx::query("UPDATE tasks SET status = 'failed', status_detail = ?1 WHERE id = ?2")
            .bind(detail)
            .bind(&successor.id)
            .execute(&fx.pool())
            .await
            .unwrap();
        assert!(pushes(actor, failed(detail)).await, "{detail}");
    }
    // 13a: an ungated settlement and a gate result push; a settlement deferred to the gate does not.
    let rows = fx
        .boot
        .repo
        .events_for_track(fx.track(), &[SETTLED_KIND, GATE_RESULT_KIND], None)
        .await
        .unwrap();
    let mut seen = std::collections::BTreeSet::new();
    for row in rows {
        let expected = !matches!(&row.event, Event::TaskGitDeliverySettled { wake_reason, .. }
            if *wake_reason == calm_types::git_candidate::DeliveryWakeReason::DeferredToGate);
        seen.insert((row.event.kind_tag().to_string(), expected));
        assert_eq!(
            pushes(row.actor.clone(), row.event.clone()).await,
            expected,
            "{:?}",
            row.event
        );
    }
    assert_eq!(
        seen.len(),
        3,
        "ungated settlement, deferred settlement and gate result: {seen:?}"
    );
    // The Planner's own replace emits nothing that wakes it.
    let own: Vec<_> = fx
        .boot
        .repo
        .events_for_track(fx.track(), &["plan.updated", "track.report_edited"], None)
        .await
        .unwrap()
        .into_iter()
        .filter(|row| row.id > before)
        .collect();
    assert_eq!(own.len(), 2, "{own:?}");
    for row in own {
        assert!(
            !pushes(row.actor.clone(), row.event.clone()).await,
            "{:?}",
            row.event
        );
    }
}
