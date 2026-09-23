//! #1784: typing into a codex task Worker's remote TUI interrupts its turn without starting
//! a replacement one (#1782), so the Planner's terminal input and claim on that card are
//! refused before a byte reaches the PTY. A Claude task Worker keeps accepting input.
use crate::task_terminal::{snapshot, stop, worker_running};
use crate::terminal_support::Harness;
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, Instant};

const REFUSAL: &str = include_str!("../../prompts/terminal/codex-task-worker-input-refused.md");

/// A viewer that records every byte it receives on stdin, unechoed and unbuffered.
fn stdin_recorder(log: &Path) -> String {
    format!(
        "stty raw -echo; printf 'WORKER_READY\\r\\n'; exec cat > '{}'",
        log.display()
    )
}

fn assert_refused(reply: &Value) {
    assert!(REFUSAL.starts_with("a running codex worker cannot be redirected yet"));
    let error = reply
        .get("error")
        .unwrap_or_else(|| panic!("codex task Worker input must be refused, got {reply}"));
    assert_eq!(error["code"], -32403, "{reply}");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains(REFUSAL),
        "{reply}"
    );
    assert_eq!(
        error["data"]["refusal"], "codex_task_worker_input",
        "{reply}"
    );
}

async fn wait_for_bytes(log: &Path, needle: &[u8]) {
    let start = Instant::now();
    loop {
        let bytes = std::fs::read(log).unwrap_or_default();
        if bytes.windows(needle.len()).any(|window| window == needle) {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "recorder never received {needle:?}; got {bytes:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn codex_task_worker_refuses_terminal_input_and_writes_no_bytes() {
    let h = Harness::start().await;
    let codex_log = h.root.path().join("codex-worker-stdin.bin");
    let claude_log = h.root.path().join("claude-worker-stdin.bin");
    let codex = worker_running(&h, "codex", &h.track, Some(&stdin_recorder(&codex_log))).await;
    let claude = worker_running(&h, "claude", &h.track, Some(&stdin_recorder(&claude_log))).await;
    h.observe_text(&codex.terminal, "WORKER_READY").await;
    h.observe_text(&claude.terminal, "WORKER_READY").await;

    // The #1782 redirect: Escape, then the new scope, each with a claim-if-unowned. The replies
    // are judged after the byte check, so a lost refusal shows up as bytes on the PTY.
    let before = snapshot(&h, json!({"task_id":codex.task})).await;
    let mut replies = Vec::new();
    for (request_id, action) in [
        ("interrupt", json!({"type":"key","key":"Escape"})),
        (
            "new-scope",
            json!({"type":"submit","text":"narrowed scope"}),
        ),
    ] {
        for target in [
            json!({"task_id":codex.task}),
            json!({"terminal_id":codex.terminal}),
        ] {
            let mut args = target;
            args["observation_id"] = before["observation_id"].clone();
            args["request_id"] = json!(request_id);
            args["claim"] = json!(true);
            args["action"] = action.clone();
            replies.push(h.call("calm.terminal.input", args).await);
        }
    }
    replies.push(
        h.call(
            "calm.terminal.control",
            json!({"task_id":codex.task,"action":"claim"}),
        )
        .await,
    );

    // Positive control: the same recorder does capture a Claude task Worker's input, and it
    // is written after the codex attempts, so their bytes would have landed by now.
    h.ok(
        "calm.terminal.control",
        json!({"task_id":claude.task,"action":"claim"}),
    )
    .await;
    let claude_before = snapshot(&h, json!({"task_id":claude.task})).await;
    let written = h
        .ok(
            "calm.terminal.input",
            json!({"task_id":claude.task,"observation_id":claude_before["observation_id"],
                "request_id":"probe","action":{"type":"text","text":"probe"}}),
        )
        .await;
    assert_eq!(written["outcome"], "written", "{written}");
    wait_for_bytes(&claude_log, b"probe").await;

    assert_eq!(
        std::fs::read(&codex_log).expect("codex recorder started"),
        Vec::<u8>::new(),
        "a refused codex Worker input must write zero bytes to its PTY"
    );
    for reply in &replies {
        assert_refused(reply);
    }

    // Resolve and observe stay read-only and succeed; resolve names the refusal up front.
    let resolved = h
        .ok("calm.terminal.resolve", json!({"task_id":codex.task}))
        .await;
    assert_eq!(resolved["available"], true, "{resolved}");
    assert_eq!(resolved["card_kind"], "codex", "{resolved}");
    assert_eq!(resolved["controllable"], false, "{resolved}");
    assert_eq!(resolved["input_refused"], REFUSAL, "{resolved}");
    assert!(!h.interaction().input_pending(&codex.terminal).await);
    stop(&h, &codex).await;
    stop(&h, &claude).await;
}
