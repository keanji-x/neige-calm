use super::*;
use crate::codex::delivery::{CompactDelivery, StartDelivery, SteerDelivery};

#[derive(Clone, Copy, Debug)]
enum Reply {
    Ack,
    Empty,
    Mismatch,
    Invalid,
    Remote,
    MalformedError,
    Close,
    Timeout,
}

async fn reply(peer: &mut WebSocketStream<UnixStream>, shape: Reply) {
    let request = server_recv_json(peer).await;
    let id = &request["id"];
    let result = match shape {
        Reply::Ack => match request["method"].as_str().unwrap() {
            "turn/start" => json!({"turn":{"id":"turn"}}),
            "turn/steer" => json!({"turnId":"turn"}),
            "thread/compact/start" => json!({}),
            other => panic!("unexpected method {other}"),
        },
        Reply::Empty => json!({}),
        Reply::Mismatch => json!({"turnId":"other"}),
        Reply::Invalid => json!(17),
        Reply::Remote => {
            server_send_json(peer, json!({"id":id,"error":{"code":-32602,"message":"rejected","data":{"opaque":true}}})).await;
            return;
        }
        Reply::MalformedError => {
            server_send_json(peer, json!({"id":id,"error":{"message":"missing code"}})).await;
            return;
        }
        Reply::Close => {
            peer.close(None).await.unwrap();
            return;
        }
        Reply::Timeout => return,
    };
    server_send_json(peer, json!({"id":id,"result":result})).await;
}

#[tokio::test]
async fn h2_start_delivery_classifies_wire_facts() {
    for shape in [
        Reply::Ack,
        Reply::Empty,
        Reply::Invalid,
        Reply::Remote,
        Reply::MalformedError,
        Reply::Close,
        Reply::Timeout,
    ] {
        let mut h = harness().await;
        let client = h.client.with_request_timeout(Duration::from_millis(
            if matches!(shape, Reply::Timeout) {
                50
            } else {
                1_000
            },
        ));
        let answer = tokio::spawn(async move {
            reply(&mut h.server, shape).await;
            h.server
        });
        let outcome = client
            .turn_start_delivery(
                "thread",
                vec![],
                &TurnModelSelection::inherit(),
                TurnApprovals::Unchanged,
                None,
            )
            .await;
        match (shape, outcome) {
            (
                Reply::Ack,
                StartDelivery::NativeAck {
                    turn,
                    cleanup: None,
                },
            ) => assert_eq!(turn, "turn"),
            (
                Reply::Empty
                | Reply::Invalid
                | Reply::Remote
                | Reply::MalformedError
                | Reply::Close
                | Reply::Timeout,
                StartDelivery::Unknown {
                    error,
                    response_turn: None,
                },
            ) => assert_eq!(
                error.crossing(),
                crate::codex::request_attempt::RequestCrossing::AfterSend
            ),
            other => panic!("incorrect start fact: {other:?}"),
        }
        let _peer = answer.await.unwrap();
    }
}

#[tokio::test]
async fn h2_steer_delivery_classifies_wire_and_invalid_identity_facts() {
    for shape in [
        Reply::Ack,
        Reply::Empty,
        Reply::Mismatch,
        Reply::Invalid,
        Reply::Remote,
        Reply::MalformedError,
        Reply::Close,
        Reply::Timeout,
    ] {
        let mut h = harness().await;
        let client = h.client.with_request_timeout(Duration::from_millis(
            if matches!(shape, Reply::Timeout) {
                50
            } else {
                1_000
            },
        ));
        let answer = tokio::spawn(async move {
            reply(&mut h.server, shape).await;
            h.server
        });
        let outcome = client
            .turn_steer_delivery("thread", "turn", vec![], None)
            .await;
        match (shape, outcome) {
            (Reply::Ack, SteerDelivery::NativeAck { turn }) => assert_eq!(turn, "turn"),
            (
                Reply::Empty
                | Reply::Mismatch
                | Reply::Invalid
                | Reply::Remote
                | Reply::MalformedError
                | Reply::Close
                | Reply::Timeout,
                SteerDelivery::Unknown { error },
            ) => assert_eq!(
                error.crossing(),
                crate::codex::request_attempt::RequestCrossing::AfterSend
            ),
            other => panic!("incorrect steer fact: {other:?}"),
        }
        let _peer = answer.await.unwrap();
    }
}

#[tokio::test]
async fn h2_compact_delivery_classifies_wire_facts() {
    for shape in [
        Reply::Ack,
        Reply::Invalid,
        Reply::Remote,
        Reply::MalformedError,
        Reply::Close,
        Reply::Timeout,
    ] {
        let mut h = harness().await;
        let client = h.client.with_request_timeout(Duration::from_millis(
            if matches!(shape, Reply::Timeout) {
                50
            } else {
                1_000
            },
        ));
        let answer = tokio::spawn(async move {
            reply(&mut h.server, shape).await;
            h.server
        });
        let outcome = client.compact_delivery("thread").await;
        match (shape, outcome) {
            (Reply::Ack, CompactDelivery::NativeAck) => {}
            (Reply::Invalid, CompactDelivery::Unknown { .. }) => {}
            (
                Reply::Remote | Reply::MalformedError | Reply::Close | Reply::Timeout,
                CompactDelivery::Unknown { error },
            ) => assert_eq!(
                error.crossing(),
                crate::codex::request_attempt::RequestCrossing::AfterSend
            ),
            other => panic!("incorrect compact fact: {other:?}"),
        }
        let _peer = answer.await.unwrap();
    }
}

#[tokio::test]
async fn h2_all_operations_distinguish_failed_send_from_before_send() {
    for before in [false, true] {
        let h = harness().await;
        if before {
            h.client.transport.poison();
        }
        drop(h.server);
        let start = h
            .client
            .turn_start_delivery(
                "thread",
                vec![],
                &TurnModelSelection::inherit(),
                TurnApprovals::Unchanged,
                None,
            )
            .await;
        assert!(
            matches!(
                (&start, before),
                (StartDelivery::NotWritten { .. }, true) | (StartDelivery::Unknown { .. }, false)
            ),
            "{start:?}"
        );
        // Failed send poisons the transport; use independent connections for each operation.
        let h = harness().await;
        if before {
            h.client.transport.poison();
        }
        drop(h.server);
        let steer = h
            .client
            .turn_steer_delivery("thread", "turn", vec![], None)
            .await;
        assert!(
            matches!(
                (&steer, before),
                (SteerDelivery::NotWritten { .. }, true) | (SteerDelivery::Unknown { .. }, false)
            ),
            "{steer:?}"
        );
        let h = harness().await;
        if before {
            h.client.transport.poison();
        }
        drop(h.server);
        let compact = h.client.compact_delivery("thread").await;
        assert!(
            matches!(
                (&compact, before),
                (CompactDelivery::NotWritten { .. }, true)
                    | (CompactDelivery::Unknown { .. }, false)
            ),
            "{compact:?}"
        );
    }
}

#[tokio::test]
async fn h2_empty_start_identity_is_unknown() {
    let mut h = harness().await;
    let answer = tokio::spawn(async move {
        let request = server_recv_json(&mut h.server).await;
        server_send_json(
            &mut h.server,
            json!({"id":request["id"],"result":{"turn":{"id":""}}}),
        )
        .await;
        h.server
    });
    assert!(
        matches!(h.client.turn_start_delivery("thread", vec![], &TurnModelSelection::inherit(), TurnApprovals::Unchanged, None).await,
        StartDelivery::Unknown { response_turn: Some(turn), .. } if turn.is_empty())
    );
    let _peer = answer.await.unwrap();
}
