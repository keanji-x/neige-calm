use calm_truth::db::sqlite::SqlxRepo;
use calm_truth::db::{RepoRead, RepoSyncDomainRaw};
use calm_truth::event::Event;
use calm_truth::model::{NewArea, NewCard, NewTrack, RequestTheme};
use calm_truth::session_projection_lookup::{
    project_runtime_into_card_payload, project_runtime_into_cards_payload,
    project_runtime_into_event_payload,
};
use serde_json::json;

#[tokio::test]
async fn native_card_projection_hides_legacy_endpoint_without_rewriting_history() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = repo
        .area_create(NewArea {
            name: "projection".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "projection".into(),
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
    let mut one = card.clone();
    project_runtime_into_card_payload(&repo, &mut one)
        .await
        .unwrap();
    assert!(
        one.payload.get("appserver_sock").is_none(),
        "no-runtime legacy card must be projected"
    );
    assert_eq!(one.payload["retained"], "report");
    let mut list = vec![card.clone()];
    project_runtime_into_cards_payload(&repo, &mut list)
        .await
        .unwrap();
    assert_eq!(list[0].payload, one.payload);
    for mut event in [
        Event::CardAdded(card.clone()),
        Event::CardUpdated(card.clone()),
    ] {
        project_runtime_into_event_payload(&repo, &mut event)
            .await
            .unwrap();
        match event {
            Event::CardAdded(c) | Event::CardUpdated(c) => assert_eq!(c.payload, one.payload),
            _ => unreachable!(),
        }
    }
    let stored = repo.card_get(card.id.as_str()).await.unwrap().unwrap();
    assert_eq!(
        stored.payload, payload,
        "projection must preserve historical database metadata"
    );
    let mut opaque = card.clone();
    opaque.kind = "plugin:test".into();
    project_runtime_into_card_payload(&repo, &mut opaque)
        .await
        .unwrap();
    assert_eq!(
        opaque.payload, payload,
        "plugin payload remains owned by its card kind"
    );
}
