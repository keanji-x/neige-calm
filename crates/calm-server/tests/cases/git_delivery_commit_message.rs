//! #2139 R1: the worker's `neige_task_done.commit_message` is the message of the commit the kernel
//! makes of its checkout — through the report handler, the scheduler's re-submission after a
//! crash, and a repeated report — while an invalid message refuses the report before any write
//! and a failed attempt keeps the kernel's text.
use calm_server::model::TaskStatus;
use calm_server::plugin_host::mcp::RpcError;
use serde_json::{Value, json};

use crate::git_delivery::{Fx, fixture, git_output};
use crate::mcp_track_report::call_tool;
use crate::task_recovery::current;

/// Shaped like the #2129 worker's message: the kernel carries the ownership lines as plain text.
const OWNERSHIP_MESSAGE: &str = "fix(forge): include exact CI evidence in planner wakes\n\n\
     Closes #2129\n\n\
     OWNERSHIP-CHANGE: fe/core/api/schemas.ts — regenerated wire schema (#2129)\n\
     OWNERSHIP-CHANGE: fe/core/api/generated/wire.ts — regenerated wire schema (#2129)\n";

async fn done_with(
    fx: &Fx,
    worker: &calm_server::mcp_server::registry::ToolCallIdentity,
    attempt: &str,
    commit_message: Value,
) -> Result<Value, RpcError> {
    call_tool(
        &fx.boot,
        "neige_task_done",
        worker.clone(),
        json!({"attempt_id": attempt, "result": {"ok": true}, "commit_message": commit_message}),
    )
    .await
}

/// The stored `task_git_deliveries.commit_message` of the attempt's row (NULL = the kernel text).
async fn stored_message(fx: &Fx, attempt: &str) -> Option<String> {
    sqlx::query_scalar(
        "SELECT commit_message FROM task_git_deliveries WHERE producer_attempt_id = ?1 \
         ORDER BY ordinal DESC LIMIT 1",
    )
    .bind(attempt)
    .fetch_one(&fx.pool())
    .await
    .unwrap()
}

/// The raw message of `sha` as git stored it (`git cat-file commit`, after the header).
fn commit_message_of(dir: &std::path::Path, sha: &str) -> String {
    let output = git_output(dir, &["cat-file", "commit", sha]);
    assert!(output.status.success(), "{output:?}");
    let raw = String::from_utf8(output.stdout).unwrap();
    raw.split_once("\n\n")
        .map(|(_, message)| message.to_string())
        .unwrap_or_else(|| panic!("commit {sha} has no message: {raw:?}"))
}

async fn task_completed_events(fx: &Fx, attempt: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM events WHERE kind = 'task.completed' \
         AND json_extract(payload, '$.idempotency_key') = ?1",
    )
    .bind(attempt)
    .fetch_one(&fx.pool())
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_commit_message_is_the_candidate_commit_message() {
    let fx = fixture().await;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("owned", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "owned\n").unwrap();

    done_with(&fx, &worker, &task.id, json!(OWNERSHIP_MESSAGE))
        .await
        .unwrap();
    fx.wait_settled(&task.id).await;

    assert_eq!(
        stored_message(&fx, &task.id).await.as_deref(),
        Some(OWNERSHIP_MESSAGE)
    );
    let candidate = fx.candidate_row(&task.id).await.expect("candidate row");
    assert_ne!(
        candidate.commit_sha, candidate.base_sha,
        "the edit committed"
    );
    assert_eq!(
        commit_message_of(&lease.path, &candidate.commit_sha),
        OWNERSHIP_MESSAGE
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crash_before_submission_commits_the_worker_message() {
    let mut fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("handoff", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "handoff\n").unwrap();

    fx.report_only_with_message(&worker, &task.id, OWNERSHIP_MESSAGE)
        .await;
    assert_eq!(fx.forge_op_count().await, 0, "no submission happened");
    assert_eq!(
        stored_message(&fx, &task.id).await.as_deref(),
        Some(OWNERSHIP_MESSAGE)
    );

    // The boot sweep rebuilds the argv from the row alone; the MCP arguments are gone.
    fx.reboot().await;
    fx.wait_settled(&task.id).await;
    assert_eq!(fx.forge_op_count().await, 1);
    let candidate = fx.candidate_row(&task.id).await.expect("candidate");
    assert_eq!(
        commit_message_of(&lease.path, &candidate.commit_sha),
        OWNERSHIP_MESSAGE
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_completion_keeps_the_first_commit_message() {
    let fx = fixture().await;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("twice", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "once\n").unwrap();

    done_with(&fx, &worker, &task.id, json!("fix: first\n"))
        .await
        .unwrap();
    // A repeated report is an idempotent success that writes nothing, its message included.
    done_with(&fx, &worker, &task.id, json!("fix: second\n"))
        .await
        .unwrap();
    fx.wait_settled(&task.id).await;
    done_with(&fx, &worker, &task.id, json!("fix: third\n"))
        .await
        .unwrap();

    assert_eq!(fx.delivery_count(&task.id).await, 1);
    assert_eq!(
        stored_message(&fx, &task.id).await.as_deref(),
        Some("fix: first\n")
    );
    let candidate = fx.candidate_row(&task.id).await.expect("candidate row");
    assert_eq!(
        commit_message_of(&lease.path, &candidate.commit_sha),
        "fix: first\n"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invalid_commit_message_refuses_the_report_before_any_write() {
    let fx = fixture().await;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("refused", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "refused\n").unwrap();

    for (message, refusal) in [
        (
            json!("fix: x\0y"),
            "neige_task_done: commit_message has a NUL byte",
        ),
        (
            json!(42),
            "neige_task_done: commit_message must be a string",
        ),
    ] {
        let error = done_with(&fx, &worker, &task.id, message.clone())
            .await
            .expect_err("an invalid commit_message refuses the report");
        assert_eq!(error.code, RpcError::INVALID_PARAMS, "{message}: {error:?}");
        assert_eq!(error.message, refusal, "{message}");
        assert_eq!(
            current(&fx.boot, "refused").await.status,
            TaskStatus::Running,
            "the attempt is still running"
        );
        assert_eq!(fx.delivery_count(&task.id).await, 0, "no delivery row");
        assert_eq!(task_completed_events(&fx, &task.id).await, 0);
    }

    // The corrected report is admitted: the attempt was never ended.
    done_with(&fx, &worker, &task.id, json!("fix: corrected\n"))
        .await
        .unwrap();
    assert_eq!(current(&fx.boot, "refused").await.status, TaskStatus::Done);
    assert_eq!(task_completed_events(&fx, &task.id).await, 1);
    assert_eq!(
        stored_message(&fx, &task.id).await.as_deref(),
        Some("fix: corrected\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_attempt_commits_with_the_kernel_message() {
    let fx = fixture().await;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("broken", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "partial\n").unwrap();

    call_tool(
        &fx.boot,
        "neige_task_fail",
        worker.clone(),
        json!({"attempt_id": task.id, "reason": "could not finish"}),
    )
    .await
    .unwrap();
    fx.wait_settled(&task.id).await;

    let row = fx.delivery_row(&task.id).await.expect("delivery row");
    assert_eq!(stored_message(&fx, &task.id).await, None);
    let candidate = fx.candidate_row(&task.id).await.expect("candidate row");
    assert_eq!(
        commit_message_of(&lease.path, &candidate.commit_sha),
        format!(
            "neige: attempt {} failed (delivery {})\n",
            task.id, row.delivery_id
        )
    );
}
