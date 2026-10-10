//! #2493 feature B: the Planner's `message` to a running task worker, over the production MCP
//! socket, renderer, client pump and supervisor writer. A message is one bracketed-paste write
//! headed by the kernel; typed keys into a codex task worker stay refused (#1784), a Claude worker
//! takes both, and a parked or unreadable worker takes nothing.
use super::task_terminal::{Worker, stop, worker_running};
use super::terminal_support::{Harness, human_takeover};
use calm_server::db::prelude::*;
use calm_server::model::now_ms;
use calm_server::terminal_interaction::message_header;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const KEYS_REFUSED: &str = include_str!("../../prompts/terminal/worker-keys-refused.md");

/// A worker TUI stand-in: raw, unechoed, bracketed paste on (unless `paste` is false), and every
/// `read(2)` of its stdin logged as one hex line, so the log shows how many writes arrived.
/// With `flip`, paste mode turns off once that file exists.
fn recorder(log: &Path, paste: bool, flip: Option<&Path>) -> String {
    let mode = if paste { "\\033[?2004h" } else { "" };
    let flip = flip.map_or(String::new(), |flip| {
        format!(
            "(while [ ! -e '{}' ]; do sleep 0.05; done; printf '\\033[?2004lPASTE_OFF\\r\\n') & ",
            flip.display()
        )
    });
    format!(
        "stty raw -echo; printf '{mode}WORKER_READY\\r\\n'; {flip}while :; do dd bs=65536 count=1 \
         2>/dev/null | od -An -v -tx1 | tr -d ' \\n'; echo; done > '{}'",
        log.display()
    )
}

/// The reads the worker saw, each decoded from its hex line.
pub(crate) fn reads(log: &Path) -> Vec<Vec<u8>> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            (0..line.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(&line[at..at + 2], 16).unwrap())
                .collect()
        })
        .collect()
}

pub(crate) async fn wait_for_reads(log: &Path, count: usize) -> Vec<Vec<u8>> {
    let start = Instant::now();
    loop {
        let seen = reads(log);
        if seen.len() >= count {
            // A second physical write lands 40 ms after the first; give it the time to show.
            tokio::time::sleep(Duration::from_millis(200)).await;
            return reads(log);
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "worker saw {seen:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub(crate) fn expected(attempt: &str, text: &str) -> Vec<u8> {
    [
        b"\x1b[200~".as_slice(),
        message_header(attempt).as_bytes(),
        b"\n",
        text.as_bytes(),
        b"\x1b[201~\r",
    ]
    .concat()
}

pub(crate) struct Fixture {
    pub(crate) worker: Worker,
    pub(crate) log: PathBuf,
    pub(crate) flip: PathBuf,
}

pub(crate) async fn running(h: &Harness, kind: &str, paste: bool, flip: bool) -> Fixture {
    let tag = uuid::Uuid::new_v4();
    let log = h.root.path().join(format!("{kind}-{tag}.log"));
    let flip_file = h.root.path().join(format!("{kind}-{tag}.flip"));
    let script = recorder(&log, paste, flip.then_some(flip_file.as_path()));
    let worker = worker_running(h, kind, &h.track, Some(&script)).await;
    h.observe_text(&worker.terminal, "WORKER_READY").await;
    Fixture {
        worker,
        log,
        flip: flip_file,
    }
}

pub(crate) async fn message(h: &Harness, target: Value, key: &str, text: &str) -> Value {
    let mut args = target;
    args["idempotency_key"] = json!(key);
    args["action"] = json!({"type":"message","text":text});
    h.call("neige_terminal_input", args).await
}

pub(crate) fn assert_refusal(reply: &Value, code: i64, refusal: Option<&str>, text: &str) {
    let error = reply
        .get("error")
        .unwrap_or_else(|| panic!("expected a refusal, got {reply}"));
    assert_eq!(error["code"], code, "{reply}");
    assert!(
        error["message"].as_str().unwrap_or_default().contains(text),
        "{reply}"
    );
    assert_eq!(
        error["data"]["refusal"].as_str(),
        refusal,
        "data.refusal: {reply}"
    );
}

pub(crate) fn written(reply: &Value) -> &Value {
    assert!(reply.get("error").is_none(), "{reply}");
    let receipt = &reply["result"]["structuredContent"];
    assert_eq!(receipt["outcome"], "written", "{receipt}");
    receipt
}

#[tokio::test]
async fn message_is_one_bracketed_paste_write() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    let text = "rerun the gate\n\twith --locked";
    let reply = message(&h, json!({"attempt_id":f.worker.task}), "m1", text).await;
    let receipt = written(&reply);
    assert_eq!(receipt["attempt_id"], f.worker.task);
    assert_eq!(
        wait_for_reads(&f.log, 1).await,
        vec![expected(&f.worker.task, text)],
        "one write: paste start, header, text, paste end and CR"
    );
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn message_never_leads_with_slash_or_bang() {
    let h = Harness::start().await;
    let f = running(&h, "codex", true, false).await;
    let texts = ["/clear", "!rm -rf target"];
    for (at, text) in texts.into_iter().enumerate() {
        let key = format!("m{at}");
        written(&message(&h, json!({"terminal_id":f.worker.terminal}), &key, text).await);
    }
    let seen = wait_for_reads(&f.log, 2).await;
    assert_eq!(
        seen,
        texts.map(|text| expected(&f.worker.task, text)).to_vec()
    );
    for read in seen {
        assert!(read.starts_with(b"\x1b[200~[neige] "), "{read:?}");
    }
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn message_text_refuses_controls() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    for (at, (text, why)) in [
        ("end\x1b[201~/clear", "U+001B at byte 3"),
        ("line\rsubmit", "U+000D at byte 4"),
        ("stop\x03", "U+0003 at byte 4"),
        ("del\x7f", "U+007F at byte 3"),
        ("", "message text is empty"),
    ]
    .into_iter()
    .enumerate()
    {
        let reply = message(
            &h,
            json!({"attempt_id":f.worker.task}),
            &format!("c{at}"),
            text,
        )
        .await;
        assert_refusal(&reply, -32602, None, why);
    }
    // Positive control, written after the refusals: their bytes would have landed by now.
    written(&message(&h, json!({"attempt_id":f.worker.task}), "ok", "ok").await);
    assert_eq!(
        wait_for_reads(&f.log, 1).await,
        vec![expected(&f.worker.task, "ok")]
    );
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn message_over_cap_is_refused() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    let room = calm_server::terminal_interaction::MESSAGE_BYTES_MAX
        - message_header(&f.worker.task).len()
        - 1;
    let reply = message(
        &h,
        json!({"attempt_id":f.worker.task}),
        "big",
        &"x".repeat(room + 1),
    )
    .await;
    assert_refusal(&reply, -32602, None, "the cap is 8000");
    written(&message(&h, json!({"attempt_id":f.worker.task}), "ok", "ok").await);
    assert_eq!(
        wait_for_reads(&f.log, 1).await,
        vec![expected(&f.worker.task, "ok")]
    );
    stop(&h, &f.worker).await;
}

pub(crate) async fn set_status(h: &Harness, task: &str, status: &str) {
    sqlx::query("UPDATE tasks SET status=?2,finished_at_ms=?3 WHERE id=?1")
        .bind(task)
        .bind(status)
        .bind(now_ms())
        .execute(h.sql.pool())
        .await
        .unwrap();
}

/// A parked worker takes no message; `text` and `submit` keep refusing under the write rule.
async fn parked(status: &str, next: &str) {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
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
    set_status(&h, &f.worker.task, status).await;
    let reply = message(&h, json!({"attempt_id":f.worker.task}), "late", "x").await;
    let text = format!(
        "attempt {} (task {}) is {status}; its worker takes no input. {next}",
        f.worker.task,
        f.worker.task.split_once(':').unwrap().1
    );
    assert_refusal(&reply, -32403, Some("worker_parked"), &text);
    // Every agent write path refuses with the same typed refusal (the owner's S1 review comment
    // on #2493).
    for (key, action) in [
        ("text", json!({"type":"text","text":"x"})),
        ("submit", json!({"type":"submit","text":"x"})),
        ("key", json!({"type":"key","key":"Enter"})),
    ] {
        let typed = h
            .call(
                "neige_terminal_input",
                json!({"attempt_id":f.worker.task,"observation_id":view["observation_id"],
                    "idempotency_key":key,"action":action}),
            )
            .await;
        assert_refusal(&typed, -32403, Some("worker_parked"), &text);
    }
    let claim = h
        .call(
            "neige_terminal_control",
            json!({"attempt_id":f.worker.task,"action":"claim"}),
        )
        .await;
    assert_refusal(&claim, -32403, Some("worker_parked"), &text);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(reads(&f.log).is_empty(), "{:?}", reads(&f.log));
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn parked_worker_refuses_message_done() {
    parked("done", "Declare a new task for a fresh worker.").await;
}
#[tokio::test]
async fn parked_worker_refuses_message_verifying() {
    parked("verifying", "Wait for its gate.").await;
}
#[tokio::test]
async fn parked_worker_refuses_message_failed() {
    parked("failed", "Declare a new task for a fresh worker.").await;
}

#[tokio::test]
async fn message_to_dispatched_attempt_is_refused() {
    let h = Harness::start().await;
    let f = running(&h, "codex", true, false).await;
    sqlx::query("UPDATE tasks SET status='dispatched' WHERE id=?1")
        .bind(&f.worker.task)
        .execute(h.sql.pool())
        .await
        .unwrap();
    let reply = message(&h, json!({"attempt_id":f.worker.task}), "early", "x").await;
    let text = format!(
        "attempt {} is dispatched; its worker is starting. Read again, then send.",
        f.worker.task
    );
    assert_refusal(&reply, -32403, Some("worker_starting"), &text);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(reads(&f.log).is_empty());
    stop(&h, &f.worker).await;
}

/// Replaces #1784's `codex_task_worker_refuses_terminal_input_and_writes_no_bytes`: typed keys
/// and a claim are still refused before a byte, and `message` is written.
#[tokio::test]
async fn codex_running_worker_takes_message_refuses_keys() {
    let h = Harness::start().await;
    let f = running(&h, "codex", true, false).await;
    let codex = &f.worker;
    let refusal = KEYS_REFUSED.replace("{provider}", "codex");
    let before = h
        .ok(
            "neige_terminal_read",
            json!({"attempt_id":codex.task,"wait_ms":50}),
        )
        .await;
    let mut replies = Vec::new();
    for (key, action) in [
        ("interrupt", json!({"type":"key","key":"Escape"})),
        ("scope", json!({"type":"submit","text":"narrowed scope"})),
    ] {
        for target in [
            json!({"attempt_id":codex.task}),
            json!({"terminal_id":codex.terminal}),
        ] {
            let mut args = target;
            args["observation_id"] = before["observation_id"].clone();
            args["idempotency_key"] = json!(key);
            args["claim"] = json!(true);
            args["action"] = action.clone();
            replies.push(h.call("neige_terminal_input", args).await);
        }
    }
    replies.push(
        h.call(
            "neige_terminal_control",
            json!({"attempt_id":codex.task,"action":"claim"}),
        )
        .await,
    );
    let text = "narrow the scope to the parser";
    written(&message(&h, json!({"attempt_id":codex.task}), "m", text).await);
    assert_eq!(
        wait_for_reads(&f.log, 1).await,
        vec![expected(&codex.task, text)],
        "only the message reached the PTY"
    );
    for reply in &replies {
        assert_refusal(reply, -32403, Some("worker_keys_refused"), &refusal);
    }
    let shown = h
        .ok("neige_terminal_show", json!({"attempt_id":codex.task}))
        .await;
    assert_eq!(shown["controllable"], false, "{shown}");
    assert_eq!(shown["input_refused"], refusal, "{shown}");
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn claude_running_worker_takes_message_and_keys() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    let claude = &f.worker;
    let shown = h
        .ok("neige_terminal_show", json!({"attempt_id":claude.task}))
        .await;
    assert_eq!(shown["controllable"], true, "{shown}");
    assert!(shown.get("input_refused").is_none(), "{shown}");
    h.ok(
        "neige_terminal_control",
        json!({"attempt_id":claude.task,"action":"claim"}),
    )
    .await;
    let view = h
        .ok(
            "neige_terminal_read",
            json!({"attempt_id":claude.task,"wait_ms":50}),
        )
        .await;
    let typed = h.input(
        &claude.terminal,
        &view,
        "keys",
        json!({"type":"text","text":"keys"}),
    );
    assert_eq!(typed.await["outcome"], "written");
    wait_for_reads(&f.log, 1).await;
    written(&message(&h, json!({"attempt_id":claude.task}), "m", "message").await);
    assert_eq!(
        wait_for_reads(&f.log, 2).await,
        vec![b"keys".to_vec(), expected(&claude.task, "message")]
    );
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn plain_terminal_refuses_message() {
    let h = Harness::start().await;
    let log = h.root.path().join("plain.log");
    let opened = h
        .ok(
            "neige_terminal_open",
            json!({"idempotency_key":"plain","program":recorder(&log, true, None)}),
        )
        .await;
    let plain = opened["terminal_id"].as_str().unwrap().to_owned();
    h.observe_text(&plain, "WORKER_READY").await;
    let reply = message(&h, json!({"terminal_id":plain}), "m", "x").await;
    let text = format!(
        "action \"message\" needs a task worker whose agent declares it (claude, codex); \
         terminal {plain} runs terminal. Use \"text\" or \"submit\""
    );
    assert_refusal(&reply, -32403, Some("message_unsupported"), &text);
    // A terminal-kind task worker runs no agent conversation either.
    let task = running(&h, "terminal", true, false).await;
    let reply = message(&h, json!({"attempt_id":task.worker.task}), "m", "x").await;
    assert_refusal(&reply, -32403, Some("message_unsupported"), "runs terminal");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(reads(&log).is_empty() && reads(&task.log).is_empty());
    h.state.terminal_renderer.drop_entry(&plain).await;
    stop(&h, &task.worker).await;
}

#[tokio::test]
async fn message_replay_writes_once() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    let target = json!({"attempt_id":f.worker.task});
    let first = message(&h, target.clone(), "once", "only once").await;
    let replay = message(&h, target.clone(), "once", "only once").await;
    assert_eq!(written(&first)["outcome"], written(&replay)["outcome"]);
    let conflict = message(&h, target, "once", "other text").await;
    assert_refusal(&conflict, -32403, None, "reused with different arguments");
    assert_eq!(
        wait_for_reads(&f.log, 1).await,
        vec![expected(&f.worker.task, "only once")]
    );
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn terminal_without_readable_view_refuses_message() {
    let h = Harness::start().await;
    let worker = worker_running(&h, "claude", &h.track, None).await;
    let reply = message(&h, json!({"attempt_id":worker.task}), "m", "x").await;
    assert_refusal(
        &reply,
        -32403,
        Some("terminal_unreadable"),
        "the worker's terminal has no live view (after a server restart until reattached, #2499)",
    );
    assert!(h.state.terminal_renderer.get(&worker.terminal).is_none());
}

#[tokio::test]
async fn bracketed_paste_off_refuses_message() {
    let h = Harness::start().await;
    let f = running(&h, "codex", false, false).await;
    let reply = message(&h, json!({"attempt_id":f.worker.task}), "m", "x").await;
    assert_refusal(
        &reply,
        -32403,
        Some("terminal_unreadable"),
        "or bracketed paste is off; nothing was sent",
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(reads(&f.log).is_empty());
    stop(&h, &f.worker).await;
}

/// Hold the renderer's write admission once the message client has attached and before its input
/// is sent; `change` runs while the write waits in the writer's queue, then admission resumes.
async fn queued_change(h: &Harness, f: &Fixture, change: impl std::future::Future<Output = ()>) {
    let held = Arc::new(Mutex::new(None));
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let registry = h.state.terminal_renderer.clone();
    let slot = held.clone();
    h.interaction()
        .set_message_write_seam(Box::new(move |terminal: String| {
            Box::pin(async move {
                let entry = registry.get(&terminal).unwrap();
                let guard = entry.handle.input_barrier.hold_for_test().await;
                *slot.lock().unwrap() = Some(guard);
                let _ = entered.send(());
            })
        }));
    let driver = async {
        entered_rx.await.unwrap();
        // Let the pump hand the write to the writer, which now waits for admission.
        tokio::time::sleep(Duration::from_millis(200)).await;
        change.await;
        drop(held.lock().unwrap().take());
    };
    let (reply, ()) = tokio::join!(
        message(h, json!({"attempt_id":f.worker.task}), "queued", "x"),
        driver
    );
    assert!(reply.get("error").is_none(), "{reply}");
    let receipt = &reply["result"]["structuredContent"];
    assert_eq!(receipt["outcome"], "refused", "{receipt}");
    assert_eq!(
        receipt["reason"],
        "terminal input control or scope was revoked before write"
    );
    // The writer dropped the refused item before answering; nothing of it can land later.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(reads(&f.log).is_empty(), "{:?}", reads(&f.log));
    // A proven refusal is not cached: a resend of the key is decided anew (refused by the rule
    // or the terminal check now), never answered with the refused receipt.
    let resend = message(h, json!({"attempt_id":f.worker.task}), "queued", "x").await;
    assert_eq!(resend["error"]["code"], -32403, "{resend}");
}

#[tokio::test]
async fn queued_message_refused_when_paste_mode_turns_off() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, true).await;
    let entry = h.state.terminal_renderer.get(&f.worker.terminal).unwrap();
    queued_change(&h, &f, async {
        std::fs::write(&f.flip, b"").unwrap();
        let start = Instant::now();
        while entry
            .handle
            .model_view
            .lock()
            .unwrap()
            .capture(0)
            .unwrap()
            .0
            .input_surface()
            .bracketed_paste()
        {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "paste mode stayed on"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn queued_message_refused_when_task_finishes() {
    let h = Harness::start().await;
    let f = running(&h, "codex", true, false).await;
    queued_change(&h, &f, set_status(&h, &f.worker.task, "done")).await;
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn message_delivered_while_another_client_owns_the_terminal() {
    let h = Harness::start().await;
    let f = running(&h, "codex", true, false).await;
    let entry = h.state.terminal_renderer.get(&f.worker.terminal).unwrap();
    let user = uuid::Uuid::new_v4();
    let (browser, _input) = human_takeover(&entry, &f.worker.terminal, user).await;
    let reply = message(&h, json!({"attempt_id":f.worker.task}), "m", "while owned").await;
    written(&reply);
    assert_eq!(
        wait_for_reads(&f.log, 1).await,
        vec![expected(&f.worker.task, "while owned")]
    );
    assert_eq!(
        entry.handle.owner_registry.lock().unwrap().current_owner(),
        Some(user),
        "the message never claims or takes control"
    );
    browser.abort();
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn message_refuses_observation_and_control_options() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    let view = h
        .ok(
            "neige_terminal_read",
            json!({"attempt_id":f.worker.task,"wait_ms":50}),
        )
        .await;
    for (option, value) in [
        ("observation_id", view["observation_id"].clone()),
        ("allow_output_since_observation", json!(true)),
        ("claim", json!(false)),
        ("release", json!(true)),
    ] {
        let mut args = json!({"attempt_id":f.worker.task,"idempotency_key":option,
            "action":{"type":"message","text":"x"}});
        args[option] = value;
        let reply = h.call("neige_terminal_input", args).await;
        assert_refusal(
            &reply,
            -32602,
            None,
            &format!("action \"message\" does not take {option}; valid: terminal_id, attempt_id"),
        );
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(reads(&f.log).is_empty());
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn message_to_ended_worker_is_refused() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    h.sql
        .session_projection_set_status_for_card(
            &f.worker.card,
            calm_server::session_projection_repo::WorkerSessionState::Exited,
        )
        .await
        .unwrap();
    let reply = message(&h, json!({"attempt_id":f.worker.task}), "m", "x").await;
    let text = format!(
        "the worker of attempt {} has ended (exited); this call wrote nothing. Declare a new task.",
        f.worker.task
    );
    assert_refusal(&reply, -32403, Some("worker_ended"), &text);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(reads(&f.log).is_empty());
    stop(&h, &f.worker).await;
}

/// A replay answers with its first receipt and writes nothing, whatever changed since: the
/// lookup comes before every new-write check.
async fn replay_after(h: &Harness, f: &Fixture, change: impl std::future::Future<Output = ()>) {
    let target = json!({"attempt_id":f.worker.task});
    let first = message(h, target.clone(), "once", "only once").await;
    written(&first);
    assert_eq!(wait_for_reads(&f.log, 1).await.len(), 1);
    change.await;
    let replay = message(h, target, "once", "only once").await;
    assert_eq!(
        written(&replay)["attempt_id"],
        first["result"]["structuredContent"]["attempt_id"]
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        reads(&f.log),
        vec![expected(&f.worker.task, "only once")],
        "the replay wrote nothing"
    );
}

#[tokio::test]
async fn message_replay_after_task_completes_returns_the_receipt() {
    let h = Harness::start().await;
    let f = running(&h, "codex", true, false).await;
    replay_after(&h, &f, set_status(&h, &f.worker.task, "done")).await;
    // A new key on the finished attempt is refused as parked.
    let fresh = message(&h, json!({"attempt_id":f.worker.task}), "new", "x").await;
    assert_refusal(
        &fresh,
        -32403,
        Some("worker_parked"),
        "its worker takes no input",
    );
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn message_replay_after_paste_mode_off_returns_the_receipt() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, true).await;
    let entry = h.state.terminal_renderer.get(&f.worker.terminal).unwrap();
    replay_after(&h, &f, async {
        std::fs::write(&f.flip, b"").unwrap();
        let start = Instant::now();
        while entry
            .handle
            .model_view
            .lock()
            .unwrap()
            .capture(0)
            .unwrap()
            .0
            .input_surface()
            .bracketed_paste()
        {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "paste mode stayed on"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let fresh = message(&h, json!({"attempt_id":f.worker.task}), "new", "x").await;
    assert_refusal(
        &fresh,
        -32403,
        Some("terminal_unreadable"),
        "bracketed paste is off",
    );
    stop(&h, &f.worker).await;
}
