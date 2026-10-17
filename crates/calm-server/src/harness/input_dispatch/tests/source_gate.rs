//! Revoke the actual issuer after the committed marker, while native connection waits.
use super::*;
use crate::shared_codex_appserver::native_owner_tests::{connect, request, send};
use serde_json::json;

async fn input_gate_case(erase_private_body: bool) {
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
    let (_client, mut peer) = connect(&fixture.daemon).await;
    let gate = fixture.daemon.hold_connection_read_for_test().await;
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
    eprintln!("stage: original marker committed; real native core gate held");
    if erase_private_body {
        let mut tx = begin_immediate_tx(fixture.repo.pool()).await.unwrap();
        erase_message_revisions_tx(
            &mut tx,
            &fixture.message.message_id,
            &PrivateHistoryOperationId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert!(fixture.source.current_principal().is_ok());
        eprintln!("stage: actual private body erased before releasing native gate");
    } else {
        fixture.auth.sessions.remove(&fixture.session).unwrap();
        assert!(fixture.source.current_principal().is_err());
        eprintln!("stage: actual request issuer revoked before releasing native gate");
    }
    drop(gate);
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
    let mut tx = begin_immediate_tx(fixture.repo.pool()).await.unwrap();
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
    if erase_private_body {
        assert!(
            checkpoint.pending.is_empty(),
            "erased private content must never be restored to pending"
        );
        let mut tx = begin_immediate_tx(fixture.repo.pool()).await.unwrap();
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
}

#[tokio::test]
async fn revoked_source_cannot_cross_after_waiting_for_the_actual_native_client() {
    input_gate_case(false).await;
}

#[tokio::test]
async fn privately_erased_body_cannot_cross_after_waiting_for_the_actual_native_client() {
    input_gate_case(true).await;
}
