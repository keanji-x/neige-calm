//! HTTP contract for #1050's mount-time cove summary read.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::NewCove;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn boot() -> (axum::Router, Arc<SqlxRepo>) {
    let concrete = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo: Arc<dyn Repo> = concrete.clone();
    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo,
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-summary-plugins"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );
    (routes::coves::router().with_state(state), concrete)
}

async fn get(app: axum::Router, cove_id: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/coves/{cove_id}/task-summary"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn missing_cove_summary_is_404_without_principal_extraction() {
    let (app, _) = boot().await;
    let (status, body) = get(app, "missing").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body:?}");
}

#[tokio::test]
async fn existing_empty_cove_summary_is_200_and_all_zero() {
    let (app, repo) = boot().await;
    let cove = repo
        .cove_create(NewCove {
            name: "empty".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let (status, body) = get(app, cove.id.as_str()).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(
        body,
        json!({
            "pending": 0,
            "inFlight": 0,
            "done": 0,
            "failed": 0,
            "canceled": 0,
            "legacyLive": 0,
            "blockLive": 0,
            "specLive": 0,
            "userLive": 0,
            "waves": [],
            "truncated": false
        })
    );
}
