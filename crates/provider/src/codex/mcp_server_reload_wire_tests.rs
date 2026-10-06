use super::*;

/// The prod 0.159.2 schema types `params` as `null` and the response as an empty object (#2014).
#[tokio::test]
async fn mcp_server_reload_sends_null_params_and_accepts_an_empty_object() {
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
    client.mcp_server_reload().await.unwrap();
    let request = answer.await.unwrap();
    assert_eq!(request["method"], "config/mcpServer/reload");
    assert_eq!(request["params"], Value::Null);
}
