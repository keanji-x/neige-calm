use super::*;

#[tokio::test]
async fn thread_compact_sends_start_without_a_user_message() {
    let (client, _notifications, mut peer) = CodexAppServer::connect_pair_for_test().await;
    let answer = tokio::spawn(async move {
        let frame = peer.next().await.unwrap().unwrap();
        let request: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
        peer.send(Message::Text(
            json!({"jsonrpc":"2.0", "id":request["id"], "result":{}}).to_string(),
        ))
        .await
        .unwrap();
        request
    });
    client.thread_compact_start("thread-1").await.unwrap();
    let request = answer.await.unwrap();
    assert_eq!(request["method"], "thread/compact/start");
    assert_eq!(request["params"], json!({"threadId":"thread-1"}));
}
