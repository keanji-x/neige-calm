use super::*;
use serde_json::json;

#[tokio::test]
async fn initialize_accepts_minimal_response_without_optional_capabilities() {
    let (connection, mut reader, mut writer) = peer();
    let agent = tokio::spawn(async move {
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "initialize");
        assert_eq!(request["params"]["clientCapabilities"], json!({}));
        send(
            &mut writer,
            json!({"jsonrpc":"2.0","id":request["id"],"result":{"protocolVersion":1}}),
        )
        .await;
    });
    let response = protocol::initialize(&connection.client, "client", "1")
        .await
        .unwrap();
    assert!(!response.agent_capabilities.load_session);
    assert!(!response.agent_capabilities.prompt_capabilities.image);
    assert!(
        !response
            .agent_capabilities
            .prompt_capabilities
            .embedded_context
    );
    assert!(
        response
            .agent_capabilities
            .session_capabilities
            .resume
            .is_none()
    );
    assert!(
        response
            .agent_capabilities
            .session_capabilities
            .close
            .is_none()
    );
    agent.await.unwrap();
}
#[tokio::test]
async fn initialize_rejects_unsupported_version_missing_protocol_and_malformed_capabilities() {
    for response in [
        json!({"protocolVersion":2,"agentCapabilities":{}}),
        json!({}),
        json!({"agentCapabilities":{}}),
        json!({"protocolVersion":"1"}),
        json!({"protocolVersion":null}),
        json!({"protocolVersion":1,"agentCapabilities":null}),
        json!({"protocolVersion":1,"agentCapabilities":true}),
        json!({"protocolVersion":1,"agentCapabilities":"invalid"}),
        json!({"protocolVersion":1,"agentCapabilities":{"promptCapabilities":null}}),
        json!({"protocolVersion":1,"agentCapabilities":{"promptCapabilities":{"image":"yes"}}}),
        json!({"protocolVersion":1,"agentCapabilities":{"promptCapabilities":{"embeddedContext":1}}}),
        json!({"protocolVersion":1,"agentCapabilities":{"sessionCapabilities":false}}),
        json!({"protocolVersion":1,"agentCapabilities":{"sessionCapabilities":{"resume":true}}}),
        json!({"protocolVersion":1,"agentCapabilities":{"sessionCapabilities":{"close":"yes"}}}),
        json!({"protocolVersion":1,"agentCapabilities":{"loadSession":"yes"}}),
    ] {
        let (connection, mut reader, mut writer) = peer();
        let agent = tokio::spawn(async move {
            let request = read(&mut reader).await;
            assert_eq!(request["params"]["clientCapabilities"], json!({}));
            send(
                &mut writer,
                json!({"jsonrpc":"2.0","id":request["id"],"result":response}),
            )
            .await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        });
        assert!(matches!(
            protocol::initialize(&connection.client, "client", "1").await,
            Err(Error::Protocol(_))
        ));
        agent.await.unwrap();
    }
}
#[tokio::test]
async fn initialize_empty_and_nested_default_capabilities_allow_text_but_refuse_images() {
    for result in [
        json!({"protocolVersion":1}),
        json!({"protocolVersion":1,"agentCapabilities":{}}),
        json!({"protocolVersion":1,"agentCapabilities":{"promptCapabilities":{},"sessionCapabilities":{}}}),
    ] {
        let (connection, mut reader, mut writer) = peer();
        let agent = tokio::spawn(async move {
            let init = read(&mut reader).await;
            assert_eq!(init["params"]["clientCapabilities"], json!({}));
            send(
                &mut writer,
                json!({"jsonrpc":"2.0","id":init["id"],"result":result}),
            )
            .await;
            let new = read(&mut reader).await;
            assert_eq!(new["method"], "session/new");
            send(
                &mut writer,
                json!({"jsonrpc":"2.0","id":new["id"],"result":{"sessionId":"native"}}),
            )
            .await;
            let prompt = read(&mut reader).await;
            assert_eq!(prompt["method"], "session/prompt");
            assert_eq!(
                prompt["params"]["prompt"],
                json!([{"type":"text","text":"hello"}])
            );
            send(
                &mut writer,
                json!({"jsonrpc":"2.0","id":prompt["id"],"result":{"stopReason":"end_turn"}}),
            )
            .await;
        });
        let response = protocol::initialize(&connection.client, "client", "1")
            .await
            .unwrap();
        let capabilities = response.agent_capabilities;
        assert!(!capabilities.load_session);
        assert!(!capabilities.prompt_capabilities.embedded_context);
        assert!(capabilities.session_capabilities.resume.is_none());
        assert!(capabilities.session_capabilities.close.is_none());
        assert!(matches!(
            protocol::prompt(
                &connection.client,
                &capabilities,
                "native",
                &[protocol::ContentBlock::Image {
                    data: "image".into(),
                    mime_type: "image/png".into()
                }],
                &allow()
            )
            .await,
            Err(WriteFailure::BeforeWrite(Error::Protocol(_)))
        ));
        let new = connection
            .client
            .request(
                "session/new",
                protocol::encode_new_session(std::path::Path::new("/workspace"), &[]),
                BUDGET,
            )
            .await
            .unwrap();
        let native: protocol::NewSessionResponse = protocol::decode(new).unwrap();
        let pending = protocol::prompt(
            &connection.client,
            &capabilities,
            &native.session_id,
            &[protocol::ContentBlock::text("hello")],
            &allow(),
        )
        .await
        .unwrap();
        assert!(matches!(
            protocol::decode::<protocol::PromptResponse>(pending.wait(BUDGET).await.unwrap())
                .unwrap()
                .stop_reason,
            protocol::StopReason::EndTurn
        ));
        agent.await.unwrap();
    }
}

#[tokio::test]
async fn initialize_preserves_advertised_capabilities() {
    let (connection, mut reader, mut writer) = peer();
    let agent = tokio::spawn(async move {
        let init = read(&mut reader).await;
        send(&mut writer, json!({"jsonrpc":"2.0","id":init["id"],"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true,"promptCapabilities":{"image":true,"embeddedContext":true},"sessionCapabilities":{"resume":{},"close":{}}}}})).await;
    });
    let response = protocol::initialize(&connection.client, "client", "1")
        .await
        .unwrap();
    let capabilities = response.agent_capabilities;
    assert!(capabilities.load_session);
    assert!(capabilities.prompt_capabilities.embedded_context);
    assert!(capabilities.session_capabilities.resume.is_some());
    assert!(capabilities.session_capabilities.close.is_some());
    assert!(
        protocol::validate_prompt(
            &capabilities,
            "native",
            &[protocol::ContentBlock::Image {
                data: "image".into(),
                mime_type: "image/png".into()
            }]
        )
        .is_ok()
    );
    agent.await.unwrap();
}
