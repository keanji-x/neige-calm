use super::delivery::RevertDelivery;
use super::request_attempt::RequestCrossing;
use super::*;

/// Answer the one request the peer receives with `answer` (a `result` or an `error` member).
async fn answer_once(mut peer: WebSocketStream<UnixStream>, answer: Value) -> Value {
    let frame = peer.next().await.unwrap().unwrap();
    let request: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
    let mut response = json!({"jsonrpc": "2.0", "id": request["id"]});
    for (key, value) in answer.as_object().unwrap() {
        response[key] = value.clone();
    }
    peer.send(Message::Text(response.to_string()))
        .await
        .unwrap();
    request
}

/// The pinned binary's method and params (#1923); `thread/rollback` is the vendored protocol's
/// stale name, which it rejects.
#[tokio::test]
async fn h2_thread_revert_sends_the_pinned_wire_and_reads_a_revert() {
    let (client, _notifications, peer) = CodexAppServer::connect_pair_for_test().await;
    let answer = tokio::spawn(answer_once(
        peer,
        json!({"result": {"thread": {"id": "thread-1"}}}),
    ));
    assert!(matches!(
        client.thread_revert_delivery("thread-1", "turn-2").await,
        RevertDelivery::NativeAck
    ));
    let request = answer.await.unwrap();
    assert_eq!(request["method"], "thread/revert");
    assert_eq!(
        request["params"],
        json!({"threadId": "thread-1", "beforeTurnId": "turn-2"})
    );
}

/// An after-send error is diagnostic evidence, not proof that history was untouched.
#[tokio::test]
async fn h2_thread_revert_remote_error_retains_structured_unknown_facts() {
    let (client, _notifications, peer) = CodexAppServer::connect_pair_for_test().await;
    let answer = tokio::spawn(answer_once(
        peer,
        json!({"error": {"code": -32600, "message": "turn not found: turn-2", "data":{"opaque":true}}}),
    ));
    let outcome = client.thread_revert_delivery("thread-1", "turn-2").await;
    let request = answer.await.unwrap();
    let RevertDelivery::Unknown { error } = outcome else {
        panic!("remote error lost uncertainty: {outcome:?}");
    };
    assert_eq!(error.crossing(), RequestCrossing::AfterSend);
    assert_eq!(error.request_id(), request["id"].as_u64());
    let remote = error.remote_error().unwrap();
    assert_eq!(remote.code(), -32600);
    assert_eq!(remote.message(), "turn not found: turn-2");
    assert_eq!(remote.data(), Some(&json!({"opaque":true})));
}

/// A JSON-RPC success envelope alone is not a receipt for this thread's cut.
#[tokio::test]
async fn h2_thread_revert_requires_matching_native_thread_receipt() {
    for result in [
        Value::Null,
        json!({}),
        json!({"thread": {}}),
        json!({"thread": {"id": ""}}),
        json!({"thread": {"id": "another-thread"}}),
    ] {
        let (client, _notifications, peer) = CodexAppServer::connect_pair_for_test().await;
        let answer = tokio::spawn(answer_once(peer, json!({"result": result.clone()})));
        let outcome = client.thread_revert_delivery("thread-1", "turn-2").await;
        let request = answer.await.unwrap();
        assert_eq!(request["method"], "thread/revert");
        assert!(
            matches!(outcome, RevertDelivery::Unknown { .. }),
            "unqualified revert was accepted: {result}"
        );
    }
}

#[tokio::test]
async fn h2_thread_revert_distinguishes_before_send_and_failed_send() {
    for before in [false, true] {
        let (client, _notifications, peer) = CodexAppServer::connect_pair_for_test().await;
        if before {
            client.transport.poison();
        }
        drop(peer);
        let outcome = client.thread_revert_delivery("thread-1", "turn-2").await;
        match (before, outcome) {
            (true, RevertDelivery::NotWritten { error }) => {
                assert_eq!(error.crossing(), RequestCrossing::BeforeSend)
            }
            (false, RevertDelivery::Unknown { error }) => {
                assert_eq!(error.crossing(), RequestCrossing::AfterSend)
            }
            other => panic!("incorrect revert crossing: {other:?}"),
        }
    }
}

#[tokio::test]
async fn h2_thread_revert_timeout_and_malformed_error_are_unknown() {
    for malformed in [false, true] {
        let (client, _notifications, mut peer) = CodexAppServer::connect_pair_for_test().await;
        let client = client.with_request_timeout(Duration::from_millis(50));
        let answer = tokio::spawn(async move {
            let frame = peer.next().await.unwrap().unwrap();
            let request: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            if malformed {
                peer.send(Message::Text(
                    json!({"id":request["id"],"error":{"message":"missing code"}}).to_string(),
                ))
                .await
                .unwrap();
            }
            peer
        });
        assert!(
            matches!(client.thread_revert_delivery("thread-1", "turn-2").await, RevertDelivery::Unknown { error } if error.crossing() == RequestCrossing::AfterSend)
        );
        let _peer = answer.await.unwrap();
    }
}
