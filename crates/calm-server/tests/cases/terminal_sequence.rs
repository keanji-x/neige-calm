//! #1666 S2 — `sequence`: a bounded edit in ONE physical write, through the
//! real MCP tools, renderer and PTY.
use crate::terminal_support::Harness;
use serde_json::{Value, json};
use std::time::Duration;

fn receipt(response: &Value) -> &Value {
    assert!(response.get("error").is_none(), "{response}");
    &response["result"]["structuredContent"]
}
fn observation(response: &Value) -> &Value {
    let result = receipt(response);
    assert_eq!(result["observation"]["status"], "available", "{result}");
    &result["observation"]["state"]
}
fn error_text(response: &Value) -> String {
    response["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("expected an error: {response}"))
        .to_owned()
}
fn has_line(state: &Value, needle: &str) -> bool {
    state["text"]
        .as_array()
        .unwrap()
        .iter()
        .any(|line| line.as_str().unwrap().contains(needle))
}
fn revision(state: &Value) -> u64 {
    state["observation_revision"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}
fn live_revision(h: &Harness, terminal: &str) -> u64 {
    h.state
        .terminal_renderer
        .get(terminal)
        .unwrap()
        .handle
        .model_view
        .lock()
        .unwrap()
        .capture(0)
        .unwrap()
        .1
}
async fn wait_past(h: &Harness, terminal: &str, after: u64) -> u64 {
    let start = std::time::Instant::now();
    loop {
        let now = live_revision(h, terminal);
        if now > after {
            return now;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "no output landed"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
async fn open_claimed(h: &Harness, program: &str, request: &str) -> String {
    let opened = h
        .ok(
            "calm.terminal.open",
            json!({"program":program,"request_id":request,"claim":true}),
        )
        .await;
    assert_eq!(opened["claim"]["status"], "claimed", "{opened}");
    opened["terminal_id"].as_str().unwrap().to_owned()
}
fn edit() -> Value {
    json!({"type":"sequence","steps":[
        {"type":"text","text":"7200 + 19"},
        {"type":"key","key":"Left","repeat":5},
        {"type":"key","key":"Backspace"},
        {"type":"text","text":"9"}]})
}

/// One sequence is ONE request: the connection's acknowledged input
/// sequence advances by exactly one for four steps, the PTY receives the
/// concatenated bytes in order (`cat -v` in raw mode shows them), and a
/// replay writes nothing more.
#[tokio::test]
async fn sequence_is_one_write_with_the_concatenated_bytes_in_order() {
    let h = Harness::start().await;
    // Raw mode without echo: the only bytes on the screen are what cat -v
    // prints for what it received (ESC as ^[, DEL as ^?).
    let terminal = open_claimed(&h, "stty raw -echo; exec cat -v", "sequence-bytes").await;
    let before = h
        .interaction()
        .input_ack_sequence(&terminal)
        .await
        .expect("the open established the Planner's client");
    let sent = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"edit-1","action":edit(),"observe":true,"wait_for":"change","wait_ms":3000}),
        )
        .await;
    let written = receipt(&sent);
    assert_eq!(written["outcome"], "written", "{sent}");
    assert_eq!(written["steps"], 4, "{written}");
    assert_eq!(written["application_result"], "unverified");
    assert_eq!(written["output_since_observation"], false);
    let state = observation(&sent);
    assert!(
        has_line(state, "7200 + 19^[[D^[[D^[[D^[[D^[[D^?9"),
        "the PTY received the steps in order in one write: {state}"
    );
    assert_eq!(
        h.interaction().input_ack_sequence(&terminal).await,
        Some(before + 1),
        "four steps, one acknowledged write"
    );
    // The replay returns the cached receipt with a fresh readback and no
    // second write; the action (its steps) is part of the fingerprint.
    let replay = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"edit-1","action":edit(),"observe":true,"wait_ms":200}),
        )
        .await;
    assert_eq!(receipt(&replay)["outcome"], "written");
    assert_eq!(receipt(&replay)["steps"], 4);
    assert_eq!(
        h.interaction().input_ack_sequence(&terminal).await,
        Some(before + 1)
    );
    let text = observation(&replay)["text"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|line| line.as_str().unwrap().contains("7200"))
        .count();
    assert_eq!(text, 1, "no second copy on the screen: {replay}");
    let mut other = edit();
    other["steps"][1]["repeat"] = json!(4);
    let conflicting = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"edit-1","action":other}),
        )
        .await;
    assert!(
        error_text(&conflicting).contains("reused with different arguments"),
        "{conflicting}"
    );
    h.stop(&terminal).await;
}

/// The bounded-edit shape end to end on a real line editor: one sequence
/// turns the draft `7200 + 19` into `7209 + 19`, the readback shows the
/// corrected draft, and Enter is a separate action.
#[tokio::test]
async fn sequence_corrects_one_digit_of_a_readline_draft_in_one_write() {
    let h = Harness::start().await;
    let terminal = open_claimed(&h, "exec /bin/bash --noprofile --norc", "sequence-readline").await;
    let sequence = json!({"type":"sequence","steps":[
        {"type":"text","text":"echo 7200 + 19"},
        {"type":"key","key":"Left","repeat":5},
        {"type":"key","key":"Backspace"},
        {"type":"text","text":"9"}]});
    let edited = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"draft","action":sequence,"observe":true,"wait_for":"change","wait_ms":3000}),
        )
        .await;
    assert_eq!(receipt(&edited)["outcome"], "written", "{edited}");
    assert_eq!(receipt(&edited)["steps"], 4);
    let draft = observation(&edited);
    assert!(
        has_line(draft, "echo 7209 + 19"),
        "corrected draft: {draft}"
    );
    assert!(!has_line(draft, "7200"), "{draft}");
    let submitted = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"enter","action":{"type":"key","key":"Enter"},"observe":true,"wait_for":"change","wait_ms":3000}),
        )
        .await;
    let out = observation(&submitted);
    assert!(
        out["text"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap() == "7209 + 19"),
        "{out}"
    );
    h.stop(&terminal).await;
}

/// Submission and control keys, submit, click, nesting, step count and size
/// are refused as invalid actions before any reservation or write.
#[tokio::test]
async fn sequence_rejects_submission_keys_and_shapes_before_any_write() {
    let h = Harness::start().await;
    let terminal = open_claimed(
        &h,
        "i=0; printf 'READY\\n'; while IFS= read -r line; do i=$((i+1)); printf x >> physical-lines; printf 'COUNT:%s:%s\\n' \"$i\" \"$line\"; done",
        "sequence-invalid",
    )
    .await;
    h.observe_text(&terminal, "READY").await;
    let before = h.interaction().input_ack_sequence(&terminal).await.unwrap();
    let text = json!({"type":"text","text":"x"});
    let steps = |list: Value| json!({"type":"sequence","steps":list});
    for (action, expected) in [
        (
            steps(json!([text, {"type":"key","key":"Enter"}])),
            "a sequence step may send only",
        ),
        (
            steps(json!([text, {"type":"key","key":"Ctrl+J"}])),
            "a sequence step may send only",
        ),
        (
            steps(json!([text, {"type":"key","key":"Escape"}])),
            "a sequence step may send only",
        ),
        (
            steps(json!([text, {"type":"key","key":"Ctrl+C"}])),
            "a sequence step may send only",
        ),
        (
            steps(json!([text, {"type":"submit","text":"x"}])),
            "sequence steps must be text or key actions",
        ),
        (
            steps(json!([text, {"type":"click","column":0,"row":0}])),
            "sequence steps must be text or key actions",
        ),
        (
            steps(json!([text, steps(json!([text, text]))])),
            "sequence steps must be text or key actions",
        ),
        (steps(json!([text])), "sequence must carry 2..8 steps"),
        (
            steps(json!([
                text, text, text, text, text, text, text, text, text
            ])),
            "sequence must carry 2..8 steps",
        ),
        (
            steps(json!([text, {"type":"text","text":"a\rb"}])),
            "nonempty printable text",
        ),
        (
            steps(
                json!([{"type":"text","text":"x".repeat(9000)},{"type":"text","text":"y".repeat(9000)}]),
            ),
            "sequence exceeds 16384 encoded bytes",
        ),
        (
            json!({"type":"sequence","steps":[text, text],"repeat":2}),
            "sequence action accepts only type/steps",
        ),
    ] {
        let response = h
            .call(
                "calm.terminal.input",
                json!({"terminal_id":terminal,"request_id":"bad","action":action}),
            )
            .await;
        assert!(
            error_text(&response).contains(expected),
            "{action}: {response}"
        );
        assert!(!h.interaction().input_pending(&terminal).await, "{action}");
    }
    assert_eq!(
        h.interaction().input_ack_sequence(&terminal).await,
        Some(before),
        "nothing was written"
    );
    assert!(!h.root.path().join("physical-lines").exists());
    // The request_id is free: nothing was cached for the refused shapes.
    let ok = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"bad","action":steps(json!([{"type":"text","text":"ab"},{"type":"key","key":"Backspace"},{"type":"text","text":"c"}])),"observe":true,"wait_for":"change","wait_ms":3000}),
        )
        .await;
    assert_eq!(receipt(&ok)["outcome"], "written", "{ok}");
    assert_eq!(receipt(&ok)["steps"], 3);
    let entered = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"enter","action":{"type":"key","key":"Enter"},"observe":true,"wait_for":"change","wait_ms":3000}),
        )
        .await;
    assert!(has_line(observation(&entered), "COUNT:1:ac"), "{entered}");
    assert_eq!(
        std::fs::read(h.root.path().join("physical-lines")).unwrap(),
        b"x"
    );
    h.stop(&terminal).await;
}

/// A sequence sits behind the same fences as every action: a moved
/// revision is a structured stale result (with `screen_diff`), nothing is
/// written or cached, and the advised resend writes the sequence once.
#[tokio::test]
async fn sequence_behind_a_stale_observation_is_refused_then_resent() {
    let h = Harness::start().await;
    let terminal = open_claimed(
        &h,
        "printf 'READY\\n'; while [ ! -e go ]; do sleep 0.02; done; printf 'STATUS_LINE\\n'; cat >/dev/null",
        "sequence-stale",
    )
    .await;
    let latest = h.observe_text(&terminal, "READY").await;
    std::fs::write(h.root.path().join("go"), b"x").unwrap();
    wait_past(&h, &terminal, revision(&latest)).await;
    let before = h.interaction().input_ack_sequence(&terminal).await.unwrap();
    let response = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"observation_id":latest["observation_id"],"request_id":"edit","action":edit()}),
        )
        .await;
    let stale = receipt(&response);
    assert_eq!(stale["outcome"], "stale_observation", "{stale}");
    assert!(stale.get("steps").is_none());
    assert!(
        stale["screen_diff"]["rows_changed_total"].as_u64().unwrap() >= 1,
        "{stale}"
    );
    assert_eq!(
        h.interaction().input_ack_sequence(&terminal).await,
        Some(before)
    );
    assert!(!h.interaction().input_pending(&terminal).await);
    let resent = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"edit","action":edit(),"allow_output_since_observation":true,"observe":true,"wait_ms":200}),
        )
        .await;
    assert_eq!(receipt(&resent)["outcome"], "written", "{resent}");
    assert_eq!(receipt(&resent)["steps"], 4);
    assert_eq!(
        h.interaction().input_ack_sequence(&terminal).await,
        Some(before + 1)
    );
    h.stop(&terminal).await;
}
