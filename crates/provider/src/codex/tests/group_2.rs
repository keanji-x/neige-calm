use super::*;

#[tokio::test]
async fn thread_start_with_params_sends_runtime_fields() {
    let mut h = harness().await;
    let client = h.client.with_request_timeout(Duration::from_secs(5));
    let req_fut = client.thread_start_with_params(ThreadStartParams {
        cwd: "/workspace".into(),
        approval_policy: "never".into(),
        sandbox_mode: "workspace-write".into(),
        developer_instructions: None,
        config: None,
    });

    let server_task = tokio::spawn(async move {
        let req = server_recv_json(&mut h.server).await;
        assert_eq!(
            req.get("method").and_then(Value::as_str),
            Some("thread/start")
        );
        let params = req.get("params").expect("params");
        assert_eq!(
            params.get("cwd").and_then(Value::as_str),
            Some("/workspace")
        );
        assert_eq!(
            params.get("approvalPolicy").and_then(Value::as_str),
            Some("never")
        );
        assert_eq!(
            params.get("sandbox").and_then(Value::as_str),
            Some("workspace-write")
        );
        assert!(
            !params
                .as_object()
                .is_some_and(|params| params.contains_key("additionalContext")),
            "PR5 must not send additionalContext: {req}"
        );
        assert!(
            !params
                .as_object()
                .is_some_and(|params| params.contains_key("config")),
            "thread/start must omit config when None: {req}"
        );

        let id = req.get("id").cloned().unwrap();
        server_send_json(
            &mut h.server,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "thread": { "id": "with-runtime-fields" }, "model": "m" },
            }),
        )
        .await;
        h.server
    });

    let result = req_fut.await.expect("thread/start");
    assert_eq!(result.thread_id(), Some("with-runtime-fields"));
    let _server = server_task.await.unwrap();
}

#[tokio::test]
async fn thread_start_with_params_sends_config_when_some() {
    let mut h = harness().await;
    let client = h.client.with_request_timeout(Duration::from_secs(5));
    let expected = json!({
        "shell_environment_policy": {
            "set": {
                "NEIGE_MCP_SOCKET": "/tmp/calm.sock",
                "NEIGE_MCP_TOKEN": "raw-per-card",
            }
        }
    });
    let req_fut = client.thread_start_with_params(ThreadStartParams {
        cwd: "/workspace".into(),
        approval_policy: "never".into(),
        sandbox_mode: "workspace-write".into(),
        developer_instructions: None,
        config: Some(expected.clone()),
    });

    let server_task = tokio::spawn(async move {
        let req = server_recv_json(&mut h.server).await;
        assert_eq!(
            req.get("method").and_then(Value::as_str),
            Some("thread/start")
        );
        let params = req.get("params").expect("params");
        assert_eq!(params.get("config"), Some(&expected));

        let id = req.get("id").cloned().unwrap();
        server_send_json(
            &mut h.server,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "thread": { "id": "with-config" }, "model": "m" },
            }),
        )
        .await;
        h.server
    });

    let result = req_fut.await.expect("thread/start");
    assert_eq!(result.thread_id(), Some("with-config"));
    let _server = server_task.await.unwrap();
}

#[tokio::test]
async fn thread_start_with_params_omits_config_when_none() {
    let mut h = harness().await;
    let client = h.client.with_request_timeout(Duration::from_secs(5));
    let req_fut = client.thread_start_with_params(ThreadStartParams {
        cwd: "/workspace".into(),
        approval_policy: "never".into(),
        sandbox_mode: "workspace-write".into(),
        developer_instructions: None,
        config: None,
    });

    let server_task = tokio::spawn(async move {
        let req = server_recv_json(&mut h.server).await;
        assert_eq!(
            req.get("method").and_then(Value::as_str),
            Some("thread/start")
        );
        let params = req.get("params").expect("params");
        assert!(!params.as_object().unwrap().contains_key("config"));

        let id = req.get("id").cloned().unwrap();
        server_send_json(
            &mut h.server,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "thread": { "id": "without-config" }, "model": "m" },
            }),
        )
        .await;
        h.server
    });

    let result = req_fut.await.expect("thread/start");
    assert_eq!(result.thread_id(), Some("without-config"));
    let _server = server_task.await.unwrap();
}

#[tokio::test]
async fn thread_resume_with_config_sends_config_when_some() {
    let mut h = harness().await;
    let client = h.client.with_request_timeout(Duration::from_secs(5));
    let expected = json!({
        "shell_environment_policy": {
            "set": {
                "NEIGE_MCP_SOCKET": "/tmp/calm.sock",
                "NEIGE_MCP_TOKEN": "raw-per-card",
            }
        }
    });
    let req_fut = client.thread_resume_with_config("thread-123", Some(expected.clone()));

    let server_task = tokio::spawn(async move {
        let req = server_recv_json(&mut h.server).await;
        assert_eq!(
            req.get("method").and_then(Value::as_str),
            Some("thread/resume")
        );
        let params = req.get("params").expect("params");
        assert_eq!(
            params.get("threadId").and_then(Value::as_str),
            Some("thread-123")
        );
        assert_eq!(params.get("config"), Some(&expected));

        let id = req.get("id").cloned().unwrap();
        server_send_json(
            &mut h.server,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "thread": { "id": "thread-123" }, "model": "m" },
            }),
        )
        .await;
        h.server
    });

    let result = req_fut.await.expect("thread/resume");
    assert_eq!(result.thread_id(), Some("thread-123"));
    let _server = server_task.await.unwrap();
}

#[tokio::test]
async fn thread_resume_with_config_omits_config_when_none() {
    let mut h = harness().await;
    let client = h.client.with_request_timeout(Duration::from_secs(5));
    let req_fut = client.thread_resume_with_config("thread-456", None);

    let server_task = tokio::spawn(async move {
        let req = server_recv_json(&mut h.server).await;
        assert_eq!(
            req.get("method").and_then(Value::as_str),
            Some("thread/resume")
        );
        let params = req.get("params").expect("params");
        assert_eq!(
            params.get("threadId").and_then(Value::as_str),
            Some("thread-456")
        );
        assert!(!params.as_object().unwrap().contains_key("config"));

        let id = req.get("id").cloned().unwrap();
        server_send_json(
            &mut h.server,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "thread": { "id": "thread-456" }, "model": "m" },
            }),
        )
        .await;
        h.server
    });

    let result = req_fut.await.expect("thread/resume");
    assert_eq!(result.thread_id(), Some("thread-456"));
    let _server = server_task.await.unwrap();
}

#[test]
fn input_item_text_serializes_to_schema_shape() {
    let item = InputItem::text("hello");
    let v = serde_json::to_value(&item).unwrap();
    assert_eq!(v, json!({ "type": "text", "text": "hello" }));
}

#[test]
fn thread_result_plucks_id_and_tolerates_extra_fields() {
    // Extra top-level + nested fields must not break deserialization.
    let raw = json!({
        "thread": { "id": "abc-123", "status": { "type": "idle" }, "turns": [] },
        "model": "gpt-5.5",
        "cwd": "/tmp",
        "approvalPolicy": "never",
        "unknownFutureField": 42
    });
    let r: ThreadResult = serde_json::from_value(raw).unwrap();
    assert_eq!(r.thread_id(), Some("abc-123"));
    assert_eq!(r.model.as_deref(), Some("gpt-5.5"));
}

#[test]
fn thread_result_without_a_model_reads_none() {
    let r: ThreadResult = serde_json::from_value(json!({ "thread": { "id": "abc" } })).unwrap();
    assert_eq!(r.model, None);
}

/// The assertion is on the frame, because the frame is the entire contract.
#[test]
fn a_chosen_model_and_effort_reach_the_turn_start_frame() {
    let frame = turn_start_params(
        "thread-1",
        &[InputItem::text("hi")],
        &TurnModelSelection {
            model: Some("gpt-5".into()),
            effort: Some("high".into()),
        },
        TurnApprovals::Unchanged,
        None,
    );
    assert_eq!(frame["threadId"], json!("thread-1"));
    assert_eq!(frame["model"], json!("gpt-5"));
    assert_eq!(frame["effort"], json!("high"));
}

/// Asserted in both directions so a fixture whose two identifiers are equal cannot let a read of the wrong field pass.
#[test]
fn the_frame_carries_a_slug_and_never_a_preset_id() {
    let catalog_entry = json!({ "id": "preset-abc", "model": "gpt-5" });
    let frame = turn_start_params(
        "thread-1",
        &[],
        &TurnModelSelection {
            model: catalog_entry["model"].as_str().map(ToOwned::to_owned),
            effort: None,
        },
        TurnApprovals::Unchanged,
        None,
    );
    assert_eq!(frame["model"], json!("gpt-5"));
    assert_ne!(frame["model"], json!("preset-abc"));
}

/// `inherit` sends no `model`, no `effort`, and in particular no explicit `null`.
#[test]
fn inherit_sends_neither_key_and_not_a_null_either() {
    let frame = turn_start_params(
        "thread-1",
        &[],
        &TurnModelSelection::inherit(),
        TurnApprovals::Unchanged,
        None,
    );
    let map = frame.as_object().expect("params is an object");
    assert!(!map.contains_key("model"), "frame was {frame}");
    assert!(!map.contains_key("effort"), "frame was {frame}");
    assert_eq!(map.len(), 2, "only threadId and input: {frame}");
}

/// Half a selection puts half a frame on the wire; the absent half stays
/// absent rather than becoming a null.
#[test]
fn each_key_is_omitted_independently() {
    let model_only = turn_start_params(
        "t",
        &[],
        &TurnModelSelection {
            model: Some("gpt-5".into()),
            effort: None,
        },
        TurnApprovals::Unchanged,
        None,
    );
    assert_eq!(model_only["model"], json!("gpt-5"));
    assert!(!model_only.as_object().unwrap().contains_key("effort"));

    let effort_only = turn_start_params(
        "t",
        &[],
        &TurnModelSelection {
            model: None,
            effort: Some("low".into()),
        },
        TurnApprovals::Unchanged,
        None,
    );
    assert_eq!(effort_only["effort"], json!("low"));
    assert!(!effort_only.as_object().unwrap().contains_key("model"));
}

/// Codex accepts any non-empty effort string, so an unrecognised one must reach the wire unaltered.
#[test]
fn an_unrecognised_effort_string_is_not_filtered_out() {
    let frame = turn_start_params(
        "t",
        &[],
        &TurnModelSelection {
            model: None,
            effort: Some("ludicrous".into()),
        },
        TurnApprovals::Unchanged,
        None,
    );
    assert_eq!(frame["effort"], json!("ludicrous"));
}

/// The drain's client id reaches the frame under codex's own key, and its absence is an absent key rather than `null`.
#[test]
fn a_client_user_message_id_reaches_the_frame_and_is_omitted_otherwise() {
    let frame = turn_start_params(
        "t",
        &[],
        &TurnModelSelection::inherit(),
        TurnApprovals::Unchanged,
        Some("entry-0001"),
    );
    assert_eq!(frame["clientUserMessageId"], json!("entry-0001"));
    let bare = turn_start_params(
        "t",
        &[],
        &TurnModelSelection::inherit(),
        TurnApprovals::Unchanged,
        None,
    );
    assert!(
        !bare
            .as_object()
            .unwrap()
            .contains_key("clientUserMessageId"),
        "frame was {bare}"
    );
}

/// The steer frame carries the three required keys plus the client id, omitted rather than `null` when absent; no `model`, no `effort`.
#[test]
fn a_steer_frame_carries_the_client_id_and_omits_it_otherwise() {
    let input = vec![InputItem::text("now")];
    let frame = turn_steer_params("t", "turn-7", &input, Some("entry-0002"));
    assert_eq!(frame["threadId"], json!("t"));
    assert_eq!(frame["expectedTurnId"], json!("turn-7"));
    assert_eq!(frame["input"], serde_json::to_value(&input).unwrap());
    assert_eq!(frame["clientUserMessageId"], json!("entry-0002"));
    let keys = frame
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(keys.len(), 4, "no settings ride on a steer: {keys:?}");

    let bare = turn_steer_params("t", "turn-7", &input, None);
    assert!(
        !bare
            .as_object()
            .unwrap()
            .contains_key("clientUserMessageId"),
        "frame was {bare}"
    );
    assert_eq!(bare.as_object().unwrap().len(), 3);
}

#[test]
fn turn_start_result_plucks_turn_id() {
    let raw = json!({ "turn": { "id": "turn-9", "status": "inProgress", "items": [] } });
    let r: TurnStartResult = serde_json::from_value(raw).unwrap();
    assert_eq!(r.turn_id(), Some("turn-9"));
}

#[test]
fn turn_steer_result_camel_case() {
    let raw = json!({ "turnId": "turn-42" });
    let r: TurnSteerResult = serde_json::from_value(raw).unwrap();
    assert_eq!(r.turn_id, "turn-42");
}

#[test]
fn initialize_result_tolerates_unknown_fields() {
    let raw = json!({
        "userAgent": "codex/0.133.0",
        "codexHome": "/home/x/.codex",
        "platformFamily": "unix",
        "platformOs": "linux",
        "somethingNew": true
    });
    let r: InitializeResult = serde_json::from_value(raw).unwrap();
    assert_eq!(r.platform_os, "linux");
}

#[test]
fn notification_parse_maps_known_methods() {
    let n = Notification::parse(
        "turn/completed".into(),
        json!({ "threadId": "t1", "turn": { "id": "u1" } }),
    );
    match n {
        Notification::TurnCompleted { thread_id, turn } => {
            assert_eq!(thread_id, "t1");
            assert_eq!(turn.get("id").and_then(Value::as_str), Some("u1"));
        }
        other => panic!("expected TurnCompleted, got {other:?}"),
    }

    let n = Notification::parse(
        "thread/status/changed".into(),
        json!({ "threadId": "t1", "status": { "type": "active", "activeFlags": [] } }),
    );
    assert!(matches!(n, Notification::ThreadStatusChanged { .. }));

    let n = Notification::parse("item/agentMessage/delta".into(), json!({ "delta": "x" }));
    match n {
        Notification::Item { method, .. } => assert_eq!(method, "item/agentMessage/delta"),
        other => panic!("expected Item, got {other:?}"),
    }
}

/// `turn/plan/updated` needs no variant: it must reach the run loop through `Other` with `params` unchanged (`Value`-level equality) and `thread_id()` resolving from the top-level `threadId`.
#[test]
fn notification_parse_preserves_turn_plan_updated_frame() {
    let params = json!({
        "threadId": "t-plan",
        "turnId": "turn-plan-1",
        "explanation": null,
        "plan": [
            { "step": "first", "status": "inProgress" },
            { "step": "second", "status": "pending" }
        ]
    });
    let n = Notification::parse("turn/plan/updated".into(), params.clone());
    assert_eq!(n.thread_id(), Some("t-plan"));
    match n {
        Notification::Other {
            method,
            params: got,
        } => {
            assert_eq!(method, "turn/plan/updated");
            assert_eq!(
                got, params,
                "params must survive parse with no field dropped, added or reshaped"
            );
        }
        other => panic!("expected Other, got {other:?}"),
    }
}

#[test]
fn notification_parse_unknown_method_is_other_not_error() {
    let n = Notification::parse("thread/realtime/sdp".into(), json!({ "anything": 1 }));
    match n {
        Notification::Other { method, .. } => assert_eq!(method, "thread/realtime/sdp"),
        other => panic!("expected Other, got {other:?}"),
    }
}

#[test]
fn notification_thread_id_reads_direct_variants() {
    let status = Notification::ThreadStatusChanged {
        thread_id: "thread-status".into(),
        status: json!({ "type": "idle" }),
    };
    let started = Notification::TurnStarted {
        thread_id: "thread-started".into(),
        turn: json!({ "id": "turn-1" }),
    };
    let completed = Notification::TurnCompleted {
        thread_id: "thread-completed".into(),
        turn: json!({ "id": "turn-1" }),
    };

    assert_eq!(status.thread_id(), Some("thread-status"));
    assert_eq!(started.thread_id(), Some("thread-started"));
    assert_eq!(completed.thread_id(), Some("thread-completed"));
}

#[test]
fn notification_thread_id_reads_thread_started_params() {
    let nested = Notification::ThreadStarted {
        params: json!({ "thread": { "id": "thread-nested" } }),
    };
    let flat = Notification::ThreadStarted {
        params: json!({ "threadId": "thread-flat" }),
    };

    assert_eq!(nested.thread_id(), Some("thread-nested"));
    assert_eq!(flat.thread_id(), Some("thread-flat"));
}

#[test]
fn notification_thread_id_reads_item_and_other_params() {
    let item = Notification::Item {
        method: "item/completed".into(),
        params: json!({ "threadId": "thread-item" }),
    };
    let other = Notification::Other {
        method: "approval/request".into(),
        params: json!({ "threadId": "thread-other" }),
    };

    assert_eq!(item.thread_id(), Some("thread-item"));
    assert_eq!(other.thread_id(), Some("thread-other"));
}

/// Asserts the exact bytes because the only reader is codex, whose types are not a compilable dependency: deleting the variant-level `#[serde(rename = "localImage")]` turns only this red.
#[test]
fn local_image_serializes_with_the_camel_case_tag() {
    let json = serde_json::to_value(InputItem::local_image("/w/.neige/a.png")).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"type": "localImage", "path": "/w/.neige/a.png"}),
    );
    // The sibling variant is unaffected: the rename is scoped to the one variant that needs it.
    assert_eq!(
        serde_json::to_value(InputItem::text("hi")).unwrap(),
        serde_json::json!({"type": "text", "text": "hi"}),
    );
}

/// `detail` is optional on codex's side and we deliberately send nothing.
#[test]
fn a_local_image_item_has_exactly_two_keys() {
    let json = serde_json::to_value(InputItem::local_image("/w/a.png")).unwrap();
    let object = json.as_object().expect("an input item is an object");
    let mut keys = object.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    assert_eq!(keys, vec!["path".to_string(), "type".to_string()]);
}
