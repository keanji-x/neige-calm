//! `GET /api/version` — surface for the kernel/REST/sync/MCP version
//! quadruple plus optional build metadata.
//!
//! The endpoint is intentionally stateless, but we wire a real `AppState`
//! so the test exercises the same router merge path the production
//! binary uses — that's the layer where an OpenAPI / route-registration
//! mismatch would show up.

use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::mcp::KERNEL_PROTOCOL_VERSION;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::routes::version::{API_VERSION, SYNC_EVENT_VERSION};
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn fresh_state() -> AppState {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
        )),
        Arc::new(CodexClient::new_stub()),
    )
}

#[tokio::test]
async fn get_version_returns_all_fields_with_expected_sources() {
    let state = fresh_state().await;
    let app = axum::Router::new()
        .merge(routes::router())
        .with_state(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/version")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    // All six fields present.
    let obj = v.as_object().expect("response is a JSON object");
    for key in [
        "kernelVersion",
        "apiVersion",
        "syncEventVersion",
        "mcpProtocolVersion",
        "minWebBuildId",
        "buildSha",
    ] {
        assert!(obj.contains_key(key), "missing field: {key}");
    }

    // Type correctness.
    assert!(v["kernelVersion"].is_string());
    assert!(v["apiVersion"].is_string());
    assert!(v["syncEventVersion"].is_number());
    assert!(v["mcpProtocolVersion"].is_string());
    // `null` is a valid JSON value; `is_null()` confirms the default
    // shape until a future PR threads real build artifacts in.
    assert!(v["minWebBuildId"].is_null() || v["minWebBuildId"].is_string());
    assert!(v["buildSha"].is_null() || v["buildSha"].is_string());

    // Source agreement.
    assert_eq!(v["kernelVersion"].as_str().unwrap(), env!("CARGO_PKG_VERSION"));
    assert_eq!(v["mcpProtocolVersion"].as_str().unwrap(), KERNEL_PROTOCOL_VERSION);
    assert_eq!(v["apiVersion"].as_str().unwrap(), API_VERSION);
    assert_eq!(v["apiVersion"].as_str().unwrap(), "1");
    assert_eq!(v["syncEventVersion"].as_u64().unwrap(), SYNC_EVENT_VERSION as u64);
    assert_eq!(v["syncEventVersion"].as_u64().unwrap(), 1);

    // minWebBuildId is always null until a later PR populates it.
    assert!(v["minWebBuildId"].is_null());
}
