use super::*;
use crate::codex::request_attempt::{FailureCause, RequestCrossing};

#[tokio::test]
async fn expired_attempt_has_pre_send_evidence_and_never_reaches_peer() {
    let mut h = harness().await;
    let deadline = tokio::time::Instant::now() - Duration::from_secs(1);
    let failure = h
        .client
        .request_attempt_until::<Value>("turn/start", json!({}), deadline)
        .await
        .unwrap_err();
    assert_eq!(failure.crossing, RequestCrossing::BeforeSend);
    assert_eq!(failure.method, "turn/start");
    assert_eq!(failure.request_id, None);
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
    assert_eq!(failure.crossing, RequestCrossing::BeforeSend);
    assert_eq!(failure.request_id, None);
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
    assert_eq!(failure.crossing, RequestCrossing::AfterSend);
    assert_eq!(failure.method, "turn/steer");
    assert_eq!(failure.request_id, Some(request_id));
    assert!(!format!("{failure:?}").contains("provider-owned-detail"));
    let FailureCause::Response(RpcResponseError::Remote(remote)) = &failure.cause else {
        panic!("remote error not retained: {failure:?}");
    };
    assert_eq!(remote.code, -32602);
    assert_eq!(remote.message, "invalid input");
    assert_eq!(remote.data, Some(json!({"detail":"provider-owned-detail"})));
    assert!(!format!("{remote:?}").contains("provider-owned-detail"));
    let legacy = failure.into_legacy_error();
    assert!(
        matches!(&legacy, Error::Refused(message)
            if message == "turn/steer failed: invalid input (code -32602)"),
        "{legacy}"
    );
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
    assert_eq!(failure.crossing, RequestCrossing::AfterSend);
    assert!(failure.request_id.is_some());
    assert!(matches!(failure.cause, FailureCause::Local(_)));
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
    assert_eq!(failure.crossing, RequestCrossing::AfterSend);
    assert_eq!(failure.request_id, Some(request_id));
    assert!(matches!(failure.cause, FailureCause::Local(_)));
    assert!(client.pending.lock().unwrap().is_empty());
}

#[tokio::test]
async fn failed_send_is_after_send_and_poisons_transport() {
    let h = harness().await;
    drop(h.server);
    let failure = h
        .client
        .turn_steer_attempt("thread", "turn", Vec::new(), Some("submission"))
        .await
        .unwrap_err();
    assert_eq!(failure.crossing, RequestCrossing::AfterSend);
    assert!(failure.request_id.is_some());
    assert!(h.client.pending.lock().unwrap().is_empty());
    assert!(
        h.client.transport.check().is_err(),
        "an incomplete send leaves the outcome unknown"
    );
    let legacy = failure.into_legacy_error();
    assert!(
        matches!(&legacy, Error::Transport(message)
            if message.starts_with("send turn/steer: ")),
        "{legacy}"
    );
}
