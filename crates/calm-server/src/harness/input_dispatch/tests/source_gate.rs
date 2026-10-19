//! Revoke the actual issuer after its marker, during native connection or sink waits.
use super::*;
use crate::shared_codex_appserver::native_owner_tests::{connect, request, send};
use serde_json::json;

async fn input_gate_case(change: u8, outbound: bool) {
    let mut fixture = Fixture::new().await;
    fixture.daemon = SharedCodexAppServer::new_stub(fixture.repo.clone());
    let card = fixture.repo.card_get(&fixture.card).await.unwrap().unwrap();
    fixture.backend = PlannerBackend::codex(
        fixture.daemon.clone(),
        &fixture.card,
        card.track_id.as_str(),
        &fixture.runtime,
    )
    .unwrap();
    let fixture = Arc::new(fixture);
    let (client, mut peer) = connect(&fixture.daemon).await;
    let connection_gate = if outbound {
        None
    } else {
        Some(fixture.daemon.hold_connection_read_for_test().await)
    };
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let blocker = if outbound {
        let client = client.clone();
        let blocker_entered = entered.clone();
        let release = release.clone();
        // Another real RPC retains the actual sink while its async caller gate
        // waits. The canonical dispatch must queue behind this in-flight call.
        let blocker = tokio::spawn(async move {
            client
                .config_read_gated(
                    None,
                    tokio::time::Instant::now() + Duration::from_secs(10),
                    &provider::codex::PreSendGate::new(|| {
                        Box::pin(async {
                            blocker_entered.notify_one();
                            release.notified().await;
                            Ok(())
                        })
                    }),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        Some(blocker)
    } else {
        None
    };
    let mut dispatch = tokio::spawn({
        let fixture = fixture.clone();
        async move { fixture.dispatch().await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(attempt) = fixture.checkpoint().await.attempt(&fixture.submission) {
                assert!(matches!(
                    attempt.delivery,
                    InputDeliveryFact::CrossingUnknown
                ));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the real input owner must commit its marker before native access");
    if outbound {
        tokio::time::timeout(Duration::from_secs(5), async {
            while client.pending_requests_for_test() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dispatch must reach the actual outbound sink wait");
    }
    eprintln!("stage: original marker committed; real native gate held");
    if change == 1 {
        let mut tx = begin_immediate_tx(fixture.repo.db()).await.unwrap();
        // Model an already-persisted erasure fact; there is no canonical erase command.
        sqlx::query(
            "UPDATE conversation_message_revisions SET payload_json=?1 WHERE message_id=?2",
        )
        .bind(
            serde_json::to_string(
                &PrivateHistoryPayload::<HumanPayload>::ErasedByPrivateHistoryOperation {
                    operation_id: PrivateHistoryOperationId::parse(
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    )
                    .unwrap(),
                },
            )
            .unwrap(),
        )
        .bind((fixture.message.message_id).as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert!(fixture.source.current_principal().is_ok());
        eprintln!("stage: actual private body erased before releasing native gate");
    } else if change >= 2 {
        let mut tx = begin_immediate_tx(fixture.repo.db()).await.unwrap();
        let area = conversation_area_tx(&mut tx, &fixture.card).await.unwrap();
        if change != 4 {
            crate::db::sqlite::area_membership::application_user_register(
                &mut tx, "alice", "Alice",
            )
            .await
            .unwrap();
            if change == 3 {
                sqlx::query("INSERT INTO area_members(area_id,user_id,role,joined_at) VALUES(?1,'alice','member',0)")
                    .bind(area.id.as_str()).execute(&mut **tx).await.unwrap();
            }
        } else {
            sqlx::query("UPDATE areas SET kind='user' WHERE kind='system'")
                .execute(&mut **tx)
                .await
                .unwrap();
            sqlx::query("UPDATE areas SET kind='system' WHERE id=?1")
                .bind(area.id.as_str())
                .execute(&mut **tx)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
    } else {
        fixture.auth.sessions.remove(&fixture.session).unwrap();
        assert!(fixture.source.current_principal().is_err());
        eprintln!("stage: actual request issuer revoked before releasing native gate");
    }
    drop(connection_gate);
    if let Some(blocker) = blocker {
        release.notify_one();
        let call = request(&mut peer).await;
        assert_eq!(
            call["method"], "config/read",
            "only the already admitted blocker may send"
        );
        send(&mut peer, json!({"id":call["id"],"result":{"config":{}}})).await;
        blocker.await.unwrap().unwrap();
    }
    let unexpected = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = &mut dispatch => {
                let _ = result.unwrap();
                None
            }
            call = request(&mut peer) => {
                let method = call["method"].as_str().unwrap().to_owned();
                eprintln!("stage: native method after issuer revocation: {method}");
                assert_eq!(method, "turn/start");
                send(&mut peer, json!({"id":call["id"],"result":{"turn":{"id":"revoked-gate-turn"}}})).await;
                dispatch.await.unwrap().unwrap();
                Some(method)
            }
        }
    })
    .await
    .expect("the actual dispatch must return or expose its private effect");
    let mut tx = begin_immediate_tx(fixture.repo.db()).await.unwrap();
    let original = message_tx(&mut tx, &fixture.message.message_id)
        .await
        .unwrap();
    assert_eq!(original.current, fixture.message);
    tx.rollback().await.unwrap();
    assert_eq!(
        unexpected, None,
        "the native RPC owner must revalidate the live issuer and payload after waiting for its client"
    );
    let checkpoint = fixture.checkpoint().await;
    assert!(checkpoint.active.is_none());
    if change == 1 {
        assert!(
            checkpoint.pending.is_empty(),
            "erased private content must never be restored to pending"
        );
        let mut tx = begin_immediate_tx(fixture.repo.db()).await.unwrap();
        assert!(matches!(
            revision_tx(&mut tx, &fixture.message)
                .await
                .unwrap()
                .payload,
            PrivateHistoryPayload::ErasedByPrivateHistoryOperation { .. }
        ));
        tx.rollback().await.unwrap();
    } else {
        assert_eq!(checkpoint.pending.len(), 1);
        assert_eq!(
            checkpoint.pending[0].input_ref(),
            AcceptedInputRef::Human {
                message: fixture.message.clone()
            }
        );
    }
    assert!(matches!(
        checkpoint.attempt(&fixture.submission).unwrap().delivery,
        InputDeliveryFact::NotDispatched { .. }
    ));
    if outbound {
        assert_eq!(
            checkpoint.attempt(&fixture.submission).unwrap().delivery,
            InputDeliveryFact::NotDispatched {
                basis: "transport_before_send".into()
            }
        );
        assert!(
            checkpoint
                .attempt(&fixture.submission)
                .unwrap()
                .correlation
                .rpc_request_id
                .is_some()
        );
    }
}

#[tokio::test]
async fn revoked_source_cannot_cross_after_waiting_for_the_actual_native_client() {
    input_gate_case(0, false).await;
}

#[tokio::test]
async fn privately_erased_body_cannot_cross_after_waiting_for_the_actual_native_client() {
    input_gate_case(1, false).await;
}

#[tokio::test]
async fn human_private_fence_other_registered_user_late_native_crossing() {
    input_gate_case(2, false).await;
}

#[tokio::test]
async fn human_private_fence_extra_user_area_member_late_native_crossing() {
    input_gate_case(3, false).await;
}

#[tokio::test]
async fn human_private_fence_nonempty_system_area_late_native_crossing() {
    input_gate_case(4, false).await;
}

#[tokio::test]
async fn revoked_source_cannot_cross_after_outbound_sink_wait() {
    input_gate_case(0, true).await;
}

#[tokio::test]
async fn changed_membership_cannot_cross_after_outbound_sink_wait() {
    input_gate_case(3, true).await;
}
