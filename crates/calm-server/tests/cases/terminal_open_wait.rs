//! #1677 S1 — `calm.terminal.open` waits like a readback: the observe wait
//! arguments run as the open's final observation, after the claim when there
//! is one, through the real MCP tools, renderer and PTY.
use crate::terminal_support::{Harness, human_takeover};
use calm_server::db::prelude::*;
use serde_json::{Value, json};
use std::time::Duration;

/// Paints a trust dialog 300 ms after start, then idles.
const TRUST_DIALOG: &str =
    "sleep 0.3; printf 'Do you trust this folder?\\n\\n  1. Yes, proceed\\n'; cat >/dev/null";
/// Paints READY, waits for the `go` marker, paints LATER, then idles.
const READY_THEN_LATER: &str =
    "printf 'READY\\n'; while [ ! -e go ]; do sleep 0.02; done; printf 'LATER\\n'; cat >/dev/null";

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
fn summary(response: &Value) -> &str {
    response["result"]["content"][0]["text"].as_str().unwrap()
}
fn text_wait(patterns: Value) -> Value {
    json!({"wait_for":"text","wait_text":patterns,"wait_ms":5000})
}
fn open_args(program: &str, request: &str, extra: Value) -> Value {
    let mut args = json!({"program":program,"request_id":request});
    for (key, value) in extra.as_object().unwrap() {
        args[key] = value.clone();
    }
    args
}
async fn terminal_cards(h: &Harness) -> usize {
    h.sql
        .cards_by_track(&h.track)
        .await
        .unwrap()
        .iter()
        .filter(|card| card.kind == "terminal")
        .count()
}
/// Run `call` and write `marker` once the call's wait has subscribed to the
/// projection (the same device as the text-wait tests), so the program's
/// output lands after the wait began.
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
                "the open's wait never subscribed"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        std::fs::write(h.root.path().join(marker), b"x").unwrap();
    };
    let (response, ()) = tokio::join!(call, release);
    response
}

/// One call starts the program and returns its first screen: the dialog is
/// painted 300 ms after the open, the wait names the pattern and the
/// returned text shows it. With `claim:true` the waited state is the
/// owner's (the wait ran after the claim), with the claim attached.
#[tokio::test]
async fn open_with_program_and_text_wait_returns_the_first_screen_in_one_call() {
    let h = Harness::start().await;
    let response = h
        .call(
            "calm.terminal.open",
            open_args(
                TRUST_DIALOG,
                "trust-observer",
                text_wait(json!(["trust this folder", "❯"])),
            ),
        )
        .await;
    assert!(response.get("error").is_none(), "{response}");
    let opened = &response["result"]["structuredContent"];
    assert_eq!(opened["wait"]["mode"], "text", "{opened}");
    assert_eq!(opened["wait"]["outcome"], "matched", "{opened}");
    assert_eq!(opened["wait"]["text"]["pattern"], "trust this folder");
    assert!(
        rows(opened)
            .iter()
            .any(|row| row.contains("trust this folder")),
        "{opened}"
    );
    assert_eq!(opened["role"], "observer");
    assert_eq!(opened["control_id"], Value::Null);
    assert!(
        opened.get("claim").is_none(),
        "no claim requested: {opened}"
    );
    assert!(opened["card_id"].is_string() && opened["operation_id"].is_string());
    // The immediate read established the connection: the waited observation
    // is compared against it, not against a fresh-connection null.
    assert!(
        opened["previous_observation_revision"].is_string(),
        "{opened}"
    );
    assert!(
        summary(&response).contains(" wait matched; full state in structuredContent"),
        "{}",
        summary(&response)
    );
    let first = opened["terminal_id"].as_str().unwrap().to_owned();

    let claimed = h
        .ok(
            "calm.terminal.open",
            open_args(
                TRUST_DIALOG,
                "trust-owner",
                json!({"claim":true,"wait_for":"text","wait_text":["trust this folder"],"wait_ms":5000}),
            ),
        )
        .await;
    assert_eq!(claimed["wait"]["outcome"], "matched", "{claimed}");
    assert_eq!(claimed["claim"]["status"], "claimed", "{claimed}");
    assert_eq!(
        claimed["role"], "owner",
        "the wait ran after the claim: {claimed}"
    );
    assert_eq!(claimed["control_id"], claimed["claim"]["control_id"]);
    assert!(claimed["control_id"].is_string());
    assert!(
        rows(&claimed)
            .iter()
            .any(|row| row.contains("trust this folder")),
        "{claimed}"
    );
    let second = claimed["terminal_id"].as_str().unwrap().to_owned();
    assert_ne!(first, second);
    h.state.terminal_renderer.drop_entry(&second).await;
    h.stop(&first).await;
}

/// A human holds control: the replayed open's claim is unavailable and the
/// waited state is still returned (observer role, the LATER line the wait
/// ended on), on the same terminal without a second create.
#[tokio::test]
async fn open_with_claim_while_a_human_holds_control_returns_the_waited_state() {
    let h = Harness::start().await;
    let terminal = h
        .ok(
            "calm.terminal.open",
            open_args(READY_THEN_LATER, "contended-wait", json!({})),
        )
        .await["terminal_id"]
        .as_str()
        .unwrap()
        .to_owned();
    h.observe_text(&terminal, "READY").await;
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    let user = uuid::Uuid::new_v4();
    let (pump, _incoming) = human_takeover(&entry, &terminal, user).await;
    let cards = terminal_cards(&h).await;
    let response = call_then_release(
        &h,
        &terminal,
        "go",
        h.call(
            "calm.terminal.open",
            open_args(
                READY_THEN_LATER,
                "contended-wait",
                json!({"claim":true,"wait_for":"text","wait_text":["LATER"],"wait_ms":5000}),
            ),
        ),
    )
    .await;
    assert!(response.get("error").is_none(), "{response}");
    let opened = &response["result"]["structuredContent"];
    assert_eq!(opened["terminal_id"], terminal, "the same terminal");
    assert_eq!(opened["claim"]["status"], "unavailable", "{opened}");
    assert_eq!(
        opened["claim"]["reason"], "terminal is controlled by another client",
        "{opened}"
    );
    assert_eq!(opened["wait"]["outcome"], "matched", "{opened}");
    assert_eq!(opened["wait"]["text"]["pattern"], "LATER");
    assert_eq!(opened["wait"]["text"]["already"], false, "{opened}");
    assert!(rows(opened).iter().any(|row| row == "LATER"), "{opened}");
    assert_eq!(opened["role"], "observer");
    assert_eq!(opened["control_id"], Value::Null);
    assert_eq!(terminal_cards(&h).await, cards, "no second create");
    assert_eq!(
        entry.handle.owner_registry.lock().unwrap().current_owner(),
        Some(user),
        "the human keeps control"
    );
    pump.abort();
    h.stop(&terminal).await;
}

/// A change wait on the default shell: the wait runs with the change
/// contract (either outcome is legitimate on a shell that may or may not
/// still be painting its prompt); without wait arguments an open returns at
/// once as before.
#[tokio::test]
async fn open_with_change_wait_on_the_default_shell_runs_the_wait() {
    let h = Harness::start().await;
    let opened = h
        .ok(
            "calm.terminal.open",
            json!({"request_id":"shell-change","wait_for":"change","wait_ms":300}),
        )
        .await;
    assert_eq!(opened["wait"]["mode"], "change", "{opened}");
    let waited = opened["wait"]["waited_ms"].as_u64().unwrap();
    match opened["wait"]["outcome"].as_str() {
        Some("changed") => assert!(waited < 300, "{opened}"),
        Some("unchanged") => assert!(waited >= 300, "{opened}"),
        other => panic!("a change wait ends changed or unchanged, not {other:?}: {opened}"),
    }
    assert_eq!(
        opened["wait"]["baseline_revision"],
        opened["previous_observation_revision"]
    );
    let terminal = opened["terminal_id"].as_str().unwrap().to_owned();
    let plain = h
        .ok("calm.terminal.open", json!({"request_id":"shell-plain"}))
        .await;
    assert_eq!(
        plain["wait"],
        json!({"mode":"elapsed","outcome":"elapsed","waited_ms":0,"settled":false,
            "baseline_revision":plain["wait"]["baseline_revision"],"baseline_signal_seq":0}),
        "{plain}"
    );
    assert_eq!(plain["previous_observation_revision"], Value::Null);
    h.state
        .terminal_renderer
        .drop_entry(plain["terminal_id"].as_str().unwrap())
        .await;
    h.stop(&terminal).await;
}

/// The wait arguments are validated as on observe, before the create: an
/// invalid open creates no card.
#[tokio::test]
async fn open_wait_argument_validation_matches_observe_and_creates_nothing() {
    let h = Harness::start().await;
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
            json!({"wait_for":"text","wait_text":[long]}),
            "1..200 bytes of printable text",
        ),
        (
            json!({"wait_for":"text","wait_text":["x"],"signal_events":["stop"]}),
            "signal_events requires wait_for=signal",
        ),
        (
            json!({"wait_for":"change","repaint_ms":10}),
            "repaint_ms requires wait_for=signal",
        ),
        (
            json!({"settle_ms":10}),
            "settle_ms requires wait_for=change",
        ),
        (json!({"wait_ms":20001}), "wait_ms must be 0..20000"),
        (
            json!({"wait_for":"signal","signal_events":["Stop"]}),
            "unknown signal event",
        ),
        (json!({"wait_for":"later"}), "unknown variant"),
    ] {
        let mut open = args.clone();
        open["request_id"] = json!("invalid-wait");
        let response = h.call("calm.terminal.open", open).await;
        assert_eq!(response["error"]["code"], -32602, "{args}: {response}");
        assert!(
            error_text(&response).contains(expected),
            "{args}: {response}"
        );
    }
    assert_eq!(terminal_cards(&h).await, 0, "invalid waits create no card");
    let opened = h
        .ok(
            "calm.terminal.open",
            json!({"request_id":"invalid-wait","wait_for":"elapsed","wait_ms":50}),
        )
        .await;
    assert_eq!(opened["wait"]["outcome"], "elapsed", "{opened}");
    assert!(opened["wait"]["waited_ms"].as_u64().unwrap() >= 50);
    assert_eq!(terminal_cards(&h).await, 1);
    h.stop(opened["terminal_id"].as_str().unwrap()).await;
}

/// The wait arguments never enter the idempotency hash: a replayed
/// request_id with another wait returns the same terminal (no second
/// create) and runs the wait it asked for; `format=image` with a wait is one
/// capture whose `wait` block is the waited one.
#[tokio::test]
async fn replayed_open_with_a_different_wait_reuses_the_terminal_and_waits() {
    let h = Harness::start().await;
    let first = h
        .ok(
            "calm.terminal.open",
            open_args(READY_THEN_LATER, "replay-wait", json!({})),
        )
        .await;
    let terminal = first["terminal_id"].as_str().unwrap().to_owned();
    h.observe_text(&terminal, "READY").await;
    let cards = terminal_cards(&h).await;
    let replayed = h
        .ok(
            "calm.terminal.open",
            open_args(
                READY_THEN_LATER,
                "replay-wait",
                json!({"wait_for":"text","wait_text":["READY"],"wait_ms":5000,"settle_ms":0}),
            ),
        )
        .await;
    assert_eq!(replayed["terminal_id"], terminal);
    assert_eq!(replayed["operation_id"], first["operation_id"]);
    assert_eq!(
        replayed["terminal_session_id"],
        first["terminal_session_id"]
    );
    assert_eq!(replayed["wait"]["outcome"], "matched", "{replayed}");
    assert_eq!(replayed["wait"]["text"]["already"], true, "{replayed}");
    assert_eq!(terminal_cards(&h).await, cards, "no second create");
    let image = call_then_release(
        &h,
        &terminal,
        "go",
        h.call(
            "calm.terminal.open",
            open_args(
                READY_THEN_LATER,
                "replay-wait",
                json!({"format":"image","wait_for":"text","wait_text":["LATER"],"wait_ms":5000}),
            ),
        ),
    )
    .await;
    assert!(image.get("error").is_none(), "{image}");
    let state = &image["result"]["structuredContent"];
    assert_eq!(state["terminal_id"], terminal);
    assert_eq!(state["wait"]["outcome"], "matched", "{state}");
    assert_eq!(state["wait"]["text"]["pattern"], "LATER");
    assert_eq!(state["image_source"], "rmux_client_projection", "{state}");
    assert!(
        state.get("image").is_none(),
        "the render succeeded: {state}"
    );
    assert!(
        image["result"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part["type"] == "image" && part["mimeType"] == "image/png"),
        "{image}"
    );
    assert!(rows(state).iter().any(|row| row == "LATER"), "{state}");
    assert_eq!(terminal_cards(&h).await, cards);
    h.stop(&terminal).await;
}
