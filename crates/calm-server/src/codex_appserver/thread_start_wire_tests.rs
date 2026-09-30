use super::*;

#[test]
fn thread_start_params_debug_redacts_secrets() {
    let params = ThreadStartParams {
        cwd: "/workspace".into(),
        approval_policy: "never".into(),
        sandbox_mode: "workspace-write".into(),
        developer_instructions: None,
        config: Some(
            json!({"mcp_servers":{"calm":{"env":{"NEIGE_MCP_TOKEN":"MCP_SECRET"},"http_headers":{"Authorization":"HEADER_SECRET"}}},"shell_environment_policy":{"set":{"TOKEN":"SHELL_SECRET"}}}),
        ),
    };
    let debug = format!("{params:?}");
    for secret in ["MCP_SECRET", "HEADER_SECRET", "SHELL_SECRET"] {
        assert!(!debug.contains(secret));
    }
}

async fn answer_thread(mut peer: WebSocketStream<UnixStream>) -> Value {
    let frame = peer.next().await.unwrap().unwrap();
    let request: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
    peer.send(Message::Text(
        json!({"jsonrpc":"2.0","id":request["id"],"result":{"thread":{"id":"owned-thread"}}})
            .to_string(),
    ))
    .await
    .unwrap();
    request
}

#[tokio::test]
async fn thread_start_with_params_keeps_the_sandbox_wire() {
    let (client, _notifications, peer) = CodexAppServer::connect_pair_for_test().await;
    let answer = tokio::spawn(answer_thread(peer));
    client
        .thread_start_with_params(ThreadStartParams {
            cwd: "/old-workspace".into(),
            approval_policy: "never".into(),
            sandbox_mode: "workspace-write".into(),
            developer_instructions: Some("old instructions".into()),
            config: Some(json!({"model":"unchanged"})),
        })
        .await
        .unwrap();
    let request = answer.await.unwrap();
    assert_eq!(
        serde_json::to_string(&request).unwrap(),
        "{\"id\":1,\"jsonrpc\":\"2.0\",\"method\":\"thread/start\",\"params\":{\"approvalPolicy\":\"never\",\"config\":{\"model\":\"unchanged\"},\"cwd\":\"/old-workspace\",\"developerInstructions\":\"old instructions\",\"sandbox\":\"workspace-write\"}}"
    );
}
