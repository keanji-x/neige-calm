//! #1666 S4 — `allow_output_below_cursor`: a status-line refresh below the
//! input box is not a stale observation, everything else still is. Real MCP
//! tools, renderer and PTY; screen changes at or above the cursor are
//! injected through the render plane exactly as a program would paint them.
use crate::terminal_support::Harness;
use serde_json::{Value, json};
use std::time::Duration;

/// A title row, an input row (cursor stays there, echo on) and a hint row
/// two below the cursor that a background loop repaints every 100 ms with
/// DECSC/DECRC around the write.
const HINT_BOX: &str = "printf 'Title line\\nType here: '; ( i=0; while :; do i=$((i+1)); printf '\\0337\\033[4;1Hhint %s\\0338' $i; sleep 0.1; done ) & cat >/dev/null";
/// The same box without the hint loop: only injected output moves it.
const QUIET_BOX: &str = "printf 'Title line\\nType here: '; cat >/dev/null";

fn receipt(response: &Value) -> &Value {
    assert!(response.get("error").is_none(), "{response}");
    &response["result"]["structuredContent"]
}
fn observation(response: &Value) -> &Value {
    let result = receipt(response);
    assert_eq!(result["observation"]["status"], "available", "{result}");
    &result["observation"]["state"]
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
async fn wait_past(h: &Harness, terminal: &str, after: u64) {
    let start = std::time::Instant::now();
    while live_revision(h, terminal) <= after {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "no output landed"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
fn inject(h: &Harness, terminal: &str, bytes: &[u8]) {
    h.state
        .terminal_renderer
        .get(terminal)
        .unwrap()
        .handle
        .render_plane
        .lock()
        .unwrap()
        .on_pty_chunk(bytes.to_vec());
}
async fn open_claimed(h: &Harness, program: &str, request: &str) -> (String, Value) {
    let opened = h
        .ok(
            "calm.terminal.open",
            json!({"program":program,"request_id":request,"claim":true}),
        )
        .await;
    assert_eq!(opened["claim"]["status"], "claimed", "{opened}");
    let terminal = opened["terminal_id"].as_str().unwrap().to_owned();
    let view = h.observe_text(&terminal, "Type here:").await;
    assert_eq!(view["cursor"]["row"], 1, "{view}");
    assert_eq!(view["cursor"]["column"], 11, "{view}");
    assert_eq!(view["cursor"]["visible"], true, "{view}");
    (terminal, view)
}
fn typed(terminal: &str, request: &str, observation: &Value, extra: Value) -> Value {
    let mut args = json!({"terminal_id":terminal,"observation_id":observation["observation_id"],"request_id":request,
        "action":{"type":"text","text":"abc"},"observe":true,"wait_for":"change","wait_ms":2000});
    for (key, value) in extra.as_object().unwrap() {
        args[key] = value.clone();
    }
    args
}

/// The hint line keeps refreshing below the cursor: a plain input is a
/// stale result whose `screen_diff` shows only rows below the cursor
/// changed; the same request with `allow_output_below_cursor` writes and
/// the receipt names the tolerance and the rows it admitted.
#[tokio::test]
async fn hint_line_refresh_below_the_cursor_is_admitted_with_the_tolerance() {
    let h = Harness::start().await;
    let (terminal, view) = open_claimed(&h, HINT_BOX, "below-hint").await;
    wait_past(&h, &terminal, revision(&view)).await;
    let refused = h
        .call(
            "calm.terminal.input",
            typed(&terminal, "type", &view, json!({})),
        )
        .await;
    let stale = receipt(&refused);
    assert_eq!(stale["outcome"], "stale_observation", "{stale}");
    let diff = &stale["screen_diff"];
    assert_eq!(
        diff["compared"]["observed_revision"].as_u64().unwrap(),
        revision(&view)
    );
    assert!(diff["compared"]["current_revision"].as_u64().unwrap() > revision(&view));
    assert_eq!(
        diff["cursor"],
        json!({"moved":false,"visible":true}),
        "{diff}"
    );
    assert_eq!(diff["rows_changed_at_or_above_cursor"], 0, "{diff}");
    assert!(
        diff["rows_changed_below_cursor"].as_u64().unwrap() >= 1,
        "{diff}"
    );
    assert_eq!(
        diff["rows_changed_total"],
        diff["rows_changed_below_cursor"]
    );
    assert!(
        stale["next"]
            .as_str()
            .unwrap()
            .contains("allow_output_below_cursor=true")
    );
    assert!(!h.interaction().input_pending(&terminal).await);
    let fresh = observation(&refused).clone();
    assert!(!rows(&fresh)[1].contains("abc"), "nothing written: {fresh}");
    // Resend as advised on the fresh observation once the hint moved again.
    wait_past(&h, &terminal, revision(&fresh)).await;
    let admitted = h
        .call(
            "calm.terminal.input",
            typed(
                &terminal,
                "type",
                &fresh,
                json!({"allow_output_below_cursor":true}),
            ),
        )
        .await;
    let written = receipt(&admitted);
    assert_eq!(written["outcome"], "written", "{written}");
    assert_eq!(written["output_since_observation"], true);
    let drift = &written["observation_drift"];
    assert_eq!(
        drift["observed_revision"].as_u64().unwrap(),
        revision(&fresh)
    );
    assert!(drift["input_revision"].as_u64().unwrap() > revision(&fresh));
    assert_eq!(drift["tolerance"], "below_cursor", "{drift}");
    assert_eq!(
        drift["rows_changed_below_cursor"],
        json!([3]),
        "the hint row: {drift}"
    );
    assert_eq!(drift["rows_changed_total"], 1);
    assert_eq!(drift["truncated"], false);
    let after = observation(&admitted);
    assert_eq!(rows(after)[1], "Type here: abc", "{after}");
    // The tolerance is part of the fingerprint: the same request_id without
    // it conflicts.
    let toggled = h
        .call(
            "calm.terminal.input",
            typed(&terminal, "type", &fresh, json!({})),
        )
        .await;
    assert!(
        toggled["error"]["message"]
            .as_str()
            .unwrap()
            .contains("reused with different arguments"),
        "{toggled}"
    );
    h.stop(&terminal).await;
}

/// A cursor hidden in both captures (Claude Code keeps DECTCEM off in its
/// draft box while positioning the cursor at the edit point, #1677): the
/// hint refresh below it is stale without the flag, with `screen_diff`
/// reporting `visible: false`, and admitted with it on the position alone.
#[tokio::test]
async fn hidden_cursor_in_both_captures_is_admitted_on_its_position() {
    let h = Harness::start().await;
    let hidden = format!("printf '\\033[?25l'; {HINT_BOX}");
    let opened = h
        .ok(
            "calm.terminal.open",
            json!({"program":hidden,"request_id":"below-hidden","claim":true}),
        )
        .await;
    assert_eq!(opened["claim"]["status"], "claimed", "{opened}");
    let terminal = opened["terminal_id"].as_str().unwrap().to_owned();
    let view = h.observe_text(&terminal, "Type here:").await;
    assert_eq!(view["cursor"]["visible"], false, "{view}");
    assert_eq!(view["cursor"]["row"], 1, "{view}");
    assert_eq!(view["cursor"]["column"], 11, "{view}");
    wait_past(&h, &terminal, revision(&view)).await;
    let refused = h
        .call(
            "calm.terminal.input",
            typed(&terminal, "type", &view, json!({})),
        )
        .await;
    let stale = receipt(&refused);
    assert_eq!(stale["outcome"], "stale_observation", "{stale}");
    assert_eq!(
        stale["screen_diff"]["cursor"],
        json!({"moved":false,"visible":false}),
        "{stale}"
    );
    assert_eq!(stale["screen_diff"]["rows_changed_at_or_above_cursor"], 0);
    assert!(
        stale["screen_diff"]["rows_changed_below_cursor"]
            .as_u64()
            .unwrap()
            >= 1,
        "{stale}"
    );
    let before = h.interaction().input_ack_sequence(&terminal).await.unwrap();
    let admitted = h
        .call(
            "calm.terminal.input",
            typed(
                &terminal,
                "type",
                &view,
                json!({"allow_output_below_cursor":true}),
            ),
        )
        .await;
    let written = receipt(&admitted);
    assert_eq!(written["outcome"], "written", "{written}");
    assert_eq!(written["output_since_observation"], true);
    assert_eq!(
        written["observation_drift"]["tolerance"], "below_cursor",
        "{written}"
    );
    assert_eq!(
        h.interaction().input_ack_sequence(&terminal).await,
        Some(before + 1)
    );
    let after = observation(&admitted);
    assert_eq!(after["cursor"]["visible"], false, "{after}");
    assert_eq!(rows(after)[1], "Type here: abc", "{after}");
    h.stop(&terminal).await;
}

/// The refusal table with the tolerance requested: a change on the cursor
/// row, a text change above, a presentation-only (bold) change above and a
/// cursor move each stay stale, with `screen_diff` naming why; a hidden
/// cursor is an input-mode change and is refused by the surface fence
/// before the rows are compared; nothing is written.
#[tokio::test]
async fn changes_at_or_above_the_cursor_and_cursor_changes_stay_stale() {
    let h = Harness::start().await;
    let (terminal, _view) = open_claimed(&h, QUIET_BOX, "below-table").await;
    let before = h.interaction().input_ack_sequence(&terminal).await.unwrap();
    // Each row: (name, painted bytes, expected screen_diff facts).
    let table: [(&str, &[u8], Value); 4] = [
        (
            "cursor-row",
            b"\x1b7\x1b[2;40Hzz\x1b8",
            json!({"cursor":{"moved":false,"visible":true},"rows_changed_at_or_above_cursor":1,"rows_changed_below_cursor":0}),
        ),
        (
            "row-above-text",
            b"\x1b7\x1b[1;1HOther line\x1b8",
            json!({"cursor":{"moved":false,"visible":true},"rows_changed_at_or_above_cursor":1,"rows_changed_below_cursor":0}),
        ),
        (
            "row-above-bold",
            b"\x1b7\x1b[1;1H\x1b[1mOther line\x1b[0m\x1b8",
            json!({"cursor":{"moved":false,"visible":true},"rows_changed_at_or_above_cursor":1,"rows_changed_below_cursor":0}),
        ),
        (
            "cursor-moved",
            b"\x1b[C",
            json!({"cursor":{"moved":true,"visible":true},"rows_changed_at_or_above_cursor":0,"rows_changed_below_cursor":0}),
        ),
    ];
    for (name, bytes, expected) in table {
        let view = h
            .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
            .await;
        let text_before = rows(&view);
        inject(&h, &terminal, bytes);
        wait_past(&h, &terminal, revision(&view)).await;
        let refused = h
            .call(
                "calm.terminal.input",
                typed(
                    &terminal,
                    name,
                    &view,
                    json!({"allow_output_below_cursor":true}),
                ),
            )
            .await;
        let stale = receipt(&refused);
        assert_eq!(stale["outcome"], "stale_observation", "{name}: {stale}");
        let diff = &stale["screen_diff"];
        for (key, value) in expected.as_object().unwrap() {
            assert_eq!(&diff[key], value, "{name}/{key}: {diff}");
        }
        assert_eq!(
            diff["rows_changed_total"].as_u64().unwrap(),
            diff["rows_changed_at_or_above_cursor"].as_u64().unwrap()
                + diff["rows_changed_below_cursor"].as_u64().unwrap()
        );
        assert!(!h.interaction().input_pending(&terminal).await, "{name}");
        let fresh = observation(&refused);
        if name == "row-above-bold" {
            assert_eq!(
                rows(fresh),
                text_before,
                "a presentation-only change leaves the text identical: {fresh}"
            );
        }
        assert!(!rows(fresh)[1].contains("abc"), "{name}: {fresh}");
        // Restore the cursor so the next row starts from the input line.
        if name == "cursor-moved" {
            inject(&h, &terminal, b"\x1b[D");
        }
    }
    // A hidden cursor flips an input mode (`modes`), which the surface fence
    // refuses first, tolerance or not; the row comparison never runs.
    let view = h
        .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
        .await;
    inject(&h, &terminal, b"\x1b[?25l");
    wait_past(&h, &terminal, revision(&view)).await;
    let hidden = h
        .call(
            "calm.terminal.input",
            typed(
                &terminal,
                "cursor-hidden",
                &view,
                json!({"allow_output_below_cursor":true}),
            ),
        )
        .await;
    assert!(
        hidden["error"]["message"].as_str().unwrap().contains(
            "terminal surface changed since observation (size, input modes or alternate screen)"
        ),
        "{hidden}"
    );
    assert!(!h.interaction().input_pending(&terminal).await);
    inject(&h, &terminal, b"\x1b[?25h");
    assert_eq!(
        h.interaction().input_ack_sequence(&terminal).await,
        Some(before),
        "nothing was written"
    );
    // The wider opt-in still wins when both are set: a row above changed.
    let view = h
        .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
        .await;
    inject(&h, &terminal, b"\x1b7\x1b[1;1HAnother title\x1b8");
    wait_past(&h, &terminal, revision(&view)).await;
    let both = h
        .call(
            "calm.terminal.input",
            typed(
                &terminal,
                "both",
                &view,
                json!({"allow_output_below_cursor":true,"allow_output_since_observation":true}),
            ),
        )
        .await;
    assert_eq!(receipt(&both)["outcome"], "written", "{both}");
    assert_eq!(receipt(&both)["output_since_observation"], true);
    assert!(
        receipt(&both)["observation_drift"]
            .get("tolerance")
            .is_none(),
        "the same-surface fence admitted it, not the row comparison: {both}"
    );
    assert!(
        rows(observation(&both))[1].starts_with("Type here: abc"),
        "{both}"
    );
    h.stop(&terminal).await;
}

/// #1666 r1 (F) — the tolerance admits draft edits only. Every other action
/// is refused as invalid params by the MCP layer before any fence runs, and
/// the service refuses the same shapes on its own when called directly
/// (nothing reserved or written either way); an editing key is admitted.
#[tokio::test]
async fn below_cursor_tolerance_is_refused_for_everything_but_draft_edits() {
    use calm_server::terminal_interaction::{InputOptions, Target};
    let h = Harness::start().await;
    let (terminal, view) = open_claimed(&h, QUIET_BOX, "below-edits-only").await;
    let before = h.interaction().input_ack_sequence(&terminal).await.unwrap();
    let refused: [(&str, Value); 8] = [
        ("submit", json!({"type":"submit","text":"abc"})),
        ("click", json!({"type":"click","column":0,"row":0})),
        ("enter", json!({"type":"key","key":"Enter"})),
        ("tab", json!({"type":"key","key":"Tab"})),
        ("escape", json!({"type":"key","key":"Escape"})),
        ("ctrl-c", json!({"type":"key","key":"Ctrl+C"})),
        ("ctrl-j", json!({"type":"key","key":"Ctrl+J"})),
        ("page-down", json!({"type":"key","key":"PageDown"})),
    ];
    for (name, action) in &refused {
        let response = h
            .call(
                "calm.terminal.input",
                json!({"terminal_id":terminal,"request_id":name,"action":action,"allow_output_below_cursor":true}),
            )
            .await;
        assert_eq!(response["error"]["code"], -32602, "{name}: {response}");
        assert!(
            response["error"]["message"].as_str().unwrap().contains(
                "allow_output_below_cursor admits only text, sequence, replace and editing keys"
            ),
            "{name}: {response}"
        );
        let direct = h
            .interaction()
            .input(
                &h.identity(),
                &Target::Terminal(terminal.clone()),
                None,
                name,
                action.clone(),
                InputOptions {
                    allow_output_below_cursor: true,
                    ..InputOptions::default()
                },
                None,
            )
            .await;
        let message = direct.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(
            message.contains(
                "allow_output_below_cursor admits only text, sequence, replace and editing keys"
            ),
            "{name} (service): {message}"
        );
        assert!(!h.interaction().input_pending(&terminal).await, "{name}");
    }
    assert_eq!(
        h.interaction().input_ack_sequence(&terminal).await,
        Some(before)
    );
    // An editing key is admitted: a row below the cursor changed, the write
    // proceeds with the tolerance.
    inject(&h, &terminal, b"\x1b7\x1b[5;1Hhint\x1b8");
    wait_past(&h, &terminal, revision(&view)).await;
    let admitted = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"observation_id":view["observation_id"],"request_id":"left","action":{"type":"key","key":"Left"},"allow_output_below_cursor":true,"observe":true,"wait_ms":100}),
        )
        .await;
    assert_eq!(receipt(&admitted)["outcome"], "written", "{admitted}");
    assert_eq!(
        receipt(&admitted)["observation_drift"]["tolerance"],
        "below_cursor"
    );
    h.stop(&terminal).await;
}

/// Many rows below the cursor changed: the receipt lists the first 16 and
/// says it truncated; the stale result reports counts only.
#[tokio::test]
async fn admitted_rows_are_listed_up_to_sixteen_and_counted_in_full() {
    let h = Harness::start().await;
    let (terminal, view) = open_claimed(&h, QUIET_BOX, "below-many").await;
    let mut paint = Vec::new();
    paint.extend_from_slice(b"\x1b7");
    for row in 3..=21 {
        paint.extend_from_slice(format!("\x1b[{row};1Hline {row}").as_bytes());
    }
    paint.extend_from_slice(b"\x1b8");
    inject(&h, &terminal, &paint);
    wait_past(&h, &terminal, revision(&view)).await;
    let refused = h
        .call(
            "calm.terminal.input",
            typed(&terminal, "many", &view, json!({})),
        )
        .await;
    let diff = &receipt(&refused)["screen_diff"];
    assert_eq!(diff["rows_changed_below_cursor"], 19, "{diff}");
    assert_eq!(diff["rows_changed_total"], 19);
    assert_eq!(diff["rows_changed_at_or_above_cursor"], 0);
    let admitted = h
        .call(
            "calm.terminal.input",
            typed(
                &terminal,
                "many-2",
                &view,
                json!({"allow_output_below_cursor":true}),
            ),
        )
        .await;
    let drift = &receipt(&admitted)["observation_drift"];
    assert_eq!(receipt(&admitted)["outcome"], "written", "{admitted}");
    assert_eq!(
        drift["rows_changed_below_cursor"],
        json!((2..18).collect::<Vec<u64>>()),
        "{drift}"
    );
    assert_eq!(drift["rows_changed_total"], 19);
    assert_eq!(drift["truncated"], true);
    h.stop(&terminal).await;
}
