use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_create_with_id_tx};
use calm_server::event::EventBus;
use calm_server::model::{Card, CardRole, HarnessItem, NewCard, NewCove, NewWave, new_id};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_cove_cache::WaveCoveCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    spec_card: Card,
    plain_card: Card,
}

async fn boot() -> Boot {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "items-rest".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "items rest".into(),
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
            wave_id: wave.id.clone(),
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
    let plain_card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            wave_id: wave.id,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        },
        CardRole::Plain,
        true,
        &role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-items-rest"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(role_cache.clone(), wave_cove_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(role_cache),
        Some(wave_cove_cache),
    );
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    Boot {
        app,
        repo,
        spec_card,
        plain_card,
    }
}

async fn get(app: axum::Router, uri: String) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
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
async fn harness_items_route_returns_rows_in_order_and_paginates() {
    let boot = boot().await;
    let mut inserted = Vec::new();
    for (uuid, text) in [
        ("item-rest-1", "first"),
        ("item-rest-2", "second"),
        ("item-rest-3", "third"),
    ] {
        let id = boot
            .repo
            .harness_item_insert(
                "runtime-rest",
                boot.spec_card.id.as_str(),
                boot.spec_card.wave_id.as_str(),
                "thread-rest",
                Some("turn-rest"),
                Some(uuid),
                Some("agent_message"),
                "item/completed",
                &json!({ "item": { "id": uuid, "type": "agent_message", "text": text } })
                    .to_string(),
            )
            .await
            .unwrap();
        inserted.push(id);
    }

    let (status, body) = get(
        boot.app.clone(),
        format!("/api/cards/{}/harness/items", boot.spec_card.id.as_str()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let rows: Vec<HarnessItem> = serde_json::from_value(body).unwrap();
    assert_eq!(rows.iter().map(|row| row.id).collect::<Vec<_>>(), inserted);
    assert_eq!(
        rows.iter()
            .map(|row| row.item_uuid.as_deref())
            .collect::<Vec<_>>(),
        vec![
            Some("item-rest-1"),
            Some("item-rest-2"),
            Some("item-rest-3")
        ]
    );

    let (status, body) = get(
        boot.app.clone(),
        format!(
            "/api/cards/{}/harness/items?after_id={}&limit=1",
            boot.spec_card.id.as_str(),
            inserted[0]
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let rows: Vec<HarnessItem> = serde_json::from_value(body).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, inserted[1]);
    assert_eq!(rows[0].item_uuid.as_deref(), Some("item-rest-2"));

    let (status, body) = get(
        boot.app.clone(),
        format!(
            "/api/cards/{}/harness/items?direction=desc&limit=2",
            boot.spec_card.id.as_str()
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let rows: Vec<HarnessItem> = serde_json::from_value(body).unwrap();
    assert_eq!(
        rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![inserted[1], inserted[2]]
    );
    assert_eq!(
        rows.iter()
            .map(|row| row.item_uuid.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("item-rest-2"), Some("item-rest-3")]
    );
}

#[tokio::test]
async fn harness_items_desc_cursor_uses_less_than_after_id() {
    let boot = boot().await;
    let mut inserted = Vec::new();
    for index in 1..=5 {
        let uuid = format!("item-desc-{index}");
        let id = boot
            .repo
            .harness_item_insert(
                "runtime-desc",
                boot.spec_card.id.as_str(),
                boot.spec_card.wave_id.as_str(),
                "thread-desc",
                Some("turn-desc"),
                Some(&uuid),
                Some("agent_message"),
                "item/completed",
                &json!({ "item": { "id": uuid, "type": "agent_message" } }).to_string(),
            )
            .await
            .unwrap();
        inserted.push(id);
    }

    let (status, body) = get(
        boot.app.clone(),
        format!(
            "/api/cards/{}/harness/items?direction=desc&limit=2&after_id={}",
            boot.spec_card.id.as_str(),
            inserted[2]
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    let rows: Vec<HarnessItem> = serde_json::from_value(body).unwrap();
    assert_eq!(
        rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![inserted[0], inserted[1]]
    );
}

#[tokio::test]
async fn harness_items_route_rejects_non_spec_card() {
    let boot = boot().await;
    let (status, body) = get(
        boot.app,
        format!("/api/cards/{}/harness/items", boot.plain_card.id.as_str()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
}

#[tokio::test]
async fn harness_items_route_preserves_mcp_tool_call_camelcase() {
    let boot = boot().await;
    let started_id = boot
        .repo
        .harness_item_insert(
            "runtime-mcp",
            boot.spec_card.id.as_str(),
            boot.spec_card.wave_id.as_str(),
            "thread-mcp",
            Some("turn-mcp-1"),
            Some("mcp-1"),
            Some("mcpToolCall"),
            "item/started",
            &json!({
                "item": {
                    "id": "mcp-1",
                    "type": "mcpToolCall",
                    "status": "inProgress",
                    "server": "neige",
                    "tool": "calm.wave.cat"
                }
            })
            .to_string(),
        )
        .await
        .unwrap();
    let completed_id = boot
        .repo
        .harness_item_insert(
            "runtime-mcp",
            boot.spec_card.id.as_str(),
            boot.spec_card.wave_id.as_str(),
            "thread-mcp",
            Some("turn-mcp-1"),
            Some("mcp-1"),
            Some("mcpToolCall"),
            "item/completed",
            &json!({
                "item": {
                    "id": "mcp-1",
                    "type": "mcpToolCall",
                    "status": "completed",
                    "server": "neige",
                    "tool": "calm.wave.cat",
                    "result": { "content": [{ "type": "text", "text": "ok" }] }
                }
            })
            .to_string(),
        )
        .await
        .unwrap();

    let (status, body) = get(
        boot.app.clone(),
        format!("/api/cards/{}/harness/items", boot.spec_card.id.as_str()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let rows: Vec<HarnessItem> = serde_json::from_value(body).unwrap();
    assert_eq!(
        rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![started_id, completed_id]
    );

    assert_eq!(rows[0].item_type.as_deref(), Some("mcpToolCall"));
    let started_params: Value = serde_json::from_str(&rows[0].params).unwrap();
    assert_eq!(started_params["item"]["status"], "inProgress");

    assert_eq!(rows[1].item_type.as_deref(), Some("mcpToolCall"));
    let completed_params: Value = serde_json::from_str(&rows[1].params).unwrap();
    assert_eq!(completed_params["item"]["status"], "completed");
}
