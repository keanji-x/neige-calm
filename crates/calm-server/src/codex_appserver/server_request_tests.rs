use super::*;

async fn recv(server: &mut WebSocketStream<UnixStream>) -> Value {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Message::Text(text) = server.next().await.unwrap().unwrap() {
                return serde_json::from_str(&text).unwrap();
            }
        }
    })
    .await
    .expect("response deadline")
}

async fn send(server: &mut WebSocketStream<UnixStream>, value: Value) {
    server.send(Message::Text(value.to_string())).await.unwrap();
}

#[tokio::test]
async fn server_request_id_collision_does_not_consume_client_response() {
    let (client, _notifications, mut server) = CodexAppServer::connect_pair_for_test().await;
    let peer = async {
        let request = recv(&mut server).await;
        send(
            &mut server,
            json!({"id":request["id"],"method":"unknown/request","params":{}}),
        )
        .await;
        send(
            &mut server,
            json!({"id":request["id"],"result":{"ack":true}}),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let (result, ()) = tokio::join!(client.request::<Value>("probe", json!({})), peer);
    assert_eq!(result.unwrap(), json!({"ack":true}));
}

fn call(id: Value) -> Value {
    json!({"id":id,"method":"item/tool/call","params":{
        "threadId":"thread-a","turnId":"turn-a","callId":"call-a",
        "tool":"probe","arguments":{"threadId":"forged","key":"a"}
    }})
}

#[tokio::test]
async fn dynamic_tool_roundtrip_preserves_provider_identity_and_string_id() {
    let (client, mut notifications, mut server) = CodexAppServer::connect_pair_for_test().await;
    let mut requests = client.take_dynamic_tool_requests().unwrap();
    assert!(client.take_dynamic_tool_requests().is_err());
    send(&mut server, call(json!("1"))).await;
    let request = requests.recv().await.unwrap();
    assert_eq!(request.id, ServerRequestId::String("1".into()));
    assert_eq!(request.params.thread_id, "thread-a");
    assert_eq!(request.params.turn_id, "turn-a");
    assert_eq!(request.params.call_id, "call-a");
    assert_eq!(request.params.tool, "probe");
    assert_eq!(
        request.params.arguments,
        json!({"threadId":"forged","key":"a"})
    );
    request
        .respond(DynamicToolCallResponse::text(true, "ok"))
        .unwrap();
    assert_eq!(
        recv(&mut server).await,
        json!({"jsonrpc":"2.0","id":"1","result":{
            "success":true,"contentItems":[{"type":"inputText","text":"ok"}]
        }})
    );
    assert!(
        notifications.rx.try_recv().is_err(),
        "server calls must not be broadcast notifications"
    );
}

#[tokio::test]
async fn slow_dynamic_handler_does_not_block_rpc_or_notifications() {
    let (client, mut notifications, mut server) = CodexAppServer::connect_pair_for_test().await;
    let mut requests = client.take_dynamic_tool_requests().unwrap();
    send(&mut server, call(json!("slow"))).await;
    let held = requests.recv().await.unwrap();
    let peer = async {
        let rpc = recv(&mut server).await;
        send(&mut server, json!({"id":rpc["id"],"result":{"ack":true}})).await;
        send(
            &mut server,
            json!({"method":"turn/completed","params":{"threadId":"other"}}),
        )
        .await;
    };
    let (result, ()) = tokio::join!(client.request::<Value>("probe", json!({})), peer);
    assert_eq!(result.unwrap(), json!({"ack":true}));
    assert_eq!(
        notifications.recv().await.unwrap().thread_id(),
        Some("other")
    );
    held.respond(DynamicToolCallResponse::text(true, "late"))
        .unwrap();
    assert_eq!(recv(&mut server).await["id"], "slow");
}

#[tokio::test]
async fn unknown_unhandled_and_malformed_requests_are_explicitly_rejected() {
    let (client, _notifications, mut server) = CodexAppServer::connect_pair_for_test().await;
    for (frame, code) in [
        (json!({"id":-7,"method":"unsupported","params":{}}), -32601),
        (call(json!(8)), -32601),
        (
            json!({"id":9,"method":"item/tool/call","params":{"threadId":"t"}}),
            -32602,
        ),
        (call(json!(1.5)), -32600),
    ] {
        let expected_id = if code == -32600 {
            Value::Null
        } else {
            frame["id"].clone()
        };
        send(&mut server, frame).await;
        let response = recv(&mut server).await;
        assert_eq!(response["id"], expected_id);
        assert_eq!(response["error"]["code"], code);
    }
    let mut requests = client.take_dynamic_tool_requests().unwrap();
    let mut missing = call(json!(10));
    missing["params"]
        .as_object_mut()
        .unwrap()
        .remove("arguments");
    send(&mut server, missing).await;
    assert_eq!(recv(&mut server).await["error"]["code"], -32602);
    assert!(requests.try_recv().is_err());
}

#[tokio::test]
async fn dropped_handler_returns_error_without_hanging() {
    let (client, _notifications, mut server) = CodexAppServer::connect_pair_for_test().await;
    let mut requests = client.take_dynamic_tool_requests().unwrap();
    send(&mut server, call(json!(1))).await;
    drop(requests.recv().await.unwrap());
    assert_eq!(recv(&mut server).await["error"]["code"], -32603);
    drop(requests);
    send(&mut server, call(json!(2))).await;
    assert_eq!(recv(&mut server).await["error"]["code"], -32000);
}

#[tokio::test]
async fn timed_out_handler_cannot_reply_later() {
    let (client, _notifications, mut server) = CodexAppServer::connect_pair_for_test().await;
    let mut requests = client.take_dynamic_tool_requests().unwrap();
    send(&mut server, call(json!(1))).await;
    let held = requests.recv().await.unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(31)).await;
    tokio::task::yield_now().await;
    tokio::time::resume();
    assert_eq!(recv(&mut server).await["error"]["code"], -32001);
    assert!(held.is_cancelled());
    assert!(
        held.respond(DynamicToolCallResponse::text(true, "too late"))
            .is_err()
    );
}

#[tokio::test]
async fn outstanding_dynamic_handlers_are_bounded() {
    let (client, _notifications, mut server) = CodexAppServer::connect_pair_for_test().await;
    let mut requests = client.take_dynamic_tool_requests().unwrap();
    let mut held = Vec::new();
    for id in 0..16 {
        send(&mut server, call(json!(id))).await;
        held.push(requests.recv().await.unwrap());
    }
    send(&mut server, call(json!(16))).await;
    let rejected = recv(&mut server).await;
    assert_eq!(rejected["id"], 16);
    assert_eq!(rejected["error"]["code"], -32000);
    assert!(requests.try_recv().is_err());
    drop(held);
}

#[tokio::test]
async fn duplicate_pending_server_id_closes_connection_without_second_delivery() {
    let (client, mut notifications, mut server) = CodexAppServer::connect_pair_for_test().await;
    let mut requests = client.take_dynamic_tool_requests().unwrap();
    send(&mut server, call(json!("duplicate"))).await;
    let held = requests.recv().await.unwrap();
    send(&mut server, call(json!("duplicate"))).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(2), notifications.recv())
            .await
            .unwrap()
            .is_none()
    );
    assert!(requests.try_recv().is_err());
    assert!(
        held.respond(DynamicToolCallResponse::text(true, "late"))
            .is_err()
    );
    assert!(client.transport.check().is_err());
}

#[tokio::test]
async fn client_drop_cancels_old_reply_and_does_not_touch_new_connection() {
    let (old, mut old_notifications, mut old_server) =
        CodexAppServer::connect_pair_for_test().await;
    let mut old_requests = old.take_dynamic_tool_requests().unwrap();
    send(&mut old_server, call(json!(1))).await;
    let held = old_requests.recv().await.unwrap();
    drop(old);
    assert!(old_notifications.recv().await.is_none());
    let (new, _new_notifications, mut server) = CodexAppServer::connect_pair_for_test().await;
    let mut requests = new.take_dynamic_tool_requests().unwrap();
    assert!(
        held.respond(DynamicToolCallResponse::text(true, "old"))
            .is_err()
    );
    send(&mut server, call(json!(1))).await;
    requests
        .recv()
        .await
        .unwrap()
        .respond(DynamicToolCallResponse::text(true, "new"))
        .unwrap();
    assert_eq!(
        recv(&mut server).await["result"]["contentItems"][0]["text"],
        "new"
    );
}

#[tokio::test]
async fn server_reply_write_timeout_poison_closes_original_connection() {
    let (client, mut notifications, mut server) = CodexAppServer::connect_pair_for_test().await;
    let mut requests = client.take_dynamic_tool_requests().unwrap();
    send(&mut server, call(json!(1))).await;
    let held = requests.recv().await.unwrap();
    let lock = client.sink.lock().await;
    held.respond(DynamicToolCallResponse::text(true, "blocked write"))
        .unwrap();
    tokio::task::yield_now().await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(6)).await;
    tokio::task::yield_now().await;
    tokio::time::resume();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), notifications.recv())
            .await
            .unwrap()
            .is_none()
    );
    assert!(client.transport.check().is_err());
    drop(lock);
}
