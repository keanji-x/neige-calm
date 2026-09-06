use super::*;

#[test]
fn dedicated_codex_profile_allowed_is_required_and_secrets_are_redacted() {
    assert!(
        serde_json::from_value::<PermissionProfileListResponse>(
            json!({"data":[{"id":"neige-delivery-v1"}]})
        )
        .is_err()
    );
    let params = PermissionThreadStartParams {
        cwd: "/workspace".into(),
        approval_policy: "never".into(),
        permissions: ThreadPermissionSelection::NamedProfile("neige-delivery-v1".into()),
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

#[tokio::test]
async fn dedicated_codex_profile_query_preserves_cwd_and_pagination() {
    let (client, _notifications, mut peer) = CodexAppServer::connect_pair_for_test().await;
    let response = tokio::spawn(async move {
        let request: Value =
            serde_json::from_str(peer.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(request["method"], "permissionProfile/list");
        assert_eq!(
            request["params"],
            json!({"cwd":"/workspace","cursor":"page-two"})
        );
        peer.send(Message::Text(json!({"jsonrpc":"2.0","id":request["id"],"result":{"data":[{"id":"neige-delivery-v1","allowed":true}],"nextCursor":null}}).to_string())).await.unwrap();
    });
    let page = client
        .permission_profile_list("/workspace", Some("page-two"))
        .await
        .unwrap();
    assert!(page.data[0].allowed);
    assert!(page.next_cursor.is_none());
    response.await.unwrap();
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
async fn dedicated_codex_named_profile_excludes_legacy_sandbox_on_wire() {
    let (client, _notifications, peer) = CodexAppServer::connect_pair_for_test().await;
    let answer = tokio::spawn(answer_thread(peer));
    client
        .thread_start_with_permissions(PermissionThreadStartParams {
            cwd: "/workspace".into(),
            approval_policy: "never".into(),
            permissions: ThreadPermissionSelection::NamedProfile("neige-delivery-v1".into()),
            developer_instructions: Some("worker instructions".into()),
            config: None,
        })
        .await
        .unwrap();
    let request = answer.await.unwrap();
    assert_eq!(
        request["params"],
        json!({"cwd":"/workspace","approvalPolicy":"never","permissions":"neige-delivery-v1","developerInstructions":"worker instructions"})
    );
}

#[tokio::test]
async fn dedicated_codex_legacy_wrapper_keeps_existing_wire() {
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

#[tokio::test]
async fn dedicated_codex_fixture_command_has_named_profile_and_bounded_wire() {
    let (client, _notifications, mut peer) = CodexAppServer::connect_pair_for_test().await;
    let answer = tokio::spawn(async move {
        let request: Value =
            serde_json::from_str(peer.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(request["method"], "command/exec");
        assert_eq!(
            request["params"],
            json!({"command":["/bin/true"],"cwd":"/workspace","permissionProfile":"neige-delivery-v1","env":{},"timeoutMs":10000,"outputBytesCap":32768})
        );
        peer.send(Message::Text(
            json!({"id":request["id"],"result":{"exitCode":0,"stdout":"","stderr":""}}).to_string(),
        ))
        .await
        .unwrap();
    });
    assert_eq!(
        client
            .command_exec_for_fixture(vec!["/bin/true".into()], Default::default())
            .await
            .unwrap()["exitCode"],
        0
    );
    answer.await.unwrap();
}
