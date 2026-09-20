//! `theme` is a required field on every card-creation DTO and on `NewTrack`; a body missing it is
//! rejected at the deserialize step (422), before any DB or spawn work.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewArea, NewTrack};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    area_id: String,
    track_id: String,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let area = repo
        .area_create(NewArea {
            name: "theme-required-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "theme-required-test".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events,
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-theme-required"),
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
    );

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    Boot {
        app,
        area_id: area.id.to_string(),
        track_id: track.id.to_string(),
        _tmp: tmp,
    }
}

/// Returns `(status, json_or_null, raw_text)`. Axum's 422 from a serde-rejected `Json<T>` is
/// `text/plain`, not JSON, so the raw text is kept for the missing-field substring assertion.
async fn post(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value, String) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json, text)
}

#[tokio::test]
async fn post_tracks_without_theme_is_rejected_with_422() {
    let boot = boot().await;
    // Body includes every other required field so the 422 fires on the missing `theme` and not some other field.
    let (status, _body, text) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "no theme here",
            "cwd": "/tmp/issue-177-pr1-test",
            "attach_folder": true,
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "expected 422 on missing `theme` field; body={text}",
    );
    assert!(
        text.contains("theme"),
        "422 must name `theme` as the rejected field; got body={text}",
    );
}

/// JSON `null` must NOT deserialize into `RequestTheme` (no `Option`, no `#[serde(default)]`).
#[tokio::test]
async fn post_tracks_with_null_theme_is_rejected_with_422() {
    let boot = boot().await;
    // Body includes every other required field so the 422 fires on `theme: null` and not a missing field.
    let (status, _body, text) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "null theme",
            "cwd": "/tmp/issue-177-pr1-test",
            "attach_folder": true,
            "theme": Value::Null,
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "expected 422 on `theme: null`; body={text}",
    );
    assert!(
        text.contains("theme"),
        "422 must name `theme` as the rejected field; got body={text}",
    );
}

#[tokio::test]
async fn post_codex_cards_without_theme_is_rejected_with_422() {
    let boot = boot().await;
    let (status, _body, text) = post(
        boot.app.clone(),
        &format!("/api/tracks/{}/codex-cards", boot.track_id),
        json!({ "cwd": "/tmp" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "expected 422 on `codex-cards` body missing `theme`; body={text}",
    );
}

#[tokio::test]
async fn post_terminal_cards_without_theme_is_rejected_with_422() {
    let boot = boot().await;
    let (status, _body, text) = post(
        boot.app.clone(),
        &format!("/api/tracks/{}/terminal-cards", boot.track_id),
        json!({ "program": "/bin/sh" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "expected 422 on `terminal-cards` body missing `theme`; body={text}",
    );
}
