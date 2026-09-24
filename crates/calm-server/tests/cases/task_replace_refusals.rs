//! #1785 S2: `calm.task.replace` refusals (design §4.7): each is decided before the first write,
//! so a refused request stops nothing and writes no receipt.
use super::task_replace::*;
use crate::task_recovery::{current, declare};
use calm_server::model::TaskStatus;
use calm_server::session_projection_repo::AgentProvider;
use calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR;
use calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE;
use serde_json::json;

/// §4.7 `dispatched`, and `running` without a worker card.
#[tokio::test]
async fn replace_refuses_dispatched_and_cardless_running_predecessors() {
    let fx = replace_fixture().await;
    for (key, status) in [
        ("disp", TaskStatus::Dispatched),
        ("cardless", TaskStatus::Running),
    ] {
        declare(
            &fx.boot,
            json!({"key": key, "kind": "codex", "goal": "g",
            "declared_by": PLANNER_DECLARATION_AUTHOR, "ready": true, "no_gate_reason": "f"}),
        )
        .await;
        let task = current(&fx.boot, key).await;
        sqlx::query("UPDATE tasks SET status = ?1 WHERE id = ?2")
            .bind(
                serde_json::to_value(status)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string(),
            )
            .bind(&task.id)
            .execute(&fx.pool())
            .await
            .unwrap();
        let result = replace(&fx, replace_args(&task, key)).await;
        assert_refusal(&result, "predecessor_dispatching");
        assert_eq!(fx.task_columns(&task.id).await.status, status);
    }
    assert_eq!(receipt_count(&fx).await, 0);
}

/// §4.7 declare-and-wait: refused before the running predecessor is stopped.
#[tokio::test]
async fn replace_refuses_declare_and_wait_before_stopping() {
    let fx = replace_fixture().await;
    let (_worker, task) = running(&fx, "waiting", json!({})).await;
    sqlx::query("UPDATE tracks SET automation_policy = 'declare-and-wait' WHERE id = ?1")
        .bind(fx.track())
        .execute(&fx.pool())
        .await
        .unwrap();

    let result = replace(&fx, replace_args(&task, "w1")).await;

    assert_refusal(&result, "requires_user_release");
    assert_eq!(fx.task_columns(&task.id).await.status, TaskStatus::Running);
    assert_eq!(receipt_count(&fx).await, 0);
}

/// §4.7 `verifying`.
#[tokio::test]
async fn replace_refuses_verifying_predecessor() {
    let fx = replace_fixture().await;
    let (_worker, task) = running(&fx, "gating", json!({})).await;
    sqlx::query("UPDATE tasks SET status = 'verifying' WHERE id = ?1")
        .bind(&task.id)
        .execute(&fx.pool())
        .await
        .unwrap();
    assert_refusal(
        &replace(&fx, replace_args(&task, "v1")).await,
        "predecessor_verifying",
    );
}

/// §4.7 unsettled delivery: the report committed, the delivery has not settled.
#[tokio::test]
async fn replace_refuses_pending_delivery() {
    let fx = replace_fixture().await;
    let worker = fx.new_worker("unsettled", AgentProvider::Codex).await;
    fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("unsettled", "codex", &worker.card_id, json!({}))
        .await;
    fx.report_only(&worker, &task.id).await;
    let task = current(&fx.boot, "unsettled").await;
    assert!(
        fx.delivery_row(&task.id)
            .await
            .unwrap()
            .settlement
            .is_none()
    );

    assert_refusal(
        &replace(&fx, replace_args(&task, "c1")).await,
        "candidate_pending",
    );
}

/// §4.7 `expected_attempt_id` is not current.
#[tokio::test]
async fn replace_refuses_stale_attempt() {
    let fx = replace_fixture().await;
    let (_worker, task) = running(&fx, "stale", json!({})).await;
    let mut args = replace_args(&task, "s1");
    args["expected_attempt_id"] = json!("not-the-attempt");
    assert_refusal(&replace(&fx, args).await, "stale_attempt");
}

/// §4.7 an attempt already replaced under another request key.
#[tokio::test]
async fn replace_refuses_second_successor_for_one_attempt() {
    let fx = replace_fixture().await;
    let (task, _) = produced(&fx, "once", &[("a.txt", "A\n")], json!({})).await;
    replace(&fx, replace_args(&task, "o1")).await.unwrap();

    let result = replace(&fx, replace_args(&task, "o2")).await;

    assert_refusal(&result, "already_replaced");
    assert!(result.unwrap_err().message.contains("once.2"));
    assert_eq!(receipt_count(&fx).await, 1);
}

/// §4.7 unfinished dependents.
#[tokio::test]
async fn replace_refuses_unfinished_dependents() {
    let fx = replace_fixture().await;
    let (task, _) = produced(&fx, "base", &[("a.txt", "A\n")], json!({})).await;
    declare(
        &fx.boot,
        json!({"key": "review", "kind": "codex", "goal": "review it", "depends_on": ["base"],
        "declared_by": PLANNER_DECLARATION_AUTHOR, "ready": true, "no_gate_reason": "f"}),
    )
    .await;

    let result = replace(&fx, replace_args(&task, "u1")).await;

    assert_refusal(&result, "pending_dependents");
    assert!(result.unwrap_err().message.contains("review"));
}

/// §4.7 off the replaceable route: a managed Track, a terminal task, a child-Track route, an
/// isolated selector.
#[tokio::test]
async fn replace_refuses_unsupported_routes() {
    let fx = replace_fixture().await;
    let (task, _) = produced(&fx, "routed", &[("a.txt", "A\n")], json!({})).await;
    sqlx::query("UPDATE tracks SET workspace_kind = 'managed' WHERE id = ?1")
        .bind(fx.track())
        .execute(&fx.pool())
        .await
        .unwrap();
    assert_refusal(
        &replace(&fx, replace_args(&task, "m1")).await,
        "unsupported_route",
    );
    hold_claims(&fx).await;
    let cases = [
        json!({"key": "term", "kind": "terminal", "command": "true"}),
        json!({"key": "child", "kind": "codex", "goal": "g", "spawn": TASK_CHILD_TRACK_ROUTE}),
        json!({"key": "iso", "kind": "codex", "goal": "g",
            "context": {"neige_execution": {"version": "isolated-codex-v1", "workspace": "empty"}}}),
    ];
    for mut declaration in cases {
        declaration["declared_by"] = json!(PLANNER_DECLARATION_AUTHOR);
        declaration["ready"] = json!(true);
        declaration["no_gate_reason"] = json!("f");
        let key = declaration["key"].as_str().unwrap().to_string();
        declare(&fx.boot, declaration).await;
        let task = current(&fx.boot, &key).await;
        assert_refusal(
            &replace(&fx, replace_args(&task, &key)).await,
            "unsupported_route",
        );
        assert_eq!(
            current(&fx.boot, &key).await.status,
            TaskStatus::Pending,
            "{key} not stopped"
        );
    }
}

/// §4.7 the derived key is taken, or longer than 64.
#[tokio::test]
async fn replace_refuses_taken_and_too_long_derived_keys() {
    let fx = replace_fixture().await;
    let (task, _) = produced(&fx, "taken", &[("a.txt", "A\n")], json!({})).await;
    declare(
        &fx.boot,
        json!({"key": "taken.2", "kind": "codex", "goal": "g",
        "declared_by": PLANNER_DECLARATION_AUTHOR, "ready": false, "no_gate_reason": "f"}),
    )
    .await;
    assert_refusal(
        &replace(&fx, replace_args(&task, "k1")).await,
        "derived_key_taken",
    );

    let long = "l".repeat(63);
    let (task, _) = produced(&fx, &long, &[("b.txt", "B\n")], json!({})).await;
    assert_refusal(
        &replace(&fx, replace_args(&task, "k2")).await,
        "derived_key_too_long",
    );
    assert_eq!(receipt_count(&fx).await, 0);
}

/// §4.7 an ended Track.
#[tokio::test]
async fn replace_refuses_on_an_ended_track() {
    let fx = replace_fixture().await;
    let (task, _) = produced(&fx, "ended", &[("a.txt", "A\n")], json!({})).await;
    sqlx::query("UPDATE tracks SET lifecycle = 'done' WHERE id = ?1")
        .bind(fx.track())
        .execute(&fx.pool())
        .await
        .unwrap();
    assert_refusal(
        &replace(&fx, replace_args(&task, "e1")).await,
        "track_terminal",
    );
}

/// The successor declaration would not be projected (here: the Planner ceiling was lowered to 0
/// while the predecessor ran): the whole replacement rolls back with a named refusal instead of
/// canceling the predecessor and answering an attempt that does not exist.
#[tokio::test]
async fn replace_refuses_an_unschedulable_successor_and_keeps_the_predecessor_running() {
    let fx = replace_fixture().await;
    let (_worker, task) = running(&fx, "capped", json!({})).await;
    sqlx::query("UPDATE tracks SET planner_task_ceiling = 0 WHERE id = ?1")
        .bind(fx.track())
        .execute(&fx.pool())
        .await
        .unwrap();
    let blocks = report_blocks(&fx).await;
    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(&fx.pool())
        .await
        .unwrap();

    let result = replace(&fx, replace_args(&task, "cap1")).await;

    assert_refusal(&result, "successor_unschedulable");
    let message = result.unwrap_err().message;
    assert!(message.contains("capped.2: planner_task_ceiling"), "{message}");
    assert_eq!(
        current(&fx.boot, "capped").await.status,
        TaskStatus::Running
    );
    assert_eq!(receipt_count(&fx).await, 0);
    assert_eq!(report_blocks(&fx).await, blocks);
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(&fx.pool())
        .await
        .unwrap();
    assert_eq!(after, events);
}

/// §4.7 (added): the predecessor's task block is gone from the report, so there is nothing to copy.
#[tokio::test]
async fn replace_refuses_a_predecessor_without_a_block() {
    let fx = replace_fixture().await;
    let (task, _) = produced(&fx, "gone-block", &[("a.txt", "A\n")], json!({})).await;
    let block = block_of(&fx, "gone-block").await;
    crate::mcp_track_report::call_tool(
        &fx.boot,
        "calm.report.blocks.delete",
        crate::mcp_track_report::planner_identity(&fx.boot),
        json!({"id": block.id, "if_rev": block.rev}),
    )
    .await
    .unwrap();
    assert!(
        report_blocks(&fx)
            .await
            .iter()
            .all(|b| b.payload["key"] != "gone-block")
    );

    assert_refusal(
        &replace(&fx, replace_args(&task, "gb1")).await,
        "predecessor_undeclared",
    );
    assert_eq!(receipt_count(&fx).await, 0);
}

/// A dependent declared `ready: false` has no execution row yet; it still depends on the
/// predecessor's key, so the replacement is refused before the running predecessor is stopped.
#[tokio::test]
async fn replace_refuses_an_unready_dependent_declaration() {
    let fx = replace_fixture().await;
    let (_worker, task) = running(&fx, "upstream-work", json!({})).await;
    declare(
        &fx.boot,
        json!({"key": "later-review", "kind": "codex", "goal": "review", "depends_on": ["upstream-work"],
            "declared_by": PLANNER_DECLARATION_AUTHOR, "ready": false, "no_gate_reason": "f"}),
    )
    .await;

    let result = replace(&fx, replace_args(&task, "ud1")).await;

    assert_refusal(&result, "pending_dependents");
    assert!(result.unwrap_err().message.contains("later-review"));
    assert_eq!(
        current(&fx.boot, "upstream-work").await.status,
        TaskStatus::Running
    );
    assert_eq!(receipt_count(&fx).await, 0);
}

/// The predecessor's block was edited to a child-Track route after its execution started: the
/// successor would copy that route, so the replacement is refused before the stop.
#[tokio::test]
async fn replace_refuses_a_predecessor_block_edited_off_the_route() {
    let fx = replace_fixture().await;
    let (_worker, task) = running(&fx, "moved", json!({})).await;
    let block = block_of(&fx, "moved").await;
    let mut payload = block.payload.clone();
    payload["spawn"] = json!(TASK_CHILD_TRACK_ROUTE);
    crate::mcp_track_report::call_tool(
        &fx.boot,
        "calm.report.blocks.upsert",
        crate::mcp_track_report::planner_identity(&fx.boot),
        json!({"id": block.id, "kind": "task", "payload": payload, "if_rev": block.rev}),
    )
    .await
    .unwrap();
    let blocks = report_blocks(&fx).await;

    let result = replace(&fx, replace_args(&task, "mv1")).await;

    assert_refusal(&result, "unsupported_route");
    assert_eq!(current(&fx.boot, "moved").await.status, TaskStatus::Running);
    assert_eq!(receipt_count(&fx).await, 0);
    assert_eq!(report_blocks(&fx).await, blocks);
}
