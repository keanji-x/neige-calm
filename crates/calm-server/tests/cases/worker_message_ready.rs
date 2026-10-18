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
use super::worker_message_lifecycle::assert_waited;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// What the refusal says once the call waited `waited` seconds: what it waited for, and the event
/// to send again on (#2571).
fn not_started(attempt: &str, waited: &str) -> String {
    format!(
        "attempt {attempt} is running, but its worker has not started its task after this call \
         waited {waited} s for it to submit its task prompt; nothing was written. Its agent is \
         likely held on a startup screen: the worker watcher looks at a worker after at least 60 s \
         of quiet and reports to you. Send again after its report"
    )
}

/// A refusal is answered once the call waited for the worker's start: keep that wait short.
const SHORT_WAIT: Duration = Duration::from_millis(300);

async fn assert_not_started(h: &Harness, f: &Fixture, key: &str) {
    h.interaction().set_message_start_wait(SHORT_WAIT);
    let reply = message(h, json!({"attempt_id":f.worker.task}), key, "x").await;
    assert_waited(&reply, SHORT_WAIT, "prompt_submitted", |waited| {
        not_started(&f.worker.task, waited)
    });
}

async fn post_hook(h: &Harness, worker: &Worker, body: Value) -> axum::http::StatusCode {
    h.post_claude_hook(&worker.card, &body).await
}

#[tokio::test]
async fn message_to_claude_worker_before_prompt_submit_is_refused() {
    let h = Harness::start().await;
    let f = unstarted(&h, "claude", true, false).await;
    let sent = Instant::now();
    assert_not_started(&h, &f, "early").await;
    assert!(sent.elapsed() >= SHORT_WAIT, "refused after the wait");
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
        vec![expected(&h, &f.worker.task, text).await],
        "only the message reached the PTY"
    );
    stop(&h, &f.worker).await;
}

/// #2554: a message to a starting worker waits inside the call and is written once the worker
/// starts; the Planner does not guess when to send again.
#[tokio::test]
async fn message_waits_for_its_worker_to_start() {
    let h = Harness::start().await;
    let f = unstarted(&h, "claude", true, false).await;
    let text = "narrow the scope to the parser";
    let delay = Duration::from_millis(800);
    let sent = Instant::now();
    let (reply, ()) = tokio::join!(
        message(&h, json!({"attempt_id":f.worker.task}), "m", text),
        async {
            tokio::time::sleep(delay).await;
            assert!(reads(&f.log).is_empty(), "{:?}", reads(&f.log));
            prompt_submitted(&h, &f.worker).await;
        }
    );
    written(&reply);
    assert!(sent.elapsed() >= delay, "{:?}", sent.elapsed());
    assert_eq!(
        wait_for_reads(&f.log, 1).await,
        vec![expected(&h, &f.worker.task, text).await],
        "written once, after the start"
    );
    stop(&h, &f.worker).await;
}

/// The common start inside the wait: dispatched with no worker card yet, then running with its card
/// stamped, then the task prompt submitted; the message is written once, after all three.
#[tokio::test]
async fn message_waits_through_dispatch_for_its_worker_card_and_start() {
    let h = Harness::start().await;
    let f = unstarted(&h, "claude", true, false).await;
    sqlx::query("UPDATE tasks SET status='dispatched',worker_card_id=NULL WHERE id=?1")
        .bind(&f.worker.task)
        .execute(h.sql.pool())
        .await
        .unwrap();
    let start = async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        sqlx::query("UPDATE tasks SET status='running',worker_card_id=?2 WHERE id=?1")
            .bind(&f.worker.task)
            .bind(&f.worker.card)
            .execute(h.sql.pool())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(reads(&f.log).is_empty(), "{:?}", reads(&f.log));
        prompt_submitted(&h, &f.worker).await;
    };
    let (reply, ()) = tokio::join!(
        message(&h, json!({"attempt_id":f.worker.task}), "m", "x"),
        start
    );
    written(&reply);
    assert_eq!(
        wait_for_reads(&f.log, 1).await,
        vec![expected(&h, &f.worker.task, "x").await]
    );
    stop(&h, &f.worker).await;
}

/// A waiting message writes only to the binding it first resolved: a worker session that replaces
/// it on the same terminal once the call waits is refused, typed, and nothing is written.
#[tokio::test]
async fn message_waiting_for_a_replaced_worker_session_writes_nothing() {
    let h = Harness::start().await;
    let f = unstarted(&h, "claude", true, false).await;
    let replaced = calm_server::model::new_id();
    let (sql, id) = (h.sql.clone(), replaced.clone());
    let (session, card, terminal) = (
        f.worker.session.clone(),
        f.worker.card.clone(),
        f.worker.terminal.clone(),
    );
    // Runs when the call first waits: its first resolved try has pinned the binding.
    h.interaction().set_message_wait_seam(Box::new(move |_| {
        Box::pin(async move { replace_session(&sql, &session, &card, &terminal, id).await })
    }));
    let sent = Instant::now();
    let reply = message(&h, json!({"attempt_id":f.worker.task}), "m", "x").await;
    assert_refusal(
        &reply,
        -32403,
        Some("binding_changed"),
        &format!(
            "the terminal binding this call resolved changed (worker session {} is now \
             {replaced}); show and read again",
            f.worker.session
        ),
    );
    assert!(
        sent.elapsed() < Duration::from_secs(5),
        "refused before the wait ran out"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(reads(&f.log).is_empty(), "{:?}", reads(&f.log));
    stop(&h, &f.worker).await;
}

/// A message queued on the caller's connection behind another is decided under the serial guard
/// (#2571): a worker session that replaced its binding while it queued is refused, typed, and
/// neither message is written.
#[tokio::test]
async fn message_queued_behind_another_refuses_a_replaced_session() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    let held = Arc::new(Mutex::new(None));
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (registry, slot) = (h.state.terminal_renderer.clone(), held.clone());
    h.interaction()
        .set_message_write_seam(Box::new(move |terminal: String| {
            Box::pin(async move {
                let entry = registry.get(&terminal).unwrap();
                *slot.lock().unwrap() = Some(entry.handle.input_barrier.hold_for_test().await);
                let _ = entered.send(());
            })
        }));
    let replaced = calm_server::model::new_id();
    let target = json!({"attempt_id":f.worker.task});
    let holder = message(&h, target.clone(), "held", "x");
    let driver = async {
        entered_rx.await.unwrap();
        let queued = message(&h, target.clone(), "queued", "y");
        let replace = async {
            let start = Instant::now();
            while h.interaction().serial_waiters(&f.worker.terminal).await == 0 {
                assert!(start.elapsed() < Duration::from_secs(10), "never queued");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            replace_session(
                &h.sql,
                &f.worker.session,
                &f.worker.card,
                &f.worker.terminal,
                replaced.clone(),
            )
            .await;
            drop(held.lock().unwrap().take());
        };
        tokio::join!(queued, replace).0
    };
    let (held_reply, queued) = tokio::join!(holder, driver);
    assert_eq!(
        held_reply["result"]["structuredContent"]["outcome"], "refused",
        "{held_reply}"
    );
    assert_refusal(
        &queued,
        -32403,
        Some("binding_changed"),
        &format!(
            "the terminal binding this call resolved changed (worker session {} is now \
             {replaced}); show and read again",
            f.worker.session
        ),
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(reads(&f.log).is_empty(), "{:?}", reads(&f.log));
    stop(&h, &f.worker).await;
}

/// A written message whose binding moves during its readback wait keeps its `written` receipt; the
/// readback says the binding changed and claims nothing about input (#2571).
#[tokio::test]
async fn binding_change_during_a_written_message_readback_keeps_the_receipt() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    let replaced = calm_server::model::new_id();
    let target = json!({"attempt_id":f.worker.task,"read":true,"wait_ms":3000});
    let replace = async {
        wait_for_reads(&f.log, 1).await;
        replace_session(
            &h.sql,
            &f.worker.session,
            &f.worker.card,
            &f.worker.terminal,
            replaced.clone(),
        )
        .await;
    };
    let (reply, ()) = tokio::join!(message(&h, target, "m", "x"), replace);
    let receipt = written(&reply);
    assert_eq!(
        receipt["observation"],
        json!({"status":"unavailable","reason":format!(
            "the terminal binding this call resolved changed (worker session {} is now \
             {replaced}); show and read again",
            f.worker.session
        )}),
        "{receipt}"
    );
    assert_eq!(reads(&f.log), vec![expected(&h, &f.worker.task, "x").await]);
    stop(&h, &f.worker).await;
}

/// Another worker session, `id`, takes over the card and terminal of `session`.
async fn replace_session(
    sql: &calm_server::db::sqlite::SqlxRepo,
    session: &str,
    card: &str,
    terminal: &str,
    id: String,
) {
    use calm_server::db::prelude::*;
    use calm_server::db::sqlite::session_supersede_and_start_tx;
    use calm_server::session_projection_repo::{
        AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    let old = sql
        .session_get_by_id(&session.to_owned().into())
        .await
        .unwrap()
        .unwrap();
    let mut tx = sql.pool().begin().await.unwrap();
    session_supersede_and_start_tx(
        &mut tx,
        &session.to_owned(),
        WorkerSessionInit {
            id,
            card_id: card.to_owned(),
            kind: WorkerSessionKind::ClaudeCard,
            agent_provider: Some(AgentProvider::Claude),
            status: WorkerSessionState::Running,
            terminal_run_id: Some(terminal.to_owned()),
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: old.spawn_op_id,
            now_ms: calm_server::model::now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
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
        vec![expected(&h, &other.worker.task, "x").await]
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
        vec![expected(&h, &f.worker.task, "x").await]
    );
    stop(&h, &f.worker).await;
}

/// The started fact is a persisted hook event, which the retention pruner may delete: a write
/// queued when it goes is refused at the physical write.
#[tokio::test]
async fn queued_message_refused_when_prompt_submit_is_pruned() {
    let h = Harness::start().await;
    h.interaction().set_message_start_wait(SHORT_WAIT);
    let f = running(&h, "claude", true, false).await;
    queued_change(
        &h,
        &f,
        async {
            sqlx::query("UPDATE events SET at=0 WHERE kind='claude.hook' AND scope_card=?1")
                .bind(&f.worker.card)
                .execute(h.sql.pool())
                .await
                .unwrap();
            let pruned = calm_server::events_prune::prune_events_once(
                h.sql.db(),
                &calm_server::events_prune::EventsRetentionPolicy::default(),
            )
            .await
            .unwrap();
            assert!(pruned >= 1, "{pruned}");
        },
        false,
    )
    .await;
    assert_not_started(&h, &f, "after").await;
    stop(&h, &f.worker).await;
}
