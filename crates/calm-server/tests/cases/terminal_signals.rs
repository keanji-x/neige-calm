//! #1620 — hook signals and bounded composite actions through the real MCP
//! tools, the real operation runtime, a real PTY and the production ingest
//! route. A fake `claude` (shell script) reads the generated `--settings`
//! file and runs the registered hook command with synthetic payloads, so the
//! whole path settings file → bridge command → `/internal/claude/hook` →
//! renderer ring → `wait_for=signal` is exercised end to end.
use crate::terminal_support::Harness;
use calm_server::event::Event;
use calm_server::model::{CardRole, new_id};
use calm_server::routes::theme::RequestTheme;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::time::Duration;

/// Parses `--settings <file>`, extracts the registered hook command, prints
/// `READY <card id>` and then, per stdin line, runs the hook command with
/// synthetic Claude Code payloads (`perm` → a permission Notification,
/// anything else → UserPromptSubmit then Stop) before echoing the turn. Like
/// the real Claude, the Stop body is byte-identical every turn: only the
/// bridge's per-invocation occurrence id tells the turns apart.
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
/// #1628 fixture: the same settings parsing and hook command, but the PTY
/// echo is off (so the screen changes only when the fake prints) and each
/// stdin line selects when the answer is painted relative to the Stop hook:
/// `late:<x>` posts Stop, then paints `ANSWER:<x>` 300 ms later (the real
/// Claude order); `early:<x>` paints the answer, stays quiet 600 ms, then
/// posts Stop; `burst` posts Stop and then paints a line every 50 ms for
/// three seconds; anything else stays quiet 400 ms, posts Stop and paints
/// nothing (the quiet screen at the signal has no change since the baseline,
/// so it must not pass for `already`).
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
/// Open a claimed #1628 repaint fake and return its terminal id.
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
    // #1628: the readback settles the repaint after the signal. The echo of
    // the submitted text may or may not have been quiet for settle_ms when
    // the Stop landed, so either verdict is legal here; the precise cases
    // are the `repaint_*` tests below.
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

    // No new signal: the budget elapses with outcome unchanged and no signal.
    let idle = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_for":"signal","wait_ms":400}),
        )
        .await;
    assert_eq!(idle["wait"]["outcome"], "unchanged", "{idle}");
    assert_eq!(idle["wait"]["signal"], Value::Null);
    assert_eq!(idle["wait"]["signal_at_ms"], Value::Null);
    assert_eq!(idle["wait"]["repaint"], Value::Null);
    assert_eq!(idle["wait"]["settled"], false);
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
    // The wait ended on the prompt: the fake's Stop and its turn line land
    // later. Await the turn's projected completion so the next submission
    // observes a settled screen (a turn line arriving between its observation
    // and the write would make it `stale_observation`).
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
    assert_eq!(none["wait"]["outcome"], "unchanged", "{none}");
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

/// #1628 (a) — Stop lands before the answer is painted (the real Claude
/// order): one submit + signal readback already contains the answer, with
/// `repaint.outcome == settled`; no second observation is needed.
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

/// #1628 (b) — the answer was painted and quiet well before Stop: the
/// readback returns at the signal with `already`, not after `repaint_ms`.
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

/// #1628 (c)/(d) — Stop with nothing painted afterwards: `none` after
/// `repaint_ms`; `repaint_ms: 0` restores the immediate return (`skipped`).
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
    // settle_ms is accepted in signal mode now (#1628) and only bounds the
    // quiet window; with nothing painted the verdict is still `none`.
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

/// #1628 (e) — the budget ends while the post-signal burst is still
/// painting: `unsettled`, the signal kept, `waited_ms` at the budget.
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

/// #1628 (f) — `repaint_ms` is refused outside signal mode on every wait
/// carrier, with the same invalid-params shape as `settle_ms`.
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
    // A signal that lands after this connection's previous observation but
    // BEFORE the input call must not satisfy the readback wait: the readback
    // baseline is the seq read just before the physical write, not the
    // previous observation.
    // Direct POSTs stand in for two bridge invocations: the body differs only
    // by the per-invocation occurrence id the bridge stamps.
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
    h.stop(&terminal).await;
}

/// #1620 F1 — Claude's `Stop` body is byte-identical every turn. Two bridge
/// invocations with the same stdin must produce two ring entries (seq 1 and
/// 2): the bridge's per-invocation occurrence id is what keys them apart at
/// the server, while a retry of one invocation (same body) stays a duplicate.
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

/// #1620 F3 — terminal hook POSTs never occupy the bounded worker dedupe
/// cache: a flood of them (more than the cache holds) must not evict a worker
/// key, so a duplicate delivery of that worker hook is still suppressed.
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

/// #1620 F2 — a human who claims control between the Planner's create and
/// its claim keeps control: the claim is applied by the pump only if nobody
/// else owns the terminal at that moment (decided under the registry lock,
/// never from the Planner connection's cached owner, which was read before
/// the human claimed and may not have seen the `OwnerChanged` yet).
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

/// #1620 F4 — track deletion goes through the quiesce path, not the reap
/// helper; the generated settings file must still go with the committed
/// delete.
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

/// A human client on `terminal` (its own pump, no command channel) that has
/// just taken control: returns once its `OwnerChanged` names `user`. The
/// pump is aborted through the returned handle; the sender keeps it alive.
async fn human_takeover(
    entry: &std::sync::Arc<calm_server::terminal_renderer::RendererEntry>,
    terminal: &str,
    user: uuid::Uuid,
) -> (
    tokio::task::AbortHandle,
    tokio::sync::mpsc::Sender<calm_session::ClientMsg>,
) {
    use calm_server::terminal_renderer::{ClientInputScope, ClientPumpContext, run_client_pump};
    use calm_session::{
        ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
        RenderEncoding,
    };
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
            terminal_id: terminal.to_owned(),
        },
    ));
    incoming
        .send(ClientMsg::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            terminal_id: terminal.to_owned(),
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
    (pump.abort_handle(), incoming)
}

/// #1620 R1 — the hook route keys on the card's durable execution identity
/// (its terminal row), not on the patchable `cards.kind`. After a public
/// PATCH sets the kind to `codex` (the terminal row, process and hook
/// settings all stay), a Claude hook for the card still lands in the ring
/// and is never persisted or projected as worker state.
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
    // Never worker state: nothing was persisted or broadcast for the card.
    // (The MCP tools' own admission fence refuses the retargeted card, so the
    // ring is read directly rather than through an observation.)
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

/// #1620 — the provenance marker is server-owned: a client PATCH carrying
/// `terminal_signals` is refused, and a PATCH that replaces the whole payload
/// of a Planner-opened terminal keeps the marker (the kernel re-stamps it),
/// so its hooks still route to the ring.
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

/// #1620 R2 — a replayed `open claim:true` after a human takeover that this
/// connection has not applied yet (its `OwnerChanged` delivery is held) must
/// not report `claimed` from the cached lease: "already owned by this
/// connection" is decided against the owner registry, and the replay
/// reports the takeover reason while the human keeps control.
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

/// #1620 R6 — a human takeover applied back to back with the Planner's own
/// grant never shows `owner == me` on the Planner's connection. The verdict
/// comes from the pump's reply (`Granted`, sent under the registry lock and
/// independent of protocol delivery); the claim then waits for that grant to
/// be applied and reads the takeover instead of idling to its 7 s budget.
/// Delivery on the Planner's connection is held from inside the claim window
/// until the human has taken over, so the grant and the takeover are applied
/// in one go — a deterministic fold, not a timing window: the outcome does
/// not depend on which delivery the reader applies first.
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

/// #1620 R3 regression (a) — a Codex Worker card created through the
/// production path (`card_with_codex_create_tx`, which also creates a
/// terminal row) keeps its hook contract: a codex hook is persisted as a
/// `codex.hook` event and projected onto the card FSM. Owning a terminal
/// row must never route a worker's hook to the terminal-signal branch.
#[tokio::test]
#[allow(deprecated)] // the state's role cache is the one `enforce_role` reads
async fn codex_worker_card_hooks_are_still_persisted_and_projected() {
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
    calm_server::card_fsm::spawn(
        h.state.repo.clone(),
        h.state.events.clone(),
        h.state.write().clone(),
    );
    tokio::task::yield_now().await;
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
    await_card_state(&h, &card_id, "AwaitingInput").await;
    // The same body again is deduped by the worker cache, not appended twice.
    assert_eq!(h.post_codex_hook(&card_id, &stop).await, 204);
    assert_eq!(h.persisted_hook_events().await, 1);
    h.stop(&term.id).await;
}

/// #1620 R3 regression (b) — a Claude Worker card created through the
/// production path (`card_with_claude_create_tx`, terminal row included)
/// keeps its hook contract: a Claude `Stop` hook is persisted as a
/// `claude.hook` event and projected onto the card FSM (`AwaitingInput`).
#[tokio::test]
#[allow(deprecated)] // the state's role cache is the one `enforce_role` reads
async fn claude_worker_card_hooks_are_still_persisted_and_projected() {
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
    calm_server::card_fsm::spawn(
        h.state.repo.clone(),
        h.state.events.clone(),
        h.state.write().clone(),
    );
    tokio::task::yield_now().await;
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
    await_card_state(&h, &card_id, "AwaitingInput").await;
    h.stop(&term.id).await;
}

/// #1620 R3 regression (d) — the provenance marker lives on the card: after
/// a public PATCH retargets `kind` to `codex` AND the sweeper has reaped the
/// terminal (row gone), a delayed Claude hook for the card is still
/// acknowledged as a signal (dropped: no live entry) and never persisted or
/// projected as worker state.
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
    calm_server::card_fsm::spawn(
        h.state.repo.clone(),
        h.state.events.clone(),
        h.state.write().clone(),
    );
    tokio::task::yield_now().await;
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
            .iter()
            .all(|overlay| overlay.kind != "status"),
        "no FSM projection"
    );
    h.stop(&terminal).await;
}

/// Polls the card's `status` overlay until the FSM projected `expected`.
async fn await_card_state(h: &Harness, card_id: &str, expected: &str) {
    let poll = async {
        loop {
            let overlays = h.state.repo.overlays_for("card", card_id).await.unwrap();
            if overlays.iter().any(|overlay| {
                overlay.kind == "status"
                    && overlay.payload.get("state").and_then(Value::as_str) == Some(expected)
            }) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    if tokio::time::timeout(Duration::from_secs(3), poll)
        .await
        .is_err()
    {
        let overlays = h.state.repo.overlays_for("card", card_id).await.unwrap();
        panic!("no `status: {expected}` overlay on {card_id}; overlays: {overlays:?}");
    }
}
