//! `GET /api/version` — the kernel/REST/sync/MCP version quadruple plus build metadata.

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
    state_on("sqlite::memory:").await
}

/// One simulated boot on the database at `url` — the same `from_parts` path
/// as `fresh_state`, so two calls on one file are two boots of one database.
async fn state_on(url: &str) -> AppState {
    let repo = Arc::new(SqlxRepo::open(url).await.unwrap());
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
                calm_server::track_area_cache::TrackAreaCache::new(),
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
        "databaseId",
        "nowMs",
    ] {
        assert!(obj.contains_key(key), "missing field: {key}");
    }

    assert!(
        !obj.contains_key("minWebBuildId"),
        "minWebBuildId should have been renamed to minWebCompatVersion"
    );

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
    assert!(v["databaseId"].is_string());
    assert!(v["nowMs"].is_i64());

    // Cheap shape check; per-process uniqueness is the dedicated test below.
    let id = v["dbInstanceId"].as_str().unwrap();
    let parsed = uuid::Uuid::parse_str(id).expect("dbInstanceId is a valid UUID");
    assert_eq!(
        parsed.get_version_num(),
        4,
        "dbInstanceId must be UUID v4, got {parsed}",
    );

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
    assert_eq!(
        v["apiVersion"].as_str().unwrap(),
        "11",
        "#1810: GET /api/models gained `source: \"built_in\"` and a nullable \
         `default_reasoning_effort`, which older clients' schemas reject"
    );
    assert_eq!(
        v["syncEventVersion"].as_u64().unwrap(),
        SYNC_EVENT_VERSION as u64
    );
    // `scripts/gate-sync-event-version-lockstep.sh` binds the constant to this literal, so bumping the constant alone cannot make this file agree with itself.
    assert_eq!(v["syncEventVersion"].as_u64().unwrap(), 21);

    assert_eq!(
        v["webCompatVersion"].as_u64().unwrap(),
        WEB_COMPAT_VERSION as u64,
    );
    assert_eq!(v["webCompatVersion"].as_u64().unwrap(), 31);
    assert_eq!(
        v["minWebCompatVersion"].as_u64().unwrap(),
        WEB_COMPAT_VERSION as u64,
    );
    assert_eq!(v["minWebCompatVersion"].as_u64().unwrap(), 31);
    assert_eq!(
        v["supervisorControlVersion"].as_u64().unwrap(),
        SUPERVISOR_CONTROL_VERSION as u64,
    );
}

/// `dbInstanceId` is what the web client relies on for IDB cache busting on DB resets.
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

/// The literal floor is historical; do not bump it along with `WEB_COMPAT_VERSION`, or the assertion could never fail.
#[tokio::test]
async fn web_compat_floor_is_above_the_previous_bundle() {
    /// The last bundle generation that spoke the pre-rename track-create field names.
    const LAST_PRE_RENAME_FLOOR: u64 = 16;

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

    let floor = v["minWebCompatVersion"]
        .as_u64()
        .expect("minWebCompatVersion is a number");
    assert!(
        floor > LAST_PRE_RENAME_FLOOR,
        "minWebCompatVersion must exclude pre-rename bundles, got {floor}"
    );
}

#[tokio::test]
async fn web_compat_floor_excludes_track_detail_without_resume_capability() {
    const LAST_TRACK_DETAIL_WITHOUT_CAN_RESUME: u64 = 21;

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

    let floor = v["minWebCompatVersion"]
        .as_u64()
        .expect("minWebCompatVersion is a number");
    assert!(
        floor > LAST_TRACK_DETAIL_WITHOUT_CAN_RESUME,
        "minWebCompatVersion must exclude bundles without can_resume, got {floor}"
    );
}

/// The last floor whose bundles did not know `harness.queue.changed` / `restored`; historical literal, do not move it with `WEB_COMPAT_VERSION`.
#[tokio::test]
async fn web_compat_floor_excludes_bundles_that_cannot_decode_a_restored_queue_entry() {
    const LAST_FLOOR_WITHOUT_RESTORED: u64 = 27;

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

    let floor = v["minWebCompatVersion"]
        .as_u64()
        .expect("minWebCompatVersion is a number");
    assert!(
        floor > LAST_FLOOR_WITHOUT_RESTORED,
        "minWebCompatVersion must exclude bundles that reject `restored`, got {floor}"
    );
}

/// The last floor whose bundles did not require `lastTurnCompletedAt`; historical literal, do not move it with `WEB_COMPAT_VERSION`.
#[tokio::test]
async fn web_compat_floor_excludes_bundles_without_last_turn_completed_at() {
    const LAST_FLOOR_WITHOUT_LAST_TURN_COMPLETED_AT: u64 = 28;

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

    let floor = v["minWebCompatVersion"]
        .as_u64()
        .expect("minWebCompatVersion is a number");
    assert!(
        floor > LAST_FLOOR_WITHOUT_LAST_TURN_COMPLETED_AT,
        "minWebCompatVersion must exclude bundles without `lastTurnCompletedAt`, got {floor}"
    );
}

/// The last floor whose bundles reject the Claude alias catalog (`source: "built_in"`, a `null`
/// `default_reasoning_effort`, #1810); historical literal, do not move it with `WEB_COMPAT_VERSION`.
#[tokio::test]
async fn web_compat_floor_excludes_bundles_that_reject_the_claude_catalog() {
    const LAST_FLOOR_WITHOUT_CLAUDE_CATALOG: u64 = 30;

    let floor = version_body(fresh_state().await).await["minWebCompatVersion"]
        .as_u64()
        .expect("minWebCompatVersion is a number");
    assert!(
        floor > LAST_FLOOR_WITHOUT_CLAUDE_CATALOG,
        "minWebCompatVersion must exclude bundles that reject the Claude alias catalog, got {floor}"
    );
}

async fn version_body(state: AppState) -> serde_json::Value {
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
    serde_json::from_slice(&bytes).unwrap()
}

/// `databaseId` names the database and `dbInstanceId` names the boot.
#[tokio::test]
async fn database_id_survives_reboot() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("reboot.db").display()
    );

    let before = calm_server::model::now_ms();
    let boot_a = version_body(state_on(&url).await).await;
    let boot_b = version_body(state_on(&url).await).await;
    let after = calm_server::model::now_ms();

    let database_a = boot_a["databaseId"].as_str().unwrap();
    let database_b = boot_b["databaseId"].as_str().unwrap();
    uuid::Uuid::parse_str(database_a).expect("databaseId is a uuid");
    assert_eq!(
        database_a, database_b,
        "databaseId must survive a reboot of the same database"
    );
    assert_ne!(
        boot_a["dbInstanceId"], boot_b["dbInstanceId"],
        "dbInstanceId must still change per boot"
    );
    assert_ne!(
        database_a,
        boot_a["dbInstanceId"].as_str().unwrap(),
        "the two ids are different facts and must not be the same value"
    );

    // Another database is another identity.
    let other = version_body(fresh_state().await).await;
    assert_ne!(other["databaseId"].as_str().unwrap(), database_a);

    // `nowMs` is the server clock at response time.
    let now = boot_a["nowMs"].as_i64().unwrap();
    assert!(
        (before..=after).contains(&now),
        "nowMs {now} outside [{before}, {after}]"
    );
}
