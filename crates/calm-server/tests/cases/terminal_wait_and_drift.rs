//! #1618: change waiting, drift-tolerant input, implicit observation, receipt
//! wording and one copy of the state. Actual MCP server, renderer and PTY.
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
    std::fs::write(h.root.path().join("go"), b"x").unwrap();
    let response = h
        .call(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_for":"change","wait_ms":5000}),
        )
        .await;
    let view = receipt(&response);
    assert_eq!(view["wait"]["mode"], "change", "{view}");
    assert_eq!(view["wait"]["outcome"], "changed", "{view}");
    assert_eq!(view["wait"]["settled"], true, "{view}");
    let waited = view["wait"]["waited_ms"].as_u64().unwrap();
    assert!((400..5000).contains(&waited), "waited_ms={waited}");
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
        json!({"mode":"elapsed","outcome":"elapsed","waited_ms":200,"settled":false})
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
    let inject = async {
        tokio::time::sleep(Duration::from_millis(500)).await;
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
    // A reply 300 ms after the write is included without a second call.
    let second = h.call("calm.terminal.input", json!({"terminal_id":terminal,"request_id":"second","action":{"type":"key","key":"Enter"},"observe":true,"wait_for":"change","wait_ms":3000})).await;
    let state = observation(&second);
    assert_eq!(state["wait"]["outcome"], "changed", "{state}");
    assert!(
        state["wait"]["waited_ms"].as_u64().unwrap() >= 300,
        "{state}"
    );
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
    tokio::time::sleep(Duration::from_millis(200)).await;
    let refused = h.call("calm.terminal.input", json!({"terminal_id":terminal,"observation_id":observed["observation_id"],"request_id":"escape","action":{"type":"key","key":"Escape"}})).await;
    assert!(
        error_text(&refused).contains("terminal changed since observation"),
        "{refused}"
    );
    let conflicting = h.call("calm.terminal.input", json!({"terminal_id":terminal,"observation_id":observed["observation_id"],"request_id":"escape","action":{"type":"key","key":"Escape"},"allow_output_since_observation":true})).await;
    // The refusal above never cached a receipt, so the same request_id is
    // free; the flag is part of the fingerprint once a receipt exists.
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
    let typed = h.call("calm.terminal.input", json!({"terminal_id":terminal,"request_id":"type","action":{"type":"text","text":"printf 'IMPLICIT_OK\\n'"},"observe":true,"wait_for":"change"})).await;
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
    let entered = h.call("calm.terminal.input", json!({"terminal_id":terminal,"request_id":"enter","action":{"type":"key","key":"Enter"},"observe":true,"wait_for":"change"})).await;
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
