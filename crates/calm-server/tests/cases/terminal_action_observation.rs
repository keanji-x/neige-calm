//! Actual MCP and PTY; readback is presentation, never another physical action.
use crate::terminal_support::Harness;
use serde_json::{Value, json};
use std::time::Duration;

fn receipt(response: &Value) -> &Value {
    assert!(response.get("error").is_none(), "{response}");
    let content = response["result"]["content"].as_array().unwrap();
    assert_eq!(content.len(), 1, "action readback must not include images");
    assert_eq!(content[0]["type"], "text");
    &response["result"]["structuredContent"]
}
fn observation(response: &Value) -> &Value {
    let result = receipt(response);
    assert_eq!(result["observation"]["status"], "available", "{result}");
    let state = &result["observation"]["state"];
    assert!(state.get("image_source").is_none());
    uuid::Uuid::parse_str(state["observation_id"].as_str().unwrap()).unwrap();
    state
}
fn has_line(state: &Value, expected: &str) -> bool {
    state["text"]
        .as_array()
        .unwrap()
        .iter()
        .any(|line| line.as_str() == Some(expected))
}
async fn open(h: &Harness) -> String {
    h.ok(
        "calm.terminal.open",
        json!({"program":"exec /bin/sh","request_id":"action-view"}),
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
async fn input(h: &Harness, terminal: &str, state: &Value, request: &str, action: Value) -> Value {
    h.call("calm.terminal.input", json!({"terminal_id":terminal,"observation_id":state["observation_id"],"request_id":request,"action":action,"observe":true,"wait_ms":100})).await
}

#[tokio::test]
async fn terminal_actions_return_fresh_text_observations() {
    let h = Harness::start().await;
    let terminal = open(&h).await;
    let claimed = claim(&h, &terminal).await;
    let before = observation(&claimed);
    assert_eq!(before["role"], "owner");
    assert_eq!(before["control_id"], receipt(&claimed)["control_id"]);
    let typed = input(
        &h,
        &terminal,
        before,
        "type",
        json!({"type":"text","text":"printf '%s%s\\n' ACTION_ OBSERVED"}),
    )
    .await;
    assert_eq!(receipt(&typed)["outcome"], "written");
    let typed_state = observation(&typed);
    assert!(
        typed_state["text"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap().contains("printf"))
    );
    assert_ne!(typed_state["observation_id"], before["observation_id"]);
    let entered = input(
        &h,
        &terminal,
        typed_state,
        "enter",
        json!({"type":"key","key":"Enter"}),
    )
    .await;
    assert_eq!(receipt(&entered)["outcome"], "written");
    assert_eq!(receipt(&entered)["application_result"], "unverified");
    let after = observation(&entered);
    assert!(
        has_line(after, "ACTION_OBSERVED"),
        "must observe application output, not command echo: {after}"
    );
    assert_eq!(after["terminal_session_id"], before["terminal_session_id"]);
    let released = h
        .call(
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"release","observe":true}),
        )
        .await;
    assert_eq!(receipt(&released)["control_id"], Value::Null);
    assert_eq!(observation(&released)["role"], "observer");
    assert_eq!(
        observation(&released)["connection_id"],
        before["connection_id"]
    );
    // #1618 G3: a release readback repeats text only when the screen moved
    // since the previous observation (the `entered` readback); the shell
    // prompt may or may not have repainted by now, so both shapes are legal
    // and each is checked exactly. The deterministic cases live in
    // terminal_wait_and_drift.rs.
    let released_state = observation(&released);
    match released_state.get("text") {
        Some(_) => {
            assert!(has_line(released_state, "ACTION_OBSERVED"));
            assert!(released_state.get("text_omitted").is_none());
            assert_ne!(
                released_state["observation_revision"],
                after["observation_revision"]
            );
        }
        None => {
            assert_eq!(
                released_state["text_omitted"],
                json!(format!(
                    "unchanged since previous observation {}",
                    after["observation_id"].as_str().unwrap()
                ))
            );
            assert_eq!(
                released_state["observation_revision"],
                after["observation_revision"]
            );
        }
    }
    h.stop(&terminal).await;
}

#[tokio::test]
async fn changing_readback_options_replays_receipt_without_duplicate_input() {
    let h = Harness::start().await;
    let terminal = h.ok("calm.terminal.open", json!({"program":"i=0; printf 'READY\\n'; while IFS= read -r line; do i=$((i+1)); printf x >> physical-lines; printf 'COUNT:%s:%s\\n' \"$i\" \"$line\"; done","request_id":"counter"})).await["terminal_id"].as_str().unwrap().to_owned();
    let claimed = claim(&h, &terminal).await;
    assert!(has_line(observation(&claimed), "READY"));
    let typed = input(
        &h,
        &terminal,
        observation(&claimed),
        "text",
        json!({"type":"text","text":"PAYLOAD"}),
    )
    .await;
    let args = json!({"terminal_id":terminal,"observation_id":observation(&typed)["observation_id"],"request_id":"enter-once","action":{"type":"key","key":"Enter"}});
    let original = h.call("calm.terminal.input", args.clone()).await;
    let original_receipt = receipt(&original);
    assert_eq!(original_receipt["outcome"], "written");
    assert!(original_receipt.get("observation").is_none());
    let mut readback_args = args.clone();
    readback_args["observe"] = json!(true);
    readback_args["wait_ms"] = json!(100);
    let repeated = h.call("calm.terminal.input", readback_args.clone()).await;
    let mut repeated_receipt = receipt(&repeated).clone();
    repeated_receipt
        .as_object_mut()
        .unwrap()
        .remove("observation");
    assert_eq!(repeated_receipt, *original_receipt);
    assert!(has_line(observation(&repeated), "COUNT:1:PAYLOAD"));
    readback_args["wait_ms"] = json!(0);
    let again = h.call("calm.terminal.input", readback_args).await;
    assert_eq!(receipt(&again)["outcome"], "written");
    assert_ne!(
        observation(&again)["observation_id"],
        observation(&repeated)["observation_id"]
    );
    assert_eq!(
        receipt(&h.call("calm.terminal.input", args).await),
        original_receipt,
        "readback must not enter the cached physical receipt"
    );
    let probe = input(
        &h,
        &terminal,
        observation(&again),
        "probe-text",
        json!({"type":"text","text":"PROBE"}),
    )
    .await;
    let probe = input(
        &h,
        &terminal,
        observation(&probe),
        "probe-enter",
        json!({"type":"key","key":"Enter"}),
    )
    .await;
    assert!(has_line(observation(&probe), "COUNT:2:PROBE"));
    assert_eq!(
        std::fs::read(h.root.path().join("physical-lines")).unwrap(),
        b"xx"
    );
    h.stop(&terminal).await;
}

#[tokio::test]
async fn failed_readback_preserves_written_receipt_and_replay() {
    let h = Harness::start().await;
    let terminal = open(&h).await;
    let claimed = claim(&h, &terminal).await;
    let typed = input(
        &h,
        &terminal,
        observation(&claimed),
        "text",
        json!({"type":"text","text":"printf x >> action-completed"}),
    )
    .await;
    let args = json!({"terminal_id":terminal,"observation_id":observation(&typed)["observation_id"],"request_id":"execute-once","action":{"type":"key","key":"Enter"},"observe":true,"wait_ms":2000});
    let fail_after_execution = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !h.root.path().join("action-completed").exists() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        h.state
            .terminal_renderer
            .get(&terminal)
            .unwrap()
            .handle
            .model_view
            .lock()
            .unwrap()
            .invalidate("readback source lost after execution");
    };
    let (response, ()) = tokio::join!(
        h.call("calm.terminal.input", args.clone()),
        fail_after_execution
    );
    let result = receipt(&response);
    assert_eq!(
        result["outcome"], "written",
        "readback failure must retain the physical write receipt"
    );
    assert_eq!(result["application_result"], "unverified");
    assert_eq!(result["observation"]["status"], "unavailable");
    assert!(
        result["observation"]["reason"]
            .as_str()
            .unwrap()
            .contains("readback source lost")
    );
    assert!(result["observation"].get("state").is_none());
    let mut replay = args;
    replay.as_object_mut().unwrap().remove("observe");
    replay.as_object_mut().unwrap().remove("wait_ms");
    let replay = h.call("calm.terminal.input", replay).await;
    assert_eq!(receipt(&replay)["outcome"], "written");
    assert!(receipt(&replay).get("observation").is_none());
    assert_eq!(
        std::fs::read(h.root.path().join("action-completed")).unwrap(),
        b"x"
    );
    h.stop(&terminal).await;
}

#[tokio::test]
async fn failed_control_readback_keeps_claim_receipt() {
    let h = Harness::start().await;
    let terminal = open(&h).await;
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    let invalidate_after_claim = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while entry
                .handle
                .owner_registry
                .lock()
                .unwrap()
                .current_owner()
                .is_none()
            {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        entry
            .handle
            .model_view
            .lock()
            .unwrap()
            .invalidate("capture failed after claim");
    };
    let (response, ()) = tokio::join!(
        h.call(
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"claim","observe":true,"wait_ms":2000})
        ),
        invalidate_after_claim
    );
    let result = receipt(&response);
    assert!(result["control_id"].is_string());
    assert_eq!(result["observation"]["status"], "unavailable");
    assert!(
        result["observation"]["reason"]
            .as_str()
            .unwrap()
            .contains("capture failed")
    );
    h.stop(&terminal).await;
}

#[tokio::test]
async fn invalid_action_readback_options_fail_before_side_effects() {
    let h = Harness::start().await;
    let terminal = open(&h).await;
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    for options in [
        json!({"observe":true,"wait_ms":20001}),
        json!({"observe":true,"settle_ms":10}),
        json!({"observe":true,"wait_for":"change","settle_ms":2001}),
        json!({"wait_ms":0}),
        json!({"observe":false,"wait_ms":1}),
        json!({"observe":true,"wait_ms":-1}),
    ] {
        let mut args = options;
        args["terminal_id"] = json!(terminal);
        args["action"] = json!("claim");
        assert_eq!(
            h.call("calm.terminal.control", args).await["error"]["code"],
            -32602
        );
        assert!(
            entry
                .handle
                .owner_registry
                .lock()
                .unwrap()
                .current_owner()
                .is_none()
        );
    }
    let claimed = claim(&h, &terminal).await;
    for options in [
        json!({"observe":true,"wait_ms":20001}),
        json!({"observe":true,"settle_ms":10}),
        json!({"observe":true,"wait_for":"change","settle_ms":2001}),
        json!({"wait_ms":0}),
        json!({"observe":false,"wait_ms":1}),
    ] {
        let mut args = options;
        args["terminal_id"] = json!(terminal);
        args["observation_id"] = observation(&claimed)["observation_id"].clone();
        args["request_id"] = json!("invalid");
        args["action"] = json!({"type":"text","text":"BAD"});
        assert_eq!(
            h.call("calm.terminal.input", args).await["error"]["code"],
            -32602
        );
    }
    let invalid_detach = h
        .call(
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"detach","observe":true}),
        )
        .await;
    assert_eq!(invalid_detach["error"]["code"], -32602);
    let current = h
        .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
        .await;
    assert_eq!(
        current["connection_id"],
        observation(&claimed)["connection_id"]
    );
    assert_eq!(current["control_id"], observation(&claimed)["control_id"]);
    assert!(
        !current["text"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap().contains("BAD"))
    );
    h.stop(&terminal).await;
}

#[tokio::test]
async fn repeated_navigation_corrects_one_character_without_replaying_movement() {
    let h = Harness::start().await;
    let terminal = h
        .ok(
            "calm.terminal.open",
            json!({"program":"exec /bin/bash --noprofile --norc","request_id":"readline"}),
        )
        .await["terminal_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let claimed = claim(&h, &terminal).await;
    let typed = input(
        &h,
        &terminal,
        observation(&claimed),
        "draft",
        json!({"type":"text","text":"printf '%s\\n' 'ABC1XYZ'"}),
    )
    .await;
    let args = json!({"terminal_id":terminal,"observation_id":observation(&typed)["observation_id"],"request_id":"left-four","action":{"type":"key","key":"Left","repeat":4},"observe":true,"wait_ms":100});
    let moved = h.call("calm.terminal.input", args.clone()).await;
    assert_eq!(receipt(&moved)["outcome"], "written");
    assert_eq!(
        observation(&moved)["cursor"]["column"].as_u64().unwrap() + 4,
        observation(&typed)["cursor"]["column"].as_u64().unwrap()
    );
    let replay = h.call("calm.terminal.input", args.clone()).await;
    assert_eq!(
        observation(&replay)["cursor"],
        observation(&moved)["cursor"],
        "replayed request must not move the caret again"
    );
    let mut conflicting = args;
    conflicting["action"]["repeat"] = json!(3);
    assert!(
        h.call("calm.terminal.input", conflicting)
            .await
            .get("error")
            .is_some(),
        "repeat is part of the physical action fingerprint"
    );
    let deleted = input(
        &h,
        &terminal,
        observation(&replay),
        "delete",
        json!({"type":"key","key":"Backspace"}),
    )
    .await;
    let corrected = input(
        &h,
        &terminal,
        observation(&deleted),
        "replace",
        json!({"type":"text","text":"9"}),
    )
    .await;
    let submitted = input(
        &h,
        &terminal,
        observation(&corrected),
        "submit",
        json!({"type":"key","key":"Enter"}),
    )
    .await;
    assert!(has_line(observation(&submitted), "ABC9XYZ"), "{submitted}");
    h.stop(&terminal).await;
}

#[tokio::test]
async fn submission_keys_cannot_repeat_or_write_before_validation() {
    let h = Harness::start().await;
    let terminal = h.ok("calm.terminal.open", json!({"program":"i=0; printf 'READY\\n'; while IFS= read -r line; do i=$((i+1)); printf x >> physical-lines; printf 'COUNT:%s:%s\\n' \"$i\" \"$line\"; done","request_id":"key-boundary"})).await["terminal_id"].as_str().unwrap().to_owned();
    let claimed = claim(&h, &terminal).await;
    let typed = input(
        &h,
        &terminal,
        observation(&claimed),
        "text",
        json!({"type":"text","text":"PROBE"}),
    )
    .await;
    let forbidden = h.call("calm.terminal.input", json!({"terminal_id":terminal,"observation_id":observation(&typed)["observation_id"],"request_id":"forbidden","action":{"type":"key","key":"Enter","repeat":2},"observe":true,"wait_ms":100})).await;
    // A real input-counting application detects any accidental repeated Enter,
    // including a blank line; error text alone is not the physical evidence.
    let current = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_ms":100}),
        )
        .await;
    let submitted = input(
        &h,
        &terminal,
        &current,
        "submit",
        json!({"type":"key","key":"Enter"}),
    )
    .await;
    assert_eq!(
        std::fs::read(h.root.path().join("physical-lines")).unwrap(),
        b"x",
        "forbidden repeated Enter must never reach the application"
    );
    assert!(has_line(observation(&submitted), "COUNT:1:PROBE"));
    assert!(forbidden.get("error").is_some());
    for key in [
        "Escape", "Tab", "Ctrl+C", "Ctrl+D", "Ctrl+J", "Home", "PageUp",
    ] {
        let rejected = h.call("calm.terminal.input", json!({"terminal_id":terminal,"observation_id":observation(&submitted)["observation_id"],"request_id":key,"action":{"type":"key","key":key,"repeat":2}})).await;
        assert!(rejected.get("error").is_some(), "{rejected}");
    }
    for (index, repeat) in [
        json!(null),
        json!(0),
        json!(33),
        json!(-1),
        json!(1.5),
        json!("2"),
    ]
    .into_iter()
    .enumerate()
    {
        let rejected = h.call("calm.terminal.input", json!({"terminal_id":terminal,"observation_id":observation(&submitted)["observation_id"],"request_id":format!("invalid-{index}"),"action":{"type":"key","key":"Left","repeat":repeat}})).await;
        assert!(rejected.get("error").is_some(), "{rejected}");
    }
    h.stop(&terminal).await;
}

#[tokio::test]
async fn action_readback_does_not_follow_a_replacement_worker_session() {
    use calm_server::db::prelude::*;
    use calm_server::db::sqlite::session_supersede_and_start_tx;
    use calm_server::model::{new_id, now_ms};
    use calm_server::session_projection_repo::{
        WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    let h = Harness::start().await;
    let terminal = open(&h).await;
    let claimed = claim(&h, &terminal).await;
    let state = observation(&claimed);
    let old = h
        .sql
        .session_get_by_id(&state["worker_session_id"].as_str().unwrap().into())
        .await
        .unwrap()
        .unwrap();
    let old_id = old.id.to_string();
    let next = new_id();
    let typed = input(
        &h,
        &terminal,
        state,
        "text",
        json!({"type":"text","text":"printf x >> replace-after-write"}),
    )
    .await;
    let replace_after_execution = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !h.root.path().join("replace-after-write").exists() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        let mut tx = h.sql.pool().begin().await.unwrap();
        session_supersede_and_start_tx(
            &mut tx,
            &old_id,
            WorkerSessionInit {
                id: next.clone(),
                card_id: state["card_id"].as_str().unwrap().into(),
                kind: WorkerSessionKind::Terminal,
                agent_provider: None,
                status: WorkerSessionState::Running,
                terminal_run_id: Some(terminal.clone()),
                thread_id: None,
                session_id: None,
                active_turn_id: None,
                handle_state_json: None,
                spawn_op_id: old.spawn_op_id.clone(),
                now_ms: now_ms(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    };
    let (response, ()) = tokio::join!(h.call("calm.terminal.input", json!({"terminal_id":terminal,"observation_id":observation(&typed)["observation_id"],"request_id":"execute","action":{"type":"key","key":"Enter"},"observe":true,"wait_ms":2000})), replace_after_execution);
    let result = receipt(&response);
    assert_eq!(result["outcome"], "written");
    assert_eq!(result["observation"]["status"], "unavailable");
    assert!(result["observation"]["state"].is_null());
    let resolved = h
        .ok("calm.terminal.resolve", json!({"terminal_id":terminal}))
        .await;
    assert_eq!(resolved["worker_session_id"], next);
    assert_eq!(
        std::fs::read(h.root.path().join("replace-after-write")).unwrap(),
        b"x"
    );
    h.stop(&terminal).await;
}

#[tokio::test]
async fn explicit_ctrl_j_reaches_the_pty_as_lf_and_enter_as_cr() {
    let h = Harness::start().await;
    // Raw mode disables line-discipline CR/LF conversion. A real byte-reading
    // application observes exactly what the production PTY writer delivered.
    let terminal = h.ok("calm.terminal.open", json!({"program":"stty raw -echo; printf 'READY\\r\\n'; dd bs=1 count=2 2>/dev/null | od -An -tu1; cat >/dev/null","request_id":"raw-keys"})).await["terminal_id"].as_str().unwrap().to_owned();
    let claimed = claim(&h, &terminal).await;
    assert!(has_line(observation(&claimed), "READY"));
    let refused_text = h.call("calm.terminal.input", json!({"terminal_id":terminal,"observation_id":observation(&claimed)["observation_id"],"request_id":"raw-newline","action":{"type":"text","text":"first\nsecond"}})).await;
    assert!(
        refused_text.get("error").is_some(),
        "text still must exclude control characters"
    );
    let lf = input(
        &h,
        &terminal,
        observation(&claimed),
        "line-feed",
        json!({"type":"key","key":"Ctrl+J"}),
    )
    .await;
    assert_eq!(receipt(&lf)["outcome"], "written");
    let cr = input(
        &h,
        &terminal,
        observation(&lf),
        "carriage-return",
        json!({"type":"key","key":"Enter"}),
    )
    .await;
    let state = observation(&cr);
    assert!(
        state["text"].as_array().unwrap().iter().any(|line| line
            .as_str()
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>()
            == ["10", "13"]),
        "physical bytes must be LF then CR: {state}"
    );
    h.stop(&terminal).await;
}
