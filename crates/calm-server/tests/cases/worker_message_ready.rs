//! #2532: a Claude worker takes a `message` only once its agent reported submitting the task
//! prompt (its first `UserPromptSubmit` hook for this worker session, through the production
//! ingest route). Before that, its terminal may show a startup screen (the folder-trust dialog)
//! whose selected option a message's Enter would confirm. Typed keys keep their rule: the worker
//! watcher answers a startup screen with them. A codex worker's first turn starts before its
//! terminal UI, so it takes a message from `running` on.
use super::task_terminal::{Worker, stop};
use super::terminal_support::Harness;
use super::worker_message::{
    Fixture, assert_refusal, expected, message, queued_change, reads, running, unstarted,
    wait_for_reads, written,
};
use serde_json::{Value, json};
use std::time::Duration;

/// The Claude worker's `UserPromptSubmit` hook for its own session, through the production ingest
/// route: what Claude posts once it submits the task prompt.
pub(crate) async fn prompt_submitted(h: &Harness, worker: &Worker) {
    let status = h
        .post_claude_hook(
            &worker.card,
            &json!({"hook_event_name":"UserPromptSubmit","session_id":agent_session(h, worker).await,
                "prompt":"the task"}),
        )
        .await;
    assert!(status.is_success(), "{status}");
}

/// The Claude session id the worker's hooks carry.
pub(crate) async fn agent_session(h: &Harness, worker: &Worker) -> String {
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT agent_session_id FROM worker_sessions WHERE id=?1",
    )
    .bind(&worker.session)
    .fetch_one(h.sql.pool())
    .await
    .unwrap()
    .expect("claude worker session carries its agent session id")
}

fn not_started(attempt: &str) -> String {
    format!(
        "attempt {attempt} is running, but its worker has not started its task yet; its agent may \
         still be on a startup screen, which the worker watcher handles. Read its screen or wait, \
         then send again"
    )
}

async fn assert_not_started(h: &Harness, f: &Fixture, key: &str) {
    let reply = message(h, json!({"attempt_id":f.worker.task}), key, "x").await;
    assert_refusal(
        &reply,
        -32403,
        Some("worker_starting"),
        &not_started(&f.worker.task),
    );
}

async fn post_hook(h: &Harness, worker: &Worker, body: Value) -> axum::http::StatusCode {
    h.post_claude_hook(&worker.card, &body).await
}

#[tokio::test]
async fn message_to_claude_worker_before_prompt_submit_is_refused() {
    let h = Harness::start().await;
    let f = unstarted(&h, "claude", true, false).await;
    assert_not_started(&h, &f, "early").await;
    // By terminal id too: one rule per resolution.
    let reply = message(&h, json!({"terminal_id":f.worker.terminal}), "early-t", "x").await;
    assert_refusal(&reply, -32403, Some("worker_starting"), "has not started");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(reads(&f.log).is_empty(), "{:?}", reads(&f.log));
    // Typed keys stay accepted: the worker watcher answers a startup screen with them.
    h.ok(
        "neige_terminal_control",
        json!({"attempt_id":f.worker.task,"action":"claim"}),
    )
    .await;
    let view = h
        .ok(
            "neige_terminal_read",
            json!({"attempt_id":f.worker.task,"wait_ms":50}),
        )
        .await;
    let typed = h
        .input(
            &f.worker.terminal,
            &view,
            "keys",
            json!({"type":"text","text":"1"}),
        )
        .await;
    assert_eq!(typed["outcome"], "written", "{typed}");
    assert_eq!(wait_for_reads(&f.log, 1).await, vec![b"1".to_vec()]);
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn message_to_claude_worker_after_prompt_submit_is_written() {
    let h = Harness::start().await;
    let f = unstarted(&h, "claude", true, false).await;
    assert_not_started(&h, &f, "early").await;
    prompt_submitted(&h, &f.worker).await;
    let text = "narrow the scope to the parser";
    written(&message(&h, json!({"attempt_id":f.worker.task}), "m", text).await);
    assert_eq!(
        wait_for_reads(&f.log, 1).await,
        vec![expected(&f.worker.task, text)],
        "only the message reached the PTY"
    );
    stop(&h, &f.worker).await;
}

/// Only this worker session's own `UserPromptSubmit` starts it: another hook kind, another card's
/// prompt, or a session the ingest could not attribute to this worker does not.
#[tokio::test]
async fn prompt_submit_of_another_session_or_card_does_not_unlock() {
    let h = Harness::start().await;
    let f = unstarted(&h, "claude", true, false).await;
    let other = unstarted(&h, "claude", true, false).await;
    let session = agent_session(&h, &f.worker).await;
    for event in ["SessionStart", "Notification", "Stop"] {
        let status = post_hook(
            &h,
            &f.worker,
            json!({"hook_event_name":event,"session_id":session}),
        )
        .await;
        assert!(status.is_success(), "{event}: {status}");
    }
    // A session id that resolves to no worker session: kept, attributed to the card only.
    let status = post_hook(
        &h,
        &f.worker,
        json!({"hook_event_name":"UserPromptSubmit","session_id":"not-a-session","prompt":"p"}),
    )
    .await;
    assert!(status.is_success(), "{status}");
    // Another worker's session on this card is rejected by the ingest.
    let status = post_hook(
        &h,
        &f.worker,
        json!({"hook_event_name":"UserPromptSubmit",
            "session_id":agent_session(&h, &other.worker).await,"prompt":"p"}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    // The other worker starts; this one does not.
    prompt_submitted(&h, &other.worker).await;
    assert_not_started(&h, &f, "still").await;
    written(&message(&h, json!({"attempt_id":other.worker.task}), "m", "x").await);
    assert_eq!(
        wait_for_reads(&other.log, 1).await,
        vec![expected(&other.worker.task, "x")]
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(reads(&f.log).is_empty(), "{:?}", reads(&f.log));
    stop(&h, &f.worker).await;
    stop(&h, &other.worker).await;
}

#[tokio::test]
async fn codex_running_worker_takes_message_without_any_hook() {
    let h = Harness::start().await;
    let f = unstarted(&h, "codex", true, false).await;
    let hooks: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM events WHERE scope_card=?1 AND kind IN ('codex.hook','claude.hook')",
    )
    .bind(&f.worker.card)
    .fetch_one(h.sql.pool())
    .await
    .unwrap();
    assert_eq!(hooks, 0);
    written(&message(&h, json!({"attempt_id":f.worker.task}), "m", "x").await);
    assert_eq!(
        wait_for_reads(&f.log, 1).await,
        vec![expected(&f.worker.task, "x")]
    );
    stop(&h, &f.worker).await;
}

/// The started fact is a persisted hook event, which the retention pruner may delete: a write
/// queued when it goes is refused at the physical write.
#[tokio::test]
async fn queued_message_refused_when_prompt_submit_is_pruned() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    queued_change(&h, &f, async {
        sqlx::query("UPDATE events SET at=0 WHERE kind='claude.hook' AND scope_card=?1")
            .bind(&f.worker.card)
            .execute(h.sql.pool())
            .await
            .unwrap();
        let pruned = calm_server::events_prune::prune_events_once(
            h.sql.pool(),
            &calm_server::events_prune::EventsRetentionPolicy::default(),
        )
        .await
        .unwrap();
        assert!(pruned >= 1, "{pruned}");
    })
    .await;
    assert_not_started(&h, &f, "after").await;
    stop(&h, &f.worker).await;
}
