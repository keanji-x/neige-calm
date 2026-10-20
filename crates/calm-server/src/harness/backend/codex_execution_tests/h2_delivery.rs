use super::*;
use crate::shared_codex_appserver::{CodexPromptDelivery, NativePromptAdmission, NativeSteer};

#[tokio::test]
async fn h2_steer_invalid_identities_are_unknown_on_the_wire() {
    for result in [
        json!({}),
        json!({"turnId":""}),
        json!({"turnId":"wrong"}),
        json!(null),
    ] {
        let fx = Fixture::new().await;
        let (_client, mut peer) = connect(&fx.daemon).await;
        accepted_legacy(&fx, &mut peer).await;
        let daemon = fx.daemon.clone();
        let thread = fx.owner.thread.clone();
        let task = tokio::spawn(async move {
            super::super::control::steer_delivery(
                daemon.turn_steer(&thread, "initial", vec![], None).await,
            )
        });
        let call = request(&mut peer).await;
        assert_eq!(call["method"], "turn/steer");
        send(&mut peer, json!({"id":call["id"],"result":result})).await;
        assert!(matches!(task.await.unwrap(), SteerOutcome::Unknown { .. }));
    }
}

#[tokio::test]
async fn h2_compact_unknown_wire_results_never_become_submitted() {
    for answer in [
        json!({"result":null}),
        json!({"result":17}),
        json!({"error":{"code":-32600,"message":"opaque"}}),
        json!({"result":{}}),
    ] {
        let ack = answer == json!({"result":{}});
        let fx = Fixture::new().await;
        let (_client, mut peer) = connect(&fx.daemon).await;
        let daemon = fx.daemon.clone();
        let thread = fx.owner.thread.clone();
        let task = tokio::spawn(async move {
            super::super::control::compact_delivery(daemon.thread_compact_delivery(&thread).await)
        });
        let call = request(&mut peer).await;
        assert_eq!(call["method"], "thread/compact/start");
        let mut response = answer;
        response["id"] = call["id"].clone();
        send(&mut peer, response).await;
        let outcome = task.await.unwrap();
        assert_eq!(matches!(outcome, CompactSubmission::Submitted { .. }), ack);
        if !ack {
            assert!(matches!(outcome, CompactSubmission::Unconfirmed { .. }));
        }
    }
}

#[tokio::test]
async fn h2_remote_steer_reports_unknown_with_structured_facts_and_cleans_ticket() {
    for code in [-32600, -32602, -32099] {
        let fx = Fixture::new().await;
        let (_client, mut peer) = connect(&fx.daemon).await;
        accepted_legacy(&fx, &mut peer).await;
        for rejected in [true, false] {
            let daemon = fx.daemon.clone();
            let thread = fx.owner.thread.clone();
            let task = tokio::spawn(async move {
                daemon
                    .turn_steer_report(
                        &thread,
                        "initial",
                        vec![],
                        Some("steer-client"),
                        NativeSteer::Legacy,
                        &NativePromptAdmission::new(|| Ok(()), || Box::pin(async { Ok(()) })),
                    )
                    .await
            });
            let call = request(&mut peer).await;
            assert_eq!(call["method"], "turn/steer");
            send(&mut peer, if rejected {
                json!({"id":call["id"],"error":{"code":code,"message":"opaque","data":{"detail":true}}})
            } else { json!({"id":call["id"],"result":{"turnId":"initial"}}) }).await;
            let report = task.await.unwrap();
            if rejected {
                assert_eq!(report.delivery(), &CodexPromptDelivery::Unknown);
                let failure = report.rpc_failure_ref().unwrap();
                assert_eq!(failure.request_id(), call["id"].as_u64());
                assert_eq!(
                    failure.crossing(),
                    provider::codex::RequestCrossing::AfterSend
                );
                let remote = failure.remote_error().unwrap();
                assert_eq!(remote.code(), code);
                assert_eq!(remote.message(), "opaque");
                assert_eq!(remote.data(), Some(&json!({"detail":true})));
            } else {
                assert!(
                    matches!(report.delivery(), CodexPromptDelivery::Accepted { turn_id } if turn_id == "initial")
                );
            }
        }
    }
}

#[tokio::test]
async fn h2_revert_consumes_only_matching_ack_and_preserves_non_ack_host_decision() {
    for result in [
        json!(null),
        json!({}),
        json!({"thread":{"id":"wrong"}}),
        json!({"thread":{"id":""}}),
        json!({"thread":{"id":"MATCH"}}),
    ] {
        let fx = Fixture::new().await;
        let (_client, mut peer) = connect(&fx.daemon).await;
        fx.accepted(&mut peer, "initial").await;
        let ack = result == json!({"thread":{"id":"MATCH"}});
        let result = if ack {
            json!({"thread":{"id":fx.owner.thread}})
        } else {
            result
        };
        let daemon = fx.daemon.clone();
        let thread = fx.owner.thread.clone();
        let task = tokio::spawn(async move {
            super::super::start::revert_codex_thread(&daemon, &thread, "initial").await
        });
        let call = request(&mut peer).await;
        assert_eq!(call["method"], "thread/revert");
        assert_eq!(
            call["params"],
            json!({"threadId":fx.owner.thread,"beforeTurnId":"initial"})
        );
        send(&mut peer, json!({"id":call["id"],"result":result})).await;
        let outcome = task.await.unwrap().unwrap();
        assert_eq!(outcome.is_none(), ack);
        assert_eq!(
            fx.daemon
                .active_turn_id_for_thread(&fx.owner.thread)
                .as_deref(),
            if ack { None } else { Some("initial") }
        );
    }
}

async fn accepted_legacy(fx: &Fixture, peer: &mut Peer) {
    let daemon = fx.daemon.clone();
    let thread = fx.owner.thread.clone();
    let task = tokio::spawn(async move {
        daemon
            .turn_start_control(
                &thread,
                vec![],
                &TurnModelSelection::inherit(),
                provider::codex::TurnApprovals::Unchanged,
                None,
            )
            .await
    });
    let call = request(peer).await;
    assert_eq!(call["method"], "turn/start");
    send(
        peer,
        json!({"id":call["id"],"result":{"turn":{"id":"initial"}}}),
    )
    .await;
    assert!(matches!(
        task.await.unwrap(),
        provider::codex::delivery::StartDelivery::NativeAck { .. }
    ));
}
