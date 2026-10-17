use super::*;

#[tokio::test]
async fn expired_attempt_has_pre_send_evidence_and_never_reaches_peer() {
    let mut h = harness().await;
    let deadline = tokio::time::Instant::now() - Duration::from_secs(1);
    let failure = h
        .client
        .request_attempt_until::<Value>("turn/start", json!({}), deadline)
        .await
        .unwrap_err();
    assert_eq!(failure.crossing(), RequestCrossing::BeforeSend);
    assert_eq!(failure.method(), "turn/start");
    assert_eq!(failure.request_id(), None);
    assert!(h.client.pending.lock().unwrap().is_empty());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), h.server.next())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn poisoned_transport_refuses_new_attempt_before_send() {
    let h = harness().await;
    h.client.transport.poison();
    let failure = h
        .client
        .turn_start_attempt(
            "thread",
            vec![InputItem::text("input")],
            &TurnModelSelection::inherit(),
            TurnApprovals::Unchanged,
            Some("submission"),
        )
        .await
        .unwrap_err();
    assert_eq!(failure.crossing(), RequestCrossing::BeforeSend);
    assert_eq!(failure.request_id(), None);
}

#[tokio::test]
async fn remote_steer_error_preserves_structure_and_is_not_non_delivery_proof() {
    let mut h = harness().await;
    let responder = tokio::spawn(async move {
        let request = server_recv_json(&mut h.server).await;
        assert_eq!(request["method"], "turn/steer");
        assert_eq!(request["params"]["clientUserMessageId"], "submission");
        server_send_json(
            &mut h.server,
            json!({"id":request["id"],"error":{
                "code":-32602,"message":"invalid input",
                "data":{"detail":"provider-owned-detail"}
            }}),
        )
        .await;
        (request["id"].as_u64().unwrap(), h.server)
    });
    let failure = h
        .client
        .turn_steer_attempt(
            "thread",
            "turn",
            vec![InputItem::text("input")],
            Some("submission"),
        )
        .await
        .unwrap_err();
    let (request_id, _peer) = responder.await.unwrap();
    assert_eq!(failure.crossing(), RequestCrossing::AfterSend);
    assert_eq!(failure.method(), "turn/steer");
    assert_eq!(failure.request_id(), Some(request_id));
    let remote = failure.remote_error().unwrap();
    assert_eq!(remote.code(), -32602);
    assert_eq!(remote.message(), "invalid input");
    assert_eq!(
        remote.data(),
        Some(&json!({"detail":"provider-owned-detail"}))
    );
    assert!(!format!("{failure:?}").contains("provider-owned-detail"));
}

#[tokio::test]
async fn malformed_result_after_wire_retains_unknown_crossing() {
    let mut h = harness().await;
    let responder = tokio::spawn(async move {
        let request = server_recv_json(&mut h.server).await;
        server_send_json(&mut h.server, json!({"id":request["id"],"result":17})).await;
        h.server
    });
    let failure = h
        .client
        .turn_steer_attempt("thread", "turn", Vec::new(), Some("submission"))
        .await
        .unwrap_err();
    assert_eq!(failure.crossing(), RequestCrossing::AfterSend);
    assert!(failure.request_id().is_some());
    assert!(failure.remote_error().is_none());
    assert!(matches!(failure.into_legacy_error(), Error::Transport(_)));
    let _peer = responder.await.unwrap();
}

#[tokio::test]
async fn unanswered_attempt_after_wire_retains_request_correlation() {
    let mut h = harness().await;
    let client = h.client.with_request_timeout(Duration::from_millis(50));
    let responder = tokio::spawn(async move {
        let request = server_recv_json(&mut h.server).await;
        (request["id"].as_u64().unwrap(), h.server)
    });
    let failure = client
        .turn_steer_attempt("thread", "turn", Vec::new(), Some("submission"))
        .await
        .unwrap_err();
    let (request_id, _peer) = responder.await.unwrap();
    assert_eq!(failure.crossing(), RequestCrossing::AfterSend);
    assert_eq!(failure.request_id(), Some(request_id));
    assert!(failure.remote_error().is_none());
    assert!(client.pending.lock().unwrap().is_empty());
}
