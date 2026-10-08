use super::*;
use crate::events::{ItemPhase, PlannerEventKind};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

const BUDGET: Duration = Duration::from_secs(2);
fn peer() -> (
    Connection,
    BufReader<tokio::io::ReadHalf<DuplexStream>>,
    tokio::io::WriteHalf<DuplexStream>,
) {
    let (client, agent) = tokio::io::duplex(64 * 1024);
    let (read, write) = tokio::io::split(client);
    let (agent_read, agent_write) = tokio::io::split(agent);
    (
        Connection::new(read, write),
        BufReader::new(agent_read),
        agent_write,
    )
}
async fn read(reader: &mut BufReader<tokio::io::ReadHalf<DuplexStream>>) -> Value {
    let mut line = String::new();
    tokio::time::timeout(BUDGET, reader.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}
async fn send(writer: &mut tokio::io::WriteHalf<DuplexStream>, value: Value) {
    writer
        .write_all(format!("{value}\n").as_bytes())
        .await
        .unwrap();
}

#[tokio::test]
async fn concurrent_requests_correlate_out_of_order_responses_and_preserve_peer_request_ids() {
    let (mut connection, mut reader, mut writer) = peer();
    let first = connection.client.submit("one", json!({})).await.unwrap();
    let second = connection.client.submit("two", json!({})).await.unwrap();
    let one = read(&mut reader).await;
    let two = read(&mut reader).await;
    send(
        &mut writer,
        json!({"jsonrpc":"2.0","id":two["id"],"result":{"value":2}}),
    )
    .await;
    send(&mut writer,json!({"jsonrpc":"2.0","method":"session/request_permission","id":"native-id","params":{"sessionId":"native"}})).await;
    send(
        &mut writer,
        json!({"jsonrpc":"2.0","id":one["id"],"result":{"value":1}}),
    )
    .await;
    assert_eq!(first.wait(BUDGET).await.unwrap(), json!({"value":1}));
    assert_eq!(second.wait(BUDGET).await.unwrap(), json!({"value":2}));
    let Incoming::Request { id, method, .. } = connection.incoming.recv().await.unwrap() else {
        panic!("request")
    };
    assert_eq!(method, "session/request_permission");
    connection
        .client
        .respond(id, json!({"outcome":{"outcome":"cancelled"}}))
        .await
        .unwrap();
    assert_eq!(
        read(&mut reader).await,
        json!({"jsonrpc":"2.0","id":"native-id","result":{"outcome":{"outcome":"cancelled"}}})
    );
}
#[tokio::test]
async fn dispatched_prompt_disconnect_is_unknown_and_never_resent() {
    let (connection, mut reader, mut writer) = peer();
    let pending = connection
        .client
        .submit(
            "session/prompt",
            json!({"sessionId":"native","prompt":[{"type":"text","text":"act once"}]}),
        )
        .await
        .unwrap();
    let frame = read(&mut reader).await;
    assert_eq!(frame["method"], "session/prompt");
    writer.shutdown().await.unwrap();
    drop(writer);
    assert_eq!(pending.wait(BUDGET).await, Err(Error::Closed));
    assert_eq!(
        connection
            .client
            .submit("session/prompt", json!({}))
            .await
            .err(),
        Some(Error::Closed)
    );
    let mut residue = String::new();
    assert_eq!(
        tokio::time::timeout(BUDGET, reader.read_line(&mut residue))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}
#[tokio::test]
async fn timeout_closes_connection_without_retrying() {
    let (connection, mut reader, _writer) = peer();
    let pending = connection
        .client
        .submit("session/prompt", json!({}))
        .await
        .unwrap();
    assert_eq!(read(&mut reader).await["method"], "session/prompt");
    assert_eq!(
        pending.wait(Duration::from_millis(10)).await,
        Err(Error::Timeout)
    );
    assert_eq!(
        connection
            .client
            .submit("session/prompt", json!({}))
            .await
            .err(),
        Some(Error::Closed)
    );
}
#[tokio::test]
async fn malformed_and_uncorrelated_frames_poison_all_pending_requests() {
    for invalid in [
        json!({"jsonrpc":"1.0","id":1,"result":{}}),
        json!({"jsonrpc":"2.0","id":99,"result":{}}),
        json!({"jsonrpc":"2.0","id":1,"result":{},"error":{"code":-1,"message":"bad"}}),
    ] {
        let (connection, mut reader, mut writer) = peer();
        let pending = connection
            .client
            .submit("initialize", json!({}))
            .await
            .unwrap();
        read(&mut reader).await;
        send(&mut writer, invalid).await;
        assert!(matches!(
            pending.wait(BUDGET).await,
            Err(Error::Protocol(_))
        ));
        assert_eq!(
            connection.client.submit("again", json!({})).await.err(),
            Some(Error::Closed)
        );
    }
}
#[tokio::test]
async fn frames_can_be_fragmented_but_cannot_exceed_the_limit() {
    let (connection, mut reader, mut writer) = peer();
    let pending = connection
        .client
        .submit("initialize", json!({}))
        .await
        .unwrap();
    let request = read(&mut reader).await;
    let frame = format!(
        "{}\n",
        json!({"jsonrpc":"2.0","id":request["id"],"result":{"value":"完整"}})
    );
    for byte in frame.as_bytes() {
        writer.write_all(&[*byte]).await.unwrap();
    }
    assert_eq!(pending.wait(BUDGET).await.unwrap()["value"], "完整");
    let pending = connection
        .client
        .submit("oversized", json!({}))
        .await
        .unwrap();
    read(&mut reader).await;
    let write = tokio::spawn(async move {
        let _ = writer.write_all(&vec![b'x'; 8 * 1024 * 1024 + 1]).await;
    });
    assert_eq!(
        pending.wait(BUDGET).await,
        Err(Error::Protocol("frame exceeds byte limit"))
    );
    write.await.unwrap();
}
#[tokio::test]
async fn initialize_requires_supported_version_and_required_capabilities_shape() {
    for response in [
        json!({"protocolVersion":2,"agentCapabilities":{}}),
        json!({"protocolVersion":1}),
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
async fn new_load_and_cancel_use_native_identity_without_creating_a_prompt() {
    let (connection, mut reader, mut writer) = peer();
    let agent = tokio::spawn(async move {
        let init = read(&mut reader).await;
        send(&mut writer,json!({"jsonrpc":"2.0","id":init["id"],"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true}}})).await;
        let new = read(&mut reader).await;
        assert_eq!(new["method"], "session/new");
        assert_eq!(new["params"]["cwd"], "/workspace");
        assert_eq!(new["params"]["mcpServers"][0]["name"], "registered-server");
        send(
            &mut writer,
            json!({"jsonrpc":"2.0","id":new["id"],"result":{"sessionId":"ses_native"}}),
        )
        .await;
        let load = read(&mut reader).await;
        assert_eq!(load["method"], "session/load");
        assert_eq!(load["params"]["sessionId"], "ses_native");
        send(
            &mut writer,
            json!({"jsonrpc":"2.0","id":load["id"],"result":{}}),
        )
        .await;
        let cancel = read(&mut reader).await;
        assert_eq!(
            cancel,
            json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"ses_native"}})
        );
    });
    let initialized = protocol::initialize(&connection.client, "test", "1")
        .await
        .unwrap();
    let mcp = [protocol::McpServer::Stdio {
        name: "registered-server".into(),
        command: "/shim".into(),
        args: vec![],
        env: vec![protocol::EnvVariable {
            name: "TOKEN".into(),
            value: "explicit".into(),
        }],
    }];
    let native = protocol::new_session(&connection.client, "/workspace", &mcp)
        .await
        .unwrap();
    protocol::load_session(
        &connection.client,
        &initialized.agent_capabilities,
        &native,
        "/workspace",
        &mcp,
    )
    .await
    .unwrap();
    protocol::cancel(&connection.client, &native).await.unwrap();
    agent.await.unwrap();
}
#[tokio::test]
async fn unsupported_load_and_image_are_refused_before_any_write() {
    let (connection, mut reader, _writer) = peer();
    let capabilities = protocol::AgentCapabilities::default();
    assert!(
        protocol::load_session(&connection.client, &capabilities, "native", "/w", &[])
            .await
            .is_err()
    );
    assert!(
        protocol::prompt(
            &connection.client,
            &capabilities,
            "native",
            &[protocol::ContentBlock::Image {
                data: "abc".into(),
                mime_type: "image/png".into()
            }]
        )
        .await
        .is_err()
    );
    assert!(
        protocol::new_session(&connection.client, "relative", &[])
            .await
            .is_err()
    );
    let mut line = String::new();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), reader.read_line(&mut line))
            .await
            .is_err()
    );
}
fn translator() -> translate::TurnTranslator {
    translate::TurnTranslator::new(translate::TurnContext {
        thread_id: "thread".into(),
        turn_id: "turn".into(),
        native_session_id: "native".into(),
    })
}

#[test]
fn text_tool_text_have_distinct_identities_and_complete_in_execution_order() {
    let mut translator = translator();
    let mut events = Vec::new();
    for frame in [
        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"before"}}),
        json!({"sessionUpdate":"tool_call","toolCallId":"1","title":"operation","status":"completed"}),
        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"after"}}),
    ] {
        events.extend(translator.update(&update(frame), 10).unwrap());
    }
    events.extend(translator.finish(protocol::StopReason::EndTurn, 20));
    let items: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            PlannerEventKind::Item {
                phase: ItemPhase::Completed,
                params,
                ..
            } => Some(&params["item"]),
            _ => None,
        })
        .collect();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["text"], "before");
    assert_eq!(items[1]["type"], "dynamicToolCall");
    assert_eq!(items[2]["text"], "after");
    let ids: std::collections::BTreeSet<_> = items
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids.len(),
        3,
        "native tool ids cannot alias generated text ids"
    );
}
fn update(value: Value) -> Value {
    json!({"sessionId":"native","update":value})
}
#[test]
fn streaming_reply_starts_empty_and_accumulates_exact_text_once() {
    let mut translator = translator();
    for (i, text) in ["hello ", "world"].iter().enumerate() {
        let events=translator.update(&update(json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}})),10).unwrap();
        if i == 0 {
            let PlannerEventKind::Item { params, phase, .. } = &events[0].kind else {
                panic!("start")
            };
            assert_eq!(*phase, ItemPhase::Started);
            assert_eq!(params["item"]["text"], "");
            assert_eq!(events.len(), 2);
        } else {
            assert_eq!(events.len(), 1);
        }
        assert!(
            matches!(&events.last().unwrap().kind,PlannerEventKind::ReplyDelta{delta,..} if delta==text)
        );
    }
    let events = translator.finish(protocol::StopReason::EndTurn, 20);
    let PlannerEventKind::Item { params, phase, .. } = &events[0].kind else {
        panic!("completion")
    };
    assert_eq!(*phase, ItemPhase::Completed);
    assert_eq!(params["item"]["text"], "hello world");
    assert!(
        matches!(&events.last().unwrap().kind,PlannerEventKind::TurnCompleted{turn} if turn["status"]=="completed")
    );
}
#[test]
fn tool_updates_merge_native_content_without_inventing_kernel_tool_identity() {
    let mut translator = translator();
    translator.update(&update(json!({"sessionUpdate":"tool_call","toolCallId":"tool","title":"Run command","kind":"execute","rawInput":{"command":"pwd"}})),1).unwrap();
    let events=translator.update(&update(json!({"sessionUpdate":"tool_call_update","toolCallId":"tool","status":"completed","content":[{"type":"content","content":{"type":"text","text":"/w"}}]})),2).unwrap();
    let PlannerEventKind::Item { params, phase, .. } = &events[0].kind else {
        panic!("tool")
    };
    assert_eq!(*phase, ItemPhase::Completed);
    assert_eq!(params["item"]["type"], "dynamicToolCall");
    assert_eq!(params["item"]["arguments"]["command"], "pwd");
    assert_eq!(params["item"]["tool"], "Run command");
    assert!(params["item"].get("server").is_none());
    assert_eq!(params["item"]["native"]["kind"], "execute");
}
#[test]
fn malformed_known_updates_and_foreign_sessions_are_rejected_but_extensions_are_ignored() {
    let mut translator = translator();
    for invalid in [
        json!({"sessionId":"foreign","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"wrong"}}}),
        update(json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text"}})),
        update(
            json!({"sessionUpdate":"tool_call_update","toolCallId":"missing","status":"completed"}),
        ),
        update(json!({"sessionUpdate":"plan","entries":[{"content":"work","status":"bogus"}]})),
    ] {
        assert!(translator.update(&invalid, 0).is_err());
    }
    assert!(
        translator
            .update(
                &update(json!({"sessionUpdate":"_future_update","value":true})),
                0
            )
            .unwrap()
            .is_empty()
    );
}
#[test]
fn cancellation_is_not_reported_as_success() {
    let events = translator().finish(protocol::StopReason::Cancelled, 0);
    assert!(
        matches!(&events[0].kind,PlannerEventKind::TurnCompleted{turn} if turn["status"]=="interrupted")
    );
}
#[test]
fn an_agent_error_fails_the_turn_with_its_text_after_completing_streamed_text() {
    let mut translator = translator();
    translator.update(&update(json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"partial"}})),1).unwrap();
    let events = translator.fail("Internal error: no balance", 2);
    assert!(
        matches!(&events[0].kind,PlannerEventKind::Item{phase:ItemPhase::Completed,params,..} if params["item"]["text"]=="partial")
    );
    assert!(
        matches!(&events[1].kind,PlannerEventKind::TurnCompleted{turn} if turn["status"]=="failed" && turn["error"]["message"]=="Internal error: no balance")
    );
}

#[tokio::test]
async fn abandoned_dispatched_response_closes_the_pipe_before_more_requests() {
    let (connection, mut reader, _writer) = peer();
    let pending = connection
        .client
        .submit("session/prompt", json!({}))
        .await
        .unwrap();
    assert_eq!(read(&mut reader).await["method"], "session/prompt");
    drop(pending);
    assert_eq!(
        connection
            .client
            .submit("session/prompt", json!({}))
            .await
            .err(),
        Some(Error::Closed)
    );
    let mut residue = String::new();
    assert_eq!(
        tokio::time::timeout(BUDGET, reader.read_line(&mut residue))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}
