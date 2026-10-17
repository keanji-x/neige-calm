//! Revoke the real request issuer between actual provider metadata requests.
use super::*;

#[tokio::test]
async fn canonical_revoked_source_cannot_continue_from_config_to_catalog() {
    let fx = Fixture::new().await;
    let card_id = fx.harness.inner.card_id.as_str();
    // Use the settings owner's real value transition: an explicit effort was
    // selected, then changed to the installation default. A config with no
    // effort must consult the actual model catalog to resolve that default.
    let mut tx = begin_immediate_tx(fx.original.repo.pool()).await.unwrap();
    let mut payload = crate::operation::codex_adapter::card_payload_get_tx(&mut tx, card_id)
        .await
        .unwrap();
    let fields = payload.as_object_mut().unwrap();
    crate::planner_model::CardModelSelection::apply_to_payload(
        fields,
        Some("fixture-private-model"),
        Some("medium"),
    );
    crate::planner_model::CardModelSelection::apply_to_payload(
        fields,
        Some("fixture-private-model"),
        None,
    );
    crate::db::sqlite::card_update_tx(
        &mut tx,
        card_id,
        crate::model::CardPatch {
            title: None,
            kind: None,
            sort: None,
            payload: Some(payload),
            deletable: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let before = fx.frame().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    fx.harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone())
        .await
        .unwrap();
    let config = request(&mut peer).await;
    assert_eq!(config["method"], "config/read");
    eprintln!("stage: actual config/read received; no response sent yet");
    assert_eq!(fx.frame().await, before);
    fx.auth.sessions.remove(&fx.session).unwrap();
    assert!(fx.source.current_principal().is_err());
    eprintln!("stage: actual SessionStore credential revoked");
    send(
        &mut peer,
        json!({"id":config["id"],"result":{"config":{
            "model":"fixture-private-model","model_reasoning_effort":null
        }}}),
    )
    .await;

    // This existing production command can be answered only when the running
    // loop has left selection. Its revoked source is rejected, not inserted.
    // It gives the no-next-RPC branch a real completion barrier, not a sleep.
    let settled = fx
        .harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone());
    tokio::pin!(settled);
    let unexpected = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = &mut settled => {
                assert!(matches!(result, Err(CalmError::Unauthorized)));
                None
            }
            call = request(&mut peer) => {
                let method = call["method"].as_str().unwrap().to_owned();
                eprintln!("stage: native RPC after source revocation: {method}");
                // Let the real owner complete so the independent claim/marker
                // assertions and cleanup execute before the decisive failure.
                let reply = if method == "model/list" {
                    json!({"id":call["id"],"result":{"data":[{
                        "id":"fixture-preset","model":"fixture-private-model",
                        "displayName":"Fixture model","description":"test peer",
                        "supportedReasoningEfforts":[{"reasoningEffort":"medium","description":"test effort"}],
                        "defaultReasoningEffort":"medium","isDefault":true
                    }],"nextCursor":null}})
                } else {
                    json!({"id":call["id"],"error":{"code":-32000,"message":"unexpected post-revocation RPC"}})
                };
                send(&mut peer, reply).await;
                assert!(matches!(settled.await, Err(CalmError::Unauthorized)));
                Some(method)
            }
        }
    })
    .await
    .expect("the actual loop must finish or expose its next native request");
    let after = fx.frame().await;
    assert_eq!(
        after, before,
        "revoked input stays in its original pending position"
    );
    assert!(after.canonical_inputs.active.is_none());
    assert!(after.canonical_inputs.attempts.is_empty());
    let submissions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM conversation_input_submissions WHERE card_id=?1")
            .bind(card_id)
            .fetch_one(fx.original.repo.pool())
            .await
            .unwrap();
    assert_eq!(submissions, 0);
    eprintln!("stage: loop completed; original pending preserved; no claim or marker");
    tokio::time::timeout(Duration::from_secs(5), fx.harness.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        unexpected, None,
        "a revoked source cannot authorize a new private model/list request after config/read"
    );
}
