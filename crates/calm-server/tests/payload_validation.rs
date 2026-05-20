//! Integration tests for D4 per-kind payload validators wired into the
//! `cards` and `overlays` route layer.
//!
//! Boots a minimal Axum app with the cards + overlays routers + a stub-only
//! AppState (in-memory SqlxRepo, EventBus, stub DaemonClient, stub PluginHost),
//! then POSTs payloads through `tower::ServiceExt::oneshot` to verify HTTP-level
//! behavior:
//!
//!   * Bad terminal Card payload → 400 with a clear `bad_request` error code.
//!   * `ui://` Card with arbitrary garbage payload → 201 (opaque path works).
//!   * Bad `status` Overlay payload → 400.
//!   * Good `status` Overlay payload → 200.
//!   * Card `PATCH` with bad payload for an existing `terminal` card → 400.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::Repo;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewCard, NewCove, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Build a minimal AppState + seed one cove + wave + (optional) card. Returns
/// the wave id (and an optional card id) the test will hit.
async fn boot() -> (AppState, String) {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite repo"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "demo".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "demo".into(),
            sort: None,
        })
        .await
        .unwrap();
    let state = AppState {
        repo: repo.clone(),
        events: EventBus::new(),
        daemon: Arc::new(DaemonClient::new_stub()),
        plugin: Arc::new(PluginHost::new(Arc::new(PluginRegistry::empty()), repo)),
    };
    (state, wave.id)
}

fn app(state: AppState) -> axum::Router {
    axum::Router::new()
        .merge(routes::cards::router())
        .merge(routes::overlays::router())
        .with_state(state)
}

async fn body_to_json(resp: axum::http::Response<Body>) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn post_card(app: axum::Router, wave_id: &str, body: Value) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(format!("/api/waves/{wave_id}/cards"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn patch_card(app: axum::Router, card_id: &str, body: Value) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("PATCH")
            .uri(format!("/api/cards/{card_id}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn post_overlay(app: axum::Router, body: Value) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/api/overlays")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

// --------------------------------------------------------------------------
// Cards
// --------------------------------------------------------------------------

#[tokio::test]
async fn post_terminal_card_with_bad_payload_returns_400() {
    let (state, wave_id) = boot().await;
    let resp = post_card(
        app(state),
        &wave_id,
        json!({
            "kind": "terminal",
            // terminal_id must be a string when present.
            "payload": { "terminal_id": 42 }
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_to_json(resp).await;
    assert_eq!(body["code"], "bad_request");
    assert!(
        body["error"].as_str().unwrap().contains("terminal"),
        "error message should mention terminal: {body:?}"
    );
}

#[tokio::test]
async fn post_terminal_card_with_valid_payload_creates() {
    let (state, wave_id) = boot().await;
    let resp = post_card(
        app(state),
        &wave_id,
        json!({
            "kind": "terminal",
            "payload": { "terminal_id": "t1" }
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn post_terminal_card_with_no_payload_is_accepted() {
    // Payload defaults to null on the wire — validator must accept that
    // because freshly-created terminal cards have no PTY yet.
    let (state, wave_id) = boot().await;
    let resp = post_card(app(state), &wave_id, json!({ "kind": "terminal" })).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn post_ui_kind_card_with_junk_payload_is_accepted() {
    // D4 acceptance criterion: `ui://*` cards stay opaque — a junk payload
    // must NOT be rejected. Proves the plugin-defined opt-out works.
    let (state, wave_id) = boot().await;
    let resp = post_card(
        app(state),
        &wave_id,
        json!({
            "kind": "ui://example/view",
            "payload": { "junk": "ok", "any": [1, 2, 3] }
        }),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "ui:// kind must be opaque"
    );
}

#[tokio::test]
async fn patch_terminal_card_with_bad_payload_returns_400() {
    // Seed a terminal card directly via the repo so we can patch it.
    let (state, wave_id) = boot().await;
    let seeded = state
        .repo
        .card_create(NewCard {
            wave_id: wave_id.clone(),
            kind: "terminal".into(),
            sort: None,
            payload: json!({ "terminal_id": "t1" }),
        })
        .await
        .unwrap();

    let resp = patch_card(
        app(state),
        &seeded.id,
        json!({ "payload": { "terminal_id": 99 } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_to_json(resp).await;
    assert_eq!(body["code"], "bad_request");
}

#[tokio::test]
async fn patch_ui_card_with_junk_payload_is_accepted() {
    // Patching a ui://* card must remain opaque too.
    let (state, wave_id) = boot().await;
    let seeded = state
        .repo
        .card_create(NewCard {
            wave_id: wave_id.clone(),
            kind: "ui://example/view".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();

    let resp = patch_card(
        app(state),
        &seeded.id,
        json!({ "payload": { "garbage": true } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

// --------------------------------------------------------------------------
// Overlays
// --------------------------------------------------------------------------

#[tokio::test]
async fn post_status_overlay_with_bad_payload_returns_400() {
    let (state, wave_id) = boot().await;
    let resp = post_overlay(
        app(state),
        json!({
            "plugin_id": "p1",
            "entity_kind": "wave",
            "entity_id": wave_id,
            "kind": "status",
            "payload": {} // missing required `state` field
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_to_json(resp).await;
    assert_eq!(body["code"], "bad_request");
}

#[tokio::test]
async fn post_status_overlay_with_valid_payload_returns_200() {
    let (state, wave_id) = boot().await;
    let resp = post_overlay(
        app(state),
        json!({
            "plugin_id": "p1",
            "entity_kind": "wave",
            "entity_id": wave_id,
            "kind": "status",
            "payload": { "state": "running" }
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn post_progress_overlay_with_string_value_returns_400() {
    let (state, wave_id) = boot().await;
    let resp = post_overlay(
        app(state),
        json!({
            "plugin_id": "p1",
            "entity_kind": "wave",
            "entity_id": wave_id,
            "kind": "progress",
            "payload": { "value": "fast" }
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn post_unknown_overlay_kind_with_arbitrary_payload_returns_200() {
    // Plugin-defined overlay kinds remain opaque.
    let (state, wave_id) = boot().await;
    let resp = post_overlay(
        app(state),
        json!({
            "plugin_id": "p1",
            "entity_kind": "wave",
            "entity_id": wave_id,
            "kind": "my-plugin-badge",
            "payload": { "anything": [1, 2, 3], "nested": { "ok": true } }
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}
