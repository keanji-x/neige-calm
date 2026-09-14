//! #1677 S2 — `replace`: the server derives Left/Right, Backspace and text
//! from the live cursor row, in one ordered write, against a real readline
//! line editor (Python's `input()` with GNU readline under a UTF-8 locale)
//! through the real MCP tools, renderer and PTY.
use crate::terminal_support::{Harness, human_takeover};
use calm_server::terminal_interaction::{InputOptions, Target};
use serde_json::{Value, json};
use std::time::Duration;

/// A readline loop: prints a header row, then reads lines at a `> ` prompt
/// and echoes each as `GOT:<line>`. Python and readline follow LANG.
const READLINE: &str = "LANG=C.UTF-8 exec python3 -c \"import readline\nprint('HEADER 42')\nwhile True: print('GOT:' + input('> '))\"";

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
fn has_line(state: &Value, needle: &str) -> bool {
    rows(state).iter().any(|line| line.contains(needle))
}
/// The cursor row's text.
fn cursor_row(state: &Value) -> String {
    rows(state)[state["cursor"]["row"].as_u64().unwrap() as usize].clone()
}
fn replace(from: &str, to: &str) -> Value {
    json!({"type":"replace","from":from,"to":to})
}
fn edit(terminal: &str, request: &str, action: Value, extra: Value) -> Value {
    let mut args = json!({"terminal_id":terminal,"request_id":request,"action":action,
        "observe":true,"wait_for":"change","wait_ms":3000});
    for (key, value) in extra.as_object().unwrap() {
        args[key] = value.clone();
    }
    args
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
/// Open the readline loop, claim it and type `draft` at its prompt.
async fn readline_draft(h: &Harness, request: &str, draft: &str) -> (String, Value) {
    let terminal = open_claimed(h, READLINE, request).await;
    h.observe_text(&terminal, ">").await;
    let typed = h
        .call(
            "calm.terminal.input",
            edit(
                &terminal,
                "draft",
                json!({"type":"text","text":draft}),
                json!({}),
            ),
        )
        .await;
    assert_eq!(receipt(&typed)["outcome"], "written", "{typed}");
    let state = observation(&typed).clone();
    assert_eq!(cursor_row(&state), format!("> {draft}"), "{state}");
    (terminal, state)
}
async fn ack(h: &Harness, terminal: &str) -> u64 {
    h.interaction()
        .input_ack_sequence(terminal)
        .await
        .expect("the open established the Planner's client")
}

/// Cursor at the end of the draft: the plan moves Left, erases `11` and
/// inserts `19` in ONE write (the ack sequence advances by one), the receipt
/// carries the plan, the readback IS the preview, Enter is separate. A
/// replay returns the cached plan (`11` is gone from the row, so a
/// recomputation could not succeed) and writes nothing more.
#[tokio::test]
async fn replace_moves_left_from_the_end_and_the_receipt_carries_the_plan() {
    let h = Harness::start().await;
    let (terminal, draft) = readline_draft(&h, "replace-left", "7200 + 11 done").await;
    let row = draft["cursor"]["row"].as_u64().unwrap();
    assert_eq!(draft["cursor"]["column"], 16, "{draft}");
    let before = ack(&h, &terminal).await;
    let edited = h
        .call(
            "calm.terminal.input",
            edit(&terminal, "fix", replace("11", "19"), json!({})),
        )
        .await;
    let written = receipt(&edited);
    assert_eq!(written["outcome"], "written", "{edited}");
    assert_eq!(written["application_result"], "unverified");
    assert_eq!(
        written["replace"],
        json!({"row":row,"cursor_index":16,"cursor_visible":true,"moves":{"key":"Left","repeat":5},"erased":2,"inserted":"19"}),
        "{written}"
    );
    assert!(written.get("steps").is_none());
    assert_eq!(written["summary"]["action"], "written");
    let preview = observation(&edited);
    assert_eq!(cursor_row(preview), "> 7200 + 19 done", "{preview}");
    assert_eq!(ack(&h, &terminal).await, before + 1, "one write, one ack");
    // Replay: the same receipt with the cached plan, no second write.
    let replay = h
        .call(
            "calm.terminal.input",
            edit(
                &terminal,
                "fix",
                replace("11", "19"),
                json!({"wait_for":"elapsed","wait_ms":100}),
            ),
        )
        .await;
    assert_eq!(receipt(&replay)["outcome"], "written", "{replay}");
    assert_eq!(receipt(&replay)["replace"], written["replace"]);
    assert_eq!(ack(&h, &terminal).await, before + 1);
    assert_eq!(cursor_row(observation(&replay)), "> 7200 + 19 done");
    // The action is part of the fingerprint.
    let conflicting = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"fix","action":replace("11", "18")}),
        )
        .await;
    assert!(
        error_text(&conflicting).contains("reused with different arguments"),
        "{conflicting}"
    );
    let submitted = h
        .call(
            "calm.terminal.input",
            edit(
                &terminal,
                "enter",
                json!({"type":"key","key":"Enter"}),
                json!({}),
            ),
        )
        .await;
    assert!(
        has_line(observation(&submitted), "GOT:7200 + 19 done"),
        "{submitted}"
    );
    h.stop(&terminal).await;
}

/// Wide glyphs count as one character: with `松果` between `11` and the
/// cursor the plan moves Left 3 (not 5 columns); from Home the plan moves
/// Right to the end of `松果` and replaces it; the printed line after a
/// separate Enter proves both edits.
#[tokio::test]
async fn replace_counts_cjk_characters_and_moves_right_from_home() {
    let h = Harness::start().await;
    let (terminal, draft) = readline_draft(&h, "replace-cjk", "11 松果").await;
    assert_eq!(draft["cursor"]["column"], 9, "7 columns after the prompt");
    let left = h
        .call(
            "calm.terminal.input",
            edit(&terminal, "digits", replace("11", "19"), json!({})),
        )
        .await;
    assert_eq!(receipt(&left)["outcome"], "written", "{left}");
    assert_eq!(
        receipt(&left)["replace"]["moves"],
        json!({"key":"Left","repeat":3}),
        "{left}"
    );
    assert_eq!(receipt(&left)["replace"]["cursor_index"], 7);
    assert_eq!(cursor_row(observation(&left)), "> 19 松果", "{left}");
    let home = h
        .call(
            "calm.terminal.input",
            edit(
                &terminal,
                "home",
                json!({"type":"key","key":"Home"}),
                json!({}),
            ),
        )
        .await;
    assert_eq!(receipt(&home)["outcome"], "written", "{home}");
    assert_eq!(observation(&home)["cursor"]["column"], 2, "{home}");
    let right = h
        .call(
            "calm.terminal.input",
            edit(&terminal, "fruit", replace("松果", "苹果"), json!({})),
        )
        .await;
    assert_eq!(receipt(&right)["outcome"], "written", "{right}");
    assert_eq!(
        receipt(&right)["replace"],
        json!({"row":draft["cursor"]["row"],"cursor_index":2,"cursor_visible":true,
            "moves":{"key":"Right","repeat":5},"erased":2,"inserted":"苹果"}),
        "{right}"
    );
    assert_eq!(cursor_row(observation(&right)), "> 19 苹果", "{right}");
    let submitted = h
        .call(
            "calm.terminal.input",
            edit(
                &terminal,
                "enter",
                json!({"type":"key","key":"Enter"}),
                json!({}),
            ),
        )
        .await;
    assert!(
        has_line(observation(&submitted), "GOT:19 苹果"),
        "{submitted}"
    );
    h.stop(&terminal).await;
}

/// Every refusal is an RPC error before any reservation or write: absent,
/// ambiguous (overlapping occurrences included), on another row, control
/// characters and shape; the request_id stays free.
#[tokio::test]
async fn replace_refusals_are_rpc_errors_before_any_write() {
    let h = Harness::start().await;
    let (terminal, _draft) = readline_draft(&h, "replace-refused", "11 + 11 aaa").await;
    let before = ack(&h, &terminal).await;
    for (action, expected) in [
        (
            replace("11", "19"),
            "\"11\" occurs 2 times on the cursor row",
        ),
        (
            replace("aa", "b"),
            "\"aa\" occurs 2 times on the cursor row",
        ),
        (replace("zz", "19"), "\"zz\" is not on the cursor row"),
        (replace("42", "43"), "\"42\" is not on the cursor row"),
        (
            replace("a\rb", "x"),
            "replace from must be 1..200 bytes of printable text",
        ),
        (replace("", "x"), "replace from must be 1..200 bytes"),
        (replace("11", "x\n"), "replace to must be printable text"),
        (
            json!({"type":"replace","from":"11"}),
            "replace action accepts only type/from/to",
        ),
        (
            json!({"type":"replace","from":"11","to":"x","repeat":2}),
            "replace action accepts only type/from/to",
        ),
        (
            json!({"type":"sequence","steps":[replace("11", "x"), {"type":"text","text":"y"}]}),
            "sequence steps must be text or key actions",
        ),
    ] {
        let response = h
            .call(
                "calm.terminal.input",
                json!({"terminal_id":terminal,"request_id":"bad","action":action}),
            )
            .await;
        assert_eq!(response["error"]["code"], -32403, "{action}: {response}");
        assert!(
            error_text(&response).contains(expected),
            "{action}: {response}"
        );
        assert!(!h.interaction().input_pending(&terminal).await, "{action}");
    }
    assert_eq!(ack(&h, &terminal).await, before, "nothing was written");
    // Nothing was cached: the request_id is free for a valid replace.
    let ok = h
        .call(
            "calm.terminal.input",
            edit(&terminal, "bad", replace("aaa", "bbb"), json!({})),
        )
        .await;
    assert_eq!(receipt(&ok)["outcome"], "written", "{ok}");
    assert_eq!(cursor_row(observation(&ok)), "> 11 + 11 bbb");
    assert_eq!(ack(&h, &terminal).await, before + 1);
    h.stop(&terminal).await;
}

/// A hidden cursor is positioned all the same (Claude Code keeps DECTCEM
/// off in its draft box while moving the cursor to the edit point): the
/// plan uses the position, the receipt reports `cursor_visible: false`,
/// and readline applies the edit.
#[tokio::test]
async fn replace_with_a_hidden_cursor_uses_its_position() {
    let h = Harness::start().await;
    let hidden = format!("printf '\\033[?25l'; {READLINE}");
    let terminal = open_claimed(&h, &hidden, "replace-hidden").await;
    let view = h.observe_text(&terminal, ">").await;
    assert_eq!(view["cursor"]["visible"], false, "{view}");
    let typed = h
        .call(
            "calm.terminal.input",
            edit(
                &terminal,
                "draft",
                json!({"type":"text","text":"7200 + 11 done"}),
                json!({}),
            ),
        )
        .await;
    assert_eq!(receipt(&typed)["outcome"], "written", "{typed}");
    let draft = observation(&typed);
    assert_eq!(cursor_row(draft), "> 7200 + 11 done", "{draft}");
    assert_eq!(draft["cursor"]["visible"], false, "{draft}");
    assert_eq!(draft["cursor"]["column"], 16, "{draft}");
    let edited = h
        .call(
            "calm.terminal.input",
            edit(&terminal, "fix", replace("11", "19"), json!({})),
        )
        .await;
    let written = receipt(&edited);
    assert_eq!(written["outcome"], "written", "{edited}");
    assert_eq!(
        written["replace"],
        json!({"row":draft["cursor"]["row"],"cursor_index":16,"cursor_visible":false,
            "moves":{"key":"Left","repeat":5},"erased":2,"inserted":"19"}),
        "{written}"
    );
    assert_eq!(cursor_row(observation(&edited)), "> 7200 + 19 done");
    let submitted = h
        .call(
            "calm.terminal.input",
            edit(
                &terminal,
                "enter",
                json!({"type":"key","key":"Enter"}),
                json!({}),
            ),
        )
        .await;
    assert!(
        has_line(observation(&submitted), "GOT:7200 + 19 done"),
        "{submitted}"
    );
    h.stop(&terminal).await;
}

/// The cursor facts: a cursor inside a wide cell and a match that cuts
/// through a combining sequence are refused on the live frame; nothing is
/// written.
#[tokio::test]
async fn replace_refuses_wide_cell_cursor_and_unaligned_matches() {
    let h = Harness::start().await;
    let mut last = String::new();
    for (request, program, needle, from, expected) in [
        (
            "wide",
            "printf '> 松果\\033[4G'; cat >/dev/null",
            "松果",
            "果",
            "the cursor is inside a wide cell",
        ),
        (
            "combining",
            "printf 'cafe\\314\\201 au lait'; cat >/dev/null",
            "au lait",
            "cafe",
            "cuts through a cell",
        ),
    ] {
        let terminal = open_claimed(&h, program, request).await;
        let view = h.observe_text(&terminal, needle).await;
        let before = ack(&h, &terminal).await;
        let response = h
            .call(
                "calm.terminal.input",
                json!({"terminal_id":terminal,"observation_id":view["observation_id"],"request_id":request,"action":replace(from, "x")}),
            )
            .await;
        assert_eq!(response["error"]["code"], -32403, "{request}: {response}");
        assert!(
            error_text(&response).contains(expected),
            "{request}: {response}"
        );
        assert!(!h.interaction().input_pending(&terminal).await);
        assert_eq!(ack(&h, &terminal).await, before, "{request}");
        last = terminal;
    }
    h.stop(&last).await;
}

/// A refusal after a granted claim carries the claim note (an error has no
/// receipt), and the connection holds the control it claimed.
#[tokio::test]
async fn replace_refusal_after_a_granted_claim_carries_the_claim_note() {
    let h = Harness::start().await;
    let terminal = h
        .ok(
            "calm.terminal.open",
            json!({"program":READLINE,"request_id":"replace-claim"}),
        )
        .await["terminal_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let view = h.observe_text(&terminal, ">").await;
    assert_eq!(view["role"], "observer", "{view}");
    let response = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"absent","action":replace("zz", "x"),"claim":true}),
        )
        .await;
    assert_eq!(response["error"]["code"], -32403, "{response}");
    let message = error_text(&response);
    assert!(
        message.contains("\"zz\" is not on the cursor row"),
        "{message}"
    );
    assert!(
        message.contains("; control claimed (control_id "),
        "{message}"
    );
    assert!(!h.interaction().input_pending(&terminal).await);
    let held = h
        .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
        .await;
    assert_eq!(held["role"], "owner", "{held}");
    // A human takeover then leaves replace behind the same control fence as
    // every action: the observation's control is no longer held.
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    let (pump, _incoming) = human_takeover(&entry, &terminal, uuid::Uuid::new_v4()).await;
    let start = std::time::Instant::now();
    loop {
        let view = h
            .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
            .await;
        if view["role"] == "observer" {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the takeover never reached the Planner's connection: {view}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let stale_control = h
        .interaction()
        .input(
            &h.identity(),
            &Target::Terminal(terminal.clone()),
            Some(uuid::Uuid::parse_str(held["observation_id"].as_str().unwrap()).unwrap()),
            "taken",
            replace("HEADER", "x"),
            InputOptions::default(),
            None,
        )
        .await;
    let message = stale_control
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(
        message.contains("terminal control changed; observe before input"),
        "{message}"
    );
    pump.abort();
    h.stop(&terminal).await;
}

/// `allow_output_below_cursor` admits a replace: a hint line refreshing
/// below the cursor is not stale for it, the write happens with the
/// tolerance and the plan on the receipt.
#[tokio::test]
async fn replace_is_admitted_by_the_below_cursor_tolerance() {
    let h = Harness::start().await;
    // A title row, an input row and a hint row two below the cursor that a
    // background loop repaints every 100 ms (same box as the #1666 tests).
    let terminal = open_claimed(
        &h,
        "printf 'Title line\\nType here: '; ( i=0; while :; do i=$((i+1)); printf '\\0337\\033[4;1Hhint %s\\0338' $i; sleep 0.1; done ) & cat >/dev/null",
        "replace-hint",
    )
    .await;
    h.observe_text(&terminal, "Type here:").await;
    let typed = h
        .call(
            "calm.terminal.input",
            edit(
                &terminal,
                "type",
                json!({"type":"text","text":"abc"}),
                json!({"allow_output_below_cursor":true}),
            ),
        )
        .await;
    assert_eq!(receipt(&typed)["outcome"], "written", "{typed}");
    let view = observation(&typed).clone();
    assert!(has_line(&view, "Type here: abc"), "{view}");
    // Let the hint line move past the readback's revision.
    let start = std::time::Instant::now();
    loop {
        let live = h
            .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
            .await;
        if live["observation_revision"] != view["observation_revision"] {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the hint never moved"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let refused = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"observation_id":view["observation_id"],"request_id":"swap","action":replace("abc", "xyz")}),
        )
        .await;
    let stale = receipt(&refused);
    assert_eq!(stale["outcome"], "stale_observation", "{stale}");
    assert!(
        stale.get("replace").is_none(),
        "no lookup on a stale observation: {stale}"
    );
    assert_eq!(stale["screen_diff"]["rows_changed_at_or_above_cursor"], 0);
    let before = ack(&h, &terminal).await;
    let admitted = h
        .call(
            "calm.terminal.input",
            edit(
                &terminal,
                "swap",
                replace("abc", "xyz"),
                json!({"observation_id":view["observation_id"],"allow_output_below_cursor":true}),
            ),
        )
        .await;
    let written = receipt(&admitted);
    assert_eq!(written["outcome"], "written", "{written}");
    assert_eq!(
        written["observation_drift"]["tolerance"], "below_cursor",
        "{written}"
    );
    assert_eq!(
        written["replace"],
        json!({"row":1,"cursor_index":14,"cursor_visible":true,"moves":null,"erased":3,"inserted":"xyz"}),
        "{written}"
    );
    assert_eq!(ack(&h, &terminal).await, before + 1);
    assert!(
        has_line(observation(&admitted), "Type here: xyz"),
        "{admitted}"
    );
    h.stop(&terminal).await;
}
