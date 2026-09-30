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
        "tool":"Recover","arguments":{"key":"a","reason":"retry"}
    }})
}

#[tokio::test]
async fn every_server_request_is_explicitly_rejected() {
    let (_client, mut notifications, mut server) = CodexAppServer::connect_pair_for_test().await;
    for (frame, code, expected_id) in [
        (
            json!({"id":-7,"method":"unsupported","params":{}}),
            -32601,
            json!(-7),
        ),
        (call(json!("s-8")), -32601, json!("s-8")),
        (call(json!(9)), -32601, json!(9)),
        (call(json!(1.5)), -32600, Value::Null),
    ] {
        send(&mut server, frame).await;
        let response = recv(&mut server).await;
        assert_eq!(response["id"], expected_id);
        assert_eq!(response["error"]["code"], code);
        assert!(response.get("result").is_none());
    }
    assert!(
        notifications.rx.try_recv().is_err(),
        "server requests must not be broadcast as notifications"
    );
}

#[tokio::test]
async fn server_reply_write_timeout_poison_closes_original_connection() {
    let (client, mut notifications, mut server) = CodexAppServer::connect_pair_for_test().await;
    let lock = client.sink.lock().await;
    send(&mut server, call(json!(1))).await;
    // Let the reader queue the refusal so the writer blocks on the held sink.
    tokio::time::sleep(Duration::from_millis(50)).await;
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
