//! #1677 r16 — text conditions: `wait_text_absent` (and `wait_text`) gate
//! the settle of a signal wait and generalise the text wait, through the
//! real MCP tools, renderer, PTY and the production hook ingest route. The
//! fake Claude below does what the real one does in rounds 15/16: it posts
//! `Stop` while the busy hint (`esc to interrupt`) is still painted and
//! paints the answer only later.
use crate::terminal_support::Harness;
use serde_json::{Value, json};

/// Parses `--settings <file>` like the #1620 fake, echo off. Per stdin line:
/// paints a busy row, posts Stop, waits 800 ms, replaces the busy row with
/// `ANSWER:<line>` (the real Claude order: the hook fires before the final
/// paint); `hold:<x>` keeps the busy row for 4 s instead.
const FAKE_CLAUDE_BUSY: &str = r#"#!/bin/sh
settings=""
while [ $# -gt 0 ]; do case "$1" in --settings) settings="$2"; shift 2;; *) shift;; esac; done
hook=$(sed -n 's/^ *"command": "\(.*\)"[,]*$/\1/p' "$settings" | head -1)
stty -echo 2>/dev/null
stop() { printf '{"hook_event_name":"Stop","stop_hook_active":false,"session_id":"fake-session"}' | sh -c "$hook" >/dev/null 2>&1; }
printf 'READY %s\n' "$NEIGE_CARD_ID"
while IFS= read -r line; do
  printf 'busy: Noodling (esc to interrupt)\n'
  stop
  case "$line" in
    hold:*) sleep 4; printf '\033[1A\033[2KANSWER:%s\n' "${line#hold:}" ;;
    *) sleep 0.8; printf '\033[1A\033[2KANSWER:%s\n' "$line" ;;
  esac
done
"#;

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
async fn open_busy_claude(h: &Harness, request: &str) -> String {
    let script = h.root.path().join("fake-claude-busy.sh");
    std::fs::write(&script, FAKE_CLAUDE_BUSY).unwrap();
    let program = format!(
        "exec sh {} --settings \"$NEIGE_CLAUDE_SETTINGS\"",
        script.display()
    );
    let opened = h
        .ok(
            "calm.terminal.open",
            json!({"program":program,"request_id":request,"claim":true}),
        )
        .await;
    assert_eq!(opened["claim"]["status"], "claimed", "{opened}");
    let terminal = opened["terminal_id"].as_str().unwrap().to_owned();
    h.observe_text(&terminal, "READY").await;
    terminal
}
fn ask(terminal: &str, request: &str, text: &str, extra: Value) -> Value {
    let mut args = json!({"terminal_id":terminal,"request_id":request,"action":{"type":"submit","text":text},
        "observe":true,"wait_for":"signal","wait_ms":10000});
    for (key, value) in extra.as_object().unwrap() {
        args[key] = value.clone();
    }
    args
}

/// The Claude case: with `wait_text_absent ["esc to interrupt"]` the signal
/// readback returns once the busy row is gone and the answer painted
/// (`conditions.absent true`, `repaint settled`); without conditions it
/// returns at the spinner as before (#1628), the answer not yet painted.
#[tokio::test]
async fn absent_condition_holds_the_signal_readback_until_the_busy_hint_is_gone() {
    let h = Harness::start().await;
    let terminal = open_busy_claude(&h, "conditions-absent").await;
    let plain = h
        .call(
            "calm.terminal.input",
            ask(&terminal, "plain", "one", json!({})),
        )
        .await;
    let state = observation(&plain);
    assert_eq!(state["wait"]["outcome"], "signal", "{state}");
    assert!(
        matches!(
            state["wait"]["repaint"]["outcome"].as_str(),
            Some("already" | "settled")
        ),
        "{state}"
    );
    assert!(has_line(state, "esc to interrupt"), "{state}");
    assert!(
        !has_line(state, "ANSWER:one"),
        "returned at the spinner: {state}"
    );
    assert_eq!(
        state["wait"]["conditions"],
        json!({"present":null,"absent":null}),
        "{state}"
    );
    // Let the answer land before the next turn.
    h.observe_text(&terminal, "ANSWER:one").await;
    let gated = h
        .call(
            "calm.terminal.input",
            ask(
                &terminal,
                "gated",
                "two",
                json!({"wait_text_absent":["esc to interrupt"]}),
            ),
        )
        .await;
    let state = observation(&gated);
    assert_eq!(state["wait"]["outcome"], "signal", "{state}");
    assert_eq!(state["wait"]["repaint"]["outcome"], "settled", "{state}");
    assert_eq!(state["wait"]["settled"], true);
    assert_eq!(
        state["wait"]["conditions"],
        json!({"present":null,"absent":true}),
        "{state}"
    );
    assert!(!has_line(state, "esc to interrupt"), "{state}");
    assert!(has_line(state, "ANSWER:two"), "{state}");
    let repaint = state["wait"]["repaint"]["waited_ms"].as_u64().unwrap();
    assert!((800..10000).contains(&repaint), "{state}");
    assert_eq!(receipt(&gated)["summary"]["repaint"], "settled");
    // Both sides: the prompt row must show and the hint must be gone.
    h.observe_text(&terminal, "ANSWER:two").await;
    let both = h
        .call(
            "calm.terminal.input",
            ask(
                &terminal,
                "both",
                "three",
                json!({"wait_text":["ANSWER:three"],"wait_text_absent":["esc to interrupt"]}),
            ),
        )
        .await;
    let state = observation(&both);
    assert_eq!(state["wait"]["repaint"]["outcome"], "settled", "{state}");
    assert_eq!(
        state["wait"]["conditions"],
        json!({"present":true,"absent":true})
    );
    assert!(has_line(state, "ANSWER:three"));
    h.stop(&terminal).await;
}

/// The budget ends while the busy row is still there: `unsettled`,
/// `conditions.absent false`, the signal kept; the Planner observes again.
#[tokio::test]
async fn budget_with_the_hint_still_shown_is_unsettled() {
    let h = Harness::start().await;
    let terminal = open_busy_claude(&h, "conditions-unsettled").await;
    let held = h
        .call(
            "calm.terminal.input",
            ask(
                &terminal,
                "held",
                "hold:four",
                json!({"wait_text_absent":["esc to interrupt"],"wait_ms":1500}),
            ),
        )
        .await;
    let state = observation(&held);
    assert_eq!(state["wait"]["outcome"], "signal", "{state}");
    assert_eq!(state["wait"]["repaint"]["outcome"], "unsettled", "{state}");
    assert_eq!(state["wait"]["settled"], false);
    assert_eq!(
        state["wait"]["conditions"],
        json!({"present":null,"absent":false}),
        "{state}"
    );
    assert!(has_line(state, "esc to interrupt"), "{state}");
    assert!(state["wait"]["waited_ms"].as_u64().unwrap() >= 1500);
    assert_eq!(receipt(&held)["summary"]["repaint"], "unsettled");
    // A later text wait on the absence alone returns once the answer lands.
    let later = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_for":"text","wait_text_absent":["esc to interrupt"],"wait_ms":8000}),
        )
        .await;
    assert_eq!(later["wait"]["outcome"], "matched", "{later}");
    assert_eq!(later["wait"]["text"], Value::Null, "no present pattern");
    assert_eq!(
        later["wait"]["conditions"],
        json!({"present":null,"absent":true})
    );
    assert!(has_line(&later, "ANSWER:four"), "{later}");
    h.stop(&terminal).await;
}

/// The argument contract on every carrier: bounds, mode coupling
/// (`wait_text_absent` refused in change and elapsed, accepted in signal),
/// text mode needs at least one condition, a history view is refused with
/// conditions in signal mode too, and the wait arguments need observe=true.
#[tokio::test]
async fn text_condition_validation_on_every_carrier() {
    let h = Harness::start().await;
    let terminal = h
        .ok(
            "calm.terminal.open",
            json!({"program":"printf 'READY\\n'; cat >/dev/null","request_id":"conditions-validation"}),
        )
        .await["terminal_id"]
        .as_str()
        .unwrap()
        .to_owned();
    h.observe_text(&terminal, "READY").await;
    let long = "y".repeat(201);
    for (args, expected) in [
        (
            json!({"wait_text_absent":["x"]}),
            "wait_text_absent requires wait_for=text or wait_for=signal",
        ),
        (
            json!({"wait_for":"change","wait_text_absent":["x"]}),
            "wait_text_absent requires wait_for=text or wait_for=signal",
        ),
        (
            json!({"wait_for":"elapsed","wait_text":["x"]}),
            "wait_text requires wait_for=text or wait_for=signal",
        ),
        (
            json!({"wait_for":"text"}),
            "wait_for=text requires wait_text or wait_text_absent",
        ),
        (
            json!({"wait_for":"text","wait_text_absent":[]}),
            "wait_text must list 1..8 patterns",
        ),
        (
            json!({"wait_for":"signal","wait_text_absent":["x","x","x","x","x","x","x","x","x"]}),
            "wait_text must list 1..8 patterns",
        ),
        (
            json!({"wait_for":"signal","wait_text_absent":[long]}),
            "1..200 bytes of printable text",
        ),
        (
            json!({"wait_for":"signal","wait_text":["a\tb"]}),
            "1..200 bytes of printable text",
        ),
        (
            json!({"wait_for":"signal","repaint_ms":0,"wait_text_absent":["x"]}),
            "text conditions need a repaint window; repaint_ms must be > 0",
        ),
        (
            json!({"wait_for":"signal","repaint_ms":0,"wait_text":["x"]}),
            "repaint_ms must be > 0",
        ),
        (
            json!({"wait_for":"signal","wait_text_absent":["x"],"scroll_offset":1}),
            "scroll_offset must be 0",
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
        for (tool, extra) in [
            (
                "calm.terminal.control",
                json!({"action":"claim","observe":true}),
            ),
            (
                "calm.terminal.input",
                json!({"request_id":"invalid","action":{"type":"text","text":"BAD"},"observe":true}),
            ),
            ("calm.terminal.open", json!({"request_id":"invalid-open"})),
        ] {
            let mut call = args.clone();
            if tool != "calm.terminal.open" {
                call["terminal_id"] = json!(terminal);
            }
            for (key, value) in extra.as_object().unwrap() {
                call[key] = value.clone();
            }
            let response = h.call(tool, call).await;
            assert_eq!(
                response["error"]["code"], -32602,
                "{tool} {args}: {response}"
            );
            assert!(
                error_text(&response).contains(expected),
                "{tool} {args}: {response}"
            );
        }
    }
    let no_observe = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"x","action":{"type":"text","text":"x"},"wait_for":"signal","wait_text_absent":["x"]}),
        )
        .await;
    assert_eq!(no_observe["error"]["code"], -32602, "{no_observe}");
    assert!(error_text(&no_observe).contains("wait_text_absent need observe=true"));
    assert!(
        !h.interaction().input_pending(&terminal).await,
        "nothing was written"
    );
    // Accepted shapes: signal mode with each side, text mode with the
    // absent side alone (the pattern is not on the screen: matched at
    // once after the settle window).
    let absent = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_for":"text","wait_text_absent":["never painted"],"settle_ms":0}),
        )
        .await;
    assert_eq!(absent["wait"]["outcome"], "matched", "{absent}");
    assert_eq!(absent["wait"]["text"], Value::Null);
    assert_eq!(
        absent["wait"]["conditions"],
        json!({"present":null,"absent":true})
    );
    let present_still = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_for":"text","wait_text":["READY"],"wait_text_absent":["READY"],"wait_ms":200}),
        )
        .await;
    assert_eq!(
        present_still["wait"]["outcome"], "unmatched",
        "{present_still}"
    );
    assert_eq!(present_still["wait"]["text"]["pattern"], "READY");
    assert_eq!(
        present_still["wait"]["conditions"],
        json!({"present":true,"absent":false})
    );
    let signal = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_for":"signal","wait_ms":200,"wait_text":["READY"],"wait_text_absent":["x"]}),
        )
        .await;
    assert_eq!(signal["wait"]["outcome"], "no_signal", "{signal}");
    assert_eq!(
        signal["wait"]["conditions"],
        json!({"present":null,"absent":null}),
        "no signal: the repaint phase never tested the screen"
    );
    let opened = h
        .ok(
            "calm.terminal.open",
            json!({"request_id":"open-absent","program":"printf 'hello\\n'; cat >/dev/null",
                "wait_for":"text","wait_text_absent":["busy"],"settle_ms":0}),
        )
        .await;
    assert_eq!(opened["wait"]["outcome"], "matched", "{opened}");
    assert_eq!(opened["wait"]["conditions"]["absent"], true);
    h.state
        .terminal_renderer
        .drop_entry(opened["terminal_id"].as_str().unwrap())
        .await;
    h.stop(&terminal).await;
}
