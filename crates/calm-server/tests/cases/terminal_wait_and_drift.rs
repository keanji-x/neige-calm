//! #1618: change waiting, drift-tolerant input, implicit observation, receipt
//! wording, one copy of the state, and the round 07/08 slice: structured
//! stale refusals, baseline transparency and release readback economy.
//! Actual MCP server, renderer and PTY.
use crate::terminal_support::Harness;
use serde_json::{Value, json};
use std::time::Duration;

fn receipt(response: &Value) -> &Value {
    assert!(response.get("error").is_none(), "{response}");
    let content = response["result"]["content"].as_array().unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "text");
    &response["result"]["structuredContent"]
}
fn summary(response: &Value) -> &str {
    response["result"]["content"][0]["text"].as_str().unwrap()
}
fn observation(response: &Value) -> &Value {
    let result = receipt(response);
    assert_eq!(result["observation"]["status"], "available", "{result}");
    &result["observation"]["state"]
}
fn has_line(state: &Value, expected: &str) -> bool {
    state["text"]
        .as_array()
        .unwrap()
        .iter()
        .any(|line| line.as_str().unwrap().trim_end() == expected)
}
fn revision(state: &Value) -> u64 {
    state["observation_revision"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}
/// The live projection revision, read without registering an observation.
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
/// Wait until the projection moved past `after` without observing.
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
fn error_text(response: &Value) -> String {
    response["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("expected an error: {response}"))
        .to_owned()
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
async fn claim(h: &Harness, terminal: &str) -> Value {
    h.call(
        "calm.terminal.control",
        json!({"terminal_id":terminal,"action":"claim","observe":true,"wait_ms":100}),
    )
    .await
}
/// Run `call` and write `marker` into the workspace only once the call's
/// change wait has subscribed to the projection, so a program that polls for
/// the marker starts producing after the wait began, whatever the RPC
/// latency was.
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
                "the change wait never subscribed"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        std::fs::write(h.root.path().join(marker), b"x").unwrap();
    };
    let (response, ()) = tokio::join!(call, release);
    response
}
/// Forty lines then a marker: the live viewport has history above it, so an
/// observation with `scroll_offset > 0` is a real history view.
const SCROLLBACK: &str = "i=0; while [ $i -lt 40 ]; do printf \"L$i\\n\"; i=$((i+1)); done; printf 'READY\\n'; cat >/dev/null";

#[tokio::test]
async fn observe_change_wait_returns_after_late_output_and_settles() {
    let h = Harness::start().await;
    let terminal = open(
        &h,
        "while [ ! -e go ]; do sleep 0.02; done; sleep 0.5; printf 'LATE\\n'; exec /bin/sh",
        "late",
    )
    .await;
    // open already captured this connection's first observation (quiet screen).
    let response = call_then_release(
        &h,
        &terminal,
        "go",
        h.call(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_for":"change","wait_ms":5000}),
        ),
    )
    .await;
    let view = receipt(&response);
    assert_eq!(view["wait"]["mode"], "change", "{view}");
    assert_eq!(view["wait"]["outcome"], "changed", "{view}");
    assert_eq!(view["wait"]["settled"], true, "{view}");
    let waited = view["wait"]["waited_ms"].as_u64().unwrap();
    // Exact timing is covered by the paused-clock unit tests; here only the
    // budget is an upper bound, the wait ended on the output.
    assert!(waited < 5000, "waited_ms={waited}");
    assert!(has_line(view, "LATE"), "{view}");
    assert_eq!(view["changed_since_previous_observation"], true);
    assert!(
        summary(&response).contains(" wait changed; full state in structuredContent"),
        "{}",
        summary(&response)
    );
    h.stop(&terminal).await;
}

#[tokio::test]
async fn observe_change_wait_on_quiet_shell_reports_unchanged_at_budget() {
    let h = Harness::start().await;
    let terminal = open(&h, "exec /bin/sh", "quiet").await;
    let settled = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_ms":200}),
        )
        .await;
    assert_eq!(
        settled["wait"],
        json!({"mode":"elapsed","outcome":"elapsed","waited_ms":200,"settled":false,"baseline_signal_seq":0,
            "baseline_revision":settled["previous_observation_revision"]})
    );
    // open captured this connection's first observation, so the baseline is a
    // real revision string, not null.
    assert!(
        settled["previous_observation_revision"].is_string(),
        "{settled}"
    );
    let view = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_for":"change","wait_ms":300}),
        )
        .await;
    assert_eq!(view["wait"]["outcome"], "unchanged", "{view}");
    assert_eq!(view["wait"]["settled"], false);
    assert!(view["wait"]["waited_ms"].as_u64().unwrap() >= 300, "{view}");
    assert_eq!(view["changed_since_previous_observation"], false);
    assert_eq!(
        view["observation_revision"],
        settled["observation_revision"]
    );
    // G2: both baselines are named and, on a quiet screen, coincide.
    assert_eq!(
        view["wait"]["baseline_revision"],
        settled["observation_revision"]
    );
    assert_eq!(
        view["previous_observation_revision"],
        settled["observation_revision"]
    );
    let rejected = h
        .call(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"settle_ms":10}),
        )
        .await;
    assert_eq!(rejected["error"]["code"], -32602, "{rejected}");
    assert!(error_text(&rejected).contains("settle_ms requires wait_for=change"));
    for options in [
        json!({"wait_ms":20001}),
        json!({"wait_for":"change","settle_ms":2001}),
        json!({"wait_for":"later"}),
    ] {
        let mut args = options;
        args["terminal_id"] = json!(terminal);
        assert_eq!(
            h.call("calm.terminal.observe", args).await["error"]["code"],
            -32602
        );
    }
    h.stop(&terminal).await;
}

#[tokio::test]
async fn input_readback_change_wait_starts_from_the_pre_write_screen() {
    let h = Harness::start().await;
    // Raw mode without echo: the only screen changes are the program's replies.
    let terminal = open(
        &h,
        "stty raw -echo; printf 'READY\\r\\n'; dd bs=1 count=1 >/dev/null 2>&1; dd bs=1 count=1 >/dev/null 2>&1; sleep 0.3; printf 'LATER\\r\\n'; cat >/dev/null",
        "reply",
    )
    .await;
    let claimed = claim(&h, &terminal).await;
    assert!(has_line(observation(&claimed), "READY"));
    // Hold the physical write barrier so the write cannot complete, render a
    // change while the input call is parked there, then let it through. The
    // readback baseline is the screen just before the write, so that change
    // counts; a baseline read after the write would already contain it and
    // wait the whole budget.
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    let held = entry.handle.input_barrier.grant().await.unwrap();
    let service = h.interaction();
    let inject = async {
        // The input reserves its sequence right after reading the baseline
        // and before the write reaches the held barrier; injecting once that
        // reservation is visible puts the change after the baseline, without
        // a fixed sleep.
        while !service.input_pending(&terminal).await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        entry
            .handle
            .render_plane
            .lock()
            .unwrap()
            .on_pty_chunk(b"INJECTED\r\n".to_vec());
        let injected = entry
            .handle
            .model_view
            .lock()
            .unwrap()
            .capture(0)
            .unwrap()
            .1;
        drop(held);
        injected
    };
    let (first, injected) = tokio::join!(
        h.call("calm.terminal.input", json!({"terminal_id":terminal,"request_id":"first","action":{"type":"key","key":"Enter"},"observe":true,"wait_for":"change","wait_ms":3000})),
        inject
    );
    assert_eq!(receipt(&first)["outcome"], "written", "{first}");
    assert_eq!(receipt(&first)["application_result"], "unverified");
    assert!(receipt(&first).get("application_completed").is_none());
    assert_eq!(receipt(&first)["output_since_observation"], false);
    let state = observation(&first);
    assert!(
        state["observation_revision"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            >= injected
            && observation(&claimed)["observation_revision"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
                < injected,
        "the change was rendered after the claim readback and before the input readback: {state}"
    );
    assert_eq!(state["wait"]["outcome"], "changed", "{state}");
    assert_eq!(state["wait"]["settled"], true, "{state}");
    assert!(
        state["wait"]["waited_ms"].as_u64().unwrap() < 3000,
        "{state}"
    );
    assert!(has_line(state, "INJECTED"), "{state}");
    assert_eq!(state["changed_since_previous_observation"], true);
    // G2: the readback names the pre-write baseline (before the injection)
    // and, separately, the claim readback it is compared with.
    let baseline: u64 = state["wait"]["baseline_revision"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(baseline < injected, "{state}");
    assert!(baseline >= revision(observation(&claimed)), "{state}");
    assert_eq!(
        state["previous_observation_revision"],
        observation(&claimed)["observation_revision"]
    );
    // A reply 300 ms after the write is included without a second call.
    let second = h.call("calm.terminal.input", json!({"terminal_id":terminal,"request_id":"second","action":{"type":"key","key":"Enter"},"observe":true,"wait_for":"change","wait_ms":3000})).await;
    let state = observation(&second);
    assert_eq!(state["wait"]["outcome"], "changed", "{state}");
    assert!(has_line(state, "LATER"), "{state}");
    assert_eq!(
        summary(&second),
        format!(
            "terminal {terminal} input written readback available; details in structuredContent"
        )
    );
    h.stop(&terminal).await;
}

#[tokio::test]
async fn observe_change_wait_reports_process_exit() {
    let h = Harness::start().await;
    let terminal = open(&h, "sleep 0.3; exit 0", "exit").await;
    let view = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_for":"change","wait_ms":5000}),
        )
        .await;
    assert_eq!(view["wait"]["outcome"], "exited", "{view}");
    assert_eq!(view["wait"]["settled"], false);
    assert!(view["wait"]["waited_ms"].as_u64().unwrap() < 5000, "{view}");
    h.stop(&terminal).await;
}

#[tokio::test]
async fn drift_tolerant_input_interrupts_streaming_output_but_not_a_changed_surface() {
    let h = Harness::start().await;
    let terminal = open(
        &h,
        "while :; do printf 'TICK\\n'; sleep 0.05; done",
        "stream",
    )
    .await;
    let claimed = claim(&h, &terminal).await;
    let observed = observation(&claimed).clone();
    // Wait for output to land rather than sleeping a fixed time; this
    // observation is not the one the inputs below name.
    let moved = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_for":"change","wait_ms":3000}),
        )
        .await;
    assert_eq!(moved["wait"]["outcome"], "changed", "{moved}");
    let refused = h.call("calm.terminal.input", json!({"terminal_id":terminal,"observation_id":observed["observation_id"],"request_id":"escape","action":{"type":"key","key":"Escape"}})).await;
    // G1: only the revision moved, so the refusal is a structured result with
    // a fresh observation rather than an RPC error.
    let stale = receipt(&refused);
    assert_eq!(stale["outcome"], "stale_observation", "{stale}");
    assert_eq!(stale["observation_id_used"], observed["observation_id"]);
    assert_eq!(
        stale["observed_revision"].as_u64().unwrap(),
        revision(&observed)
    );
    assert!(stale["current_revision"].as_u64().unwrap() > revision(&observed));
    assert_ne!(
        observation(&refused)["observation_id"],
        observed["observation_id"]
    );
    let conflicting = h.call("calm.terminal.input", json!({"terminal_id":terminal,"observation_id":observed["observation_id"],"request_id":"escape","action":{"type":"key","key":"Escape"},"allow_output_since_observation":true})).await;
    // The stale result above cached nothing, so the same request_id is free
    // with different arguments; the flag is part of the fingerprint once a
    // receipt exists.
    let written = receipt(&conflicting);
    assert_eq!(written["outcome"], "written", "{written}");
    assert_eq!(written["output_since_observation"], true);
    assert_eq!(written["observation_id_used"], observed["observation_id"]);
    let drift = &written["observation_drift"];
    assert_eq!(
        drift["observed_revision"].as_u64().unwrap().to_string(),
        observed["observation_revision"]
    );
    assert!(
        drift["input_revision"].as_u64().unwrap() > drift["observed_revision"].as_u64().unwrap(),
        "{drift}"
    );
    let reused = h.call("calm.terminal.input", json!({"terminal_id":terminal,"observation_id":observed["observation_id"],"request_id":"escape","action":{"type":"key","key":"Escape"}})).await;
    assert!(
        error_text(&reused).contains("reused with different arguments"),
        "{reused}"
    );
    // A different input surface (here a resize) is refused even with the flag.
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    let size = entry.handle.render_plane.lock().unwrap().current_size();
    entry
        .handle
        .render_plane
        .lock()
        .unwrap()
        .on_resize(size.cols + 2, size.rows);
    let surface = h.call("calm.terminal.input", json!({"terminal_id":terminal,"observation_id":observed["observation_id"],"request_id":"escape-2","action":{"type":"key","key":"Escape"},"allow_output_since_observation":true})).await;
    assert!(
        error_text(&surface).contains("terminal surface changed since observation"),
        "{surface}"
    );
    h.stop(&terminal).await;
}

#[tokio::test]
async fn omitted_observation_id_uses_the_latest_observation_on_this_connection() {
    let h = Harness::start().await;
    let terminal = open(&h, "exec /bin/sh", "implicit").await;
    let claimed = claim(&h, &terminal).await;
    let latest = observation(&claimed)["observation_id"].clone();
    let typed = h.call("calm.terminal.input", json!({"terminal_id":terminal,"request_id":"type","action":{"type":"text","text":"printf 'IMPLICIT_OK\\n'"},"observe":true,"wait_for":"change","wait_ms":3000})).await;
    assert_eq!(receipt(&typed)["outcome"], "written", "{typed}");
    assert_eq!(receipt(&typed)["observation_id_used"], latest);
    assert_eq!(receipt(&typed)["output_since_observation"], false);
    assert!(receipt(&typed).get("observation_drift").is_none());
    let readback = observation(&typed)["observation_id"].clone();
    // The readback became the latest observation; the receipt replays for
    // the same request_id with the omitted argument hashed as null.
    let replay = h.call("calm.terminal.input", json!({"terminal_id":terminal,"request_id":"type","action":{"type":"text","text":"printf 'IMPLICIT_OK\\n'"}})).await;
    assert_eq!(receipt(&replay)["observation_id_used"], latest);
    let explicit = h.call("calm.terminal.input", json!({"terminal_id":terminal,"observation_id":latest,"request_id":"type","action":{"type":"text","text":"printf 'IMPLICIT_OK\\n'"}})).await;
    assert!(error_text(&explicit).contains("reused with different arguments"));
    let entered = h.call("calm.terminal.input", json!({"terminal_id":terminal,"request_id":"enter","action":{"type":"key","key":"Enter"},"observe":true,"wait_for":"change","wait_ms":3000})).await;
    assert_eq!(receipt(&entered)["observation_id_used"], readback);
    assert!(has_line(observation(&entered), "IMPLICIT_OK"), "{entered}");
    // A fresh connection has no observation to fall back to.
    h.ok(
        "calm.terminal.control",
        json!({"terminal_id":terminal,"action":"detach"}),
    )
    .await;
    h.ok(
        "calm.terminal.control",
        json!({"terminal_id":terminal,"action":"claim"}),
    )
    .await;
    let none = h.call("calm.terminal.input", json!({"terminal_id":terminal,"request_id":"blind","action":{"type":"key","key":"Enter"}})).await;
    assert!(
        error_text(&none).contains("no observation on this connection; observe first"),
        "{none}"
    );
    h.stop(&terminal).await;
}

/// Observe until the projection reports the requested alternate-screen state
/// (and, optionally, a marker line), driven by change waits, not sleeps.
async fn observe_until(h: &Harness, terminal: &str, alternate: bool, line: Option<&str>) -> Value {
    let start = std::time::Instant::now();
    loop {
        let view = h
            .ok(
                "calm.terminal.observe",
                json!({"terminal_id":terminal,"wait_for":"change","wait_ms":1000}),
            )
            .await;
        if view["alternate"] == alternate && line.is_none_or(|line| has_line(&view, line)) {
            return view;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "alternate={alternate} line={line:?} never observed: {view}"
        );
    }
}

#[tokio::test]
async fn drift_tolerant_input_refuses_an_alternate_screen_switch_in_either_direction() {
    let h = Harness::start().await;
    // Enter and leave the alternate screen without resizing or touching any
    // input mode; rmux tracks the switch through its saved grid.
    let terminal = open(
        &h,
        "printf 'NORMAL\\n'; while [ ! -e go ]; do sleep 0.02; done; printf '\\033[?1049h\\033[2J\\033[HMENU\\n'; while [ ! -e back ]; do sleep 0.02; done; printf '\\033[?1049l'; printf 'AGAIN\\n'; cat >/dev/null",
        "altscreen",
    )
    .await;
    let claimed = claim(&h, &terminal).await;
    let normal = observation(&claimed).clone();
    assert_eq!(normal["alternate"], false, "{normal}");
    assert!(has_line(&normal, "NORMAL"), "{normal}");
    std::fs::write(h.root.path().join("go"), b"x").unwrap();
    let menu = observe_until(&h, &terminal, true, Some("MENU")).await;
    assert_eq!(
        menu["cols"], normal["cols"],
        "the switch must not be a resize"
    );
    assert_eq!(menu["rows"], normal["rows"]);
    let into_menu = h.call("calm.terminal.input", json!({"terminal_id":terminal,"observation_id":normal["observation_id"],"request_id":"esc-normal","action":{"type":"key","key":"Escape"},"allow_output_since_observation":true})).await;
    let message = error_text(&into_menu);
    assert!(
        message.contains("terminal surface changed since observation")
            && message.contains("alternate screen"),
        "{into_menu}"
    );
    std::fs::write(h.root.path().join("back"), b"x").unwrap();
    let again = observe_until(&h, &terminal, false, Some("AGAIN")).await;
    let out_of_menu = h.call("calm.terminal.input", json!({"terminal_id":terminal,"observation_id":menu["observation_id"],"request_id":"esc-menu","action":{"type":"key","key":"Escape"},"allow_output_since_observation":true})).await;
    let message = error_text(&out_of_menu);
    assert!(
        message.contains("terminal surface changed since observation")
            && message.contains("alternate screen"),
        "{out_of_menu}"
    );
    // Same screen kind as the live frame: the flag still writes.
    let same = h.call("calm.terminal.input", json!({"terminal_id":terminal,"observation_id":again["observation_id"],"request_id":"esc-again","action":{"type":"key","key":"Escape"},"allow_output_since_observation":true})).await;
    assert_eq!(receipt(&same)["outcome"], "written", "{same}");
    h.stop(&terminal).await;
}

/// Twenty lines 30 ms apart (~600 ms), gated on a marker file so the wait
/// starts before the burst does.
const BURST: &str = "while [ ! -e go ]; do sleep 0.02; done; i=0; while [ $i -lt 20 ]; do printf \"B$i\\n\"; i=$((i+1)); sleep 0.03; done; printf 'END\\n'; cat >/dev/null";

#[tokio::test]
async fn change_wait_settles_only_after_a_sustained_burst_ends() {
    let h = Harness::start().await;
    let terminal = open(&h, BURST, "burst").await;
    let response = call_then_release(
        &h,
        &terminal,
        "go",
        h.call(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_for":"change","settle_ms":150,"wait_ms":3000}),
        ),
    )
    .await;
    let view = receipt(&response);
    assert_eq!(view["wait"]["outcome"], "changed", "{view}");
    assert_eq!(view["wait"]["settled"], true, "{view}");
    let waited = view["wait"]["waited_ms"].as_u64().unwrap();
    assert!(waited < 3000, "waited_ms={waited}: {view}");
    assert!(
        has_line(view, "END"),
        "settled before the burst ended: {view}"
    );
    h.stop(&terminal).await;
}

#[tokio::test]
async fn change_wait_budget_ends_mid_burst_unsettled() {
    let h = Harness::start().await;
    let terminal = open(&h, BURST, "burst-budget").await;
    let response = call_then_release(
        &h,
        &terminal,
        "go",
        h.call(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_for":"change","settle_ms":150,"wait_ms":300}),
        ),
    )
    .await;
    let view = receipt(&response);
    assert_eq!(view["wait"]["outcome"], "changed", "{view}");
    assert_eq!(view["wait"]["settled"], false, "{view}");
    // The budget is a deadline inside the wait, so it is a safe lower bound;
    // the burst started after the wait subscribed, so END cannot be there.
    let waited = view["wait"]["waited_ms"].as_u64().unwrap();
    assert!(waited >= 300, "waited_ms={waited}: {view}");
    assert!(!has_line(view, "END"), "{view}");
    // The burst is still running; a fresh change wait sees more of it.
    let later = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_for":"change","wait_ms":3000}),
        )
        .await;
    assert_eq!(later["changed_since_previous_observation"], true, "{later}");
    h.stop(&terminal).await;
}

#[tokio::test]
async fn omitted_wait_ms_in_change_mode_waits_for_a_late_reply() {
    let h = Harness::start().await;
    // 0.4 s of silence after the marker: a zero budget would report unchanged.
    let terminal = open(
        &h,
        "while [ ! -e go ]; do sleep 0.02; done; sleep 0.4; printf 'DEFAULTED\\n'; cat >/dev/null",
        "default-budget",
    )
    .await;
    let response = call_then_release(
        &h,
        &terminal,
        "go",
        h.call(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_for":"change"}),
        ),
    )
    .await;
    let view = receipt(&response);
    assert_eq!(view["wait"]["outcome"], "changed", "{view}");
    assert!(has_line(view, "DEFAULTED"), "{view}");
    let waited = view["wait"]["waited_ms"].as_u64().unwrap();
    assert!(waited < 2000, "waited_ms={waited}: {view}");
    // Elapsed mode without wait_ms still returns at once.
    let immediate = h
        .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
        .await;
    assert_eq!(
        immediate["wait"],
        json!({"mode":"elapsed","outcome":"elapsed","waited_ms":0,"settled":false,"baseline_signal_seq":0,
            "baseline_revision":view["observation_revision"]})
    );
    assert_eq!(
        immediate["previous_observation_revision"],
        view["observation_revision"]
    );
    h.stop(&terminal).await;
}

/// G1: a status-line style change between the latest observation and the
/// next input is not an error round trip. The stale result carries a fresh
/// observation (registered as the latest) and the same request_id can be
/// resent with the drift flag. A change of control stays an RPC error even
/// when the revision moved as well.
#[tokio::test]
async fn stale_observation_is_a_structured_result_with_a_fresh_observation() {
    let h = Harness::start().await;
    let terminal = open(
        &h,
        "printf 'READY\\n'; while [ ! -e go ]; do sleep 0.02; done; printf 'STATUS_LINE\\n'; while [ ! -e go2 ]; do sleep 0.02; done; printf 'SECOND\\n'; cat >/dev/null",
        "stale",
    )
    .await;
    h.observe_text(&terminal, "READY").await;
    let claimed = claim(&h, &terminal).await;
    let latest = observation(&claimed).clone();
    assert!(!has_line(&latest, "STATUS_LINE"));
    std::fs::write(h.root.path().join("go"), b"x").unwrap();
    let moved = wait_past(&h, &terminal, revision(&latest)).await;
    // No observation was taken since the claim readback: the implicit
    // observation is stale by exactly the program's line.
    let response = h.call("calm.terminal.input", json!({"terminal_id":terminal,"request_id":"enter","action":{"type":"key","key":"Enter"}})).await;
    let stale = receipt(&response);
    assert_eq!(stale["outcome"], "stale_observation", "{stale}");
    assert_eq!(stale["application_result"], "unverified");
    assert_eq!(stale["terminal_id"], terminal);
    assert_eq!(stale["request_id"], "enter");
    assert_eq!(stale["observation_id_used"], latest["observation_id"]);
    assert_eq!(
        stale["observed_revision"].as_u64().unwrap(),
        revision(&latest)
    );
    assert!(
        stale["current_revision"].as_u64().unwrap() >= moved,
        "{stale}"
    );
    assert!(
        stale["next"]
            .as_str()
            .unwrap()
            .contains("resend the same request_id with allow_output_since_observation=true"),
        "{stale}"
    );
    assert!(stale.get("output_since_observation").is_none());
    // The action is validated before stale is decided: against the same
    // stale observation an invalid action is an RPC error, never a
    // successful stale result, and nothing is reserved.
    for (request, action, expected) in [
        (
            "enter-x2",
            json!({"type":"key","key":"Enter","repeat":2}),
            "only navigation and editing keys may repeat",
        ),
        (
            "click-no-mouse",
            json!({"type":"click","column":0,"row":0}),
            "application has not enabled terminal mouse input",
        ),
    ] {
        let invalid = h.call("calm.terminal.input", json!({"terminal_id":terminal,"observation_id":latest["observation_id"],"request_id":request,"action":action})).await;
        assert!(
            error_text(&invalid).contains(expected),
            "{request}: {invalid}"
        );
        assert!(!h.interaction().input_pending(&terminal).await);
    }
    let fresh = observation(&response).clone();
    assert!(has_line(&fresh, "STATUS_LINE"), "{fresh}");
    assert_ne!(fresh["observation_id"], latest["observation_id"]);
    assert_eq!(
        fresh["observation_revision"].as_str().unwrap(),
        stale["current_revision"].as_u64().unwrap().to_string()
    );
    assert_eq!(
        fresh["previous_observation_revision"],
        latest["observation_revision"]
    );
    assert_eq!(fresh["changed_since_previous_observation"], true);
    assert_eq!(
        summary(&response),
        format!(
            "terminal {terminal} input stale_observation readback available; details in structuredContent"
        )
    );
    // Nothing was written: no reservation is pending and the screen is as
    // the fresh observation captured it.
    assert!(!h.interaction().input_pending(&terminal).await);
    assert_eq!(live_revision(&h, &terminal), revision(&fresh));
    // Resend as advised. The fresh observation is the connection's latest
    // and nothing was cached under "enter", so this writes rather than
    // conflicting.
    let resent = h.call("calm.terminal.input", json!({"terminal_id":terminal,"request_id":"enter","action":{"type":"key","key":"Enter"},"allow_output_since_observation":true})).await;
    let written = receipt(&resent);
    assert_eq!(written["outcome"], "written", "{written}");
    assert_eq!(written["observation_id_used"], fresh["observation_id"]);
    assert_eq!(written["output_since_observation"], false);
    // The written receipt is cached with the flag in its fingerprint.
    let replay = h.call("calm.terminal.input", json!({"terminal_id":terminal,"request_id":"enter","action":{"type":"key","key":"Enter"}})).await;
    assert!(
        error_text(&replay).contains("reused with different arguments"),
        "{replay}"
    );
    // Control changed and the revision moved: still an RPC error, never a
    // stale_observation result that would invite a flagged resend.
    let released = h
        .ok(
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"release"}),
        )
        .await;
    assert_eq!(released["control_id"], Value::Null);
    std::fs::write(h.root.path().join("go2"), b"x").unwrap();
    wait_past(&h, &terminal, revision(&fresh)).await;
    let refused = h.call("calm.terminal.input", json!({"terminal_id":terminal,"observation_id":fresh["observation_id"],"request_id":"after-release","action":{"type":"key","key":"Enter"}})).await;
    assert!(
        error_text(&refused).contains("terminal control changed; observe before input"),
        "{refused}"
    );
    h.stop(&terminal).await;
}

/// G3: a release readback of an unchanged screen omits the text array and
/// names the observation it repeats; after output it includes the text.
/// Claim readbacks always include text.
#[tokio::test]
async fn release_readback_omits_text_only_when_unchanged_since_previous_observation() {
    let h = Harness::start().await;
    let terminal = open(&h, "printf 'READY\\n'; cat >/dev/null", "release-text").await;
    h.observe_text(&terminal, "READY").await;
    let claimed = claim(&h, &terminal).await;
    let latest = observation(&claimed).clone();
    assert!(has_line(&latest, "READY"));
    let released = h
        .call(
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"release","observe":true}),
        )
        .await;
    assert_eq!(receipt(&released)["control_id"], Value::Null);
    let state = observation(&released);
    assert!(state.get("text").is_none(), "{state}");
    assert_eq!(
        state["text_omitted"],
        json!(format!(
            "unchanged since previous observation {}",
            latest["observation_id"].as_str().unwrap()
        ))
    );
    // Every other field stays, including the ids and geometry.
    assert_eq!(
        state["observation_revision"],
        latest["observation_revision"]
    );
    assert_eq!(
        state["previous_observation_revision"],
        latest["observation_revision"]
    );
    assert_eq!(state["changed_since_previous_observation"], false);
    assert_eq!(state["role"], "observer");
    assert_eq!(state["cols"], latest["cols"]);
    assert_eq!(state["cursor"], latest["cursor"]);
    assert!(state["observation_id"].is_string());
    assert_ne!(state["observation_id"], latest["observation_id"]);
    assert_eq!(state["wait"]["outcome"], "elapsed");
    // A claim readback of the same unchanged screen still carries text.
    let reclaimed = claim(&h, &terminal).await;
    let reclaimed_state = observation(&reclaimed);
    assert!(has_line(reclaimed_state, "READY"), "{reclaimed_state}");
    assert!(reclaimed_state.get("text_omitted").is_none());
    assert_eq!(
        reclaimed_state["observation_revision"],
        latest["observation_revision"]
    );
    // Output after the claim readback: the release readback includes it.
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    entry
        .handle
        .render_plane
        .lock()
        .unwrap()
        .on_pty_chunk(b"OUTPUT\r\n".to_vec());
    let released = h
        .call(
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"release","observe":true}),
        )
        .await;
    let state = observation(&released);
    assert!(has_line(state, "OUTPUT"), "{state}");
    assert!(state.get("text_omitted").is_none());
    // G2 on a control readback: the wait baseline is the call-start revision
    // (already past the injected output), while the previous-observation
    // fields still point at the claim readback.
    assert_eq!(
        state["wait"]["baseline_revision"],
        state["observation_revision"]
    );
    assert_eq!(
        state["previous_observation_revision"],
        reclaimed_state["observation_revision"]
    );
    assert_eq!(state["changed_since_previous_observation"], true);
    h.stop(&terminal).await;
}

/// The release elision compares live viewports only. The previous
/// observation is a history view (`scroll_offset` 1) of the same revision:
/// its text is not the live text, so the release readback on the quiet
/// screen must carry its own text. After a live observation the elision
/// applies again.
#[tokio::test]
async fn release_readback_keeps_text_after_a_history_view_of_the_same_revision() {
    let h = Harness::start().await;
    let terminal = open(&h, SCROLLBACK, "release-history").await;
    h.observe_text(&terminal, "READY").await;
    claim(&h, &terminal).await;
    let history = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"scroll_offset":1}),
        )
        .await;
    assert_eq!(history["scroll_offset"], 1, "{history}");
    assert!(history["history_rows"].as_u64().unwrap() >= 1);
    let released = h
        .call(
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"release","observe":true}),
        )
        .await;
    let state = observation(&released);
    assert_eq!(
        state["observation_revision"],
        history["observation_revision"]
    );
    assert_eq!(state["scroll_offset"], 0);
    assert!(state.get("text_omitted").is_none(), "{state}");
    assert!(has_line(state, "READY"), "{state}");
    // A live claim readback of the same revision: the release elides again.
    claim(&h, &terminal).await;
    let released = h
        .call(
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"release","observe":true}),
        )
        .await;
    let state = observation(&released);
    assert!(state.get("text").is_none(), "{state}");
    assert!(state["text_omitted"].is_string());
    h.stop(&terminal).await;
}

/// Drift-tolerant input negative table. With `allow_output_since_observation`
/// set, (a) an input-mode change (DECCKM) since the observation is refused by
/// the surface fence and (b) an observation taken as a history view is
/// refused by the live-viewport fence; a live observation of the changed
/// surface still writes. (c) The "prior input outcome unknown" fence (a
/// pending write whose acknowledgement was lost) is not reachable in this
/// harness: the in-process supervisor acknowledges or refuses every write,
/// which clears the reservation, so no tool sequence leaves `pending` set.
/// It is covered by `input_pending` observability only, not by a table row.
#[tokio::test]
async fn drift_tolerant_input_refuses_mode_change_and_history_views() {
    let h = Harness::start().await;
    let terminal = open(&h, SCROLLBACK, "drift-table").await;
    h.observe_text(&terminal, "READY").await;
    let claimed = claim(&h, &terminal).await;
    let live_before_mode = observation(&claimed).clone();
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    entry
        .handle
        .render_plane
        .lock()
        .unwrap()
        .on_pty_chunk(b"\x1b[?1h".to_vec());
    wait_past(&h, &terminal, revision(&live_before_mode)).await;
    let history = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"scroll_offset":1}),
        )
        .await;
    assert_eq!(history["scroll_offset"], 1, "{history}");
    let live = h
        .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
        .await;
    assert_eq!(live["scroll_offset"], 0);
    for (name, observation, expected) in [
        (
            "input-mode-change",
            &live_before_mode,
            "terminal surface changed since observation (size, input modes or alternate screen)",
        ),
        (
            "history-view",
            &history,
            "return to live viewport before input",
        ),
    ] {
        let refused = h.call("calm.terminal.input", json!({"terminal_id":terminal,"observation_id":observation["observation_id"],"request_id":name,"action":{"type":"key","key":"Escape"},"allow_output_since_observation":true})).await;
        assert!(error_text(&refused).contains(expected), "{name}: {refused}");
        assert!(!h.interaction().input_pending(&terminal).await, "{name}");
    }
    let written = h.call("calm.terminal.input", json!({"terminal_id":terminal,"observation_id":live["observation_id"],"request_id":"live","action":{"type":"key","key":"Escape"},"allow_output_since_observation":true})).await;
    assert_eq!(receipt(&written)["outcome"], "written", "{written}");
    h.stop(&terminal).await;
}

/// A change wait stops when the projection is invalidated through the
/// production route rather than idling to its budget. The supervisor output
/// stream is severed the way a lost attach connection severs it (the attach
/// reader's drop guard calls `RenderPlane::invalidate_observation`, which
/// reaches `ModelView::invalidate` through `RenderObserver::unavailable`),
/// only once the observe has subscribed to the projection; the call returns
/// an explicit projection error long before its 10 s budget.
#[tokio::test]
async fn change_wait_stops_when_the_output_source_disconnects() {
    let h = Harness::start().await;
    let terminal = open(&h, "printf 'READY\\n'; cat >/dev/null", "source-loss").await;
    h.observe_text(&terminal, "READY").await;
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    let waiters = || entry.handle.model_view.lock().unwrap().change_waiters();
    let subscribed = waiters();
    let sever = async {
        let start = std::time::Instant::now();
        while waiters() == subscribed {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "the change wait never subscribed"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        entry.disconnect_output_source_for_test();
    };
    let started = std::time::Instant::now();
    let (response, ()) = tokio::join!(
        h.call(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_for":"change","wait_ms":10000})
        ),
        sever
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "the wait idled towards its budget: {elapsed:?} {response}"
    );
    let message = error_text(&response);
    assert!(
        message.contains("terminal projection unavailable")
            && message.contains("terminal output source disconnected"),
        "{response}"
    );
    h.stop(&terminal).await;
}

#[tokio::test]
async fn detach_receipt_names_the_closed_connection() {
    let h = Harness::start().await;
    let terminal = open(&h, "exec /bin/sh", "detach").await;
    let view = h
        .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
        .await;
    let detached = h
        .call(
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"detach"}),
        )
        .await;
    assert_eq!(
        *receipt(&detached),
        json!({"detached":true,"had_client":true,"terminal_id":terminal,
            "connection_id":view["connection_id"],"terminal_session_id":view["terminal_session_id"]})
    );
    assert_eq!(
        summary(&detached),
        format!("terminal {terminal} detached had_client true; details in structuredContent")
    );
    let again = h
        .ok(
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"detach"}),
        )
        .await;
    assert_eq!(
        again,
        json!({"detached":true,"had_client":false,"terminal_id":terminal,"connection_id":null,"terminal_session_id":null})
    );
    let unknown = h
        .ok(
            "calm.terminal.control",
            json!({"terminal_id":"missing-terminal","action":"detach"}),
        )
        .await;
    assert_eq!(
        unknown,
        json!({"detached":true,"had_client":false,"terminal_id":null,"connection_id":null,"terminal_session_id":null})
    );
    h.stop(&terminal).await;
}

#[tokio::test]
async fn text_results_carry_screen_text_only_in_structured_content() {
    let h = Harness::start().await;
    let opened = h
        .call(
            "calm.terminal.open",
            json!({"program":"printf 'SCREEN_SECRET\\n'; exec /bin/sh","request_id":"single-copy"}),
        )
        .await;
    let terminal = receipt(&opened)["terminal_id"].as_str().unwrap().to_owned();
    assert!(summary(&opened).starts_with(&format!("terminal {terminal} observation ")));
    h.observe_text(&terminal, "SCREEN_SECRET").await;
    for args in [
        json!({"terminal_id":terminal}),
        json!({"terminal_id":terminal,"wait_for":"change","wait_ms":50}),
    ] {
        let response = h.call("calm.terminal.observe", args).await;
        let state = receipt(&response);
        assert!(has_line(state, "SCREEN_SECRET"), "{state}");
        let text = summary(&response);
        assert!(!text.contains("SCREEN_SECRET"), "{text}");
        assert!(!text.contains('\n'), "{text}");
        assert!(serde_json::from_str::<Value>(text).is_err(), "{text}");
        assert_eq!(
            text,
            format!(
                "terminal {terminal} observation {} revision {} observer {}x{} cursor {},{} wait {}; full state in structuredContent",
                state["observation_id"].as_str().unwrap(),
                state["observation_revision"].as_str().unwrap(),
                state["cols"],
                state["rows"],
                state["cursor"]["row"],
                state["cursor"]["column"],
                state["wait"]["outcome"].as_str().unwrap()
            )
        );
    }
    let claimed = claim(&h, &terminal).await;
    assert_eq!(
        summary(&claimed),
        format!(
            "terminal {terminal} claim control_id present readback available; details in structuredContent"
        )
    );
    assert!(has_line(observation(&claimed), "SCREEN_SECRET"));
    let resolved = h
        .call("calm.terminal.resolve", json!({"terminal_id":terminal}))
        .await;
    assert_eq!(
        summary(&resolved),
        format!(
            "terminal {terminal} resolved available true controllable true; details in structuredContent"
        )
    );
    let released = h
        .call(
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"release"}),
        )
        .await;
    assert_eq!(
        summary(&released),
        format!("terminal {terminal} release control_id null; details in structuredContent")
    );
    let image = h
        .call(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"format":"image"}),
        )
        .await;
    let content = image["result"]["content"].as_array().unwrap();
    assert_eq!(
        content.len(),
        2,
        "image results keep their native PNG block"
    );
    assert_eq!(content[1]["type"], "image");
    h.stop(&terminal).await;
}
