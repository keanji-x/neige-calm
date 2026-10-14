//! #2599: a task declared with `result_contract` is held to it at `neige_task_done`, for codex and
//! claude workers alike. A result that does not match is refused with the contract's shape and
//! writes nothing: the attempt stays running with its lease held, so the worker can report again.
//! An untyped task still accepts any result.
use calm_server::mcp_server::registry::ToolCallIdentity;
use calm_server::model::TaskStatus;
use calm_server::plugin_host::mcp::RpcError;
use calm_server::session_projection_repo::AgentProvider;
use calm_types::task_result::result_contract;
use serde_json::{Value, json};

use crate::git_delivery::{Fx, fixture};
use crate::mcp_track_report::call_tool;
use crate::task_recovery::current;

async fn done(
    fx: &Fx,
    worker: &ToolCallIdentity,
    attempt: &str,
    result: Value,
) -> Result<Value, RpcError> {
    call_tool(
        &fx.boot,
        "neige_task_done",
        worker.clone(),
        json!({"attempt_id": attempt, "result": result}),
    )
    .await
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

async fn lease_state(fx: &Fx, lease_id: &str) -> String {
    sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
        .bind(lease_id)
        .fetch_one(&fx.pool())
        .await
        .unwrap()
}

async fn refuses_a_result_violating_its_declared_result_contract(
    kind: &str,
    provider: AgentProvider,
) {
    let fx = fixture().await;
    let worker = fx.new_worker("reviewer", provider).await;
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task(
            "review",
            kind,
            &worker.card_id,
            json!({"result_contract": "review-v1"}),
        )
        .await;
    assert_eq!(task.result_contract.as_deref(), Some("review-v1"));
    let shape = result_contract("review-v1").unwrap().shape_hint;

    for (result, violations) in [
        (
            json!({"summary": "looks fine", "blocking_findings": []}),
            "verdict: required; must be \"pass\" | \"blocking\"",
        ),
        (
            json!("looks good to me"),
            "result: must be a JSON object, got string",
        ),
    ] {
        let error = done(&fx, &worker, &task.id, result.clone())
            .await
            .expect_err("a result violating the contract is refused");
        assert_eq!(error.code, RpcError::INVALID_PARAMS, "{result}: {error:?}");
        assert_eq!(
            error.message,
            format!(
                "neige_task_done: result does not match result_contract review-v1: \
                 {violations}. Expected: {shape}"
            ),
            "{result}"
        );
        assert_eq!(
            current(&fx.boot, "review").await.status,
            TaskStatus::Running,
            "the attempt is still running"
        );
        assert_eq!(task_completed_events(&fx, &task.id).await, 0);
        assert_eq!(lease_state(&fx, &lease.lease_id).await, "held");
        assert_eq!(fx.delivery_count(&task.id).await, 0, "no delivery row");
    }

    let valid = json!({"verdict": "blocking", "summary": "one defect",
        "blocking_findings": [{"title": "lease", "evidence": "decision_sink.rs:180"}]});
    assert_eq!(
        done(&fx, &worker, &task.id, valid.clone()).await.unwrap(),
        json!({"status": "report_received"})
    );
    assert_eq!(current(&fx.boot, "review").await.status, TaskStatus::Done);
    assert_eq!(task_completed_events(&fx, &task.id).await, 1);
    let recorded: String = sqlx::query_scalar(
        "SELECT json_extract(payload, '$.result') FROM events WHERE kind = 'task.completed' \
         AND json_extract(payload, '$.idempotency_key') = ?1",
    )
    .bind(&task.id)
    .fetch_one(&fx.pool())
    .await
    .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&recorded).unwrap(), valid);
    assert_ne!(lease_state(&fx, &lease.lease_id).await, "held");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn task_done_refuses_result_violating_declared_result_contract_for_codex() {
    refuses_a_result_violating_its_declared_result_contract("codex", AgentProvider::Codex).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn task_done_refuses_result_violating_declared_result_contract_for_claude() {
    refuses_a_result_violating_its_declared_result_contract("claude", AgentProvider::Claude).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn task_done_without_result_contract_still_accepts_any_result() {
    let fx = fixture().await;
    let worker = fx.codex_worker();
    fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("untyped", "codex", &worker.card_id, json!({}))
        .await;
    assert_eq!(task.result_contract, None);
    assert_eq!(
        done(&fx, &worker, &task.id, json!("prose, not review-v1"))
            .await
            .unwrap(),
        json!({"status": "report_received"})
    );
    assert_eq!(current(&fx.boot, "untyped").await.status, TaskStatus::Done);
    assert_eq!(task_completed_events(&fx, &task.id).await, 1);
}
