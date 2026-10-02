use super::*;

async fn ingress_pair() -> (
    CodexAppServer,
    mpsc::UnboundedReceiver<Value>,
    WebSocketStream<UnixStream>,
) {
    let dir = calm_test_sockets::socket_dir("native-envelope");
    let sock = dir.path().join("provider.sock");
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    let (client, server) = tokio::join!(CodexAppServer::connect_ingress(&sock), async {
        let (stream, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(stream).await.unwrap()
    });
    let (client, events) = client.unwrap();
    (client, events, server)
}
async fn receive(server: &mut WebSocketStream<UnixStream>) -> Value {
    serde_json::from_str(server.next().await.unwrap().unwrap().to_text().unwrap()).unwrap()
}
async fn send(server: &mut WebSocketStream<UnixStream>, frame: Value) {
    server.send(Message::Text(frame.to_string())).await.unwrap();
}

#[tokio::test]
async fn native_ingress_envelope_preserves_result_error_and_bidirectional_ids() {
    let (client, mut events, mut server) = ingress_pair().await;
    let expected = json!({"jsonrpc":"2.0","id":1,"error":{"code":-32001,"message":"denied","data":{"reason":"policy"}}});
    let request = client.request_envelope("initialize", json!({"clientInfo":{"name":"tui","version":"test"},"capabilities":{"experimentalApi":true,"optOutNotificationMethods":["x"]}}));
    let peer = async {
        let frame = receive(&mut server).await;
        assert_eq!(
            frame["params"]["capabilities"]["optOutNotificationMethods"],
            json!(["x"])
        );
        // Server and client requests can carry the same ID independently.
        let callback = json!({"jsonrpc":"2.0","id":1,"method":"item/tool/requestUserInput","params":{"threadId":"t","turnId":"u","questions":[{"id":"q"}]}});
        send(&mut server, callback.clone()).await;
        assert_eq!(events.recv().await.unwrap(), callback);
        send(&mut server, expected.clone()).await;
    };
    let (reply, _) = tokio::join!(request, peer);
    assert_eq!(reply.unwrap(), expected);
    let answer = json!({"jsonrpc":"2.0","id":1,"result":{"answers":{"q":{"answers":["yes"]}},"future":true}});
    client.send_protocol_frame(answer.clone()).await.unwrap();
    assert_eq!(receive(&mut server).await, answer);
    let request = client.request_envelope("thread/read", json!({"threadId":"t"}));
    let result = json!({"thread":{"id":"t"},"future":{"all":"retained"}});
    let peer = async {
        let frame = receive(&mut server).await;
        send(
            &mut server,
            json!({"jsonrpc":"2.0","id":frame["id"],"result":result}),
        )
        .await;
    };
    let (reply, _) = tokio::join!(request, peer);
    assert_eq!(reply.unwrap()["result"], result);
}

#[tokio::test]
async fn native_ingress_envelope_preserves_late_reply_and_notifications() {
    let (client, mut events, mut server) = ingress_pair().await;
    let client = client.with_request_timeout(Duration::from_millis(20));
    let request = client.request_envelope(
        "turn/start",
        json!({"threadId":"t","clientUserMessageId":"persisted-nonce"}),
    );
    let peer = receive(&mut server);
    let (reply, frame) = tokio::join!(request, peer);
    assert!(reply.unwrap_err().to_string().contains("timed out"));
    let late = json!({"jsonrpc":"2.0","id":frame["id"],"result":{"turn":{"id":"u"},"future":42}});
    send(&mut server, late.clone()).await;
    assert_eq!(events.recv().await.unwrap(), late);
    let notification = json!({"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"t","turn":{"id":"u","status":"completed"},"future":{"x":1}}});
    send(&mut server, notification.clone()).await;
    assert_eq!(events.recv().await.unwrap(), notification);
}
