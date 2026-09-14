//! #1666 S1 — `wait_for=text`: wait until the live viewport shows a target,
//! through the real MCP tools, renderer and PTY. Exact timing is covered by
//! the paused-clock tests in `terminal_interaction/text_wait.rs`; here the
//! budget is an upper bound and the settle window a lower bound.
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
fn rows(state: &Value) -> Vec<String> {
    state["text"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line.as_str().unwrap().to_owned())
        .collect()
}
fn revision(state: &Value) -> u64 {
    state["observation_revision"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}
async fn open(h: &Harness, program: &str, request: &str) -> String {
    h.ok(
        "calm.terminal.open",
        json!({"program":program,"request_id":request}),
    )
    .await["terminal_id"]
        .as_str()
        .unwrap()
        .to_owned()
}
/// Run `call` and write `marker` into the workspace only once the call's
/// wait has subscribed to the projection (same device as the change-wait
/// tests), so the program's output lands after the wait began.
async fn call_then_release(
    h: &Harness,
    terminal: &str,
    marker: &str,
    call: impl std::future::Future<Output = Value>,
) -> Value {
    let entry = h.state.terminal_renderer.get(terminal).unwrap();
    let waiters = || entry.handle.model_view.lock().unwrap().change_waiters();
    let subscribed = waiters();
    let release = async {
        let start = std::time::Instant::now();
        while waiters() == subscribed {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "the text wait never subscribed"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        std::fs::write(h.root.path().join(marker), b"x").unwrap();
    };
    let (response, ()) = tokio::join!(call, release);
    response
}
fn text_wait(terminal: &str, patterns: Value, extra: Value) -> Value {
    let mut args = json!({"terminal_id":terminal,"wait_for":"text","wait_text":patterns});
    for (key, value) in extra.as_object().unwrap() {
        args[key] = value.clone();
    }
    args
}

/// The target appears on a later revision (a trust dialog painted 300 ms
/// after the marker): one observe returns it, settled, naming the pattern,
/// the row and the revision it was confirmed on.
#[tokio::test]
async fn text_wait_matches_a_later_screen_and_settles() {
    let h = Harness::start().await;
    let terminal = open(
        &h,
        "printf 'booting\\n'; while [ ! -e go ]; do sleep 0.02; done; sleep 0.3; printf 'Do you trust the files in this folder?\\n\\n  1. Yes, proceed\\n'; cat >/dev/null",
        "text-later",
    )
    .await;
    h.observe_text(&terminal, "booting").await;
    let response = call_then_release(
        &h,
        &terminal,
        "go",
        h.call(
            "calm.terminal.observe",
            text_wait(
                &terminal,
                json!(["trust the files", "❯"]),
                json!({"wait_ms":5000}),
            ),
        ),
    )
    .await;
    let view = receipt(&response);
    assert_eq!(view["wait"]["mode"], "text", "{view}");
    assert_eq!(view["wait"]["outcome"], "matched", "{view}");
    assert_eq!(view["wait"]["settled"], true, "{view}");
    let waited = view["wait"]["waited_ms"].as_u64().unwrap();
    assert!((150..5000).contains(&waited), "waited_ms={waited}: {view}");
    let matched = &view["wait"]["text"];
    assert_eq!(matched["pattern"], "trust the files", "{view}");
    assert_eq!(matched["already"], false, "{view}");
    let row = matched["row"].as_u64().unwrap() as usize;
    assert!(
        rows(view)[row].contains("trust the files"),
        "row {row} of {view}"
    );
    let confirmed: u64 = matched["revision"].as_str().unwrap().parse().unwrap();
    let baseline: u64 = view["wait"]["baseline_revision"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        baseline < confirmed && confirmed <= revision(view),
        "baseline {baseline} < confirmed {confirmed} <= observed {}: {view}",
        revision(view)
    );
    assert_eq!(view["changed_since_previous_observation"], true);
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(" wait matched; full state in structuredContent")
    );
    // No pattern on the screen: unmatched at the budget, `text` null.
    let unmatched = h
        .ok(
            "calm.terminal.observe",
            text_wait(&terminal, json!(["never painted"]), json!({"wait_ms":300})),
        )
        .await;
    assert_eq!(unmatched["wait"]["outcome"], "unmatched", "{unmatched}");
    assert_eq!(unmatched["wait"]["settled"], false);
    assert_eq!(unmatched["wait"]["text"], Value::Null);
    assert!(unmatched["wait"]["waited_ms"].as_u64().unwrap() >= 300);
    h.stop(&terminal).await;
}

/// "Until the screen shows X": a screen that already matches returns once
/// it has been quiet for `settle_ms`, with `already: true`; tie-breaking is
/// the first pattern in argument order, then the first row top-down.
#[tokio::test]
async fn text_wait_on_an_already_matching_screen_returns_after_settle() {
    let h = Harness::start().await;
    let terminal = open(
        &h,
        "printf 'alpha\\nbeta\\nalpha again\\n'; cat >/dev/null",
        "text-already",
    )
    .await;
    h.observe_text(&terminal, "alpha again").await;
    let view = h
        .ok(
            "calm.terminal.observe",
            text_wait(
                &terminal,
                json!(["beta", "alpha"]),
                json!({"wait_ms":5000,"settle_ms":300}),
            ),
        )
        .await;
    assert_eq!(view["wait"]["outcome"], "matched", "{view}");
    assert_eq!(view["wait"]["settled"], true);
    assert_eq!(
        view["wait"]["text"],
        json!({"pattern":"beta","row":1,"revision":view["observation_revision"],"already":true}),
        "{view}"
    );
    let waited = view["wait"]["waited_ms"].as_u64().unwrap();
    assert!((300..5000).contains(&waited), "waited_ms={waited}");
    assert_eq!(view["changed_since_previous_observation"], false);
    let first_row = h
        .ok(
            "calm.terminal.observe",
            text_wait(
                &terminal,
                json!(["alpha"]),
                json!({"wait_ms":5000,"settle_ms":0}),
            ),
        )
        .await;
    assert_eq!(first_row["wait"]["text"]["row"], 0, "{first_row}");
    assert_eq!(first_row["wait"]["text"]["already"], true);
    h.stop(&terminal).await;
}

/// A match that vanishes before the quiet window ends does not end the
/// wait: READY is painted, cleared 50 ms later, and painted again 600 ms
/// after that; the observe returns the second screen with `already: false`.
#[tokio::test]
async fn text_wait_ignores_a_match_that_vanishes_before_the_quiet_window() {
    let h = Harness::start().await;
    let terminal = open(
        &h,
        "printf 'booting\\n'; while [ ! -e go ]; do sleep 0.02; done; printf 'READY\\n'; sleep 0.05; printf '\\033[2J\\033[Hgone\\n'; sleep 0.6; printf 'READY again\\n'; cat >/dev/null",
        "text-vanish",
    )
    .await;
    h.observe_text(&terminal, "booting").await;
    let response = call_then_release(
        &h,
        &terminal,
        "go",
        h.call(
            "calm.terminal.observe",
            text_wait(
                &terminal,
                json!(["READY"]),
                json!({"wait_ms":5000,"settle_ms":300}),
            ),
        ),
    )
    .await;
    let view = receipt(&response);
    assert_eq!(view["wait"]["outcome"], "matched", "{view}");
    assert_eq!(view["wait"]["settled"], true);
    assert_eq!(view["wait"]["text"]["already"], false);
    assert_eq!(rows(view)[0], "gone", "the first READY was cleared: {view}");
    assert_eq!(view["wait"]["text"]["row"], 1, "{view}");
    assert_eq!(rows(view)[1], "READY again");
    let waited = view["wait"]["waited_ms"].as_u64().unwrap();
    assert!((900..5000).contains(&waited), "waited_ms={waited}: {view}");
    h.stop(&terminal).await;
}

/// Exit ends a text wait before its budget, as in change mode.
#[tokio::test]
async fn text_wait_reports_process_exit() {
    let h = Harness::start().await;
    let terminal = open(&h, "sleep 0.3; exit 0", "text-exit").await;
    let view = h
        .ok(
            "calm.terminal.observe",
            text_wait(&terminal, json!(["never"]), json!({"wait_ms":5000})),
        )
        .await;
    assert_eq!(view["wait"]["outcome"], "exited", "{view}");
    assert_eq!(view["wait"]["settled"], false);
    assert_eq!(view["wait"]["text"], Value::Null);
    assert!(view["wait"]["waited_ms"].as_u64().unwrap() < 5000, "{view}");
    h.stop(&terminal).await;
}

/// The argument contract on every carrier: `wait_text` and `wait_for=text`
/// require each other, patterns are bounded, a history view is refused,
/// action readbacks need `observe=true`; and a readback in text mode works
/// on input and control.
#[tokio::test]
async fn text_wait_validation_and_readbacks_on_every_carrier() {
    let h = Harness::start().await;
    let terminal = open(&h, "i=0; printf 'READY\\n'; while IFS= read -r line; do i=$((i+1)); printf 'COUNT:%s:%s\\n' \"$i\" \"$line\"; done", "text-carriers").await;
    h.observe_text(&terminal, "READY").await;
    let long = "y".repeat(201);
    for (args, expected) in [
        (
            json!({"wait_text":["x"]}),
            "wait_text requires wait_for=text",
        ),
        (
            json!({"wait_for":"text"}),
            "wait_for=text requires wait_text",
        ),
        (
            json!({"wait_for":"text","wait_text":[]}),
            "wait_text must list 1..8 patterns",
        ),
        (
            json!({"wait_for":"text","wait_text":["x","x","x","x","x","x","x","x","x"]}),
            "wait_text must list 1..8 patterns",
        ),
        (
            json!({"wait_for":"text","wait_text":[""]}),
            "1..200 bytes of printable text",
        ),
        (
            json!({"wait_for":"text","wait_text":[long]}),
            "1..200 bytes of printable text",
        ),
        (
            json!({"wait_for":"text","wait_text":["a\tb"]}),
            "1..200 bytes of printable text",
        ),
        (
            json!({"wait_for":"text","wait_text":["x"],"signal_events":["stop"]}),
            "signal_events requires wait_for=signal",
        ),
        (
            json!({"wait_for":"text","wait_text":["x"],"repaint_ms":10}),
            "repaint_ms requires wait_for=signal",
        ),
        (
            json!({"wait_for":"text","wait_text":["x"],"scroll_offset":1}),
            "scroll_offset must be 0",
        ),
        (
            json!({"wait_for":"change","wait_text":["x"]}),
            "wait_text requires wait_for=text",
        ),
    ] {
        let mut observe = args.clone();
        observe["terminal_id"] = json!(terminal);
        let response = h.call("calm.terminal.observe", observe).await;
        assert_eq!(response["error"]["code"], -32602, "{args}: {response}");
        assert!(
            error_text(&response).contains(expected),
            "{args}: {response}"
        );
        if args.get("scroll_offset").is_some() {
            continue;
        }
        let mut control = args.clone();
        control["terminal_id"] = json!(terminal);
        control["action"] = json!("claim");
        control["observe"] = json!(true);
        let response = h.call("calm.terminal.control", control).await;
        assert_eq!(
            response["error"]["code"], -32602,
            "control {args}: {response}"
        );
        assert!(
            error_text(&response).contains(expected),
            "control {args}: {response}"
        );
        let mut input = args.clone();
        input["terminal_id"] = json!(terminal);
        input["request_id"] = json!("invalid");
        input["action"] = json!({"type":"text","text":"BAD"});
        input["observe"] = json!(true);
        let response = h.call("calm.terminal.input", input).await;
        assert_eq!(
            response["error"]["code"], -32602,
            "input {args}: {response}"
        );
        assert!(
            error_text(&response).contains(expected),
            "input {args}: {response}"
        );
    }
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    assert!(
        entry
            .handle
            .owner_registry
            .lock()
            .unwrap()
            .current_owner()
            .is_none(),
        "no control action ran"
    );
    // Wait arguments without observe=true on action carriers.
    let no_observe = h
        .call(
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"claim","wait_for":"text","wait_text":["x"]}),
        )
        .await;
    assert_eq!(no_observe["error"]["code"], -32602, "{no_observe}");
    assert!(error_text(&no_observe).contains("wait_text require observe=true"));
    // Readbacks in text mode: claim, then a submit whose readback waits for
    // the program's echo line.
    let claimed = h
        .call(
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"claim","observe":true,"wait_for":"text","wait_text":["READY"],"settle_ms":0}),
        )
        .await;
    assert_eq!(
        observation(&claimed)["wait"]["outcome"],
        "matched",
        "{claimed}"
    );
    assert_eq!(observation(&claimed)["wait"]["text"]["already"], true);
    assert_eq!(observation(&claimed)["role"], "owner");
    let sent = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"line-1","action":{"type":"submit","text":"hello"},"observe":true,"wait_for":"text","wait_text":["COUNT:1:hello"],"wait_ms":5000}),
        )
        .await;
    assert_eq!(receipt(&sent)["outcome"], "written", "{sent}");
    let state = observation(&sent);
    assert_eq!(state["wait"]["outcome"], "matched", "{state}");
    assert_eq!(state["wait"]["text"]["pattern"], "COUNT:1:hello");
    // `already` depends on whether the echo landed before the readback's
    // first capture; either is legal, both are reported.
    assert!(state["wait"]["text"]["already"].is_boolean(), "{state}");
    assert!(state["wait"]["waited_ms"].as_u64().unwrap() < 5000);
    assert!(
        rows(state).iter().any(|row| row == "COUNT:1:hello"),
        "{state}"
    );
    h.stop(&terminal).await;
}
