use super::*;

#[tokio::test]
async fn plugin_event_bridge_projects_legacy_native_endpoint_and_preserves_storage() {
    let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
    let (mcp, mut received) = captured_mcp_client().await;
    let mut ctx = h.ctx();
    ctx.mcp = mcp;
    dispatch(
        &ctx,
        "neige.event.subscribe",
        json!({"filter":{"events":["card.*"]}}),
    )
    .await
    .unwrap();
    let payload = json!({"schemaVersion":1,"appserver_sock":"unix:///private/native.sock","retained":"report"});
    let card = h
        .ctx_storage
        .repo
        .card_create(NewCard {
            track_id: h.track_id.clone().into(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: payload.clone(),
        })
        .await
        .unwrap();
    let event_card = card.clone();
    let (_, event_id) = write_with_event_typed(
        h.ctx_storage.repo.as_ref(),
        ActorId::User,
        EventScope::System,
        None,
        &h.ctx_storage.event_bus,
        &h.ctx_storage.write,
        move |_tx| Box::pin(async move { Ok(((), Event::CardUpdated(event_card))) }),
    )
    .await
    .unwrap();
    let frame = tokio::time::timeout(std::time::Duration::from_secs(2), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(frame["params"]["_id"], event_id);
    let public_payload = &frame["params"]["event"]["data"]["payload"];
    assert!(public_payload.get("appserver_sock").is_none());
    assert_eq!(public_payload["retained"], "report");
    let stored = h
        .ctx_storage
        .repo
        .card_get(card.id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.payload, payload);
    let event: String = sqlx::query_scalar("SELECT payload FROM events WHERE id=?1")
        .bind(event_id)
        .fetch_one(h.ctx_storage.sqlx_repo.pool())
        .await
        .unwrap();
    assert!(
        event.contains("appserver_sock"),
        "persisted replay history must stay intact"
    );
}
