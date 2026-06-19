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
use calm_server::event::{EventBus, SYNC_EVENT_VERSION};
use calm_server::mcp_server::transport::KERNEL_MCP_PROTOCOL_VERSION;
use calm_server::plugin_host::mcp::KERNEL_PROTOCOL_VERSION;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::routes::version::{API_VERSION, WEB_COMPAT_VERSION};
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_session::SUPERVISOR_CONTROL_VERSION;
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn fresh_state() -> AppState {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    AppState::from_parts(
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
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
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

    // All version fields are present and camelCase.
    let obj = v.as_object().expect("response is a JSON object");
    for key in [
        "kernelVersion",
        "apiVersion",
        "syncEventVersion",
        "mcpProtocolVersion",
        "pluginMcpProtocolVersion",
        "webCompatVersion",
        "minWebCompatVersion",
        "supervisorControlVersion",
        "buildSha",
        "dbInstanceId",
    ] {
        assert!(obj.contains_key(key), "missing field: {key}");
    }

    // The previous placeholder field is gone — frontends keying off the
    // old name need to fail loudly, not silently observe `null`.
    assert!(
        !obj.contains_key("minWebBuildId"),
        "minWebBuildId should have been renamed to minWebCompatVersion"
    );

    // Type correctness.
    assert!(v["kernelVersion"].is_string());
    assert!(v["apiVersion"].is_string());
    assert!(v["syncEventVersion"].is_number());
    assert!(v["mcpProtocolVersion"].is_string());
    assert!(v["pluginMcpProtocolVersion"].is_string());
    assert!(v["webCompatVersion"].is_number());
    assert!(v["minWebCompatVersion"].is_number());
    assert!(v["supervisorControlVersion"].is_number());
    assert!(v["buildSha"].is_null() || v["buildSha"].is_string());
    assert!(v["dbInstanceId"].is_string());

    // `dbInstanceId` is a UUID v4 (the 13th hex char is `4`, the 17th
    // is one of `8/9/a/b`). Cheap shape check — the per-process
    // uniqueness contract is exercised by the dedicated test below.
    let id = v["dbInstanceId"].as_str().unwrap();
    let parsed = uuid::Uuid::parse_str(id).expect("dbInstanceId is a valid UUID");
    assert_eq!(
        parsed.get_version_num(),
        4,
        "dbInstanceId must be UUID v4, got {parsed}",
    );

    // Source agreement.
    assert_eq!(
        v["kernelVersion"].as_str().unwrap(),
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(
        v["mcpProtocolVersion"].as_str().unwrap(),
        KERNEL_MCP_PROTOCOL_VERSION
    );
    assert_eq!(
        v["pluginMcpProtocolVersion"].as_str().unwrap(),
        KERNEL_PROTOCOL_VERSION
    );
    assert_eq!(v["apiVersion"].as_str().unwrap(), API_VERSION);
    assert_eq!(v["apiVersion"].as_str().unwrap(), "1");
    assert_eq!(
        v["syncEventVersion"].as_u64().unwrap(),
        SYNC_EVENT_VERSION as u64
    );
    assert_eq!(v["syncEventVersion"].as_u64().unwrap(), 5);

    // minWebCompatVersion must echo the in-process constant — the whole
    // point of the field is to bind frontend expectations to a value the
    // backend controls. If someone bumps `WEB_COMPAT_VERSION` without
    // bumping the response builder (or vice-versa), this assertion
    // catches it.
    assert_eq!(
        v["webCompatVersion"].as_u64().unwrap(),
        WEB_COMPAT_VERSION as u64,
    );
    assert_eq!(
        v["minWebCompatVersion"].as_u64().unwrap(),
        WEB_COMPAT_VERSION as u64,
    );
    assert_eq!(
        v["supervisorControlVersion"].as_u64().unwrap(),
        SUPERVISOR_CONTROL_VERSION as u64,
    );
}

/// `dbInstanceId` MUST be unique per `AppState` construction — that's the
/// whole correctness contract the web client relies on for IDB cache
/// busting on DB resets. Two fresh `AppState`s (= two simulated server
/// boots) must produce two distinct ids; the same `AppState` queried
/// twice must produce the same id.
#[tokio::test]
async fn db_instance_id_changes_across_boots_stable_within_boot() {
    async fn hit(state: AppState) -> String {
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
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        v["dbInstanceId"].as_str().unwrap().to_string()
    }

    // Two boots → two distinct ids.
    let boot_a = fresh_state().await;
    let boot_b = fresh_state().await;
    let id_a = hit(boot_a.clone()).await;
    let id_b = hit(boot_b).await;
    assert_ne!(
        id_a, id_b,
        "dbInstanceId must differ across server boots (got the same id twice)",
    );

    // Same boot → stable id across requests.
    let id_a_again = hit(boot_a).await;
    assert_eq!(
        id_a, id_a_again,
        "dbInstanceId must be stable within a single boot",
    );
}
