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
async fn thread_revert_sends_the_pinned_wire_and_reads_a_revert() {
    let (client, _notifications, peer) = CodexAppServer::connect_pair_for_test().await;
    let answer = tokio::spawn(answer_once(
        peer,
        json!({"result": {"thread": {"id": "thread-1"}}}),
    ));
    client.thread_revert("thread-1", "turn-2").await.unwrap();
    let request = answer.await.unwrap();
    assert_eq!(request["method"], "thread/revert");
    assert_eq!(
        request["params"],
        json!({"threadId": "thread-1", "beforeTurnId": "turn-2"})
    );
}

/// Codex answers `turn not found` alike for a wrong turn and for one already reverted (#2512), so
/// it stays a refusal like any other; only the kernel's own record says a revert went through.
#[tokio::test]
async fn turn_not_found_is_a_refusal() {
    let (client, _notifications, peer) = CodexAppServer::connect_pair_for_test().await;
    let answer = tokio::spawn(answer_once(
        peer,
        json!({"error": {"code": -32600, "message": "turn not found: turn-2"}}),
    ));
    let error = client
        .thread_revert("thread-1", "turn-2")
        .await
        .expect_err("`turn not found` is Codex's refusal");
    assert!(matches!(error, Error::Refused(_)), "{error}");
    answer.await.unwrap();
}
