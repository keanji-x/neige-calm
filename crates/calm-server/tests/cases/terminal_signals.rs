//! Hook signals and bounded composite actions through the real MCP tools, operation runtime, PTY and
//! ingest route; a fake `claude` reads the generated `--settings` file and runs the registered hook command.
use crate::terminal_support::{Harness, human_takeover};
use calm_server::db::prelude::*;
use calm_server::event::Event;
use calm_server::model::{CardRole, new_id};
use calm_server::routes::theme::RequestTheme;
use calm_server::track_activity::{
    ActivityPayload, Attention, CardState, INTERACTIVE_OUTPUT_WINDOW, ItemKind, ItemSource,
    Recompute, TrackActivityProjector,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

/// Fake claude: per stdin line runs the hook command with synthetic payloads (`perm` → a permission
/// Notification, anything else → UserPromptSubmit then Stop) before echoing the turn; the Stop body is byte-identical every turn.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
settings=""
while [ $# -gt 0 ]; do case "$1" in --settings) settings="$2"; shift 2;; *) shift;; esac; done
hook=$(sed -n 's/^ *"command": "\(.*\)"[,]*$/\1/p' "$settings" | head -1)
printf 'READY %s\n' "$NEIGE_CARD_ID"
n=0
while IFS= read -r line; do
  n=$((n+1))
  case "$line" in
    perm) printf '{"hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission to use Bash","session_id":"fake-session"}' | sh -c "$hook" >/dev/null 2>&1 ;;
    *) printf '{"hook_event_name":"UserPromptSubmit","prompt":"%s","session_id":"fake-session"}' "$line" | sh -c "$hook" >/dev/null 2>&1
       printf '{"hook_event_name":"Stop","stop_hook_active":false,"session_id":"fake-session"}' | sh -c "$hook" >/dev/null 2>&1 ;;
  esac
  printf 'TURN:%s:%s\n' "$n" "$line"
done
"#;
/// Repaint fixture (echo off): `late:<x>` posts Stop then paints 300 ms later; `early:<x>` paints, waits 600 ms, posts Stop;
/// `prestop:<x>` paints, waits 50 ms, posts Stop; `burst` posts Stop then paints every 50 ms for 3 s; else quiet 400 ms, Stop, no paint.
const FAKE_CLAUDE_REPAINT: &str = r#"#!/bin/sh
settings=""
while [ $# -gt 0 ]; do case "$1" in --settings) settings="$2"; shift 2;; *) shift;; esac; done
hook=$(sed -n 's/^ *"command": "\(.*\)"[,]*$/\1/p' "$settings" | head -1)
stty -echo 2>/dev/null
stop() { printf '{"hook_event_name":"Stop","stop_hook_active":false,"session_id":"fake-session"}' | sh -c "$hook" >/dev/null 2>&1; }
printf 'READY %s\n' "$NEIGE_CARD_ID"
while IFS= read -r line; do
  case "$line" in
    late:*) stop; sleep 0.3; printf 'ANSWER:%s\n' "${line#late:}" ;;
    early:*) printf 'ANSWER:%s\n' "${line#early:}"; sleep 0.6; stop ;;
    prestop:*) printf 'ANSWER:%s\n' "${line#prestop:}"; sleep 0.05; stop ;;
    burst) stop; i=0; while [ $i -lt 60 ]; do i=$((i+1)); printf 'BURST:%s\n' "$i"; sleep 0.05; done ;;
    *) sleep 0.4; stop ;;
  esac
done
"#;
const COUNT_PROBE: &str = "i=0; printf 'READY\\n'; while IFS= read -r line; do i=$((i+1)); printf x >> physical-lines; printf 'COUNT:%s:%s\\n' \"$i\" \"$line\"; done";
const EXPECTED_EVENTS: [&str; 7] = [
    "SessionStart",
    "UserPromptSubmit",
    "Stop",
    "Notification",
    "PermissionRequest",
    "SessionEnd",
    "SubagentStop",
];
const INJECTED_ENV: [&str; 5] = [
    "NEIGE_CLAUDE_SETTINGS",
    "NEIGE_CARD_ID",
    "NEIGE_CALM_BASE_URL",
    "NEIGE_HOOK_PROVIDER",
    "NEIGE_HOOK_URL",
];

fn fake_claude_program(h: &Harness) -> String {
    let script = h.root.path().join("fake-claude.sh");
    std::fs::write(&script, FAKE_CLAUDE).unwrap();
    format!(
        "exec sh {} --settings \"$NEIGE_CLAUDE_SETTINGS\"",
        script.display()
    )
}
fn fake_claude_repaint_program(h: &Harness) -> String {
    let script = h.root.path().join("fake-claude-repaint.sh");
    std::fs::write(&script, FAKE_CLAUDE_REPAINT).unwrap();
    format!(
        "exec sh {} --settings \"$NEIGE_CLAUDE_SETTINGS\"",
        script.display()
    )
}
fn receipt(response: &Value) -> &Value {
    assert!(response.get("error").is_none(), "{response}");
    &response["result"]["structuredContent"]
}
fn state(response: &Value) -> &Value {
    let result = receipt(response);
    assert_eq!(result["observation"]["status"], "available", "{result}");
    &result["observation"]["state"]
}
fn has_line(state: &Value, needle: &str) -> bool {
    state["text"]
        .as_array()
        .unwrap()
        .iter()
        .any(|line| line.as_str().unwrap().contains(needle))
}
fn events_of(state: &Value) -> Vec<String> {
    state["signals"]["since_previous_observation"]
        .as_array()
        .unwrap()
        .iter()
        .map(|signal| signal["event"].as_str().unwrap().to_owned())
        .collect()
}
/// Open a claimed fake-claude terminal and return (open result, terminal id).
async fn open_fake_claude(h: &Harness, request_id: &str) -> (Value, String) {
    let opened = h
        .ok(
            "calm.terminal.open",
            json!({"program":fake_claude_program(h),"request_id":request_id,"claim":true}),
        )
        .await;
    let terminal = opened["terminal_id"].as_str().unwrap().to_owned();
    let ready = h.observe_text(&terminal, "READY").await;
    assert!(
        has_line(
            &ready,
            &format!("READY {}", opened["card_id"].as_str().unwrap())
        ),
        "the child must see NEIGE_CARD_ID: {ready}"
    );
    (opened, terminal)
}
/// Open a claimed repaint fake and return its terminal id.
async fn open_fake_claude_repaint(h: &Harness, request_id: &str) -> String {
    let opened = h
        .ok(
            "calm.terminal.open",
            json!({"program":fake_claude_repaint_program(h),"request_id":request_id,"claim":true}),
        )
        .await;
    let terminal = opened["terminal_id"].as_str().unwrap().to_owned();
    h.observe_text(&terminal, "READY").await;
    terminal
}
/// One invocation of the harness bridge stand-in for `card_id` with `body` on
/// stdin, under the env a Planner terminal's hook command carries.
async fn run_bridge(h: &Harness, card_id: &str, body: &str) {
    use tokio::io::AsyncWriteExt;
    let mut child = tokio::process::Command::new("sh")
        .arg(&h.bridge)
        .env("NEIGE_CARD_ID", card_id)
        .env("NEIGE_CALM_BASE_URL", &h.base_url)
        .env("NEIGE_HOOK_PROVIDER", "claude")
        .env(
            "NEIGE_HOOK_URL",
            format!("{}/internal/claude/hook?card_id={card_id}", h.base_url),
        )
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(body.as_bytes())
        .await
        .unwrap();
    let output = child.wait_with_output().await.unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "{\"continue\":true}"
    );
}
async fn submit(h: &Harness, terminal: &str, request: &str, text: &str, wait: Value) -> Value {
    let mut args = json!({"terminal_id":terminal,"request_id":request,"action":{"type":"submit","text":text},"observe":true});
    for (key, value) in wait.as_object().unwrap() {
        args[key] = value.clone();
    }
    h.call("calm.terminal.input", args).await
}

#[tokio::test]
async fn open_writes_hook_settings_injects_env_and_replays_idempotently() {
    let h = Harness::start().await;
    let (opened, terminal) = open_fake_claude(&h, "hooks-open").await;
    let card_id = opened["card_id"].as_str().unwrap().to_owned();
    assert_eq!(opened["role"], "owner", "{opened}");
    assert!(opened["control_id"].is_string());
    assert_eq!(opened["claim"]["status"], "claimed", "{opened}");
    assert_eq!(opened["signals"]["hooks_seen"], false);

    // The terminal row carries exactly the generated keys, derived from the
    // allocated card id, on top of the (empty) request env.
    let term = h.state.repo.terminal_get(&terminal).await.unwrap().unwrap();
    let env = term.env.as_object().unwrap();
    assert_eq!(
        env.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        INJECTED_ENV.iter().copied().collect::<BTreeSet<_>>(),
        "{env:?}"
    );
    assert_eq!(env["NEIGE_CARD_ID"], card_id);
    assert_eq!(env["NEIGE_HOOK_PROVIDER"], "claude");
    assert_eq!(env["NEIGE_CALM_BASE_URL"], h.base_url);
    assert_eq!(
        env["NEIGE_HOOK_URL"],
        format!("{}/internal/claude/hook?card_id={card_id}", h.base_url)
    );
    let settings_path = std::path::PathBuf::from(env["NEIGE_CLAUDE_SETTINGS"].as_str().unwrap());
    assert_eq!(
        settings_path,
        h.state
            .codex
            .terminal_hook_settings_dir
            .join(format!("{card_id}.json"))
    );
    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(
        settings["hooks"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        EXPECTED_EVENTS
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<_>>(),
        "exactly the seven issue events"
    );
    assert!(settings.get("mcpServers").is_none());
    // No scope declared: the hooks-only file, no `permissions` key; no `claude_permissions` anywhere.
    assert!(
        settings.get("permissions").is_none(),
        "no scope, no permissions block: {settings}"
    );
    assert_eq!(
        settings.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["hooks"]
    );
    assert!(opened.get("claude_permissions").is_none(), "{opened}");
    assert!(
        opened.get("claude_permissions_source").is_none(),
        "{opened}"
    );
    let card = h.state.repo.card_get(&card_id).await.unwrap().unwrap();
    assert!(
        card.payload.get("claude_permissions").is_none(),
        "{}",
        card.payload
    );
    assert!(
        card.payload.get("claude_permissions_source").is_none(),
        "no scope, no policy: no source key either (#1704 S2): {}",
        card.payload
    );
    let operation = h
        .state
        .operation_runtime
        .find_by_kind_and_idempotency(
            "terminal-create",
            &format!("planner-terminal:{}:hooks-open", h.session_id),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        operation.payload.get("claude_permissions").is_none(),
        "{}",
        operation.payload
    );
    let tx_output = operation.tx_output.unwrap();
    assert!(
        tx_output.data.get("claude_permissions").is_none(),
        "{}",
        tx_output.data
    );
    let command = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(
        command.contains(&format!("'{}' --provider claude", h.bridge.display())),
        "{command}"
    );
    assert!(
        command.contains(&format!("NEIGE_CARD_ID='{card_id}'")),
        "{command}"
    );

    // Replay: same request_id → same terminal, control already held on this
    // connection → the current observation, no second claim, no error.
    let replayed = h
        .ok(
            "calm.terminal.open",
            json!({"program":fake_claude_program(&h),"request_id":"hooks-open","claim":true}),
        )
        .await;
    assert_eq!(replayed["terminal_id"], terminal);
    assert_eq!(replayed["card_id"], card_id);
    assert_eq!(replayed["role"], "owner");
    assert_eq!(replayed["control_id"], opened["control_id"]);
    assert_eq!(replayed["claim"]["status"], "claimed", "{replayed}");
    assert_eq!(
        h.state
            .repo
            .cards_by_track(&h.track)
            .await
            .unwrap()
            .iter()
            .filter(|c| c.kind == "terminal")
            .count(),
        1,
        "the replay must not create a second terminal card"
    );

    // A human-created terminal (REST route) gets exactly the env it asked for
    // and no settings file.
    use tower::ServiceExt;
    let body = json!({"program":"exec /bin/sh","cwd":"","env":{"FOO":"bar"},"theme":{"fg":[216,219,226],"bg":[15,20,24]}});
    let response = h
        .app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{}/terminal-cards", h.track))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::CREATED);
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let card: Value = serde_json::from_slice(&bytes).unwrap();
    let rest_card_id = card["id"].as_str().unwrap().to_owned();
    assert!(
        card["payload"].get("claude_permissions").is_none(),
        "{card}"
    );
    let rest_term = h
        .state
        .repo
        .terminal_get_by_card(&rest_card_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        rest_term.env,
        json!({"FOO":"bar"}),
        "no injected env on REST terminals"
    );
    assert!(
        !h.state
            .codex
            .terminal_hook_settings_dir
            .join(format!("{rest_card_id}.json"))
            .exists()
    );
    h.state.terminal_renderer.drop_entry(&rest_term.id).await;
    h.stop(&terminal).await;
}

#[tokio::test]
async fn hook_post_for_a_terminal_card_lands_in_the_ring_and_never_projects_state() {
    let h = Harness::start().await;
    let (opened, terminal) = open_fake_claude(&h, "hooks-ring").await;
    let card_id = opened["card_id"].as_str().unwrap().to_owned();
    let mut bus = h.state.events.subscribe();
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    let before = entry.signals.last_seq();

    let stop =
        json!({"hook_event_name":"Stop","session_id":"posted-session","message":"done\u{7}\n now"});
    assert_eq!(h.post_claude_hook(&card_id, &stop).await, 200);
    assert_eq!(entry.signals.last_seq(), before + 1);
    // Duplicate delivery of the same body: acknowledged, appended once.
    assert_eq!(h.post_claude_hook(&card_id, &stop).await, 200);
    assert_eq!(entry.signals.last_seq(), before + 1);
    // Malformed / unknown payloads: acknowledged, never appended.
    for malformed in [
        json!({"message":"no event name"}),
        json!({"hook_event_name":"NotAClaudeHook","message":"x"}),
        json!({"hook_event_name":42}),
        json!([1, 2, 3]),
    ] {
        assert_eq!(
            h.post_claude_hook(&card_id, &malformed).await,
            200,
            "{malformed}"
        );
    }
    assert_eq!(entry.signals.last_seq(), before + 1);

    // Presentation: the signal is listed with its bounded message, the
    // connection baseline advances, and a second observation lists nothing.
    let view = h
        .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
        .await;
    assert_eq!(view["signals"]["hooks_seen"], true, "{view}");
    assert_eq!(view["signals"]["last_seq"], before + 1);
    assert_eq!(view["signals"]["dropped_since_previous_observation"], 0);
    let listed = view["signals"]["since_previous_observation"]
        .as_array()
        .unwrap();
    assert_eq!(listed.len(), 1, "{view}");
    assert_eq!(listed[0]["event"], "stop");
    assert_eq!(listed[0]["seq"], before + 1);
    assert_eq!(listed[0]["message"], "done now");
    assert_eq!(listed[0]["claude_session_id"], "posted-session");
    assert!(listed[0]["received_at_ms"].as_i64().unwrap() > 0);
    assert_eq!(view["wait"]["baseline_signal_seq"], before);
    let again = h
        .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
        .await;
    assert_eq!(again["signals"]["since_previous_observation"], json!([]));
    assert_eq!(again["wait"]["baseline_signal_seq"], before + 1);

    // Never worker state: nothing was persisted or broadcast for the card.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
    while let Ok(Ok(envelope)) = tokio::time::timeout_at(deadline, bus.recv()).await {
        assert!(
            !matches!(
                envelope.event,
                Event::ClaudeHook { .. } | Event::CodexHook { .. }
            ),
            "a terminal hook must not be persisted as a hook event: {:?}",
            envelope.event
        );
    }
    let card = h.state.repo.card_get(&card_id).await.unwrap().unwrap();
    assert_eq!(card.kind, "terminal");
    assert_eq!(
        card.payload[calm_server::validation::TERMINAL_SIGNALS_PAYLOAD_KEY],
        true,
        "the Planner-opened terminal carries its provenance on the card: {}",
        card.payload
    );
    assert_eq!(h.persisted_hook_events().await, 0);
    h.stop(&terminal).await;
}

#[tokio::test]
async fn signal_wait_returns_on_the_matching_event_and_honors_the_filter() {
    let h = Harness::start().await;
    let (_, terminal) = open_fake_claude(&h, "hooks-wait").await;
    let before = h
        .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
        .await;
    assert_eq!(before["signals"]["hooks_seen"], false);
    let baseline = before["signals"]["last_seq"].as_u64().unwrap();

    // Default events: user_prompt_submit is ignored, stop ends the wait.
    let answered = submit(
        &h,
        &terminal,
        "ask-1",
        "hello",
        json!({"wait_for":"signal","wait_ms":10000}),
    )
    .await;
    assert_eq!(receipt(&answered)["outcome"], "written", "{answered}");
    let answered = state(&answered);
    assert_eq!(answered["wait"]["mode"], "signal");
    assert_eq!(answered["wait"]["outcome"], "signal", "{answered}");
    assert_eq!(answered["wait"]["signal"]["event"], "stop");
    assert_eq!(answered["wait"]["baseline_signal_seq"], baseline);
    assert!(answered["wait"]["signal"]["seq"].as_u64().unwrap() > baseline);
    // The readback settles the repaint after the signal; the echo may or may not have been quiet for
    // settle_ms when the Stop landed, so either verdict is legal here.
    let repaint = answered["wait"]["repaint"]["outcome"].as_str().unwrap();
    assert!(
        matches!(repaint, "already" | "settled"),
        "{repaint}: {answered}"
    );
    assert_eq!(answered["wait"]["settled"], true, "{answered}");
    assert!(
        answered["wait"]["signal_at_ms"].as_u64().unwrap()
            <= answered["wait"]["waited_ms"].as_u64().unwrap()
    );
    assert_eq!(answered["signals"]["hooks_seen"], true);
    assert_eq!(events_of(answered), vec!["user_prompt_submit", "stop"]);
    assert_eq!(
        answered["signals"]["since_previous_observation"][0]["message"],
        "hello"
    );
    assert_eq!(
        answered["signals"]["since_previous_observation"][1]["claude_session_id"],
        "fake-session"
    );
    // The fake posts Stop before it prints the turn, so the turn line is
    // awaited on the screen rather than expected in the signal readback.
    h.observe_text(&terminal, "TURN:1:hello").await;

    // No new signal: the budget elapses with outcome no_signal; the idle screen was quiet for settle_ms
    // at the deadline, so the wait is settled.
    let idle = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_for":"signal","wait_ms":400}),
        )
        .await;
    assert_eq!(idle["wait"]["outcome"], "no_signal", "{idle}");
    assert_eq!(idle["wait"]["signal"], Value::Null);
    assert_eq!(idle["wait"]["signal_at_ms"], Value::Null);
    assert_eq!(idle["wait"]["repaint"], Value::Null);
    assert_eq!(idle["wait"]["settled"], true, "{idle}");
    assert!(idle["wait"]["waited_ms"].as_u64().unwrap() >= 400);
    assert_eq!(idle["signals"]["since_previous_observation"], json!([]));

    // signal_events narrows the match: user_prompt_submit alone ends it ...
    let prompt = submit(
        &h,
        &terminal,
        "ask-2",
        "again",
        json!({"wait_for":"signal","wait_ms":10000,"signal_events":["user_prompt_submit"]}),
    )
    .await;
    assert_eq!(receipt(&prompt)["outcome"], "written", "{prompt}");
    assert_eq!(
        state(&prompt)["wait"]["signal"]["event"],
        "user_prompt_submit",
        "{prompt}"
    );
    // The wait ended on the prompt: await the turn's projected completion so the next submission observes
    // a settled screen (a turn line arriving between its observation and the write would be `stale_observation`).
    h.observe_text(&terminal, "TURN:2:again").await;
    // ... and session_end never arrives: budget, while the stop is still listed.
    let none = submit(
        &h,
        &terminal,
        "ask-3",
        "third",
        json!({"wait_for":"signal","wait_ms":1500,"signal_events":["session_end"]}),
    )
    .await;
    assert_eq!(receipt(&none)["outcome"], "written", "{none}");
    let none = state(&none);
    assert_eq!(none["wait"]["outcome"], "no_signal", "{none}");
    assert!(events_of(none).contains(&"stop".to_string()), "{none}");
    h.observe_text(&terminal, "TURN:3:third").await;
    // A permission notification carries its notification_type.
    let perm = submit(
        &h,
        &terminal,
        "ask-4",
        "perm",
        json!({"wait_for":"signal","wait_ms":10000}),
    )
    .await;
    assert_eq!(receipt(&perm)["outcome"], "written", "{perm}");
    let perm = state(&perm);
    assert_eq!(perm["wait"]["signal"]["event"], "notification", "{perm}");
    assert_eq!(
        perm["wait"]["signal"]["notification_type"],
        "permission_prompt"
    );
    assert_eq!(
        perm["wait"]["signal"]["message"],
        "Claude needs your permission to use Bash"
    );

    // Argument validation.
    for (args, why) in [
        (
            json!({"terminal_id":terminal,"wait_for":"change","repaint_ms":100}),
            "repaint_ms in change mode",
        ),
        (
            json!({"terminal_id":terminal,"repaint_ms":100}),
            "repaint_ms in elapsed mode",
        ),
        (
            json!({"terminal_id":terminal,"wait_for":"signal","repaint_ms":5001}),
            "repaint_ms above the limit",
        ),
        (
            json!({"terminal_id":terminal,"wait_for":"signal","settle_ms":2001}),
            "settle_ms above the limit",
        ),
        (
            json!({"terminal_id":terminal,"settle_ms":100}),
            "settle_ms in elapsed mode",
        ),
        (
            json!({"terminal_id":terminal,"wait_for":"change","signal_events":["stop"]}),
            "signal_events outside signal mode",
        ),
        (
            json!({"terminal_id":terminal,"wait_for":"signal","signal_events":[]}),
            "empty signal_events",
        ),
        (
            json!({"terminal_id":terminal,"wait_for":"signal","signal_events":["Stop"]}),
            "unknown event name",
        ),
        (
            json!({"terminal_id":terminal,"wait_for":"signal","wait_ms":20001}),
            "budget above the limit",
        ),
    ] {
        let response = h.call("calm.terminal.observe", args).await;
        assert!(response.get("error").is_some(), "{why}: {response}");
    }
    h.stop(&terminal).await;
}

#[tokio::test]
async fn signal_readback_waits_for_the_answer_painted_after_the_stop_hook() {
    let h = Harness::start().await;
    let terminal = open_fake_claude_repaint(&h, "repaint-late").await;
    let answered = submit(
        &h,
        &terminal,
        "late-1",
        "late:hello",
        json!({"wait_for":"signal"}),
    )
    .await;
    assert_eq!(receipt(&answered)["outcome"], "written", "{answered}");
    let answered = state(&answered);
    assert_eq!(answered["wait"]["outcome"], "signal", "{answered}");
    assert_eq!(answered["wait"]["signal"]["event"], "stop");
    assert_eq!(
        answered["wait"]["repaint"]["outcome"], "settled",
        "{answered}"
    );
    assert_eq!(answered["wait"]["settled"], true);
    assert!(
        has_line(answered, "ANSWER:hello"),
        "the answer must be in the signal readback: {answered}"
    );
    let signal_at = answered["wait"]["signal_at_ms"].as_u64().unwrap();
    let repaint = answered["wait"]["repaint"]["waited_ms"].as_u64().unwrap();
    let waited = answered["wait"]["waited_ms"].as_u64().unwrap();
    assert!(
        repaint >= 300 && signal_at + repaint <= waited + 1,
        "the answer was painted 300 ms after the stop: {answered}"
    );
    assert_eq!(events_of(answered), vec!["stop"]);
    // The next observation sees nothing new: the readback was complete.
    let again = h
        .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
        .await;
    assert_eq!(
        again["changed_since_previous_observation"], false,
        "{again}"
    );
    h.stop(&terminal).await;
}

#[tokio::test]
async fn signal_readback_returns_at_once_when_the_screen_settled_before_the_stop_hook() {
    let h = Harness::start().await;
    let terminal = open_fake_claude_repaint(&h, "repaint-early").await;
    let answered = submit(
        &h,
        &terminal,
        "early-1",
        "early:hi",
        json!({"wait_for":"signal"}),
    )
    .await;
    assert_eq!(receipt(&answered)["outcome"], "written", "{answered}");
    let answered = state(&answered);
    assert_eq!(answered["wait"]["outcome"], "signal", "{answered}");
    assert_eq!(
        answered["wait"]["repaint"]["outcome"], "already",
        "{answered}"
    );
    assert_eq!(answered["wait"]["settled"], true);
    assert!(has_line(answered, "ANSWER:hi"), "{answered}");
    let repaint = answered["wait"]["repaint"]["waited_ms"].as_u64().unwrap();
    assert!(
        repaint < 500,
        "already must not spend the repaint window ({repaint} ms): {answered}"
    );
    assert!(
        answered["wait"]["signal_at_ms"].as_u64().unwrap() >= 600,
        "the fake stays quiet 600 ms before Stop: {answered}"
    );
    h.stop(&terminal).await;
}

#[tokio::test]
async fn signal_readback_settles_from_an_answer_painted_just_before_the_stop_hook() {
    let h = Harness::start().await;
    let terminal = open_fake_claude_repaint(&h, "repaint-prestop").await;
    let answered = submit(
        &h,
        &terminal,
        "prestop-1",
        "prestop:hey",
        json!({"wait_for":"signal"}),
    )
    .await;
    assert_eq!(receipt(&answered)["outcome"], "written", "{answered}");
    let answered = state(&answered);
    assert_eq!(answered["wait"]["outcome"], "signal", "{answered}");
    assert_eq!(
        answered["wait"]["repaint"]["outcome"], "settled",
        "{answered}"
    );
    assert_eq!(answered["wait"]["settled"], true);
    assert!(has_line(answered, "ANSWER:hey"), "{answered}");
    let repaint = answered["wait"]["repaint"]["waited_ms"].as_u64().unwrap();
    assert!(
        repaint < 1_000,
        "a change before the signal must settle, not idle repaint_ms ({repaint} ms): {answered}"
    );
    h.stop(&terminal).await;
}

#[tokio::test]
async fn signal_readback_reports_none_after_repaint_ms_and_skipped_when_disabled() {
    let h = Harness::start().await;
    let terminal = open_fake_claude_repaint(&h, "repaint-silent").await;
    let silent = submit(
        &h,
        &terminal,
        "silent-1",
        "silent",
        json!({"wait_for":"signal","repaint_ms":700}),
    )
    .await;
    assert_eq!(receipt(&silent)["outcome"], "written", "{silent}");
    let silent = state(&silent);
    assert_eq!(silent["wait"]["outcome"], "signal", "{silent}");
    assert_eq!(silent["wait"]["repaint"]["outcome"], "none", "{silent}");
    assert_eq!(silent["wait"]["settled"], false);
    assert!(!has_line(silent, "ANSWER"), "{silent}");
    let repaint = silent["wait"]["repaint"]["waited_ms"].as_u64().unwrap();
    assert!((700..5_000).contains(&repaint), "{silent}");
    let signal_at = silent["wait"]["signal_at_ms"].as_u64().unwrap();
    assert!(
        signal_at >= 400,
        "the fake stays quiet 400 ms before Stop: {silent}"
    );
    assert!(silent["wait"]["waited_ms"].as_u64().unwrap() >= signal_at + 700);

    let skipped = submit(
        &h,
        &terminal,
        "silent-2",
        "silent",
        json!({"wait_for":"signal","repaint_ms":0}),
    )
    .await;
    assert_eq!(receipt(&skipped)["outcome"], "written", "{skipped}");
    let skipped = state(&skipped);
    assert_eq!(skipped["wait"]["outcome"], "signal", "{skipped}");
    assert_eq!(
        skipped["wait"]["repaint"],
        json!({"outcome":"skipped","waited_ms":0}),
        "{skipped}"
    );
    assert_eq!(skipped["wait"]["settled"], false);
    assert_eq!(
        skipped["wait"]["signal_at_ms"], skipped["wait"]["waited_ms"],
        "{skipped}"
    );
    // settle_ms is accepted in signal mode and only bounds the quiet window.
    let tuned = submit(
        &h,
        &terminal,
        "silent-3",
        "silent",
        json!({"wait_for":"signal","repaint_ms":300,"settle_ms":50}),
    )
    .await;
    assert_eq!(
        state(&tuned)["wait"]["repaint"]["outcome"],
        "none",
        "{tuned}"
    );
    h.stop(&terminal).await;
}

#[tokio::test]
async fn signal_readback_reports_unsettled_when_the_budget_ends_mid_burst() {
    let h = Harness::start().await;
    let terminal = open_fake_claude_repaint(&h, "repaint-burst").await;
    let burst = submit(
        &h,
        &terminal,
        "burst-1",
        "burst",
        json!({"wait_for":"signal","wait_ms":1200}),
    )
    .await;
    assert_eq!(receipt(&burst)["outcome"], "written", "{burst}");
    let burst = state(&burst);
    assert_eq!(burst["wait"]["outcome"], "signal", "{burst}");
    assert_eq!(burst["wait"]["signal"]["event"], "stop");
    assert_eq!(burst["wait"]["repaint"]["outcome"], "unsettled", "{burst}");
    assert_eq!(burst["wait"]["settled"], false);
    assert!(has_line(burst, "BURST:1"), "{burst}");
    assert!(
        burst["wait"]["waited_ms"].as_u64().unwrap() >= 1200,
        "{burst}"
    );
    // Let the burst finish before tearing the PTY down.
    h.observe_text(&terminal, "BURST:60").await;
    h.stop(&terminal).await;
}

#[tokio::test]
async fn repaint_ms_is_refused_outside_signal_mode_on_every_carrier() {
    let h = Harness::start().await;
    let terminal = open_fake_claude_repaint(&h, "repaint-args").await;
    let view = h
        .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
        .await;
    for (tool, args, why) in [
        (
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_for":"change","repaint_ms":0}),
            "observe change",
        ),
        (
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_for":"elapsed","repaint_ms":1500}),
            "observe elapsed",
        ),
        (
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"claim","observe":true,"wait_for":"change","repaint_ms":100}),
            "control change",
        ),
        (
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"claim","repaint_ms":100}),
            "control without observe",
        ),
        (
            "calm.terminal.input",
            json!({"terminal_id":terminal,"observation_id":view["observation_id"],"request_id":"r-1","action":{"type":"key","key":"Enter"},"observe":true,"wait_for":"change","repaint_ms":100}),
            "input change",
        ),
        (
            "calm.terminal.input",
            json!({"terminal_id":terminal,"observation_id":view["observation_id"],"request_id":"r-2","action":{"type":"key","key":"Enter"},"repaint_ms":100}),
            "input without observe",
        ),
    ] {
        let response = h.call(tool, args).await;
        let error = response
            .get("error")
            .unwrap_or_else(|| panic!("{why} accepted repaint_ms: {response}"));
        assert_eq!(error["code"], -32602, "{why}: {response}");
    }
    // The same shape as the settle_ms refusal in elapsed mode.
    let settle = h
        .call(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"settle_ms":100}),
        )
        .await;
    assert_eq!(settle["error"]["code"], -32602, "{settle}");
    // Nothing was written by the refused input calls.
    let after = h
        .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
        .await;
    assert_eq!(
        after["changed_since_previous_observation"], false,
        "{after}"
    );
    h.stop(&terminal).await;
}

#[tokio::test]
async fn readback_signal_baseline_is_read_before_the_physical_write() {
    let h = Harness::start().await;
    let opened = h
        .ok(
            "calm.terminal.open",
            json!({"program":COUNT_PROBE,"request_id":"pre-write","claim":true}),
        )
        .await;
    let terminal = opened["terminal_id"].as_str().unwrap().to_owned();
    let card_id = opened["card_id"].as_str().unwrap().to_owned();
    let ready = h.observe_text(&terminal, "READY").await;
    assert_eq!(ready["signals"]["last_seq"], 0);
    // A signal that lands after the previous observation but BEFORE the input call must not satisfy the
    // readback wait: the baseline is the seq read just before the physical write. Direct POSTs stand in for two bridge invocations.
    assert_eq!(
        h.post_claude_hook(
            &card_id,
            &json!({"hook_event_name":"Stop","neige_hook_occurrence":"1-1-a"})
        )
        .await,
        200
    );
    let poster = {
        let h_card = card_id.clone();
        let app = h.app.clone();
        let entry = h.state.terminal_renderer.get(&terminal).unwrap();
        tokio::spawn(async move {
            // The physical write precedes its echo on the screen, and the
            // readback baseline is read before the physical write: once the
            // marker is projected, this signal is above that baseline.
            let start = std::time::Instant::now();
            loop {
                let projected = entry
                    .handle
                    .model_view
                    .lock()
                    .unwrap()
                    .capture(0)
                    .map(|(frame, _)| frame.text.iter().any(|line| line.contains("COUNT:1:PROBE")))
                    .unwrap_or(false);
                if projected {
                    break;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(10),
                    "the submit never reached the screen"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            use tower::ServiceExt;
            let request = axum::http::Request::builder()
                .method("POST")
                .uri(format!("/internal/claude/hook?card_id={h_card}"))
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "ai:claude")
                .body(axum::body::Body::from(
                    json!({"hook_event_name":"Stop","neige_hook_occurrence":"2-2-b"}).to_string(),
                ))
                .unwrap();
            assert_eq!(app.oneshot(request).await.unwrap().status(), 200);
        })
    };
    let written = submit(
        &h,
        &terminal,
        "probe",
        "PROBE",
        json!({"wait_for":"signal","wait_ms":8000}),
    )
    .await;
    poster.await.unwrap();
    assert_eq!(receipt(&written)["outcome"], "written", "{written}");
    let written = state(&written);
    assert_eq!(written["wait"]["baseline_signal_seq"], 1, "{written}");
    assert_eq!(written["wait"]["outcome"], "signal", "{written}");
    assert_eq!(written["wait"]["signal"]["seq"], 2);
    assert!(written["wait"]["waited_ms"].as_u64().unwrap() < 8000);
    // Both signals are new relative to the previous observation on this
    // connection, so both are listed.
    assert_eq!(events_of(written), vec!["stop", "stop"]);
    assert!(
        has_line(written, "COUNT:1:PROBE"),
        "submit must complete the line: {written}"
    );
    h.stop(&terminal).await;
}

#[tokio::test]
async fn submit_writes_text_and_cr_as_one_action_and_never_repeats() {
    let h = Harness::start().await;
    let opened = h
        .ok(
            "calm.terminal.open",
            json!({"program":COUNT_PROBE,"request_id":"submit","claim":true}),
        )
        .await;
    let terminal = opened["terminal_id"].as_str().unwrap().to_owned();
    h.observe_text(&terminal, "READY").await;
    let sent = submit(
        &h,
        &terminal,
        "line-1",
        "PAYLOAD",
        json!({"wait_for":"change","wait_ms":3000}),
    )
    .await;
    assert_eq!(receipt(&sent)["outcome"], "written", "{sent}");
    assert_eq!(receipt(&sent)["application_result"], "unverified");
    assert!(
        has_line(state(&sent), "COUNT:1:PAYLOAD"),
        "one submit is one completed line: {sent}"
    );
    // Plain text still never submits.
    let typed = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"text-1","action":{"type":"text","text":"HELD"},"observe":true,"wait_ms":300}),
        )
        .await;
    assert!(!has_line(state(&typed), "COUNT:2"), "{typed}");
    let entered = submit(&h, &terminal, "line-2", "", json!({})).await;
    assert!(
        entered.get("error").is_some(),
        "empty submit is invalid: {entered}"
    );
    let entered = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"enter-2","action":{"type":"key","key":"Enter"},"observe":true,"wait_for":"change","wait_ms":3000}),
        )
        .await;
    assert!(has_line(state(&entered), "COUNT:2:HELD"), "{entered}");
    // Replaying the submit request_id never writes again.
    let replay = submit(&h, &terminal, "line-1", "PAYLOAD", json!({"wait_ms":200})).await;
    assert_eq!(receipt(&replay)["outcome"], "written");
    assert!(!has_line(state(&replay), "COUNT:3"), "{replay}");
    assert_eq!(
        std::fs::read(h.root.path().join("physical-lines")).unwrap(),
        b"xx",
        "two completed lines in total"
    );
    for (action, why) in [
        (json!({"type":"submit","text":"x","repeat":2}), "repeat"),
        (json!({"type":"submit","text":"a\nb"}), "control characters"),
        (json!({"type":"submit"}), "missing text"),
    ] {
        let response = h
            .call(
                "calm.terminal.input",
                json!({"terminal_id":terminal,"request_id":format!("bad-{why}"),"action":action}),
            )
            .await;
        assert!(response.get("error").is_some(), "{why}: {response}");
    }
    assert!(!has_line(
        &h.ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_ms":200})
        )
        .await,
        "COUNT:3"
    ));
    h.stop(&terminal).await;
}

#[tokio::test]
async fn open_with_claim_reports_takeover_on_replay_instead_of_reclaiming() {
    use calm_server::terminal_renderer::{ClientInputScope, ClientPumpContext, run_client_pump};
    use calm_session::{
        ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
        RenderEncoding,
    };
    let h = Harness::start().await;
    let opened = h
        .ok(
            "calm.terminal.open",
            json!({"program":"exec /bin/sh","request_id":"claimed","claim":true}),
        )
        .await;
    let terminal = opened["terminal_id"].as_str().unwrap().to_owned();
    assert_eq!(opened["role"], "owner", "{opened}");
    assert_eq!(opened["claim"]["status"], "claimed");
    assert_eq!(opened["claim"]["control_id"], opened["control_id"]);
    assert!(opened["text"].is_array(), "claim readback carries text");
    assert_eq!(opened["wait"]["mode"], "elapsed");
    // Without claim the open stays an observer, as before.
    let observer = h
        .ok(
            "calm.terminal.open",
            json!({"program":"exec /bin/sh","request_id":"unclaimed"}),
        )
        .await;
    assert_eq!(observer["role"], "observer");
    assert!(observer.get("claim").is_none());
    h.state
        .terminal_renderer
        .drop_entry(observer["terminal_id"].as_str().unwrap())
        .await;

    // A human takes over the claimed terminal.
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    let (incoming, rx) = tokio::sync::mpsc::channel(8);
    let (tx, mut outgoing) = tokio::sync::mpsc::channel(32);
    let user = uuid::Uuid::new_v4();
    let pump = tokio::spawn(run_client_pump(
        rx,
        tx,
        ClientPumpContext {
            input_barrier: entry.handle.input_barrier.clone(),
            input_scope: ClientInputScope::InteractiveUser,
            event_rx: entry.subscribe(),
            event_tx: entry.handle.event_tx.clone(),
            render_plane: entry.handle.render_plane.clone(),
            exit: entry.exit.clone(),
            supervisor_tx: entry.handle.supervisor_tx.clone(),
            owner_registry: entry.handle.owner_registry.clone(),
            session_id: entry.handle.session_id,
            terminal_id: terminal.clone(),
        },
    ));
    incoming
        .send(ClientMsg::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            terminal_id: terminal.clone(),
            client_id: user,
            desired_size: PtySize {
                cols: 80,
                rows: 24,
                pixel_width: None,
                pixel_height: None,
            },
            cell_size: None,
            initial_scrollback: InitialScrollback::None,
            resume_from: None,
            role_hint: None,
            capabilities: ClientCapabilities {
                render_encodings: vec![RenderEncoding::Vt],
                supports_scrollback: true,
                supports_sixel: false,
                supports_images: false,
                kernel_originated_input: false,
            },
        })
        .await
        .unwrap();
    assert!(matches!(
        outgoing.recv().await,
        Some(DaemonMsg::ServerHello { .. })
    ));
    incoming.send(ClientMsg::OwnerClaim).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if matches!(outgoing.recv().await, Some(DaemonMsg::OwnerChanged { owner_client_id: Some(id) }) if id == user) {
                break;
            }
        }
    })
    .await
    .unwrap();
    // The Planner's connection learns about the revocation.
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let view = h
                .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
                .await;
            if view["role"] == "observer" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();

    // Replayed open with claim: the terminal is returned, the human keeps
    // control, and the claim is reported unavailable with the reason.
    let replayed = h
        .ok(
            "calm.terminal.open",
            json!({"program":"exec /bin/sh","request_id":"claimed","claim":true}),
        )
        .await;
    assert_eq!(replayed["terminal_id"], terminal);
    assert_eq!(replayed["role"], "observer", "{replayed}");
    assert_eq!(replayed["control_id"], Value::Null);
    assert_eq!(replayed["claim"]["status"], "unavailable", "{replayed}");
    assert_eq!(
        replayed["claim"]["reason"], "terminal is controlled by another client",
        "{replayed}"
    );
    assert_eq!(
        entry.handle.owner_registry.lock().unwrap().current_owner(),
        Some(user),
        "the human still owns the terminal"
    );
    pump.abort();
    let _ = pump.await;
    h.stop(&terminal).await;
}

/// The round-19 ledger scope as the Planner declares it.
fn round19_scope() -> Value {
    json!({
        "edit": ["**"],
        "bash": ["python3 -m unittest", "git status", "git diff", "git log",
                 "git show", "git add", "git commit"],
        "deny": ["git push"]
    })
}
/// The effective block the kernel renders for [`round19_scope`] in `cwd`: every `git <rest>` prefix is
/// followed by its `git -C /<cwd> <rest>` spelling; `python3 -m unittest` and the non-git floor prefixes are not.
fn round19_block(cwd: &str) -> Value {
    let root = cwd.trim_matches('/');
    json!({
        "allow": [
            format!("Edit(//{root}/**)"), "Bash(python3 -m unittest *)",
            "Bash(git status *)", format!("Bash(git -C /{root} status *)"),
            "Bash(git diff *)", format!("Bash(git -C /{root} diff *)"),
            "Bash(git log *)", format!("Bash(git -C /{root} log *)"),
            "Bash(git show *)", format!("Bash(git -C /{root} show *)"),
            "Bash(git add *)", format!("Bash(git -C /{root} add *)"),
            "Bash(git commit *)", format!("Bash(git -C /{root} commit *)")
        ],
        "ask": [
            "Bash(git push *)", format!("Bash(git -C /{root} push *)"),
            "Bash(git reset --hard *)", format!("Bash(git -C /{root} reset --hard *)"),
            "Bash(rm -rf *)", "Bash(curl *)", "Bash(wget *)", "Bash(pip install *)",
            "Bash(npm install *)", format!("Edit(//{root}/.git/**)")
        ],
        "deny": ["Bash(git push *)", format!("Bash(git -C /{root} push *)")]
    })
}

#[tokio::test]
async fn open_with_scope_writes_permissions_stamps_the_card_and_echoes_the_block() {
    use tower::ServiceExt;
    let h = Harness::start().await;
    let mut bus = h.state.events.subscribe();
    let open = |scope: Option<Value>| {
        let mut args =
            json!({"program":fake_claude_program(&h),"request_id":"scoped","claim":true});
        if let Some(scope) = scope {
            args["claude_permissions"] = scope;
        }
        h.call("calm.terminal.open", args)
    };
    // An invalid scope is `invalid_params` naming the entry, before any
    // create is submitted: no card, no terminal, no operation row.
    for (scope, reason) in [
        (
            json!({"bash":["git status","git push"]}),
            "claude_permissions.bash[1] 'git push': floor command",
        ),
        (
            json!({"allow":["Bash(git status *)"]}),
            "claude_permissions: unknown key 'allow'",
        ),
        (json!({}), "claude_permissions declares nothing"),
        // Shape: a JSON array or null is not read as a scope.
        (
            json!([["**"], null, []]),
            "claude_permissions: must be an object",
        ),
        (json!(null), "claude_permissions: must be an object"),
        (
            json!({"edit":["**"],"deny":null}),
            "claude_permissions.deny: must be an array of strings",
        ),
    ] {
        let response = open(Some(scope.clone())).await;
        assert_eq!(response["error"]["code"], -32602, "{scope}: {response}");
        let message = response["error"]["message"].as_str().unwrap();
        assert!(message.contains(reason), "{scope}: {message}");
    }
    assert!(
        h.state
            .repo
            .cards_by_track(&h.track)
            .await
            .unwrap()
            .iter()
            .all(|c| c.kind != "terminal"),
        "a refused scope creates nothing"
    );
    assert!(
        h.state
            .operation_runtime
            .find_by_kind_and_idempotency(
                "terminal-create",
                &format!("planner-terminal:{}:scoped", h.session_id),
            )
            .await
            .unwrap()
            .is_none(),
        "a refused scope submits nothing"
    );

    let response = open(Some(round19_scope())).await;
    let opened = receipt(&response).clone();
    let terminal = opened["terminal_id"].as_str().unwrap().to_owned();
    let card_id = opened["card_id"].as_str().unwrap().to_owned();
    assert_eq!(opened["claim"]["status"], "claimed", "{opened}");
    let ready = h.observe_text(&terminal, "READY").await;
    assert!(has_line(&ready, &format!("READY {card_id}")), "{ready}");
    let term = h.state.repo.terminal_get(&terminal).await.unwrap().unwrap();
    assert!(term.cwd.starts_with('/'), "{}", term.cwd);
    let expected = round19_block(&term.cwd);

    // The file the child read through `--settings`: the seven hook events
    // plus exactly the effective block, nothing else.
    let settings_path =
        std::path::PathBuf::from(term.env["NEIGE_CLAUDE_SETTINGS"].as_str().unwrap());
    let text = std::fs::read_to_string(&settings_path).unwrap();
    println!("ROUND19_SETTINGS_JSON={text}");
    let settings: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(settings["permissions"], expected, "{text}");
    assert_eq!(
        settings.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["hooks", "permissions"]
    );
    assert_eq!(
        settings["hooks"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        EXPECTED_EVENTS
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<_>>()
    );
    assert_eq!(
        settings["permissions"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        vec!["allow", "ask", "deny"]
    );
    assert!(
        !text.contains("Read(") && !text.contains("defaultMode") && !text.contains("bypass"),
        "{text}"
    );

    // The card, the result, its summary line and the persisted operation.
    let card = h.state.repo.card_get(&card_id).await.unwrap().unwrap();
    assert_eq!(
        card.payload["claude_permissions"], expected,
        "{}",
        card.payload
    );
    assert_eq!(card.payload["terminal_signals"], true);
    assert_eq!(card.payload["schemaVersion"], 1);
    assert_eq!(opened["claude_permissions"], expected, "{opened}");
    let summary = response["result"]["content"][0]["text"].as_str().unwrap();
    // No Track policy: the block's source is `declared`.
    assert_eq!(card.payload["claude_permissions_source"], "declared");
    assert_eq!(opened["claude_permissions_source"], "declared", "{opened}");
    assert!(
        summary.ends_with(
            " wait elapsed permissions allow 14 ask 10 deny 2 source declared; \
             full state in structuredContent"
        ),
        "{summary}"
    );
    let operation = h
        .state
        .operation_runtime
        .find_by_kind_and_idempotency(
            "terminal-create",
            &format!("planner-terminal:{}:scoped", h.session_id),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        operation.payload["claude_permissions"],
        round19_scope(),
        "the trimmed scope is stored in the operation payload"
    );
    assert_eq!(
        operation.tx_output.as_ref().unwrap().data["claude_permissions"],
        expected,
        "the recovery input of the spawn side effect"
    );
    assert_eq!(
        operation.tx_output.unwrap().result["payload"]["claude_permissions"],
        expected,
        "the saved result is the stamped card"
    );

    // The persisted CardAdded event carries the stamped payload.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut added = None;
    while added.is_none() {
        let envelope = tokio::time::timeout_at(deadline, bus.recv())
            .await
            .expect("CardAdded within the deadline")
            .expect("event bus open");
        if let Event::CardAdded(card) = envelope.event
            && card.id.as_str() == card_id
        {
            added = Some(card);
        }
    }
    assert_eq!(
        added.unwrap().payload["claude_permissions"],
        expected,
        "CardAdded carries the stamped payload"
    );

    // Server-owned at the PATCH boundary; sticky across a replacement.
    let patch = |body: String| {
        axum::http::Request::builder()
            .method("PATCH")
            .uri(format!("/api/cards/{card_id}"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body))
            .unwrap()
    };
    let response = h
        .app
        .clone()
        .oneshot(patch(
            json!({"payload":{"schemaVersion":1,"claude_permissions":expected}}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    let response = h
        .app
        .clone()
        .oneshot(patch(
            r#"{"payload":{"schemaVersion":1,"terminal_id":"x"}}"#.to_owned(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let card = h.state.repo.card_get(&card_id).await.unwrap().unwrap();
    assert_eq!(card.payload["terminal_id"], "x");
    assert_eq!(
        card.payload["claude_permissions"], expected,
        "{}",
        card.payload
    );
    assert_eq!(card.payload["terminal_signals"], true, "{}", card.payload);

    // Replay with the same scope: the same terminal, one card, the same echo.
    let replayed = receipt(&open(Some(round19_scope())).await).clone();
    assert_eq!(replayed["terminal_id"], terminal);
    assert_eq!(replayed["card_id"], card_id);
    assert_eq!(replayed["claude_permissions"], expected, "{replayed}");
    let terminal_cards = || async {
        h.state
            .repo
            .cards_by_track(&h.track)
            .await
            .unwrap()
            .iter()
            .filter(|c| c.kind == "terminal")
            .count()
    };
    assert_eq!(terminal_cards().await, 1);

    // Replay with a different scope, or without one: the runtime's payload
    // conflict, no card, no terminal, the old terminal is not returned.
    for scope in [
        Some(json!({"deny":["git push","git reset"]})),
        Some(
            json!({"edit":["**"],"bash":["python3 -m unittest","git status","git diff","git log",
            "git show","git add","git commit"]}),
        ),
        None,
    ] {
        let response = open(scope.clone()).await;
        assert_eq!(response["error"]["code"], -32403, "{scope:?}: {response}");
        let message = response["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("already used with different payload")
                && message.contains(&format!("planner-terminal:{}:scoped", h.session_id)),
            "{scope:?}: {message}"
        );
        assert!(response.get("result").is_none(), "{response}");
        assert_eq!(terminal_cards().await, 1, "{scope:?}");
    }
    assert_eq!(
        std::fs::read_to_string(&settings_path).unwrap(),
        text,
        "the settings file is untouched by the replays"
    );
    h.stop(&terminal).await;
}

#[tokio::test]
async fn hook_settings_file_is_removed_when_the_card_is_deleted() {
    use tower::ServiceExt;
    let h = Harness::start().await;
    let (opened, terminal) = open_fake_claude(&h, "hooks-delete").await;
    let card_id = opened["card_id"].as_str().unwrap().to_owned();
    let settings_path = h
        .state
        .codex
        .terminal_hook_settings_dir
        .join(format!("{card_id}.json"));
    assert!(settings_path.exists());
    // A sibling file the server did not derive from this card stays.
    let sibling = h.state.codex.terminal_hook_settings_dir.join("other.json");
    std::fs::write(&sibling, "{}").unwrap();
    let response = h
        .app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("DELETE")
                .uri(format!("/api/cards/{card_id}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::NO_CONTENT);
    assert!(
        !settings_path.exists(),
        "settings file must go with the card"
    );
    assert!(sibling.exists());
    assert!(h.state.terminal_renderer.get(&terminal).is_none());

    // A scoped open: the same file (with the block), the same reap.
    let scoped = h
        .ok(
            "calm.terminal.open",
            json!({"program":fake_claude_program(&h),"request_id":"hooks-delete-scoped","claim":true,
                "claude_permissions":round19_scope()}),
        )
        .await;
    let scoped_terminal = scoped["terminal_id"].as_str().unwrap().to_owned();
    let scoped_card_id = scoped["card_id"].as_str().unwrap().to_owned();
    h.observe_text(&scoped_terminal, "READY").await;
    let scoped_path = h
        .state
        .codex
        .terminal_hook_settings_dir
        .join(format!("{scoped_card_id}.json"));
    let scoped_settings: Value =
        serde_json::from_str(&std::fs::read_to_string(&scoped_path).unwrap()).unwrap();
    assert!(
        scoped_settings["permissions"].is_object(),
        "{scoped_settings}"
    );
    let response = h
        .app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("DELETE")
                .uri(format!("/api/cards/{scoped_card_id}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::NO_CONTENT);
    assert!(!scoped_path.exists(), "the scoped file goes with its card");
    assert!(sibling.exists());
    assert!(h.state.terminal_renderer.get(&scoped_terminal).is_none());
    h.stop(&terminal).await;
}

/// Claude's `Stop` body is byte-identical every turn; the bridge's per-invocation occurrence id keys them apart.
#[tokio::test]
async fn two_byte_identical_stop_bodies_from_two_bridge_invocations_get_seq_1_and_2() {
    let h = Harness::start().await;
    let opened = h
        .ok(
            "calm.terminal.open",
            json!({"program":COUNT_PROBE,"request_id":"occurrence","claim":true}),
        )
        .await;
    let terminal = opened["terminal_id"].as_str().unwrap().to_owned();
    let card_id = opened["card_id"].as_str().unwrap().to_owned();
    h.observe_text(&terminal, "READY").await;
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    assert_eq!(entry.signals.last_seq(), 0);
    let stop = r#"{"hook_event_name":"Stop","stop_hook_active":false,"session_id":"fake-session"}"#;
    run_bridge(&h, &card_id, stop).await;
    assert_eq!(entry.signals.last_seq(), 1);
    run_bridge(&h, &card_id, stop).await;
    assert_eq!(
        entry.signals.last_seq(),
        2,
        "a second invocation with an identical body is a second event"
    );
    let view = h
        .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
        .await;
    let listed = view["signals"]["since_previous_observation"]
        .as_array()
        .unwrap();
    assert_eq!(listed.len(), 2, "{view}");
    assert_eq!(listed[0]["seq"], 1);
    assert_eq!(listed[1]["seq"], 2);
    assert!(listed.iter().all(|signal| signal["event"] == "stop"));
    h.stop(&terminal).await;
}

#[tokio::test]
#[allow(deprecated)] // the production ingest gate reads the same raw role cache
async fn terminal_hook_floods_do_not_evict_worker_dedupe_keys() {
    use calm_server::db::prelude::*;
    use calm_server::model::NewCard;
    use tower::ServiceExt;
    let h = Harness::start().await;
    let (opened, terminal) = open_fake_claude(&h, "hooks-flood").await;
    let card_id = opened["card_id"].as_str().unwrap().to_owned();
    let worker = h
        .sql
        .card_create(NewCard {
            track_id: h.track.clone().into(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    h.sql
        .seed_card_role_cache(h.state.write().role_cache())
        .await
        .unwrap();
    let mut bus = h.state.events.subscribe();
    let worker_body = json!({"hook_event_name":"Stop","session_id":"worker-session","transcript_path":"/tmp/w.jsonl","transcript_size_bytes":7}).to_string();
    let post_worker = || async {
        let request = axum::http::Request::builder()
            .method("POST")
            .uri(format!("/internal/codex/hook?card_id={}", worker.id))
            .header("content-type", "application/json")
            .header("X-Calm-Actor", "ai:codex")
            .body(axum::body::Body::from(worker_body.clone()))
            .unwrap();
        h.app.clone().oneshot(request).await.unwrap().status()
    };
    assert_eq!(post_worker().await, 204);
    let first = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Event::CodexHook { card_id, .. } = bus.recv().await.unwrap().event {
                break card_id;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(first.as_str(), worker.id.as_str());

    // More terminal hooks than the worker cache holds (4096), valid and
    // malformed alike; each is acknowledged and none touches the cache.
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    for i in 0..4100u32 {
        let body = if i % 2 == 0 {
            json!({"hook_event_name":"Stop","neige_hook_occurrence":format!("flood-{i}")})
        } else {
            json!({"message":"no event name","n":i})
        };
        assert_eq!(h.post_claude_hook(&card_id, &body).await, 200, "{i}");
    }
    assert_eq!(entry.signals.last_seq(), 2050);

    // The worker key survived: a duplicate delivery is still suppressed.
    assert_eq!(post_worker().await, 204);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
    while let Ok(Ok(envelope)) = tokio::time::timeout_at(deadline, bus.recv()).await {
        assert!(
            !matches!(envelope.event, Event::CodexHook { .. }),
            "the worker hook was ingested twice: {:?}",
            envelope.event
        );
    }
    h.stop(&terminal).await;
}

/// The claim is applied by the pump only if nobody else owns the terminal at that moment (decided under
/// the registry lock, never from the Planner connection's cached owner).
#[tokio::test]
async fn open_with_claim_yields_to_a_human_who_claimed_inside_the_claim_window() {
    use calm_server::terminal_renderer::{ClientInputScope, ClientPumpContext, run_client_pump};
    use calm_session::{
        ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
        RenderEncoding,
    };
    use std::sync::{Arc, Mutex};
    let h = Harness::start().await;
    let user = uuid::Uuid::new_v4();
    // The human's pump (abort handle) and its input channel, kept alive by
    // the test after the seam returns.
    type HumanClient = (
        tokio::task::AbortHandle,
        tokio::sync::mpsc::Sender<ClientMsg>,
    );
    let human: Arc<Mutex<Option<HumanClient>>> = Arc::new(Mutex::new(None));
    let seam_human = human.clone();
    let renderer = h.state.terminal_renderer.clone();
    h.interaction().set_claim_window_seam(Box::new(move |terminal_id: String| {
        Box::pin(async move {
            let entry = renderer.get(&terminal_id).unwrap();
            let (incoming, rx) = tokio::sync::mpsc::channel(8);
            let (tx, mut outgoing) = tokio::sync::mpsc::channel(32);
            let pump = tokio::spawn(run_client_pump(
                rx,
                tx,
                ClientPumpContext {
                    input_barrier: entry.handle.input_barrier.clone(),
                    input_scope: ClientInputScope::InteractiveUser,
                    event_rx: entry.subscribe(),
                    event_tx: entry.handle.event_tx.clone(),
                    render_plane: entry.handle.render_plane.clone(),
                    exit: entry.exit.clone(),
                    supervisor_tx: entry.handle.supervisor_tx.clone(),
                    owner_registry: entry.handle.owner_registry.clone(),
                    session_id: entry.handle.session_id,
                    terminal_id: terminal_id.clone(),
                },
            ));
            incoming
                .send(ClientMsg::ClientHello {
                    protocol_version: PROTOCOL_VERSION,
                    terminal_id,
                    client_id: user,
                    desired_size: PtySize {
                        cols: 80,
                        rows: 24,
                        pixel_width: None,
                        pixel_height: None,
                    },
                    cell_size: None,
                    initial_scrollback: InitialScrollback::None,
                    resume_from: None,
                    role_hint: None,
                    capabilities: ClientCapabilities {
                        render_encodings: vec![RenderEncoding::Vt],
                        supports_scrollback: true,
                        supports_sixel: false,
                        supports_images: false,
                        kernel_originated_input: false,
                    },
                })
                .await
                .unwrap();
            assert!(matches!(
                outgoing.recv().await,
                Some(DaemonMsg::ServerHello { .. })
            ));
            incoming.send(ClientMsg::OwnerClaim).await.unwrap();
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if matches!(outgoing.recv().await, Some(DaemonMsg::OwnerChanged { owner_client_id: Some(id) }) if id == user) {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(
                entry.handle.owner_registry.lock().unwrap().current_owner(),
                Some(user)
            );
            *seam_human.lock().unwrap() = Some((pump.abort_handle(), incoming));
        })
    }));

    let opened = h
        .ok(
            "calm.terminal.open",
            json!({"program":"exec /bin/sh","request_id":"contended","claim":true}),
        )
        .await;
    let terminal = opened["terminal_id"].as_str().unwrap().to_owned();
    assert!(
        human.lock().unwrap().is_some(),
        "the seam ran inside the claim window"
    );
    assert_eq!(opened["claim"]["status"], "unavailable", "{opened}");
    assert_eq!(
        opened["claim"]["reason"], "terminal is controlled by another client",
        "{opened}"
    );
    assert!(opened["card_id"].is_string(), "creation facts survive");
    assert_eq!(opened["role"], "observer");
    assert_eq!(opened["control_id"], Value::Null);
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    assert_eq!(
        entry.handle.owner_registry.lock().unwrap().current_owner(),
        Some(user),
        "the human keeps control"
    );
    let view = h
        .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
        .await;
    assert_eq!(view["role"], "observer", "{view}");
    // A deliberate `control claim` keeps its takeover semantics.
    let claimed = h
        .ok(
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"claim"}),
        )
        .await;
    assert!(claimed["control_id"].is_string(), "{claimed}");
    assert_ne!(
        entry.handle.owner_registry.lock().unwrap().current_owner(),
        Some(user)
    );
    if let Some((pump, _incoming)) = human.lock().unwrap().take() {
        pump.abort();
    }
    h.stop(&terminal).await;
}

/// Track deletion goes through the quiesce path, not the reap helper.
#[tokio::test]
async fn hook_settings_file_is_removed_when_the_track_is_deleted() {
    use tower::ServiceExt;
    let h = Harness::start().await;
    let (opened, terminal) = open_fake_claude(&h, "hooks-track-delete").await;
    let card_id = opened["card_id"].as_str().unwrap().to_owned();
    let settings_path = h
        .state
        .codex
        .terminal_hook_settings_dir
        .join(format!("{card_id}.json"));
    assert!(settings_path.exists());
    let sibling = h.state.codex.terminal_hook_settings_dir.join("other.json");
    std::fs::write(&sibling, "{}").unwrap();
    let response = h
        .app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("DELETE")
                .uri(format!("/api/tracks/{}", h.track))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::NO_CONTENT);
    assert!(
        h.state.repo.track_get(&h.track).await.unwrap().is_none(),
        "the track row is gone"
    );
    assert!(
        !settings_path.exists(),
        "settings file must go with the track"
    );
    assert!(sibling.exists());
    assert!(h.state.terminal_renderer.get(&terminal).is_none());
    h.stop(&terminal).await;
}

/// The hook route keys on the card's durable execution identity (its terminal row), not on the patchable `cards.kind`.
#[tokio::test]
async fn hook_for_a_terminal_owning_card_stays_a_signal_after_a_kind_patch() {
    use tower::ServiceExt;
    let h = Harness::start().await;
    let (opened, terminal) = open_fake_claude(&h, "hooks-kind-patch").await;
    let card_id = opened["card_id"].as_str().unwrap().to_owned();
    let response = h
        .app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("PATCH")
                .uri(format!("/api/cards/{card_id}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(r#"{"kind":"codex"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let card = h.state.repo.card_get(&card_id).await.unwrap().unwrap();
    assert_eq!(card.kind, "codex", "the PATCH is accepted as it stands");
    assert!(
        h.state
            .repo
            .terminal_get_by_card(&card_id)
            .await
            .unwrap()
            .is_some(),
        "the terminal row survives the kind change"
    );

    let mut bus = h.state.events.subscribe();
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    let before = entry.signals.last_seq();
    let stop =
        json!({"hook_event_name":"Stop","session_id":"unresolved-session","message":"after patch"});
    assert_eq!(h.post_claude_hook(&card_id, &stop).await, 200);
    assert_eq!(entry.signals.last_seq(), before + 1, "the hook is a signal");
    // A duplicate delivery is still deduped by the ring, not the worker cache.
    assert_eq!(h.post_claude_hook(&card_id, &stop).await, 200);
    assert_eq!(entry.signals.last_seq(), before + 1);
    // Never worker state. (The MCP tools' admission fence refuses the retargeted card, so the ring is read directly.)
    let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
    while let Ok(Ok(envelope)) = tokio::time::timeout_at(deadline, bus.recv()).await {
        assert!(
            !matches!(
                envelope.event,
                Event::ClaudeHook { .. } | Event::CodexHook { .. }
            ),
            "a hook for a terminal-owning card must not be persisted as a hook event: {:?}",
            envelope.event
        );
    }
    h.stop(&terminal).await;
}

#[tokio::test]
async fn payload_patch_on_a_planner_terminal_keeps_the_marker_and_the_hook_stays_a_signal() {
    use tower::ServiceExt;
    let h = Harness::start().await;
    let (opened, terminal) = open_fake_claude(&h, "hooks-payload-patch").await;
    let card_id = opened["card_id"].as_str().unwrap().to_owned();
    let patch = |body: String| {
        axum::http::Request::builder()
            .method("PATCH")
            .uri(format!("/api/cards/{card_id}"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body))
            .unwrap()
    };

    // A client cannot write the marker, whatever the value.
    for body in [
        r#"{"payload":{"schemaVersion":1,"terminal_signals":true}}"#,
        r#"{"payload":{"schemaVersion":1,"terminal_signals":false}}"#,
    ] {
        let response = h.app.clone().oneshot(patch(body.to_owned())).await.unwrap();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::BAD_REQUEST,
            "{body}"
        );
    }

    // Replacing the whole payload without the key keeps it stamped.
    let response = h
        .app
        .clone()
        .oneshot(patch(
            r#"{"payload":{"schemaVersion":1,"terminal_id":"client-replaced"}}"#.to_owned(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let card = h.state.repo.card_get(&card_id).await.unwrap().unwrap();
    assert_eq!(card.payload["terminal_id"], "client-replaced");
    assert_eq!(
        card.payload[calm_server::validation::TERMINAL_SIGNALS_PAYLOAD_KEY],
        true,
        "the marker survives a whole-payload PATCH: {}",
        card.payload
    );

    let mut bus = h.state.events.subscribe();
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    let before = entry.signals.last_seq();
    let stop = json!({"hook_event_name":"Stop","session_id":"unresolved-session","message":"after payload patch"});
    assert_eq!(h.post_claude_hook(&card_id, &stop).await, 200);
    assert_eq!(entry.signals.last_seq(), before + 1, "the hook is a signal");
    let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
    while let Ok(Ok(envelope)) = tokio::time::timeout_at(deadline, bus.recv()).await {
        assert!(
            !matches!(
                envelope.event,
                Event::ClaudeHook { .. } | Event::CodexHook { .. }
            ),
            "a hook for a Planner terminal must not be persisted as a hook event: {:?}",
            envelope.event
        );
    }
    assert_eq!(h.persisted_hook_events().await, 0);
    h.stop(&terminal).await;
}

/// "Already owned by this connection" is decided against the owner registry, not the cached lease.
#[tokio::test]
async fn open_with_claim_replay_reports_takeover_before_the_owner_change_is_delivered() {
    let h = Harness::start().await;
    let opened = h
        .ok(
            "calm.terminal.open",
            json!({"program":"exec /bin/sh","request_id":"held-replay","claim":true}),
        )
        .await;
    let terminal = opened["terminal_id"].as_str().unwrap().to_owned();
    assert_eq!(opened["claim"]["status"], "claimed", "{opened}");
    let held = h
        .interaction()
        .hold_delivery(&terminal)
        .await
        .expect("the open established the Planner's client");
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    let user = uuid::Uuid::new_v4();
    let (pump, _incoming) = human_takeover(&entry, &terminal, user).await;
    // The Planner's cache still says owner: the takeover has not been applied.
    let stale = h
        .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
        .await;
    assert_eq!(stale["role"], "owner", "delivery is held: {stale}");

    let replayed = h
        .ok(
            "calm.terminal.open",
            json!({"program":"exec /bin/sh","request_id":"held-replay","claim":true}),
        )
        .await;
    assert_eq!(replayed["terminal_id"], terminal);
    assert_eq!(replayed["claim"]["status"], "unavailable", "{replayed}");
    assert_eq!(
        replayed["claim"]["reason"], "terminal is controlled by another client",
        "{replayed}"
    );
    assert_eq!(
        entry.handle.owner_registry.lock().unwrap().current_owner(),
        Some(user),
        "the human keeps control"
    );
    drop(held);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let view = h
                .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
                .await;
            if view["role"] == "observer" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the held OwnerChanged is applied once delivery resumes");
    // A genuine replay (the lease is really this connection's) keeps it.
    pump.abort();
    let reclaimed = h
        .ok(
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"claim"}),
        )
        .await;
    let control_id = reclaimed["control_id"].as_str().unwrap().to_owned();
    let replayed = h
        .ok(
            "calm.terminal.open",
            json!({"program":"exec /bin/sh","request_id":"held-replay","claim":true}),
        )
        .await;
    assert_eq!(replayed["claim"]["status"], "claimed", "{replayed}");
    assert_eq!(
        replayed["claim"]["control_id"], control_id,
        "no second claim"
    );
    h.stop(&terminal).await;
}

/// The verdict comes from the pump's reply (`Granted`, sent under the registry lock); delivery on the
/// Planner's connection is held from inside the claim window until the human has taken over, so the fold is deterministic.
#[tokio::test]
async fn open_with_claim_reports_a_takeover_folded_with_its_grant() {
    use std::sync::{Arc, Mutex};
    let h = Harness::start().await;
    let user = uuid::Uuid::new_v4();
    type HumanClient = (
        tokio::task::AbortHandle,
        tokio::sync::mpsc::Sender<calm_session::ClientMsg>,
    );
    let human: Arc<Mutex<Option<HumanClient>>> = Arc::new(Mutex::new(None));
    let seam_human = human.clone();
    let renderer = h.state.terminal_renderer.clone();
    let service = h.interaction();
    h.interaction()
        .set_claim_window_seam(Box::new(move |terminal_id: String| {
            Box::pin(async move {
                let held = service
                    .hold_delivery(&terminal_id)
                    .await
                    .expect("the open established the Planner's client");
                let entry = renderer.get(&terminal_id).unwrap();
                tokio::spawn(async move {
                    // The Planner's claim-if-unowned is granted (the registry
                    // names an owner) while its OwnerChanged sits behind the gate.
                    let start = std::time::Instant::now();
                    while entry
                        .handle
                        .owner_registry
                        .lock()
                        .unwrap()
                        .current_owner()
                        .is_none()
                    {
                        assert!(
                            start.elapsed() < Duration::from_secs(5),
                            "grant never landed"
                        );
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    let client = human_takeover(&entry, &terminal_id, user).await;
                    *seam_human.lock().unwrap() = Some(client);
                    drop(held);
                });
            })
        }));
    let started = std::time::Instant::now();
    let opened = h
        .ok(
            "calm.terminal.open",
            json!({"program":"exec /bin/sh","request_id":"folded","claim":true}),
        )
        .await;
    let terminal = opened["terminal_id"].as_str().unwrap().to_owned();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the claim must not idle to its budget"
    );
    assert!(human.lock().unwrap().is_some(), "the human took over");
    assert_eq!(opened["claim"]["status"], "unavailable", "{opened}");
    assert_eq!(
        opened["claim"]["reason"], "terminal control was taken by another client",
        "{opened}"
    );
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    assert_eq!(
        entry.handle.owner_registry.lock().unwrap().current_owner(),
        Some(user),
        "the human keeps control"
    );
    if let Some((pump, _incoming)) = human.lock().unwrap().take() {
        pump.abort();
    }
    h.stop(&terminal).await;
}

/// Owning a terminal row must never route a worker's hook to the terminal-signal branch.
#[tokio::test]
#[allow(deprecated)] // the state's role cache is the one `enforce_role` reads
async fn codex_worker_card_hooks_are_still_persisted() {
    let h = Harness::start().await;
    let card_id = new_id();
    let mut tx = h.sql.pool().begin().await.unwrap();
    let (card, term, _token) = calm_server::db::sqlite::card_with_codex_create_tx(
        &mut tx,
        card_id.clone(),
        &new_id(),
        None,
        h.track.clone().into(),
        None,
        None,
        h.root.path().to_str().unwrap().into(),
        json!({}),
        None,
        None,
        None,
        CardRole::Worker,
        true,
        h.state.write().role_cache(),
        RequestTheme::default_dark(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(card.kind, "codex");
    assert!(
        !calm_server::routes::codex::is_planner_terminal_card(&card),
        "{}",
        card.payload
    );
    let mut bus = h.state.events.subscribe();
    let stop = json!({"hook_event_name":"Stop","session_id":"codex-worker-session"});
    assert_eq!(h.post_codex_hook(&card_id, &stop).await, 204);
    let hooked = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let envelope = bus.recv().await.unwrap();
            if let Event::CodexHook {
                card_id: hooked,
                kind,
                ..
            } = envelope.event
            {
                assert_eq!(hooked.as_str(), card_id);
                assert_eq!(kind, "hook.codex.stop");
                break;
            }
        }
    })
    .await;
    assert!(
        hooked.is_ok(),
        "the codex hook must be broadcast as worker state"
    );
    assert_eq!(
        h.persisted_hook_events().await,
        1,
        "persisted, not ring-only"
    );
    assert!(
        h.state
            .repo
            .overlays_for("card", &card_id)
            .await
            .unwrap()
            .is_empty(),
        "no kernel projection writes a card overlay from a hook"
    );
    // The same body again is deduped by the worker cache, not appended twice.
    assert_eq!(h.post_codex_hook(&card_id, &stop).await, 204);
    assert_eq!(h.persisted_hook_events().await, 1);
    h.stop(&term.id).await;
}

#[tokio::test]
#[allow(deprecated)] // the state's role cache is the one `enforce_role` reads
async fn claude_worker_card_hooks_are_still_persisted() {
    let h = Harness::start().await;
    let card_id = new_id();
    let mut tx = h.sql.pool().begin().await.unwrap();
    let (card, term) = calm_server::db::sqlite::card_with_claude_create_tx(
        &mut tx,
        card_id.clone(),
        &new_id(),
        h.track.clone().into(),
        None,
        None,
        "claude".into(),
        h.root.path().to_str().unwrap().into(),
        json!({}),
        None,
        None,
        None,
        h.root.path().join("settings.json").display().to_string(),
        new_id(),
        CardRole::Worker,
        true,
        h.state.write().role_cache(),
        RequestTheme::default_dark(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(card.kind, "claude");
    assert!(
        !calm_server::routes::codex::is_planner_terminal_card(&card),
        "{}",
        card.payload
    );
    let mut bus = h.state.events.subscribe();
    let stop = json!({"hook_event_name":"Stop","session_id":"claude-worker-session"});
    assert_eq!(h.post_claude_hook(&card_id, &stop).await, 200);
    let hooked = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let envelope = bus.recv().await.unwrap();
            if let Event::ClaudeHook {
                card_id: hooked,
                kind,
                ..
            } = envelope.event
            {
                assert_eq!(hooked.as_str(), card_id);
                assert_eq!(kind, "hook.claude.stop");
                break;
            }
        }
    })
    .await;
    assert!(
        hooked.is_ok(),
        "the Claude hook must be broadcast as worker state"
    );
    assert_eq!(
        h.persisted_hook_events().await,
        1,
        "persisted, not ring-only"
    );
    assert!(
        h.state
            .repo
            .overlays_for("card", &card_id)
            .await
            .unwrap()
            .is_empty(),
        "no kernel projection writes a card overlay from a hook"
    );
    h.stop(&term.id).await;
}

#[tokio::test]
async fn delayed_hook_after_kind_patch_and_terminal_reap_stays_a_signal() {
    use tower::ServiceExt;
    let h = Harness::start().await;
    let (opened, terminal) = open_fake_claude(&h, "hooks-reaped").await;
    let card_id = opened["card_id"].as_str().unwrap().to_owned();
    let response = h
        .app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("PATCH")
                .uri(format!("/api/cards/{card_id}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(r#"{"kind":"codex"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let term = h
        .state
        .repo
        .terminal_get_by_card(&card_id)
        .await
        .unwrap()
        .unwrap();
    calm_server::terminal_sweeper::reap_terminal_artifacts_with_renderer(
        Some(h.state.terminal_renderer.as_ref()),
        &term,
    )
    .await;
    h.state.repo.terminal_delete(&term.id).await.unwrap();
    let card = h.state.repo.card_get(&card_id).await.unwrap().unwrap();
    assert_eq!(card.kind, "codex");
    assert!(
        h.state
            .repo
            .terminal_get_by_card(&card_id)
            .await
            .unwrap()
            .is_none(),
        "the terminal row is gone"
    );
    assert!(
        calm_server::routes::codex::is_planner_terminal_card(&card),
        "the marker survives the kind PATCH: {}",
        card.payload
    );
    let mut bus = h.state.events.subscribe();
    let before = h.persisted_hook_events().await;
    let stop = json!({"hook_event_name":"Stop","session_id":"late-session","message":"late"});
    assert_eq!(h.post_claude_hook(&card_id, &stop).await, 200);
    let codex_stop = json!({"hook_event_name":"Stop","session_id":"late-codex"});
    assert_eq!(h.post_codex_hook(&card_id, &codex_stop).await, 204);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
    while let Ok(Ok(envelope)) = tokio::time::timeout_at(deadline, bus.recv()).await {
        assert!(
            !matches!(
                envelope.event,
                Event::ClaudeHook { .. } | Event::CodexHook { .. }
            ),
            "a delayed hook for a Planner-opened terminal must not become worker state: {:?}",
            envelope.event
        );
    }
    assert_eq!(h.persisted_hook_events().await, before);
    assert!(
        h.state
            .repo
            .overlays_for("card", &card_id)
            .await
            .unwrap()
            .is_empty(),
        "no card overlay"
    );
    h.stop(&terminal).await;
}

/// A shell that prints READY and then `exec`s into `sleep`, so the pid the
/// terminal row persisted IS the process the TERM must reach.
const SLEEPER: &str = "printf 'READY\\n'; exec sleep 600";

/// One opened PTY worker session and the rows the sweep is judged on.
struct PtySession {
    card: String,
    terminal: String,
    session: String,
    pid: i64,
    created_at_ms: i64,
}

/// `calm.terminal.open` (the production mint: card + terminal row + a
/// `running` session with `terminal_run_id`), waited until the child runs.
async fn open_sleeper(h: &Harness, request_id: &str) -> PtySession {
    open_program(h, request_id, SLEEPER, true).await
}

/// `open_sleeper` for any `/bin/sh -c` program; `ready` waits for the READY
/// line the program prints first (a program that prints nothing passes
/// `false`).
async fn open_program(h: &Harness, request_id: &str, program: &str, ready: bool) -> PtySession {
    let opened = h
        .ok(
            "calm.terminal.open",
            json!({"program":program,"request_id":request_id}),
        )
        .await;
    let terminal = opened["terminal_id"].as_str().unwrap().to_owned();
    let card = opened["card_id"].as_str().unwrap().to_owned();
    if ready {
        h.observe_text(&terminal, "READY").await;
    }
    let (session, state, created_at_ms): (String, String, i64) = sqlx::query_as(
        "SELECT id, state, created_at_ms FROM worker_sessions \
          WHERE card_id = ?1 AND terminal_run_id = ?2",
    )
    .bind(&card)
    .bind(&terminal)
    .fetch_one(h.sql.pool())
    .await
    .unwrap();
    assert_eq!(state, "running", "the open mints a running PTY session");
    let pid = h
        .state
        .repo
        .terminal_get(&terminal)
        .await
        .unwrap()
        .unwrap()
        .pid
        .expect("the spawn persisted the child pid");
    assert!(pid_alive(pid), "the child runs before the sweep");
    PtySession {
        card,
        terminal,
        session,
        pid,
        created_at_ms,
    }
}

/// `(state, completed_at_ms, updated_at_ms)` of a worker session row.
async fn session_row(h: &Harness, id: &str) -> (String, Option<i64>, i64) {
    sqlx::query_as(
        "SELECT state, completed_at_ms, updated_at_ms FROM worker_sessions WHERE id = ?1",
    )
    .bind(id)
    .fetch_one(h.sql.pool())
    .await
    .unwrap()
}

/// `(exit_code, signal_killed)` of a terminal row.
async fn terminal_exit(h: &Harness, id: &str) -> (Option<i32>, bool) {
    let term = h.state.repo.terminal_get(id).await.unwrap().unwrap();
    (term.exit_code, term.signal_killed)
}

/// Bounded wait for the attach reader to persist the terminal's exit.
async fn await_terminal_exit(h: &Harness, id: &str) -> (Option<i32>, bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (exit_code, signal_killed) = terminal_exit(h, id).await;
        if exit_code.is_some() || signal_killed {
            return (exit_code, signal_killed);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "terminal {id} never recorded an exit"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// `kill(pid, 0)` succeeds and the process is not a zombie.
fn pid_alive(pid: i64) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    if kill(Pid::from_raw(pid as i32), None).is_err() {
        return false;
    }
    let zombie = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(") ")
                .map(|(_, rest)| rest.starts_with('Z'))
        })
        .unwrap_or(false);
    !zombie
}

async fn await_pid_gone(pid: i64) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while pid_alive(pid) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "pid {pid} is still alive after the sweep"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// `lifecycle → done` through `track_update_tx` (the same UPDATE stamps `terminal_at`). Returns `terminal_at`.
async fn complete_track(h: &Harness, track_id: &str) -> i64 {
    use calm_server::model::{TrackLifecycle, TrackPatch};
    h.sql
        .track_update(
            track_id,
            TrackPatch {
                lifecycle: Some(TrackLifecycle::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .terminal_at
        .expect("done stamps terminal_at")
}

/// The persisted worker-session and terminal rows the sweep may touch, plus
/// the event count — for the no-write assertion.
async fn write_snapshot(
    h: &Harness,
    p: &PtySession,
) -> (
    Vec<(String, String, Option<i64>, i64)>,
    Vec<(String, Option<i64>, bool)>,
    i64,
) {
    let sessions = sqlx::query_as(
        "SELECT id, state, completed_at_ms, updated_at_ms FROM worker_sessions WHERE card_id = ?1 ORDER BY id",
    )
    .bind(&p.card)
    .fetch_all(h.sql.pool())
    .await
    .unwrap();
    let terminals = sqlx::query_as(
        "SELECT id, exit_code, signal_killed FROM terminals WHERE card_id = ?1 ORDER BY id",
    )
    .bind(&p.card)
    .fetch_all(h.sql.pool())
    .await
    .unwrap();
    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(h.sql.pool())
        .await
        .unwrap();
    (sessions, terminals, events)
}

/// The set's own SELECT (`terminal_sweeper::COMPLETED_TRACK_LIVE_SESSIONS_SQL`)
/// as the sweeper runs it: the session ids it would end right now.
async fn sweep_set(h: &Harness) -> Vec<String> {
    sqlx::query_scalar(calm_server::terminal_sweeper::COMPLETED_TRACK_LIVE_SESSIONS_SQL)
        .fetch_all(h.sql.pool())
        .await
        .unwrap()
}

/// The single candidate of the set, read the way the sweep reads it (all
/// five columns, `thread_id` included), for driving
/// `end_completed_track_session` with a candidate of the test's own.
async fn sole_candidate(h: &Harness) -> calm_server::terminal_sweeper::CompletedTrackSession {
    let (id, provider, card_id, terminal_id, thread_id): (
        String,
        String,
        String,
        String,
        Option<String>,
    ) = sqlx::query_as(calm_server::terminal_sweeper::COMPLETED_TRACK_LIVE_SESSIONS_SQL)
        .fetch_one(h.sql.pool())
        .await
        .unwrap();
    calm_server::terminal_sweeper::CompletedTrackSession {
        id,
        provider,
        card_id,
        terminal_id,
        thread_id,
    }
}

/// Turn a sleeper's session row into a shared-daemon codex worker (`provider = 'codex'`, `thread_id = 't-1'`)
/// and register `turn-1` as the thread's active turn on the fake app-server.
async fn make_codex_with_active_turn(h: &Harness, p: &PtySession) {
    sqlx::query("UPDATE worker_sessions SET provider = 'codex', thread_id = 't-1' WHERE id = ?1")
        .bind(&p.session)
        .execute(h.sql.pool())
        .await
        .unwrap();
    h.state
        .shared_codex_appserver
        .set_active_turn_for_test("t-1", "turn-1");
    assert!(
        h.state
            .shared_codex_appserver
            .interrupted_turns_for_test()
            .is_empty(),
        "nothing interrupted before the call"
    );
}

/// Written `exited` first, then killed: the attach reader records `signal_killed = 1`, the pid is gone, the renderer entry dropped.
#[tokio::test]
async fn done_track_running_pty_session_is_torn_down() {
    let h = Harness::start().await;
    let p = open_sleeper(&h, "done-teardown").await;
    let terminal_at = complete_track(&h, &h.track).await;
    assert!(
        p.created_at_ms <= terminal_at,
        "the session predates the completion: {} <= {terminal_at}",
        p.created_at_ms
    );
    assert_eq!(sweep_set(&h).await, vec![p.session.clone()]);

    calm_server::terminal_sweeper::sweep(&h.state)
        .await
        .unwrap();

    let (state, completed_at, _) = session_row(&h, &p.session).await;
    assert_eq!(state, "exited", "the sweep ends the session as `exited`");
    assert!(completed_at.is_some());
    let (exit_code, signal_killed) = await_terminal_exit(&h, &p.terminal).await;
    assert_eq!(
        (exit_code, signal_killed),
        (None, true),
        "the reader recorded a signalled exit"
    );
    await_pid_gone(p.pid).await;
    assert!(
        h.state.terminal_renderer.get(&p.terminal).is_none(),
        "the renderer entry is dropped"
    );
    assert!(sweep_set(&h).await.is_empty(), "nothing left to end");
    h.stop(&p.terminal).await;
}

/// `created_at_ms > terminal_at`: the user's own new work on a done track.
#[tokio::test]
async fn session_started_after_done_survives_sweep() {
    let h = Harness::start().await;
    let terminal_at = complete_track(&h, &h.track).await;
    // The mint stamps `now_ms()`; a millisecond of slack keeps the
    // precondition below strict rather than equal.
    tokio::time::sleep(Duration::from_millis(5)).await;
    let p = open_sleeper(&h, "after-done").await;
    assert!(
        p.created_at_ms > terminal_at,
        "the session postdates the completion: {} > {terminal_at}",
        p.created_at_ms
    );
    assert!(sweep_set(&h).await.is_empty(), "not in the set");

    calm_server::terminal_sweeper::sweep(&h.state)
        .await
        .unwrap();

    let (state, completed_at, _) = session_row(&h, &p.session).await;
    assert_eq!(state, "running", "a post-completion session survives");
    assert_eq!(completed_at, None);
    assert_eq!(terminal_exit(&h, &p.terminal).await, (None, false));
    assert!(pid_alive(p.pid), "the process is still there");
    assert!(h.state.terminal_renderer.get(&p.terminal).is_some());
    h.stop(&p.terminal).await;
}

/// The `NOT EXISTS` arm: once the task settles, the next sweep ends the worker.
#[tokio::test]
async fn in_flight_task_worker_survives_sweep() {
    let h = Harness::start().await;
    let p = open_sleeper(&h, "in-flight").await;
    let task_id = format!("{}:build", h.track);
    sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id, \
                           declared_by,created_at_ms,updated_at_ms) \
         VALUES (?1,?2,'build','terminal','test','[]','running',?3,'user',?4,?4)",
    )
    .bind(&task_id)
    .bind(&h.track)
    .bind(&p.card)
    .bind(calm_server::model::now_ms())
    .execute(h.sql.pool())
    .await
    .unwrap();
    let current: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM current_tasks WHERE id = ?1 AND status = 'running' AND worker_card_id = ?2",
    )
    .bind(&task_id)
    .bind(&p.card)
    .fetch_one(h.sql.pool())
    .await
    .unwrap();
    assert_eq!(current, 1, "the attempt is the current one");
    complete_track(&h, &h.track).await;
    assert!(
        sweep_set(&h).await.is_empty(),
        "an in-flight worker is not in the set"
    );

    calm_server::terminal_sweeper::sweep(&h.state)
        .await
        .unwrap();
    let (state, _, _) = session_row(&h, &p.session).await;
    assert_eq!(state, "running", "the in-flight worker survives");
    assert!(pid_alive(p.pid));

    // The task settles: the next sweep ends the worker.
    sqlx::query("UPDATE tasks SET status = 'done', finished_at_ms = ?2 WHERE id = ?1")
        .bind(&task_id)
        .bind(calm_server::model::now_ms())
        .execute(h.sql.pool())
        .await
        .unwrap();
    assert_eq!(sweep_set(&h).await, vec![p.session.clone()]);
    calm_server::terminal_sweeper::sweep(&h.state)
        .await
        .unwrap();
    let (state, _, _) = session_row(&h, &p.session).await;
    assert_eq!(state, "exited", "settled ⇒ ended");
    assert_eq!(await_terminal_exit(&h, &p.terminal).await, (None, true));
    await_pid_gone(p.pid).await;
    h.stop(&p.terminal).await;
}

/// A harness row is outside the set by two independent conditions — never `running`, no `terminal_run_id` — so no single-factor change admits it.
#[tokio::test]
async fn harness_session_is_never_in_the_sweep_set() {
    use calm_server::db::sqlite::{card_create_with_id_tx, session_start_runtime_tx};
    use calm_server::harness::HarnessSnapshot;
    use calm_server::model::NewCard;
    use calm_server::model::NewTrack;
    use calm_server::session_projection_repo::{
        AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    let h = Harness::start().await;
    // One planner-role card per track: the harness track already has one,
    // so the idle harness planner gets its own track (same area).
    let track = h
        .sql
        .track_create(NewTrack {
            template_input: None,
            area_id: h.area_id.clone().into(),
            title: "harness-set".into(),
            sort: None,
            cwd: h.root.path().to_str().unwrap().into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap()
        .id
        .to_string();
    let planner_card = new_id();
    let planner_ws = new_id();
    let mut tx = h.sql.pool().begin().await.unwrap();
    card_create_with_id_tx(
        &mut tx,
        planner_card.clone(),
        NewCard {
            track_id: track.clone().into(),
            title: None,
            kind: "planner".into(),
            sort: None,
            payload: json!({}),
        },
        CardRole::Planner,
        true,
        &h.state.card_role_cache,
    )
    .await
    .unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: planner_ws.clone(),
            card_id: planner_card.clone(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some("th-planner".into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(
                serde_json::to_value(HarnessSnapshot::initial(0, vec![])).unwrap(),
            ),
            spawn_op_id: None,
            now_ms: 1_000,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    complete_track(&h, &track).await;

    // Both excluding facts hold on the row itself.
    let (state, terminal_run_id, mode): (String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT state, terminal_run_id, json_extract(handle_state_json, '$.mode') \
           FROM worker_sessions WHERE id = ?1",
    )
    .bind(&planner_ws)
    .fetch_one(h.sql.pool())
    .await
    .unwrap();
    assert_eq!(mode.as_deref(), Some("harness"));
    assert_ne!(
        state, "running",
        "a harness row is idle / turn_pending, never `running`"
    );
    assert_eq!(terminal_run_id, None, "a harness row has no PTY");
    assert!(
        sweep_set(&h).await.is_empty(),
        "structurally outside the set"
    );

    calm_server::terminal_sweeper::sweep(&h.state)
        .await
        .unwrap();
    let (state, completed_at, _) = session_row(&h, &planner_ws).await;
    assert_eq!(
        state, "idle",
        "the planner can still be asked on a done track"
    );
    assert_eq!(completed_at, None);
    // The harness' own Planner card session (`starting`, PTY-backed) is
    // outside the set as well.
    let (state, _, _) = session_row(&h, &h.session_id).await;
    assert_eq!(state, "starting");
}

/// `archived_at IS NOT NULL` with `created_at_ms <= archived_at`, whatever the lifecycle says.
#[tokio::test]
async fn archived_track_session_is_torn_down() {
    use calm_server::model::TrackPatch;
    let h = Harness::start().await;
    let p = open_sleeper(&h, "archived").await;
    let track = h
        .sql
        .track_update(
            &h.track,
            TrackPatch {
                archived_at: Some(Some(calm_server::model::now_ms())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        track.lifecycle.as_db_str(),
        "draft",
        "archiving leaves the lifecycle"
    );
    assert!(p.created_at_ms <= track.archived_at.unwrap());
    assert_eq!(sweep_set(&h).await, vec![p.session.clone()]);

    calm_server::terminal_sweeper::sweep(&h.state)
        .await
        .unwrap();

    let (state, _, _) = session_row(&h, &p.session).await;
    assert_eq!(state, "exited");
    assert_eq!(await_terminal_exit(&h, &p.terminal).await, (None, true));
    await_pid_gone(p.pid).await;
    assert!(h.state.terminal_renderer.get(&p.terminal).is_none());
    h.stop(&p.terminal).await;
}

/// The terminal row is still inside the orphan arm's creation grace, so that arm is quiet too.
#[tokio::test]
async fn sweep_is_idempotent() {
    let h = Harness::start().await;
    let p = open_sleeper(&h, "idempotent").await;
    complete_track(&h, &h.track).await;
    calm_server::terminal_sweeper::sweep(&h.state)
        .await
        .unwrap();
    let (state, _, _) = session_row(&h, &p.session).await;
    assert_eq!(state, "exited");
    await_terminal_exit(&h, &p.terminal).await;
    await_pid_gone(p.pid).await;

    let before = write_snapshot(&h, &p).await;
    assert!(sweep_set(&h).await.is_empty());
    calm_server::terminal_sweeper::sweep(&h.state)
        .await
        .unwrap();
    let after = write_snapshot(&h, &p).await;
    assert_eq!(after, before, "the second sweep writes nothing");
    assert!(
        h.state
            .repo
            .terminal_get(&p.terminal)
            .await
            .unwrap()
            .is_some(),
        "the terminal row is not deleted by this arm"
    );
    h.stop(&p.terminal).await;
}

/// Reopening (`done → planning`) clears `terminal_at`; the claim re-runs the set predicate by id and leaves the row alone.
#[tokio::test]
async fn reopened_track_session_survives_a_stale_candidate() {
    use calm_server::model::{TrackLifecycle, TrackPatch};
    use calm_server::terminal_sweeper::end_completed_track_session;
    let h = Harness::start().await;
    let p = open_sleeper(&h, "stale-candidate").await;
    complete_track(&h, &h.track).await;
    // The candidate, read the way the sweep reads it.
    let stale = sole_candidate(&h).await;
    assert_eq!(stale.id, p.session);
    assert_eq!(stale.terminal_id, p.terminal);

    // Reopened before the candidate is acted on.
    let reopened = h
        .sql
        .track_update(
            &h.track,
            TrackPatch {
                lifecycle: Some(TrackLifecycle::Planning),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(reopened.terminal_at, None, "reopening clears terminal_at");
    assert!(sweep_set(&h).await.is_empty(), "no longer in the set");

    end_completed_track_session(&h.state, &stale).await.unwrap();

    let (state, completed_at, _) = session_row(&h, &p.session).await;
    assert_eq!(state, "running", "a stale candidate is not ended");
    assert_eq!(completed_at, None);
    assert_eq!(terminal_exit(&h, &p.terminal).await, (None, false));
    assert!(pid_alive(p.pid), "the process is still there");
    assert!(h.state.terminal_renderer.get(&p.terminal).is_some());
    h.stop(&p.terminal).await;
}

/// The reopen route takes no operation lock; only the claim serializes it. The reopen goes through the `before_write`
/// seam, not timing: `require_safe` is an IMMEDIATE transaction before the seam, so a reopen held open parks the sweeper there.
#[tokio::test]
async fn reopen_committed_during_the_claim_is_honoured() {
    use calm_server::model::{TrackLifecycle, TrackPatch};
    use calm_server::terminal_sweeper::end_completed_track_session_before_write_for_test;
    let h = Harness::start_with_fake_codex().await;
    let p = open_sleeper(&h, "reopen-in-window").await;
    make_codex_with_active_turn(&h, &p).await;
    complete_track(&h, &h.track).await;
    let candidate = sole_candidate(&h).await;
    assert_eq!(candidate.id, p.session);
    assert_eq!(candidate.thread_id.as_deref(), Some("t-1"));
    assert_eq!(
        sweep_set(&h).await,
        vec![p.session.clone()],
        "in the set before the call"
    );
    let interrupted_before = h.state.shared_codex_appserver.interrupted_turns_for_test();

    end_completed_track_session_before_write_for_test(&h.state, &candidate, || async {
        // The guard has passed; the reopen commits now, before the
        // IMMEDIATE write begins.
        let reopened = h
            .sql
            .track_update(
                &h.track,
                TrackPatch {
                    lifecycle: Some(TrackLifecycle::Planning),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(reopened.terminal_at, None, "reopening clears terminal_at");
    })
    .await
    .unwrap();

    let (state, completed_at, _) = session_row(&h, &p.session).await;
    assert_eq!(
        state, "running",
        "a reopen committed before the write is honoured"
    );
    assert_eq!(completed_at, None);
    assert_eq!(terminal_exit(&h, &p.terminal).await, (None, false));
    assert!(pid_alive(p.pid), "the process is still there");
    assert!(h.state.terminal_renderer.get(&p.terminal).is_some());
    let terminal_at: Option<i64> =
        sqlx::query_scalar("SELECT terminal_at FROM tracks WHERE id = ?1")
            .bind(&h.track)
            .fetch_one(h.sql.pool())
            .await
            .unwrap();
    assert_eq!(terminal_at, None, "the track stays reopened");
    assert_eq!(
        h.state.shared_codex_appserver.interrupted_turns_for_test(),
        interrupted_before,
        "a reopen that defeats the claim must not interrupt the turn"
    );
    assert_eq!(
        h.state.shared_codex_appserver.active_turn_for_test("t-1"),
        Some("turn-1".to_string()),
        "the turn is still active"
    );
    h.stop(&p.terminal).await;
}

/// The interrupt is addressed by the captured thread id, after the claim wrote the row `exited`; nothing is interrupted at the seam before the write.
#[tokio::test]
async fn codex_candidate_is_interrupted_after_the_claim() {
    use calm_server::terminal_sweeper::end_completed_track_session_before_write_for_test;
    let h = Harness::start_with_fake_codex().await;
    let p = open_sleeper(&h, "codex-interrupt").await;
    make_codex_with_active_turn(&h, &p).await;
    complete_track(&h, &h.track).await;
    let candidate = sole_candidate(&h).await;
    assert_eq!(candidate.id, p.session);
    assert_eq!(candidate.provider, "codex");
    assert_eq!(candidate.thread_id.as_deref(), Some("t-1"));

    end_completed_track_session_before_write_for_test(&h.state, &candidate, || async {
        assert!(
            h.state
                .shared_codex_appserver
                .interrupted_turns_for_test()
                .is_empty(),
            "the interrupt comes after the claim, never before the write"
        );
    })
    .await
    .unwrap();

    let (state, completed_at, _) = session_row(&h, &p.session).await;
    assert_eq!(state, "exited");
    assert!(completed_at.is_some());
    assert_eq!(
        h.state.shared_codex_appserver.interrupted_turns_for_test(),
        vec![("t-1".to_string(), "turn-1".to_string())],
        "the active turn of the captured thread is interrupted once"
    );
    assert_eq!(
        h.state.shared_codex_appserver.active_turn_for_test("t-1"),
        None,
        "the interrupt cleared the active turn"
    );
    assert_eq!(await_terminal_exit(&h, &p.terminal).await, (None, true));
    await_pid_gone(p.pid).await;
    assert!(h.state.terminal_renderer.get(&p.terminal).is_none());
    h.stop(&p.terminal).await;
}

/// Simulated with a second `AppState` over the same repo and supervisor socket (a fresh, empty registry); the old
/// registry's attach reader is severed the way a dead process would sever it.
#[tokio::test]
async fn reattach_then_reap_after_registry_reset() {
    let h = Harness::start().await;
    let p = open_sleeper(&h, "registry-reset").await;
    complete_track(&h, &h.track).await;
    let old_entry = h.state.terminal_renderer.get(&p.terminal).unwrap();
    old_entry.disconnect_output_source_for_test();

    let repo: Arc<dyn calm_server::db::Repo> = h.sql.clone();
    let fresh = calm_server::state::AppState::from_parts(
        repo,
        h.state.events.clone(),
        h.state.daemon.clone(),
        h.state.plugin.clone(),
        h.state.codex.clone(),
        Some(h.state.card_role_cache.clone()),
        Some(h.state.track_area_cache.clone()),
    );
    assert!(
        fresh.terminal_renderer.get(&p.terminal).is_none(),
        "the fresh registry is empty"
    );
    assert!(pid_alive(p.pid), "the PTY survived the 'restart'");
    assert_eq!(sweep_set(&h).await, vec![p.session.clone()]);

    calm_server::terminal_sweeper::sweep(&fresh).await.unwrap();

    let (state, _, _) = session_row(&h, &p.session).await;
    assert_eq!(state, "exited");
    assert_eq!(await_terminal_exit(&h, &p.terminal).await, (None, true));
    await_pid_gone(p.pid).await;
    assert!(
        fresh.terminal_renderer.get(&p.terminal).is_none(),
        "the reattached entry is dropped again after the reap"
    );
    h.stop(&p.terminal).await;
}

/// The projector over the harness' repo, bus and registries — the shape `AppState::new` spawns.
fn projector(h: &Harness) -> TrackActivityProjector {
    let repo: Arc<dyn calm_server::db::Repo> = h.sql.clone();
    TrackActivityProjector::new(
        repo,
        h.state.events.clone(),
        h.state.write().clone(),
        h.state.harness.clone(),
        h.state.terminal_renderer.clone(),
    )
    .expect("sqlite-backed repo")
}

/// The registry's last-output stamp of `terminal`.
fn last_output_ms(h: &Harness, terminal: &str) -> Option<i64> {
    h.state.terminal_renderer.last_output_ms(terminal)
}

/// READY may arrive in the attach replay, which is not a frame.
async fn await_output(h: &Harness, terminal: &str) -> i64 {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(at) = last_output_ms(h, terminal) {
            return at;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "terminal {terminal} never delivered an Output frame to the reader"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// One recomputation of the harness track.
async fn recompute(h: &Harness, projector: &TrackActivityProjector) -> ActivityPayload {
    match projector.recompute_track(&h.track).await.unwrap() {
        Recompute::NoTrack => panic!("track {} vanished", h.track),
        Recompute::Unchanged(p) | Recompute::Written(p) => p,
    }
}

/// The stored `kernel/track/activity` row of the harness track.
async fn stored_activity(h: &Harness) -> Option<ActivityPayload> {
    h.state
        .repo
        .overlays_for("track", &h.track)
        .await
        .unwrap()
        .into_iter()
        .find(|o| o.kind == "activity" && o.plugin_id == "kernel")
        .map(|o| serde_json::from_value(o.payload).unwrap())
}

/// The loop tests' only clock, far inside the 30 s tick.
async fn await_activity(
    h: &Harness,
    what: &str,
    bound: Duration,
    pred: impl Fn(&ActivityPayload) -> bool,
) -> ActivityPayload {
    let deadline = tokio::time::Instant::now() + bound;
    loop {
        let stored = stored_activity(h).await;
        if let Some(p) = stored.as_ref().filter(|p| pred(p)) {
            return p.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what} on {}: {stored:?}",
            h.track
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn card_state(p: &ActivityPayload, card_id: &str) -> Option<CardState> {
    p.cards
        .iter()
        .find(|c| c.card_id == card_id)
        .map(|c| c.state)
}

/// Prints READY, then a line every 200 ms until `stop` exists, then exits 0.
fn ticker_program(stop: &std::path::Path) -> String {
    format!(
        "printf 'READY\\n'; while [ ! -e '{}' ]; do echo tick; sleep 0.2; done; exit 0",
        stop.display()
    )
}

/// Prints READY and `n` more lines 100 ms apart, then runs `after`.
fn burst_program(n: u32, after: &str) -> String {
    format!(
        "printf 'READY\\n'; i=0; while [ $i -lt {n} ]; do i=$((i+1)); echo tick $i; sleep 0.1; done; {after}"
    )
}

/// The reader's ephemeral completion after a signalled exit: the session row reaches `failed`.
async fn await_session_state(h: &Harness, id: &str, expected: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (state, _, _) = session_row(h, id).await;
        if state == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session {id} is `{state}`, never `{expected}`"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn term(pid: i64) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), Signal::SIGTERM).unwrap();
}

/// The projector loop is running: the output's leading edge (quiet → output) wakes it, far inside the 30 s tick.
/// The program is quiet for its first two seconds, so the open's bus wakes (`WorkerSessionStarted`, the
/// status write) recompute on an empty replay and no frame; only the edge can light `working` inside the window.
#[tokio::test]
async fn interactive_terminal_output_is_working() {
    let h = Harness::start().await;
    let loop_task = tokio::spawn(projector(&h).run());
    let seeded = await_activity(&h, "the boot sweep's row", Duration::from_secs(3), |_| true).await;
    assert!(!seeded.working, "{seeded:?}");
    let stop = h.root.path().join("stop-output");
    let p = open_program(
        &h,
        "output-working",
        &format!("sleep 2; {}", ticker_program(&stop)),
        false,
    )
    .await;
    assert_eq!(
        last_output_ms(&h, &p.terminal),
        None,
        "an empty replay and no frame yet: the bus wakes of the open read no output"
    );
    let quiet = stored_activity(&h).await.expect("the boot row");
    assert!(!quiet.working, "before the first frame: {quiet:?}");
    let a = await_activity(
        &h,
        "working after PTY output",
        Duration::from_secs(5),
        |a| a.working,
    )
    .await;
    assert_eq!(card_state(&a, &p.card), Some(CardState::Working), "{a:?}");
    assert_eq!(a.attention, Attention::None);
    assert!(a.items.is_empty());
    loop_task.abort();
    h.stop(&p.terminal).await;
}

/// After the output stops, one reconcile drops `working` and the high-water mark is the last output
/// instant — a single unread that a second reconcile does not move.
#[tokio::test]
async fn quiet_then_tick_is_unread_once() {
    let h = Harness::start().await;
    let pj = projector(&h);
    let opened_at = calm_server::model::now_ms();
    let p = open_program(&h, "quiet-tick", &burst_program(8, "exec sleep 600"), true).await;
    h.observe_text(&p.terminal, "tick 8").await;
    let quiet_since = calm_server::model::now_ms();
    let a = recompute(&h, &pj).await;
    assert!(a.working, "output within N s: {a:?}");
    assert_eq!(card_state(&a, &p.card), Some(CardState::Working));

    tokio::time::sleep(Duration::from_millis(5_500)).await;
    pj.reconcile_all().await;
    let a = stored_activity(&h).await.unwrap();
    assert!(!a.working, "quiet after N s: {a:?}");
    assert!(a.cards.is_empty());
    assert_eq!(a.attention, Attention::None);
    let mark = a.activity_at_ms.expect("the last output is the mark");
    assert!(
        opened_at <= mark && mark <= quiet_since,
        "mark {mark} is the last output instant ({opened_at}..={quiet_since})"
    );
    assert_eq!(
        Some(mark),
        last_output_ms(&h, &p.terminal),
        "E8: the mark IS the registry's stamp"
    );
    assert!(
        matches!(
            pj.recompute_track(&h.track).await.unwrap(),
            Recompute::Unchanged(_)
        ),
        "once: nothing new on the next pass"
    );
    h.stop(&p.terminal).await;
}

/// The exit record closes the gate even though `last_output_ms` is still fresh.
#[tokio::test]
async fn exited_interactive_card_is_not_working() {
    let h = Harness::start().await;
    let pj = projector(&h);
    let p = open_program(&h, "exited-fresh", &burst_program(10, "exit 0"), true).await;
    let exit = await_terminal_exit(&h, &p.terminal).await;
    assert_eq!(exit, (Some(0), false), "a normal exit");
    let a = recompute(&h, &pj).await;
    let stamp = last_output_ms(&h, &p.terminal).expect("the reader stamped the burst");
    assert!(
        calm_server::model::now_ms() - stamp < INTERACTIVE_OUTPUT_WINDOW.as_millis() as i64,
        "the stamp is still inside the window; only the exit gate closes it"
    );
    assert!(!a.working, "an exited PTY is not working: {a:?}");
    assert!(a.cards.is_empty());
    assert_eq!(a.attention, Attention::None);
    let (state, _, _) = session_row(&h, &p.session).await;
    assert_eq!(state, "exited", "the ephemeral session followed the exit");
    h.stop(&p.terminal).await;
}

/// The same PTY's output counts while the card is never bound, and once the task is `done` the
/// still-flowing output is ignored — the mark is the task's E3, not the output.
#[tokio::test]
async fn task_bound_worker_output_is_ignored() {
    let h = Harness::start().await;
    let pj = projector(&h);
    let stop = h.root.path().join("stop-task-bound");
    let p = open_program(&h, "task-bound", &ticker_program(&stop), true).await;
    await_output(&h, &p.terminal).await;
    let a = recompute(&h, &pj).await;
    assert!(a.working, "never bound: output counts: {a:?}");

    let task_id = format!("{}:build", h.track);
    sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id, \
                           declared_by,created_at_ms,updated_at_ms) \
         VALUES (?1,?2,'build','terminal','test','[]','running',?3,'user',?4,?4)",
    )
    .bind(&task_id)
    .bind(&h.track)
    .bind(&p.card)
    .bind(calm_server::model::now_ms())
    .execute(h.sql.pool())
    .await
    .unwrap();
    let a = recompute(&h, &pj).await;
    assert!(a.working, "W: the running task: {a:?}");
    assert_eq!(card_state(&a, &p.card), Some(CardState::Working));

    let finished_at = calm_server::model::now_ms();
    sqlx::query("UPDATE tasks SET status = 'done', finished_at_ms = ?2 WHERE id = ?1")
        .bind(&task_id)
        .bind(finished_at)
        .execute(h.sql.pool())
        .await
        .unwrap();
    // The PTY keeps printing: the stamp advances past the completion.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while last_output_ms(&h, &p.terminal).is_none_or(|at| at < finished_at) {
        assert!(tokio::time::Instant::now() < deadline, "the ticker stopped");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let a = recompute(&h, &pj).await;
    assert!(
        !a.working,
        "the task is done; the card's output does not count: {a:?}"
    );
    assert!(a.cards.is_empty());
    assert_eq!(a.attention, Attention::None);
    assert_eq!(a.activity_at_ms, Some(finished_at), "E3, not the output");
    h.stop(&p.terminal).await;
}

/// `last_thread_status = active` on a PTY that printed nothing is not working.
#[tokio::test]
async fn interactive_codex_card_uses_output_not_thread_status() {
    let h = Harness::start().await;
    let pj = projector(&h);
    let p = open_program(&h, "codex-status", "exec sleep 600", false).await;
    sqlx::query(
        "UPDATE worker_sessions SET provider = 'codex', mode = 'resumable', thread_id = 't-1', \
                last_thread_status = 'active', last_activity_ms = ?2 WHERE id = ?1",
    )
    .bind(&p.session)
    .bind(calm_server::model::now_ms())
    .execute(h.sql.pool())
    .await
    .unwrap();
    let a = recompute(&h, &pj).await;
    assert_eq!(
        last_output_ms(&h, &p.terminal),
        None,
        "the PTY printed nothing"
    );
    assert!(!a.working, "thread status is not a working source: {a:?}");
    assert!(a.cards.is_empty());
    assert_eq!(a.attention, Attention::None);
    h.stop(&p.terminal).await;
}

/// The reader writes the signal-killed session `failed` ⇒ one `session` item, the card `failed`, not working.
#[tokio::test]
async fn signal_killed_ephemeral_card_is_failed() {
    let h = Harness::start().await;
    let pj = projector(&h);
    let p = open_sleeper(&h, "sigterm-ephemeral").await;
    term(p.pid);
    assert_eq!(await_terminal_exit(&h, &p.terminal).await, (None, true));
    await_session_state(&h, &p.session, "failed").await;
    let a = recompute(&h, &pj).await;
    assert!(!a.working, "{a:?}");
    assert_eq!(a.attention, Attention::Failed);
    assert_eq!(card_state(&a, &p.card), Some(CardState::Failed));
    assert_eq!(a.items.len(), 1, "{a:?}");
    assert_eq!(a.items[0].kind, ItemKind::Failed);
    assert_eq!(a.items[0].source, ItemSource::Session);
    assert_eq!(a.items[0].id, p.session);
    assert_eq!(a.items[0].card_id.as_deref(), Some(p.card.as_str()));
    h.stop(&p.terminal).await;
}

/// A signal-killed codex TUI leaves its resumable session row `running` (the thread lives on the
/// daemon) ⇒ not failed, and not working (exit gate).
#[tokio::test]
async fn signal_killed_codex_interactive_card_is_not_failed() {
    let h = Harness::start().await;
    let pj = projector(&h);
    let p = open_sleeper(&h, "sigterm-codex").await;
    sqlx::query(
        "UPDATE worker_sessions SET provider = 'codex', mode = 'resumable', thread_id = 't-1' \
          WHERE id = ?1",
    )
    .bind(&p.session)
    .execute(h.sql.pool())
    .await
    .unwrap();
    let entry = h.state.terminal_renderer.get(&p.terminal).unwrap();
    term(p.pid);
    assert_eq!(await_terminal_exit(&h, &p.terminal).await, (None, true));
    assert!(
        entry
            .wait_exit_persisted_for_test(Duration::from_secs(5))
            .await,
        "the reader ran its whole exit arm"
    );
    let (state, _, _) = session_row(&h, &p.session).await;
    assert_eq!(
        state, "running",
        "a resumable session is not ended by the PTY"
    );
    let a = recompute(&h, &pj).await;
    assert!(!a.working, "{a:?}");
    assert_eq!(a.attention, Attention::None);
    assert!(a.cards.is_empty());
    assert!(a.items.is_empty());
    h.stop(&p.terminal).await;
}

/// The boot tick is consumed first and the exit follows at once, so the whole window sits before the
/// next scheduled tick; nothing calls `reconcile_all` after the exit.
#[tokio::test]
async fn exited_interactive_card_wakes_the_projector() {
    let h = Harness::start().await;
    let loop_task = tokio::spawn(projector(&h).run());
    await_activity(&h, "the boot sweep's row", Duration::from_secs(3), |_| true).await;
    let stop = h.root.path().join("stop-wake");
    let p = open_program(&h, "exit-wakes", &ticker_program(&stop), true).await;
    let a = await_activity(&h, "working before the exit", Duration::from_secs(5), |a| {
        a.working
    })
    .await;
    assert_eq!(card_state(&a, &p.card), Some(CardState::Working));

    std::fs::write(&stop, "").unwrap();
    let exit = await_terminal_exit(&h, &p.terminal).await;
    assert_eq!(exit, (Some(0), false));
    let a = await_activity(
        &h,
        "working = false after the exit was persisted",
        Duration::from_secs(5),
        |a| !a.working,
    )
    .await;
    assert!(a.cards.is_empty(), "{a:?}");
    loop_task.abort();
    h.stop(&p.terminal).await;
}

/// After a kernel restart the registry is empty: the same open, printing PTY reads `working = false`
/// through a fresh registry while the live one reads it working — and the exit gate still reads open.
#[tokio::test]
async fn registry_empty_after_restart_reads_quiet() {
    use calm_server::terminal_renderer::TerminalRendererRegistry;
    let h = Harness::start().await;
    let stop = h.root.path().join("stop-restart");
    let p = open_program(&h, "registry-empty", &ticker_program(&stop), true).await;
    await_output(&h, &p.terminal).await;
    let live = projector(&h);
    assert!(recompute(&h, &live).await.working, "the live registry");

    let repo: Arc<dyn calm_server::db::Repo> = h.sql.clone();
    let fresh = TrackActivityProjector::new(
        repo,
        h.state.events.clone(),
        h.state.write().clone(),
        h.state.harness.clone(),
        TerminalRendererRegistry::new(),
    )
    .expect("sqlite-backed repo");
    let rows = fresh.read_rows(&h.track).await.unwrap().unwrap();
    let ws = rows
        .sessions
        .iter()
        .find(|ws| ws.id == p.session)
        .expect("the PTY session is eligible");
    assert!(ws.pty_open, "the terminal row records no exit");
    assert!(rows.output.is_empty(), "an empty registry knows no output");
    let a = recompute(&h, &fresh).await;
    assert!(!a.working, "blind until the WS reattaches the card: {a:?}");
    assert!(a.cards.is_empty());
    h.stop(&p.terminal).await;
}

/// The supervisor's own verdict on `term:<terminal>`: `proc_running` turns `false` only once the exit
/// is recorded, which is after its pty reader drained the child's last bytes into the replay ring.
async fn await_supervisor_exit(h: &Harness, terminal: &str) {
    use calm_session::control::{ControlMsg, ControlReply, ProbeRequest};
    use calm_session::{read_frame, write_frame};
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let mut stream = tokio::net::UnixStream::connect(h.supervisor_socket())
            .await
            .unwrap();
        write_frame(
            &mut stream,
            &ControlMsg::Probe(ProbeRequest {
                proc_id: format!("term:{terminal}"),
            }),
        )
        .await
        .unwrap();
        match read_frame::<ControlReply, _>(&mut stream).await.unwrap() {
            ControlReply::ProbeOk {
                proc_running: false,
                ..
            } => return,
            ControlReply::ProbeOk { .. } => {}
            other => panic!("unexpected probe reply: {other:?}"),
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the supervisor never recorded the exit of {terminal}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// A program whose whole output lands in the attach replay: the attach is held until the supervisor
/// has recorded the exit, so the reader sees `Exited` and never an `Output` frame. The fresh launch
/// stamps the replay, and the exit wake reads `working = false` with the mark advanced to that output.
#[tokio::test]
async fn one_shot_output_before_attach_is_unread() {
    use calm_server::terminal_renderer::attach_hold_for_test;
    let h = Harness::start().await;
    let loop_task = tokio::spawn(projector(&h).run());
    let seeded = await_activity(&h, "the boot sweep's row", Duration::from_secs(3), |_| true).await;
    assert!(!seeded.working, "{seeded:?}");
    let (entered, release) = attach_hold_for_test::arm();
    let spawned_at = calm_server::model::now_ms();
    let (opened, ()) = tokio::join!(
        h.ok(
            "calm.terminal.open",
            json!({"program":"printf 'done\\n'; exit 0","request_id":"one-shot"}),
        ),
        async {
            let terminal = entered.await.expect("the open reached the attach hold");
            await_supervisor_exit(&h, &terminal).await;
            release.notify_one();
        }
    );
    let terminal = opened["terminal_id"].as_str().unwrap().to_owned();
    let card = opened["card_id"].as_str().unwrap().to_owned();
    assert_eq!(
        await_terminal_exit(&h, &terminal).await,
        (Some(0), false),
        "the sticky exit followed the replay"
    );
    let a = await_activity(
        &h,
        "the mark advanced to the replayed output",
        Duration::from_secs(5),
        |a| a.activity_at_ms.is_some_and(|at| at >= spawned_at),
    )
    .await;
    assert!(!a.working, "an exited PTY is not working: {a:?}");
    assert_eq!(card_state(&a, &card), None, "{a:?}");
    assert_eq!(a.attention, Attention::None);
    let stamp = last_output_ms(&h, &terminal).expect("the fresh launch stamped its replay");
    assert!(
        spawned_at <= stamp && stamp <= calm_server::model::now_ms(),
        "the stamp is the replay instant"
    );
    assert_eq!(
        a.activity_at_ms,
        Some(stamp),
        "E8: the mark IS the replay stamp"
    );
    loop_task.abort();
    h.stop(&terminal).await;
}

/// The post-restart lazy reattach replays the old screen; that replay is an earlier lifetime's output
/// and stamps nothing: through the fresh registry the card reads `working = false` and the mark does
/// not move.
#[tokio::test]
async fn reattach_replay_does_not_stamp() {
    use calm_server::ws::terminal::{
        TestLiveRenderer, resolve_live_renderer_from_terminal_for_test,
    };
    let h = Harness::start().await;
    // READY then quiet: the reattach replay carries READY and no frame ever follows it.
    let p = open_sleeper(&h, "reattach-replay").await;
    await_output(&h, &p.terminal).await;
    let live = projector(&h);
    let before = recompute(&h, &live).await;
    assert!(before.working, "the live registry: {before:?}");
    let mark = before.activity_at_ms.expect("E8 from the live stamp");

    let repo: Arc<dyn calm_server::db::Repo> = h.sql.clone();
    let fresh = calm_server::state::AppState::from_parts(
        repo.clone(),
        h.state.events.clone(),
        h.state.daemon.clone(),
        h.state.plugin.clone(),
        h.state.codex.clone(),
        Some(h.state.card_role_cache.clone()),
        Some(h.state.track_area_cache.clone()),
    );
    assert!(fresh.terminal_renderer.get(&p.terminal).is_none());
    let term = h
        .state
        .repo
        .terminal_get(&p.terminal)
        .await
        .unwrap()
        .unwrap();
    assert!(term.pid.is_some(), "the earlier lifetime persisted the pid");
    let entry = match resolve_live_renderer_from_terminal_for_test(&fresh, term)
        .await
        .unwrap()
    {
        TestLiveRenderer::Alive(entry) => entry,
        TestLiveRenderer::ChildExited { exit_code } => {
            panic!("the lazy reattach did not find the live PTY: {exit_code:?}")
        }
    };
    assert!(
        fresh.terminal_renderer.get(&p.terminal).is_some(),
        "the reattach installed the entry"
    );
    assert!(
        entry.handle.render_plane.lock().unwrap().pty_seq() >= 1,
        "the reattach replay carried the old screen"
    );
    assert_eq!(
        fresh.terminal_renderer.last_output_ms(&p.terminal),
        None,
        "a reattach replay stamps nothing"
    );
    let reattached = TrackActivityProjector::new(
        repo,
        h.state.events.clone(),
        h.state.write().clone(),
        h.state.harness.clone(),
        fresh.terminal_renderer.clone(),
    )
    .expect("sqlite-backed repo");
    let a = recompute(&h, &reattached).await;
    assert!(!a.working, "historical bytes do not light working: {a:?}");
    assert!(a.cards.is_empty(), "{a:?}");
    assert_eq!(a.activity_at_ms, Some(mark), "the mark did not move");
    drop(entry);
    fresh.terminal_renderer.drop_entry(&p.terminal).await;
    h.stop(&p.terminal).await;
}

/// The supervisor's replay ring of `term:<terminal>` contains `needle`: a read-only attach with its
/// own reader id, dropped after the `AttachOk`.
async fn await_ring_contains(h: &Harness, terminal: &str, needle: &str) {
    use calm_session::control::{AttachRequest, ControlMsg, ControlReply};
    use calm_session::{read_frame, write_frame};
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let mut stream = tokio::net::UnixStream::connect(h.supervisor_socket())
            .await
            .unwrap();
        write_frame(
            &mut stream,
            &ControlMsg::Attach(AttachRequest {
                proc_id: format!("term:{terminal}"),
                from_cursor: None,
                reader_id: "test-ring-probe".into(),
            }),
        )
        .await
        .unwrap();
        match read_frame::<ControlReply, _>(&mut stream).await.unwrap() {
            ControlReply::AttachOk(attached) => {
                if String::from_utf8_lossy(&attached.replay).contains(needle) {
                    return;
                }
            }
            other => panic!("unexpected attach reply: {other:?}"),
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the replay ring of {terminal} never carried {needle:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// A lazy WS attach that races a fresh launch: it runs after the launch persisted the pid (its own
/// replay is not evidence) and wins the registry insert while the launch is held before its attach.
/// The launch's stamped entry loses the insert; the survivor still carries that stamp, so the card
/// reads `working` inside the window from the wake that follows the launch.
#[tokio::test]
async fn concurrent_attach_keeps_the_fresh_launch_stamp() {
    use calm_server::terminal_renderer::attach_hold_for_test;
    use calm_server::ws::terminal::{
        TestLiveRenderer, resolve_live_renderer_from_terminal_for_test,
    };
    let h = Harness::start().await;
    let loop_task = tokio::spawn(projector(&h).run());
    let seeded = await_activity(&h, "the boot sweep's row", Duration::from_secs(3), |_| true).await;
    assert!(!seeded.working, "{seeded:?}");
    let (entered, release) = attach_hold_for_test::arm();
    let spawned_at = calm_server::model::now_ms();
    // The FE's own creation route: `CardAdded` is broadcast before the spawn, and a mounted grid
    // attaches the new card's terminal on that event.
    let create = async {
        use tower::ServiceExt;
        let response = h
            .app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(format!("/api/tracks/{}/terminal-cards", h.track))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        json!({"program": SLEEPER, "cwd": "", "env": {},
                            "theme": {"fg": [216,219,226], "bg": [15,20,24]}})
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::CREATED);
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let card: Value = serde_json::from_slice(&bytes).unwrap();
        card["id"].as_str().unwrap().to_owned()
    };
    let (card, attach_entry) = tokio::join!(create, async {
        let terminal = entered.await.expect("the create reached the attach hold");
        let term = h.state.repo.terminal_get(&terminal).await.unwrap().unwrap();
        assert!(
            term.pid.is_some(),
            "the hold sits after the pid persistence: the concurrent ensure is attach-shaped"
        );
        await_ring_contains(&h, &terminal, "READY").await;
        assert!(
            h.state.terminal_renderer.get(&terminal).is_none(),
            "the held launch has not inserted its entry"
        );
        let entry = match resolve_live_renderer_from_terminal_for_test(&h.state, term)
            .await
            .unwrap()
        {
            TestLiveRenderer::Alive(entry) => entry,
            TestLiveRenderer::ChildExited { exit_code } => {
                panic!("the lazy attach did not find the live PTY: {exit_code:?}")
            }
        };
        assert!(
            h.state.terminal_renderer.get(&terminal).is_some(),
            "the attach won the insert"
        );
        assert_eq!(
            last_output_ms(&h, &terminal),
            None,
            "the attach's replay is an unproven lifetime's bytes and stamps nothing"
        );
        release.notify_one();
        entry
    });
    let terminal = h
        .state
        .repo
        .terminal_get_by_card(&card)
        .await
        .unwrap()
        .expect("the created card has a terminal row")
        .id;
    let survivor = h
        .state
        .terminal_renderer
        .get(&terminal)
        .expect("the registry keeps one entry");
    assert!(
        Arc::ptr_eq(&survivor, &attach_entry),
        "the launch's entry lost the insert to the attach's"
    );
    let stamp = last_output_ms(&h, &terminal)
        .expect("the survivor carries the fresh launch's replay stamp");
    assert!(
        spawned_at <= stamp && stamp <= calm_server::model::now_ms(),
        "the stamp is the launch's replay instant"
    );
    let a = await_activity(
        &h,
        "working from the stamp the launch handed over",
        Duration::from_secs(5),
        |a| a.working,
    )
    .await;
    assert_eq!(card_state(&a, &card), Some(CardState::Working), "{a:?}");
    assert_eq!(
        a.activity_at_ms,
        Some(stamp),
        "E8: the mark IS the handed-over stamp"
    );
    loop_task.abort();
    h.stop(&terminal).await;
}

/// Rows with NO terminal row — an idle harness session (`terminal_run_id` NULL) and an interactive
/// session whose terminal row the orphan arm deleted (FK `ON DELETE SET NULL`) — decode
/// `pty_open = false` through the `COALESCE`, and the recomputation runs on them without error.
#[tokio::test]
async fn s0_row_without_terminal_row_decodes_closed() {
    use calm_server::db::sqlite::{card_create_with_id_tx, session_start_runtime_tx};
    use calm_server::harness::HarnessSnapshot;
    use calm_server::model::{NewCard, NewTerminal, NewTrack};
    use calm_server::session_projection_repo::{
        AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    use calm_server::track_activity::sql::eligible_sessions;
    let h = Harness::start().await;
    let track = h
        .sql
        .track_create(NewTrack {
            template_input: None,
            area_id: h.area_id.clone().into(),
            title: "s0-null".into(),
            sort: None,
            cwd: h.root.path().to_str().unwrap().into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap()
        .id
        .to_string();
    // 1. the idle harness row: no PTY.
    let planner_card = new_id();
    let planner_ws = new_id();
    let mut tx = h.sql.pool().begin().await.unwrap();
    card_create_with_id_tx(
        &mut tx,
        planner_card.clone(),
        NewCard {
            track_id: track.clone().into(),
            title: None,
            kind: "planner".into(),
            sort: None,
            payload: json!({}),
        },
        CardRole::Planner,
        true,
        &h.state.card_role_cache,
    )
    .await
    .unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: planner_ws.clone(),
            card_id: planner_card.clone(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some("th-planner".into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(
                serde_json::to_value(HarnessSnapshot::initial(0, vec![])).unwrap(),
            ),
            spawn_op_id: None,
            now_ms: 1_000,
        },
    )
    .await
    .unwrap();
    // 2. the interactive terminal session whose terminal row is deleted.
    let term_card = new_id();
    let term_ws = new_id();
    card_create_with_id_tx(
        &mut tx,
        term_card.clone(),
        NewCard {
            track_id: track.clone().into(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({}),
        },
        CardRole::Worker,
        true,
        &h.state.card_role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let terminal = h
        .sql
        .terminal_create(NewTerminal {
            card_id: term_card.clone().into(),
            program: "sleep 600".into(),
            cwd: h.root.path().to_str().unwrap().into(),
            env: json!({}),
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let mut tx = h.sql.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: term_ws.clone(),
            card_id: term_card.clone(),
            kind: WorkerSessionKind::Terminal,
            agent_provider: None,
            status: WorkerSessionState::Running,
            terminal_run_id: Some(terminal.id.clone()),
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: 2_000,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    h.sql.terminal_delete(&terminal.id).await.unwrap();
    let linked: Option<String> =
        sqlx::query_scalar("SELECT terminal_run_id FROM worker_sessions WHERE id = ?1")
            .bind(&term_ws)
            .fetch_one(h.sql.pool())
            .await
            .unwrap();
    assert_eq!(linked, None, "FK ON DELETE SET NULL");

    let rows = eligible_sessions(h.sql.pool(), &track).await.unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    for ws in &rows {
        assert!(ws.terminal_run_id.is_none(), "{ws:?}");
        assert!(!ws.pty_open, "no terminal row reads closed: {ws:?}");
    }
    assert!(rows.iter().any(|ws| ws.id == planner_ws));
    assert!(rows.iter().any(|ws| ws.id == term_ws));
    let pj = projector(&h);
    let a = match pj.recompute_track(&track).await.unwrap() {
        Recompute::NoTrack => panic!("track vanished"),
        Recompute::Unchanged(p) | Recompute::Written(p) => p,
    };
    assert!(!a.working, "{a:?}");
    assert_eq!(a.attention, Attention::None);
    assert!(a.cards.is_empty());
    h.stop("no-terminal").await;
}
