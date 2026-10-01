use super::*;

#[test]
fn thread_start_params_debug_redacts_secrets() {
    let params = ThreadStartParams {
        cwd: "/workspace".into(),
        approval_policy: "never".into(),
        permissions: PermissionsChoice::SandboxMode("workspace-write".into()),
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
            permissions: PermissionsChoice::SandboxMode("workspace-write".into()),
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
async fn named_permissions_are_exclusive_on_thread_start_resume_and_turn() {
    for method in ["thread/start", "thread/resume", "turn/start"] {
        let (client, _notifications, mut peer) = CodexAppServer::connect_pair_for_test().await;
        let answer = tokio::spawn(async move {
            let frame = peer.next().await.unwrap().unwrap();
            let request: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            let result = if request["method"] == "turn/start" {
                json!({"turn":{"id":"turn"}})
            } else {
                json!({"thread":{"id":"owned-thread"},"activePermissionProfile":{"id":"neige-task-read"}})
            };
            peer.send(Message::Text(
                json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string(),
            ))
            .await
            .unwrap();
            request
        });
        let choice = PermissionsChoice::NamedProfile("neige-task-read".into());
        match method {
            "thread/start" => {
                client
                    .thread_start_with_params(ThreadStartParams {
                        cwd: "/workspace".into(),
                        approval_policy: "never".into(),
                        permissions: choice.clone(),
                        developer_instructions: None,
                        config: None,
                    })
                    .await
                    .unwrap();
            }
            "thread/resume" => {
                client
                    .thread_resume_with_permissions("owned-thread", None, &choice)
                    .await
                    .unwrap();
            }
            _ => {
                client
                    .turn_start_with_permissions(
                        "owned-thread",
                        vec![],
                        &TurnModelSelection::inherit(),
                        None,
                        &choice,
                    )
                    .await
                    .unwrap();
            }
        }
        let request = answer.await.unwrap();
        assert_eq!(request["method"], method);
        assert_eq!(request["params"]["permissions"], "neige-task-read");
        assert!(request["params"].get("sandbox").is_none());
        assert!(request["params"].get("sandboxPolicy").is_none());
    }
}

#[tokio::test]
async fn named_thread_permissions_fail_closed_without_matching_provider_confirmation() {
    for active in [Value::Null, json!({"id":"different-profile"})] {
        for resume in [false, true] {
            let active = active.clone();
            let (client, _notifications, mut peer) = CodexAppServer::connect_pair_for_test().await;
            let answer = tokio::spawn(async move {
                let frame = peer.next().await.unwrap().unwrap();
                let request: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
                peer.send(Message::Text(json!({"jsonrpc":"2.0","id":request["id"],"result":{"thread":{"id":"thread"},"activePermissionProfile":active}}).to_string())).await.unwrap();
            });
            let choice = PermissionsChoice::NamedProfile("neige-task-read".into());
            let result = if resume {
                client
                    .thread_resume_with_permissions("thread", None, &choice)
                    .await
            } else {
                client
                    .thread_start_with_params(ThreadStartParams {
                        cwd: "/workspace".into(),
                        approval_policy: "never".into(),
                        permissions: choice,
                        developer_instructions: None,
                        config: None,
                    })
                    .await
            };
            assert!(result.is_err());
            answer.await.unwrap();
        }
    }
}
