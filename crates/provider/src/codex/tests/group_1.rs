use super::*;

#[test]
fn thread_read_parses_last_turn_completed_at_null() {
    // (a) died-mid-turn: last turn `completedAt: null`.
    let resp: ThreadReadResponse = serde_json::from_value(json!({
        "thread": {
            "status": { "type": "idle" },
            "turns": [
                { "completedAt": 1700, "status": "completed" },
                { "completedAt": null, "status": "inProgress" }
            ]
        }
    }))
    .unwrap();
    assert_eq!(resp.thread.status, ThreadStatus::Idle);
    let turns = resp.thread.turns.unwrap();
    assert_eq!(turns.last().unwrap().completed_at, None);
    assert_eq!(turns.last().unwrap().status, TurnStatus::InProgress);
}

#[test]
fn thread_read_parses_last_turn_completed_at_some() {
    // (b) clean finish / deliberate abort: last turn `completedAt: <ts>`.
    let resp: ThreadReadResponse = serde_json::from_value(json!({
        "thread": {
            "status": { "type": "idle" },
            "turns": [
                { "completedAt": 1700, "status": "completed" },
                { "completedAt": 1800, "status": "failed" }
            ]
        }
    }))
    .unwrap();
    let turns = resp.thread.turns.unwrap();
    assert_eq!(turns.last().unwrap().completed_at, Some(1800));
    assert_eq!(turns.last().unwrap().status, TurnStatus::Failed);
}

#[test]
fn thread_read_parses_an_unmodelled_turn_status_as_unknown() {
    let resp: ThreadReadResponse = serde_json::from_value(json!({
        "thread": {
            "status": { "type": "idle" },
            "turns": [
                { "completedAt": 1700, "status": "queued" },
                { "completedAt": 1800, "status": "completed" },
                { "completedAt": 1900, "status": "queued" }
            ]
        }
    }))
    .unwrap();
    let statuses: Vec<_> = resp
        .thread
        .turns
        .unwrap()
        .iter()
        .map(|t| t.status)
        .collect();
    assert_eq!(
        statuses,
        [
            TurnStatus::Unknown,
            TurnStatus::Completed,
            TurnStatus::Unknown
        ]
    );
}

#[test]
fn thread_read_requires_the_turn_status_field() {
    let absent = serde_json::from_value::<ThreadReadResponse>(json!({
        "thread": { "status": { "type": "idle" }, "turns": [ { "completedAt": 1700 } ] }
    }));
    assert!(absent.is_err(), "{absent:?}");
}

#[test]
fn thread_read_parses_active_waiting_on_user_input() {
    // (c) status `active` with `activeFlags:["waitingOnUserInput"]`.
    let resp: ThreadReadResponse = serde_json::from_value(json!({
        "thread": {
            "status": {
                "type": "active",
                "activeFlags": ["waitingOnUserInput"]
            },
            "turns": []
        }
    }))
    .unwrap();
    assert_eq!(
        resp.thread.status,
        ThreadStatus::Active {
            active_flags: vec![ThreadActiveFlag::WaitingOnUserInput]
        }
    );
    // empty list deserializes to Some([]) — "no turns" for the arbiter.
    assert_eq!(resp.thread.turns, Some(vec![]));
}

#[test]
fn thread_read_parses_not_loaded_and_absent_turns() {
    // (d) `notLoaded`, and `turns` absent (include_turns=false) → None.
    let resp: ThreadReadResponse = serde_json::from_value(json!({
        "thread": { "status": { "type": "notLoaded" } }
    }))
    .unwrap();
    assert_eq!(resp.thread.status, ThreadStatus::NotLoaded);
    assert_eq!(resp.thread.turns, None);
}

#[test]
fn thread_loaded_list_plucks_data_and_tolerates_cursor() {
    let resp: ThreadLoadedListResponse = serde_json::from_value(json!({
        "data": ["t-1", "t-2"],
        "nextCursor": "opaque"
    }))
    .unwrap();
    assert_eq!(resp.data, vec!["t-1".to_string(), "t-2".to_string()]);
}

#[tokio::test]
async fn recv_result_returns_err_when_server_closes() {
    let (_client, mut notifs, mut server) = CodexAppServer::connect_pair_for_test().await;
    server.close(None).await.expect("server close");

    let err = tokio::time::timeout(Duration::from_secs(1), notifs.recv_result())
        .await
        .expect("recv_result should resolve when the server closes")
        .expect_err("closed notification stream must be an error");
    assert!(matches!(err, Error::Transport(msg) if msg.contains("notification stream closed")));
}

#[tokio::test]
async fn recv_result_returns_err_after_reader_task_exits() {
    let (client, mut notifs, _server) = CodexAppServer::connect_pair_for_test().await;
    drop(client);

    let err = tokio::time::timeout(Duration::from_secs(1), notifs.recv_result())
        .await
        .expect("recv_result should resolve after reader task exit")
        .expect_err("reader task exit must close the notification stream");
    assert!(matches!(err, Error::Transport(msg) if msg.contains("notification stream closed")));

    let err = tokio::time::timeout(Duration::from_secs(1), notifs.recv_result())
        .await
        .expect("recv_result should stay resolved after reader task exit")
        .expect_err("subsequent recv_result calls must also error");
    assert!(matches!(err, Error::Transport(msg) if msg.contains("notification stream closed")));
}

#[tokio::test]
async fn malformed_notification_frame_is_skipped() {
    let (_client, mut notifs, mut server) = CodexAppServer::connect_pair_for_test().await;
    server
        .send(Message::Text("{not valid json".to_string()))
        .await
        .expect("server send malformed frame");
    server_send_json(
        &mut server,
        json!({
            "jsonrpc": "2.0",
            "method": "item/agentMessage/delta",
            "params": { "delta": "after-malformed" },
        }),
    )
    .await;

    let notification = tokio::time::timeout(Duration::from_secs(1), notifs.recv_result())
        .await
        .expect("recv_result should skip malformed frames and reach the next notification")
        .expect("valid notification after malformed frame should be delivered");
    match notification {
        Notification::Item { method, params } => {
            assert_eq!(method, "item/agentMessage/delta");
            assert_eq!(
                params.get("delta").and_then(Value::as_str),
                Some("after-malformed")
            );
        }
        other => panic!("expected Item notification, got {other:?}"),
    }
}

#[tokio::test]
async fn await_notification_returns_err_when_closed_without_match() {
    let (_client, mut notifs, mut server) = CodexAppServer::connect_pair_for_test().await;
    server_send_json(
        &mut server,
        json!({
            "jsonrpc": "2.0",
            "method": "thread/status/changed",
            "params": { "threadId": "t1", "status": { "type": "active" } },
        }),
    )
    .await;
    server.close(None).await.expect("server close");

    let err = tokio::time::timeout(
        Duration::from_secs(1),
        notifs.await_notification(|notification| {
            matches!(notification, Notification::TurnCompleted { .. })
        }),
    )
    .await
    .expect("await_notification should resolve when the stream closes")
    .expect_err("closed stream before a predicate match must be an error");
    assert!(matches!(err, Error::Transport(msg) if msg.contains("notification stream closed")));
}

#[tokio::test]
async fn notification_await_apis_return_matching_notifications() {
    let (_client, mut notifs, mut server) = CodexAppServer::connect_pair_for_test().await;
    server_send_json(
        &mut server,
        json!({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": { "threadId": "t1", "turn": { "id": "turn-1" } },
        }),
    )
    .await;

    let notification = notifs.recv_result().await.expect("notification");
    match notification {
        Notification::TurnCompleted { thread_id, turn } => {
            assert_eq!(thread_id, "t1");
            assert_eq!(turn.get("id").and_then(Value::as_str), Some("turn-1"));
        }
        other => panic!("expected TurnCompleted, got {other:?}"),
    }

    server_send_json(
        &mut server,
        json!({
            "jsonrpc": "2.0",
            "method": "item/agentMessage/delta",
            "params": { "delta": "ignored" },
        }),
    )
    .await;
    server_send_json(
        &mut server,
        json!({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": { "threadId": "t2", "turn": { "id": "turn-2" } },
        }),
    )
    .await;

    let notification = notifs
        .await_notification(|notification| {
            matches!(
                notification,
                Notification::TurnCompleted { thread_id, .. } if thread_id == "t2"
            )
        })
        .await
        .expect("matching notification");
    match notification {
        Notification::TurnCompleted { thread_id, turn } => {
            assert_eq!(thread_id, "t2");
            assert_eq!(turn.get("id").and_then(Value::as_str), Some("turn-2"));
        }
        other => panic!("expected TurnCompleted, got {other:?}"),
    }
}

#[test]
fn default_request_timeout_is_30s() {
    assert_eq!(DEFAULT_REQUEST_TIMEOUT, Duration::from_secs(30));
}

#[tokio::test]
async fn with_request_timeout_overrides_default() {
    // Stand up only enough to construct a client (no real IO needed).
    let h = harness().await;
    let client = h.client.with_request_timeout(Duration::from_millis(5));
    assert_eq!(client.request_timeout(), Duration::from_millis(5));
}

#[test]
fn thread_start_params_debug_scrubs_neige_mcp_token() {
    let params = ThreadStartParams {
        cwd: "/workspace".into(),
        approval_policy: "never".into(),
        sandbox_mode: "workspace-write".into(),
        developer_instructions: None,
        config: Some(json!({
            "shell_environment_policy": {
                "set": {
                    "NEIGE_MCP_SOCKET": "/tmp/x.sock",
                    "NEIGE_MCP_TOKEN": "secret-abcdef",
                },
                "append": {
                    "SOME_KEY": "some_value",
                }
            }
        })),
    };

    let rendered = format!("{params:?}");
    assert!(!rendered.contains("secret-abcdef"));
    assert!(!rendered.contains("some_value"));
    assert!(rendered.contains("\"[REDACTED]\""));
}

#[test]
fn thread_start_config_redactor_preserves_inherit_key_names() {
    let redacted = redact_thread_start_config(&Some(json!({
        "shell_environment_policy": {
            "set": {
                "NEIGE_MCP_TOKEN": "secret-abcdef",
            },
            "inherit": ["KEEP_ME"],
        }
    })));

    assert_eq!(
        redacted.pointer("/shell_environment_policy/inherit"),
        Some(&json!(["KEEP_ME"]))
    );
    assert_eq!(
        redacted.pointer("/shell_environment_policy/set/NEIGE_MCP_TOKEN"),
        Some(&json!("[REDACTED]"))
    );
}

#[tokio::test]
async fn thread_start_sends_developer_instructions_when_present() {
    let mut h = harness().await;
    let client = h.client.with_request_timeout(Duration::from_secs(5));
    let req_fut = client.thread_start(Some("role prompt"));

    let server_task = tokio::spawn(async move {
        let req = server_recv_json(&mut h.server).await;
        assert_eq!(
            req.get("method").and_then(Value::as_str),
            Some("thread/start")
        );
        assert_eq!(
            req.get("params")
                .and_then(|params| params.get("developerInstructions"))
                .and_then(Value::as_str),
            Some("role prompt")
        );

        let id = req.get("id").cloned().unwrap();
        server_send_json(
            &mut h.server,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "thread": { "id": "with-prompt" }, "model": "m" },
            }),
        )
        .await;
        h.server
    });
    assert_eq!(req_fut.await.unwrap().thread_id(), Some("with-prompt"));
    let _server = server_task.await.unwrap();
}

#[tokio::test]
async fn thread_start_omits_developer_instructions_when_absent() {
    let mut h = harness().await;
    let client = h.client.with_request_timeout(Duration::from_secs(5));
    let req_fut = client.thread_start(None);

    let server_task = tokio::spawn(async move {
        let req = server_recv_json(&mut h.server).await;
        assert_eq!(
            req.get("method").and_then(Value::as_str),
            Some("thread/start")
        );
        assert!(
            !req.get("params")
                .and_then(Value::as_object)
                .is_some_and(|params| params.contains_key("developerInstructions")),
            "developerInstructions must be omitted when absent; got: {req}"
        );

        let id = req.get("id").cloned().unwrap();
        server_send_json(
            &mut h.server,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "thread": { "id": "without-prompt" }, "model": "m" },
            }),
        )
        .await;
        h.server
    });
    assert_eq!(req_fut.await.unwrap().thread_id(), Some("without-prompt"));
    let _server = server_task.await.unwrap();
}

#[tokio::test]
async fn turn_interrupt_treats_an_already_finished_turn_as_success() {
    let mut h = harness().await;
    let client = h.client.with_request_timeout(Duration::from_secs(5));
    let interrupt = client.turn_interrupt("thread-1", "turn-1");

    let server_task = tokio::spawn(async move {
        let req = server_recv_json(&mut h.server).await;
        assert_eq!(
            req.get("method").and_then(Value::as_str),
            Some("turn/interrupt")
        );
        let id = req.get("id").cloned().unwrap();
        server_send_json(
            &mut h.server,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32600, "message": "no active turn to interrupt" },
            }),
        )
        .await;
        h.server
    });

    interrupt
        .await
        .expect("interrupt is idempotent when the named turn already finished");
    let _server = server_task.await.unwrap();
}

#[tokio::test]
async fn turn_interrupt_propagates_other_rpc_errors() {
    let cases = [
        (-32600, "expected active turn id turn-1 but found turn-2"),
        (-32601, "no active turn to interrupt"),
    ];
    for (code, message) in cases {
        let mut h = harness().await;
        let client = h.client.with_request_timeout(Duration::from_secs(5));
        let interrupt = client.turn_interrupt("thread-1", "turn-1");

        let server_task = tokio::spawn(async move {
            let req = server_recv_json(&mut h.server).await;
            let id = req.get("id").cloned().unwrap();
            server_send_json(
                &mut h.server,
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": code, "message": message },
                }),
            )
            .await;
            h.server
        });

        let error = interrupt
            .await
            .expect_err("only Codex's exact no-active-turn response may be ignored");
        let Error::Refused(actual) = error else {
            panic!("RPC refusals must retain their error type: {error}");
        };
        assert_eq!(
            actual,
            format!("turn/interrupt failed: {message} (code {code})")
        );
        let _server = server_task.await.unwrap();
    }
}

/// A never-answered request times out and leaves NO entry in the pending map.
#[tokio::test]
async fn never_answered_request_times_out_and_cleans_pending() {
    let h = harness().await;
    // Keep the server end alive but silent — never reply.
    let _server = h.server;
    let client = h.client.with_request_timeout(Duration::from_millis(50));

    let err = client
        .request::<Value>("thread/start", json!({}))
        .await
        .expect_err("a never-answered request must error");
    match err {
        Error::Transport(msg) => {
            assert!(
                msg.contains("thread/start") && msg.contains("timed out"),
                "unexpected error message: {msg}"
            );
        }
        other => panic!("expected CodexAppServer timeout error, got {other:?}"),
    }

    // The pending entry for the timed-out request must be gone.
    assert!(
        client.pending.lock().unwrap().is_empty(),
        "pending map must not leak the timed-out request"
    );
}

/// A caller-supplied deadline must clean the pending map the same way the per-client one does: an outer `tokio::time::timeout` would drop the future before its own elapse arm runs and leak the entry.
#[tokio::test]
async fn a_caller_deadline_cleans_pending_the_same_way() {
    let h = harness().await;
    // Server end stays alive and silent.
    let _server = h.server;
    let client = h.client;

    let deadline = tokio::time::Instant::now() + Duration::from_millis(50);
    let err = client
        .request_until::<Value>("model/list", json!({}), deadline)
        .await
        .expect_err("a never-answered request must error");
    match err {
        Error::Transport(msg) => {
            assert!(
                msg.contains("model/list") && msg.contains("timed out"),
                "unexpected error message: {msg}"
            );
        }
        other => panic!("expected CodexAppServer timeout error, got {other:?}"),
    }

    assert!(
        client.pending.lock().unwrap().is_empty(),
        "a caller-supplied deadline must not leak the timed-out request"
    );
}

/// A budget that is already spent answers without putting a frame on the
/// wire — and therefore also without registering a pending entry.
#[tokio::test]
async fn an_expired_deadline_never_reaches_the_wire() {
    let h = harness().await;
    let _server = h.server;
    let client = h.client;

    let spent = tokio::time::Instant::now() - Duration::from_secs(1);
    let err = client
        .request_until::<Value>("config/read", json!({}), spent)
        .await
        .expect_err("a spent budget must not be waited on");
    match err {
        Error::Transport(msg) => assert!(
            msg.contains("config/read") && msg.contains("budget"),
            "unexpected error message: {msg}"
        ),
        other => panic!("expected CodexAppServer error, got {other:?}"),
    }
    assert!(
        client.pending.lock().unwrap().is_empty(),
        "nothing was sent, so nothing may be pending"
    );
}

/// With many notifications queued and NO consumer draining them, a real RPC response still routes back to the waiting request.
#[tokio::test]
async fn response_routes_while_notifications_are_undrained() {
    let mut h = harness().await;
    // Do NOT drain `h.notifs` — let notifications pile up unbounded.

    // Flood the reader with notifications first.
    for i in 0..1000u64 {
        server_send_json(
            &mut h.server,
            json!({
                "jsonrpc": "2.0",
                "method": "item/agentMessage/delta",
                "params": { "delta": format!("n{i}") },
            }),
        )
        .await;
    }

    // If notification delivery could block the reader loop, this response would never be routed and the request would time out.
    let client = h.client.with_request_timeout(Duration::from_secs(5));
    let req_fut = client.request::<ThreadResult>("thread/start", json!({}));

    // Server side: read the request frame, echo a response for its id.
    let server_task = tokio::spawn(async move {
        let req = server_recv_json(&mut h.server).await;
        let id = req.get("id").cloned().unwrap();
        server_send_json(
            &mut h.server,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "thread": { "id": "abc" }, "model": "gpt-5.5" },
            }),
        )
        .await;
        // Keep the connection open so the reader doesn't tear down.
        h.server
    });

    let result = req_fut
        .await
        .expect("response must route despite the notification backlog");
    assert_eq!(result.thread_id(), Some("abc"));
    let _server = server_task.await.unwrap();
}

/// Response correlation also works when the server echoes the id as a string.
#[tokio::test]
async fn response_correlates_with_string_id() {
    let mut h = harness().await;
    let client = h.client.with_request_timeout(Duration::from_secs(5));
    let req_fut = client.request::<ThreadResult>("thread/start", json!({}));

    let server_task = tokio::spawn(async move {
        let req = server_recv_json(&mut h.server).await;
        let id = req.get("id").and_then(Value::as_u64).unwrap();
        // Echo the id back as a STRING — the reader must still correlate.
        server_send_json(
            &mut h.server,
            json!({
                "jsonrpc": "2.0",
                "id": id.to_string(),
                "result": { "thread": { "id": "str-id" }, "model": "m" },
            }),
        )
        .await;
        h.server
    });

    let result = req_fut.await.expect("string-id response must correlate");
    assert_eq!(result.thread_id(), Some("str-id"));
    let _server = server_task.await.unwrap();
}
