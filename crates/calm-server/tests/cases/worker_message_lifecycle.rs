//! #2554: an attempt whose task gives it no worker to reach is answered from its task row before
//! any terminal lookup, with one typed refusal per lifecycle state on every terminal tool: a
//! pending task says it has not started, where its reason shows and that its task block still
//! takes edits; a dispatched one is
//! starting; a child-Track task's workers are that Track's; a task that ended before it had a
//! worker is parked.
use super::terminal_support::Harness;
use super::worker_message::assert_refusal;
use calm_server::model::{new_id, now_ms};
use calm_types::task_recovery::{TASK_CHILD_TRACK_ROUTE, TASK_IN_TRACK_ROUTE};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// A `worker_starting` refusal answered after the message waited at least `wait`: its data names
/// what the worker waits for and how long the call waited, and its full text says the same.
pub(crate) fn assert_waited(
    reply: &Value,
    wait: Duration,
    waiting_for: &str,
    text: impl Fn(&str) -> String,
) {
    let data = &reply["error"]["data"];
    assert_eq!(data["waiting_for"], waiting_for, "{reply}");
    let waited = data["waited_ms"]
        .as_u64()
        .unwrap_or_else(|| panic!("data.waited_ms: {reply}"));
    let wait = wait.as_millis() as u64;
    assert!((wait..wait + 5000).contains(&waited), "{reply}");
    let text = text(&format!("{:.1}", waited as f64 / 1000.0));
    assert_eq!(
        reply["error"]["message"],
        format!("neige_terminal_input: {text}"),
        "{reply}"
    );
    assert_refusal(reply, -32403, Some("worker_starting"), &text);
}

pub(crate) fn dispatched_after_wait(attempt: &str, waited: &str) -> String {
    format!(
        "attempt {attempt} is still dispatched after this call waited {waited} s for its worker to \
         start; nothing was written. Send again on a later wake"
    )
}

/// A task row with no worker card, as the scheduler leaves it before it runs the task.
async fn unbound(h: &Harness, status: &str, spawn: &str) -> String {
    let key = new_id();
    let task = format!("{}:{key}", h.track);
    sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,spawn,declared_by,\
         created_at_ms,updated_at_ms) VALUES (?1,?2,?3,'codex','test','[]',?4,?5,'user',?6,?6)",
    )
    .bind(&task)
    .bind(&h.track)
    .bind(key)
    .bind(status)
    .bind(spawn)
    .bind(now_ms())
    .execute(h.sql.pool())
    .await
    .unwrap();
    task
}

/// A message to `attempt`, answered by the call's own wait at most.
async fn message(h: &Harness, attempt: &str) -> Value {
    h.call(
        "neige_terminal_input",
        json!({"attempt_id":attempt,"idempotency_key":new_id(),
            "action":{"type":"message","text":"x"}}),
    )
    .await
}

/// Every terminal tool that takes an attempt but `message` (which waits for a starting worker)
/// answers it with the same typed refusal, in full.
async fn every_reader_refuses(h: &Harness, attempt: &str, refusal: &str, text: &str) {
    let target = json!({"attempt_id":attempt});
    let mut read = target.clone();
    read["wait_ms"] = json!(50);
    let mut claim = target.clone();
    claim["action"] = json!("claim");
    for (tool, args) in [
        ("neige_terminal_show", target),
        ("neige_terminal_read", read),
        ("neige_terminal_control", claim),
    ] {
        let reply: Value = h.call(tool, args).await;
        assert_refusal(&reply, -32403, Some(refusal), text);
        assert_eq!(
            reply["error"]["message"],
            format!("{tool}: {text}"),
            "{reply}"
        );
    }
}

/// Every terminal tool that takes an attempt answers it with the same typed refusal, `message`
/// without waiting: the state is not one a worker's start ends.
async fn every_tool_refuses(h: &Harness, attempt: &str, refusal: &str, text: &str) {
    every_reader_refuses(h, attempt, refusal, text).await;
    let sent = Instant::now();
    let reply = message(h, attempt).await;
    assert!(
        sent.elapsed() < Duration::from_secs(5),
        "answered without the start wait"
    );
    assert_refusal(&reply, -32403, Some(refusal), text);
    assert_eq!(
        reply["error"]["message"],
        format!("neige_terminal_input: {text}"),
        "{reply}"
    );
    assert_eq!(
        reply["error"]["data"],
        json!({"refusal":refusal}),
        "{reply}"
    );
}

#[tokio::test]
async fn pending_task_points_to_why_it_waits_and_to_an_edit_of_its_block() {
    let h = Harness::start().await;
    let attempt = unbound(&h, "pending", TASK_IN_TRACK_ROUTE).await;
    let text = format!(
        "attempt {attempt} (task {}) has not started, so it has no worker yet; neige_task_ls shows \
         why it waits. To change what it will do, edit its task block: its worker starts with it",
        attempt.split_once(':').unwrap().1
    );
    every_tool_refuses(&h, &attempt, "task_pending", &text).await;
}

/// The readers answer at once with what the worker waits for; `message` answers after its wait,
/// saying how long it waited.
#[tokio::test]
async fn dispatched_task_without_a_worker_card_is_starting() {
    let h = Harness::start().await;
    let wait = Duration::from_millis(300);
    h.interaction().set_message_start_wait(wait);
    let attempt = unbound(&h, "dispatched", TASK_IN_TRACK_ROUTE).await;
    let text = format!(
        "attempt {attempt} is dispatched; its worker is starting and takes no input yet. Send again \
         on a later wake"
    );
    every_reader_refuses(&h, &attempt, "worker_starting", &text).await;
    let reply: Value = h
        .call("neige_terminal_show", json!({"attempt_id":attempt}))
        .await;
    assert_eq!(
        reply["error"]["data"],
        json!({"refusal":"worker_starting","waiting_for":"task_running"}),
        "{reply}"
    );
    let reply = message(&h, &attempt).await;
    assert_waited(&reply, wait, "task_running", |waited| {
        dispatched_after_wait(&attempt, waited)
    });
}

#[tokio::test]
async fn child_track_task_has_no_worker_in_this_track() {
    let h = Harness::start().await;
    let attempt = unbound(&h, "running", TASK_CHILD_TRACK_ROUTE).await;
    let text = format!(
        "attempt {attempt} (task {}) runs as a child Track; its workers belong to that Track, not \
         to yours",
        attempt.split_once(':').unwrap().1
    );
    every_tool_refuses(&h, &attempt, "task_in_child_track", &text).await;
}

#[tokio::test]
async fn task_ended_before_its_worker_is_parked() {
    let h = Harness::start().await;
    let attempt = unbound(&h, "canceled", TASK_IN_TRACK_ROUTE).await;
    let text = format!(
        "attempt {attempt} (task {}) is canceled; its worker takes no input. Declare a new task.",
        attempt.split_once(':').unwrap().1
    );
    every_tool_refuses(&h, &attempt, "worker_parked", &text).await;
}
