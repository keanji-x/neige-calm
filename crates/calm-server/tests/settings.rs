//! Settings persistence + REST end-to-end.
//!
//! Two layers exercised here:
//!   1. Direct `Repo` calls against an in-memory SQLite — proves the KV
//!      surface (`settings_get_all`, `settings_upsert`, `settings_delete`)
//!      round-trips and is idempotent.
//!   2. `GET /api/settings` / `PUT /api/settings` against the full router
//!      with an `AppState` wired to the same repo — proves the route
//!      collapses empty/null values to deletes and returns the resulting
//!      bag.

use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn fresh_state() -> (AppState, Arc<SqlxRepo>) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::wave_cove_cache::WaveCoveCache::new(),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );
    (state, repo)
}

#[tokio::test]
async fn repo_round_trips_settings_kv() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    assert!(repo.settings_get_all().await.unwrap().is_empty());

    repo.settings_upsert("http_proxy", "http://127.0.0.1:10809")
        .await
        .unwrap();
    repo.settings_upsert("https_proxy", "http://127.0.0.1:10809")
        .await
        .unwrap();
    let mut rows = repo.settings_get_all().await.unwrap();
    rows.sort();
    assert_eq!(
        rows,
        vec![
            (
                "http_proxy".to_string(),
                "http://127.0.0.1:10809".to_string()
            ),
            (
                "https_proxy".to_string(),
                "http://127.0.0.1:10809".to_string()
            ),
        ]
    );

    // Upsert is idempotent and overrides.
    repo.settings_upsert("http_proxy", "http://proxy.example:3128")
        .await
        .unwrap();
    let rows = repo.settings_get_all().await.unwrap();
    let http = rows.iter().find(|(k, _)| k == "http_proxy").unwrap();
    assert_eq!(http.1, "http://proxy.example:3128");

    // Delete is idempotent.
    repo.settings_delete("http_proxy").await.unwrap();
    repo.settings_delete("http_proxy").await.unwrap();
    let rows = repo.settings_get_all().await.unwrap();
    assert!(rows.iter().all(|(k, _)| k != "http_proxy"));
}

#[tokio::test]
async fn get_settings_returns_empty_bag_initially() {
    let (state, _repo) = fresh_state().await;
    let app = axum::Router::new()
        .merge(routes::router())
        .with_state(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(v["settings"].is_object());
    assert_eq!(v["settings"].as_object().unwrap().len(), 0);
}

#[tokio::test]
async fn put_then_get_round_trips_proxy() {
    let (state, _repo) = fresh_state().await;
    let app = axum::Router::new()
        .merge(routes::router())
        .with_state(state);

    let body = serde_json::json!({
        "settings": {
            "http_proxy": "http://10.0.0.5:3128",
            "https_proxy": "http://10.0.0.5:3128",
        }
    })
    .to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/settings")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["settings"]["http_proxy"], "http://10.0.0.5:3128");
    assert_eq!(v["settings"]["https_proxy"], "http://10.0.0.5:3128");

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["settings"]["http_proxy"], "http://10.0.0.5:3128");
}

#[tokio::test]
async fn put_with_empty_or_null_clears_key() {
    let (state, repo) = fresh_state().await;
    repo.settings_upsert("http_proxy", "http://10.0.0.5:3128")
        .await
        .unwrap();
    repo.settings_upsert("https_proxy", "http://10.0.0.5:3128")
        .await
        .unwrap();

    let app = axum::Router::new()
        .merge(routes::router())
        .with_state(state);

    // Empty string + null both clear.
    let body = serde_json::json!({
        "settings": {
            "http_proxy": "",
            "https_proxy": serde_json::Value::Null,
        }
    })
    .to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/settings")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let obj = v["settings"].as_object().unwrap();
    assert!(!obj.contains_key("http_proxy"));
    assert!(!obj.contains_key("https_proxy"));
}

#[tokio::test]
async fn settings_loader_picks_up_proxy_for_codex_spawn() {
    use calm_server::routes::settings::load_settings;
    let (state, repo) = fresh_state().await;
    repo.settings_upsert("http_proxy", "http://corp.proxy:8080")
        .await
        .unwrap();

    let s = load_settings(state.repo.as_ref()).await.unwrap();
    assert_eq!(s.http_proxy.as_deref(), Some("http://corp.proxy:8080"));
    assert!(s.https_proxy.is_none());
}
