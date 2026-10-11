//! #2554: an attempt whose task gives it no worker to reach is answered from its task row before
//! any terminal lookup, with one typed refusal per lifecycle state on every terminal tool: a
//! pending task names why it waits and that its task block still takes edits; a dispatched one is
//! starting; a child-Track task's workers are that Track's; a task that ended before it had a
//! worker is parked.
use super::terminal_support::Harness;
use super::worker_message::assert_refusal;
use calm_server::model::{new_id, now_ms};
use serde_json::{Value, json};
use std::time::Duration;

/// A task row with no worker card, as the scheduler leaves it before it runs the task.
async fn unbound(h: &Harness, status: &str, spawn: &str) -> String {
    let key = new_id();
    let task = format!("{}:{key}", h.track);
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,spawn,declared_by,created_at_ms,updated_at_ms) VALUES (?1,?2,?3,'codex','test','[]',?4,?5,'user',?6,?6)")
        .bind(&task).bind(&h.track).bind(key).bind(status).bind(spawn).bind(now_ms())
        .execute(h.sql.pool()).await.unwrap();
    task
}

/// Every terminal tool that takes an attempt answers it with the same typed refusal.
async fn every_tool_refuses(h: &Harness, attempt: &str, refusal: &str, text: &str) {
    let target = json!({"attempt_id":attempt});
    let mut message = target.clone();
    message["idempotency_key"] = json!(new_id());
    message["action"] = json!({"type":"message","text":"x"});
    let mut read = target.clone();
    read["wait_ms"] = json!(50);
    let mut claim = target.clone();
    claim["action"] = json!("claim");
    for (tool, args) in [
        ("neige_terminal_show", target),
        ("neige_terminal_read", read),
        ("neige_terminal_control", claim),
        ("neige_terminal_input", message),
    ] {
        let reply: Value = h.call(tool, args).await;
        assert_refusal(&reply, -32403, Some(refusal), text);
    }
}

#[tokio::test]
async fn pending_task_names_why_it_waits_and_that_its_block_takes_edits() {
    let h = Harness::start().await;
    let attempt = unbound(&h, "pending", "in-wave").await;
    // The reason `neige_task_ls` gives: this harness Track has no report to declare the task.
    let text = format!(
        "attempt {attempt} (task {}) has not started (Task report is missing; restore its \
         declaration before execution); it has no worker yet. To change what it will do, edit its task block: \
         its worker starts with it",
        attempt.split_once(':').unwrap().1
    );
    every_tool_refuses(&h, &attempt, "task_pending", &text).await;
}

#[tokio::test]
async fn dispatched_task_without_a_worker_card_is_starting() {
    let h = Harness::start().await;
    h.interaction()
        .set_message_start_wait(Duration::from_millis(300));
    let attempt = unbound(&h, "dispatched", "in-wave").await;
    let text =
        format!("attempt {attempt} is dispatched; its worker is starting. Read again, then send.");
    every_tool_refuses(&h, &attempt, "worker_starting", &text).await;
}

#[tokio::test]
async fn child_track_task_has_no_worker_in_this_track() {
    let h = Harness::start().await;
    let attempt = unbound(&h, "running", "sub-wave").await;
    every_tool_refuses(&h, &attempt, "task_in_child_track", "runs as a child Track").await;
}

#[tokio::test]
async fn task_ended_before_its_worker_is_parked() {
    let h = Harness::start().await;
    let attempt = unbound(&h, "canceled", "in-wave").await;
    let text = format!(
        "attempt {attempt} (task {}) is canceled; its worker takes no input. Declare a new task.",
        attempt.split_once(':').unwrap().1
    );
    every_tool_refuses(&h, &attempt, "worker_parked", &text).await;
}
