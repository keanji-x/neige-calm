use super::*;
use calm_server::db::{RepoRead, RepoSyncDomainRaw};
use calm_server::model::{NewCard, NewTrack, RequestTheme};

#[tokio::test]
async fn ws_live_and_replay_project_legacy_native_endpoint_without_changing_history() {
    let (addr, repo, bus) = boot().await;
    let area = repo
        .area_create(NewArea {
            name: "legacy".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "legacy".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let payload = json!({"schemaVersion":1,"appserver_sock":"unix:///private/native.sock","retained":"report"});
    let card = repo
        .card_create(NewCard {
            track_id: track.id,
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: payload.clone(),
        })
        .await
        .unwrap();
    let write = calm_server::state::WriteContext::new(
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::track_area_cache::TrackAreaCache::new(),
    );
    let replay_id = append_event(&repo, &bus, &write, Event::CardAdded(card.clone())).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/events"))
        .await
        .unwrap();
    ws.send(TMessage::Text(r#"{"sub":["*"],"since":0}"#.into()))
        .await
        .unwrap();
    let replay = recv_json(&mut ws).await;
    assert_eq!(replay["_id"], replay_id);
    assert_eq!(replay["ev"], "card.added");
    assert!(replay["data"]["payload"].get("appserver_sock").is_none());
    assert_eq!(replay["data"]["payload"]["retained"], "report");
    assert_eq!(recv_json(&mut ws).await["ev"], "_replay_complete");
    let live_id = append_event(&repo, &bus, &write, Event::CardUpdated(card.clone())).await;
    let live = recv_json(&mut ws).await;
    assert_eq!(live["_id"], live_id);
    assert_eq!(live["ev"], "card.updated");
    assert_eq!(live["data"]["payload"], replay["data"]["payload"]);
    let stored = repo.card_get(card.id.as_str()).await.unwrap().unwrap();
    assert_eq!(stored.payload, payload);
    for id in [replay_id, live_id] {
        let stored: String = sqlx::query_scalar("SELECT payload FROM events WHERE id=?1")
            .bind(id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
        assert!(stored.contains("appserver_sock"));
    }
}

async fn append_event(
    repo: &SqlxRepo,
    bus: &EventBus,
    write: &calm_server::state::WriteContext,
    event: Event,
) -> i64 {
    let (_, id) = write_with_event_typed(
        repo,
        ActorId::User,
        EventScope::System,
        None,
        bus,
        write,
        move |_tx| Box::pin(async move { Ok(((), event)) }),
    )
    .await
    .unwrap();
    id
}
