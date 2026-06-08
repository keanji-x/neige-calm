use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_create_with_id_tx};
use calm_server::event::EventBus;
use calm_server::model::{Card, CardRole, NewCard, NewCove, NewWave, new_id};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_cove_cache::WaveCoveCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    state: AppState,
    repo: Arc<SqlxRepo>,
    spec_card: Card,
}

async fn boot() -> Boot {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "reset-clears-items".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "reset clears items".into(),
            sort: None,
            cwd: "/tmp".into(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let role_cache = CardRoleCache::new();
    let wave_cove_cache = WaveCoveCache::new();
    wave_cove_cache.insert(wave.id.clone(), cove.id);
    let mut tx = repo.pool().begin().await.unwrap();
    let spec_card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            wave_id: wave.id,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "spec_harness": true}),
        },
        CardRole::Spec,
        false,
        &role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join(format!("calm-plugins-data-reset-clears-items-{}", new_id())),
            Vec::new(),
            events,
            calm_server::state::WriteContext::new(role_cache.clone(), wave_cove_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(role_cache),
        Some(wave_cove_cache),
    )
    .with_shared_codex_appserver(SharedCodexAppServer::new_fake_running_with_pending(
        repo.clone(),
        None,
    ));
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());

    Boot {
        app,
        state,
        repo,
        spec_card,
    }
}

async fn post_empty(app: axum::Router, uri: String) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn reset_spec_card_clears_persisted_harness_items() {
    let boot = boot().await;
    for index in 1..=3 {
        let item_uuid = format!("item-before-reset-{index}");
        let params = json!({
            "item": {
                "id": item_uuid.clone(),
                "type": "agent_message",
                "text": format!("old item {index}")
            }
        })
        .to_string();
        boot.repo
            .harness_item_insert(
                "runtime-before-reset",
                boot.spec_card.id.as_str(),
                boot.spec_card.wave_id.as_str(),
                "thread-before-reset",
                Some("turn-before-reset"),
                Some(&item_uuid),
                Some("agent_message"),
                "item/completed",
                &params,
            )
            .await
            .unwrap();
    }
    assert_eq!(
        boot.repo
            .harness_item_list_by_card(boot.spec_card.id.as_str(), 0, 100, false)
            .await
            .unwrap()
            .len(),
        3
    );

    let (status, body) = post_empty(
        boot.app.clone(),
        format!("/api/cards/{}/spec/reset", boot.spec_card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["card_id"], json!(boot.spec_card.id.as_str()));
    assert!(
        boot.repo
            .harness_item_list_by_card(boot.spec_card.id.as_str(), 0, 100, false)
            .await
            .unwrap()
            .is_empty()
    );

    let active = boot
        .repo
        .runtime_get_active_for_card(&boot.spec_card.id.to_string())
        .await
        .unwrap()
        .expect("new active runtime");
    if let Some(handle) = boot.state.harness.remove(&active.id) {
        handle.shutdown().await.unwrap();
    }
}
