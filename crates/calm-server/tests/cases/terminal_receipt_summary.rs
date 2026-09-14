//! #1677 S3 — `summary` on input and control receipts: a flat digest of
//! the facts the receipt carries (readback wait, hook signal, repaint,
//! control state), and the one-line text block saying the same in words,
//! through the real MCP tools, renderer and PTY.
use crate::terminal_support::{Harness, human_takeover};
use serde_json::{Value, json};
use std::time::Duration;

/// Echoes each submitted line as `COUNT:<n>:<line>`.
const COUNT_PROBE: &str = "i=0; printf 'READY\\n'; while IFS= read -r line; do i=$((i+1)); printf 'COUNT:%s:%s\\n' \"$i\" \"$line\"; done";

fn receipt(response: &Value) -> &Value {
    assert!(response.get("error").is_none(), "{response}");
    &response["result"]["structuredContent"]
}
fn observation(response: &Value) -> &Value {
    let result = receipt(response);
    assert_eq!(result["observation"]["status"], "available", "{result}");
    &result["observation"]["state"]
}
fn summary(response: &Value) -> &str {
    response["result"]["content"][0]["text"].as_str().unwrap()
}
/// The summary every receipt must carry: all twelve keys, `fields` set.
fn digest(fields: Value) -> Value {
    let mut summary = json!({"action":null,"readback":null,"screen":null,"settled":null,
        "signal":null,"repaint":null,"matched":null,"role":null,"control_id":null,
        "exited":null,"claim":null,"release":null});
    for (key, value) in fields.as_object().unwrap() {
        summary[key] = value.clone();
    }
    summary
}
async fn open_observed(h: &Harness, request: &str) -> String {
    let terminal = h
        .ok(
            "calm.terminal.open",
            json!({"program":COUNT_PROBE,"request_id":request}),
        )
        .await["terminal_id"]
        .as_str()
        .unwrap()
        .to_owned();
    h.observe_text(&terminal, "READY").await;
    terminal
}

/// Written receipts: with a change readback (screen, settled, role), with
/// a text readback (matched pattern), without a readback, and with
/// `claim:true, release:true` in one request, where the receipt keeps the
/// granted lease while the summary's `control_id` is the readback's null.
#[tokio::test]
async fn summary_on_written_receipts_follows_the_readback_not_the_lease() {
    let h = Harness::start().await;
    let terminal = open_observed(&h, "summary-written").await;
    let first = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"first","action":{"type":"submit","text":"one"},
                "claim":true,"release":true,"observe":true,"wait_for":"change","wait_ms":3000}),
        )
        .await;
    let written = receipt(&first);
    assert_eq!(written["outcome"], "written", "{first}");
    assert_eq!(written["release"]["status"], "released", "{written}");
    let lease = written["control_id"]
        .as_str()
        .expect("the granted lease")
        .to_owned();
    assert_eq!(
        written["claim"],
        json!({"status":"claimed","control_id":lease})
    );
    assert_eq!(observation(&first)["role"], "observer");
    assert_eq!(observation(&first)["control_id"], Value::Null);
    assert_eq!(
        written["summary"],
        digest(
            json!({"action":"written","readback":"available","screen":"changed","settled":true,
            "role":"observer","control_id":null,"exited":false,"claim":"claimed","release":"released"})
        ),
        "the readback's control, not the receipt's lease: {written}"
    );
    assert_eq!(
        summary(&first),
        format!(
            "terminal {terminal} input written; screen changed settled; role observer; claim claimed; \
             release released; details in structuredContent"
        )
    );
    let text = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"second","action":{"type":"submit","text":"two"},
                "claim":true,"observe":true,"wait_for":"text","wait_text":["COUNT:2:two"],"wait_ms":5000}),
        )
        .await;
    let state = observation(&text);
    assert_eq!(state["wait"]["outcome"], "matched", "{state}");
    let digested = &receipt(&text)["summary"];
    assert_eq!(digested["action"], "written");
    assert_eq!(digested["screen"], "matched");
    assert_eq!(digested["settled"], state["wait"]["settled"]);
    assert_eq!(digested["matched"], "COUNT:2:two");
    assert_eq!(digested["signal"], Value::Null);
    assert_eq!(
        digested["claim"], "claimed",
        "a new lease after the release"
    );
    assert_eq!(digested["role"], "owner");
    assert_eq!(digested["control_id"], receipt(&text)["control_id"]);
    assert_ne!(digested["control_id"], lease);
    let settled = if state["wait"]["settled"] == true {
        " settled"
    } else {
        ""
    };
    assert_eq!(
        summary(&text),
        format!(
            "terminal {terminal} input written; screen matched{settled}; matched COUNT:2:two; \
             role owner; claim claimed; details in structuredContent"
        )
    );
    let bare = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"third","action":{"type":"text","text":"three"},"claim":true}),
        )
        .await;
    assert_eq!(
        receipt(&bare)["summary"],
        digest(json!({"action":"written","readback":"none","claim":"held"})),
        "{bare}"
    );
    assert_eq!(
        summary(&bare),
        format!(
            "terminal {terminal} input written; no readback; claim held; details in structuredContent"
        )
    );
    // The unobserved write echoed: observe before the next input.
    h.observe_text(&terminal, "three").await;
    let released = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"last","action":{"type":"key","key":"Enter"},
                "release":true,"observe":true,"wait_for":"change","wait_ms":3000}),
        )
        .await;
    let last = receipt(&released);
    assert_eq!(last["release"]["status"], "released", "{last}");
    assert_eq!(last["summary"]["release"], "released");
    assert_eq!(last["summary"]["role"], "observer");
    assert_eq!(last["summary"]["control_id"], Value::Null);
    assert!(
        summary(&released)
            .ends_with("; role observer; release released; details in structuredContent"),
        "{}",
        summary(&released)
    );
    h.stop(&terminal).await;
}

/// A signal readback: the hook event and the repaint outcome land in the
/// summary and in the text block.
#[tokio::test]
async fn summary_names_the_hook_signal_and_the_repaint() {
    let h = Harness::start().await;
    let opened = h
        .ok(
            "calm.terminal.open",
            json!({"program":COUNT_PROBE,"request_id":"summary-signal","claim":true}),
        )
        .await;
    let terminal = opened["terminal_id"].as_str().unwrap().to_owned();
    let card_id = opened["card_id"].as_str().unwrap().to_owned();
    h.observe_text(&terminal, "READY").await;
    let post = async {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let stop =
            json!({"hook_event_name":"Stop","session_id":"summary-session","message":"done"});
        assert_eq!(h.post_claude_hook(&card_id, &stop).await, 200);
    };
    let (response, ()) = tokio::join!(
        h.call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"ask","action":{"type":"submit","text":"hello"},
                "observe":true,"wait_for":"signal","wait_ms":10000}),
        ),
        post
    );
    let state = observation(&response);
    assert_eq!(state["wait"]["outcome"], "signal", "{state}");
    let repaint = state["wait"]["repaint"]["outcome"].as_str().unwrap();
    let digested = &receipt(&response)["summary"];
    assert_eq!(digested["screen"], "signal");
    assert_eq!(digested["signal"], "stop");
    assert_eq!(digested["repaint"], repaint);
    assert_eq!(digested["matched"], Value::Null);
    assert_eq!(digested["role"], "owner");
    let settled = if state["wait"]["settled"] == true {
        " settled"
    } else {
        ""
    };
    assert_eq!(
        summary(&response),
        format!(
            "terminal {terminal} input written; screen signal{settled}; signal stop, repaint {repaint}; \
             role owner; details in structuredContent"
        )
    );
    h.stop(&terminal).await;
}

/// Stale and control-unavailable receipts (immediate readbacks), a claim
/// with and without a readback and a release with one keep the summary
/// shape; detach has none. (An unavailable readback is a unit-level shape:
/// `receipt_summary.rs`.)
#[tokio::test]
async fn summary_on_refusals_control_receipts_and_unavailable_readbacks() {
    let h = Harness::start().await;
    let terminal = open_observed(&h, "summary-control").await;
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    let user = uuid::Uuid::new_v4();
    let (pump, _incoming) = human_takeover(&entry, &terminal, user).await;
    let refused = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"held","action":{"type":"submit","text":"x"},"claim":true}),
        )
        .await;
    assert_eq!(
        receipt(&refused)["outcome"],
        "control_unavailable",
        "{refused}"
    );
    assert_eq!(
        receipt(&refused)["summary"],
        digest(
            json!({"action":"control_unavailable","readback":"available","screen":"elapsed",
            "settled":false,"role":"observer","control_id":null,"exited":false,"claim":"unavailable"})
        )
    );
    assert_eq!(
        summary(&refused),
        format!(
            "terminal {terminal} input control_unavailable; screen elapsed; role observer; \
             claim unavailable; details in structuredContent"
        )
    );
    pump.abort();
    let start = std::time::Instant::now();
    while entry
        .handle
        .owner_registry
        .lock()
        .unwrap()
        .current_owner()
        .is_some()
    {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the human never left"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let claimed = h
        .call(
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"claim"}),
        )
        .await;
    let control = receipt(&claimed)["control_id"].as_str().unwrap().to_owned();
    assert_eq!(
        receipt(&claimed)["summary"],
        digest(json!({"action":"claim","readback":"none"})),
        "no readback: no state fields, not even the receipt's own lease"
    );
    assert_eq!(
        summary(&claimed),
        format!("terminal {terminal} claim; no readback; details in structuredContent")
    );
    let again = h
        .call(
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"claim","observe":true,"wait_ms":50}),
        )
        .await;
    let digested = &receipt(&again)["summary"];
    assert_eq!(digested["action"], "claim");
    assert_eq!(digested["readback"], "available");
    assert_eq!(digested["screen"], "elapsed");
    assert_eq!(digested["role"], "owner");
    assert_eq!(digested["control_id"], receipt(&again)["control_id"]);
    assert_ne!(
        digested["control_id"], control,
        "a second claim mints a new lease"
    );
    assert_eq!(
        summary(&again),
        format!(
            "terminal {terminal} claim; screen elapsed; role owner; details in structuredContent"
        )
    );
    // Stale: the implicit observation is the claim readback; the screen
    // moves before the input, whose readback is immediate.
    let latest = observation(&again).clone();
    let typed = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"line","action":{"type":"submit","text":"moved"},
                "observe":true,"wait_for":"text","wait_text":["COUNT:1:moved"],"wait_ms":5000}),
        )
        .await;
    assert_eq!(receipt(&typed)["outcome"], "written", "{typed}");
    let stale = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"observation_id":latest["observation_id"],"request_id":"late","action":{"type":"key","key":"Enter"}}),
        )
        .await;
    assert_eq!(receipt(&stale)["outcome"], "stale_observation", "{stale}");
    assert_eq!(
        receipt(&stale)["summary"],
        digest(
            json!({"action":"stale_observation","readback":"available","screen":"elapsed",
            "settled":false,"role":"owner","control_id":receipt(&again)["control_id"],"exited":false})
        )
    );
    assert_eq!(
        summary(&stale),
        format!(
            "terminal {terminal} input stale_observation; screen elapsed; role owner; \
             details in structuredContent"
        )
    );
    let released = h
        .call(
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"release","observe":true,"wait_ms":50}),
        )
        .await;
    let digested = &receipt(&released)["summary"];
    assert_eq!(digested["action"], "release");
    assert_eq!(digested["readback"], "available");
    assert_eq!(digested["role"], "observer");
    assert_eq!(digested["control_id"], Value::Null);
    assert_eq!(
        summary(&released),
        format!(
            "terminal {terminal} release; screen elapsed; role observer; details in structuredContent"
        )
    );
    // Detach has no readback and no summary: its line names the client.
    let detached = h
        .call(
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"detach"}),
        )
        .await;
    assert_eq!(receipt(&detached)["detached"], true);
    assert!(receipt(&detached).get("summary").is_none(), "{detached}");
    assert_eq!(
        summary(&detached),
        format!("terminal {terminal} detached had_client true; details in structuredContent")
    );
    h.stop(&terminal).await;
}
