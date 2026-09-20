//! `POST /api/tracks` threads `theme: { fg, bg }` through to the auto-minted planner card's terminal
//! renderer startup config, via the fixture-backed proc supervisor.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::NewArea;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::common;
struct Boot {
    app: axum::Router,
    area_id: String,
    _daemon_data_dir: PathBuf,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let area = repo
        .area_create(NewArea {
            name: "track-theme-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();

    let daemon_data_dir = tmp.path().to_path_buf();
    let daemon = Arc::new(DaemonClient {
        data_dir: daemon_data_dir.clone(),
        proc_supervisor_sock: None,
    });
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-track-theme-test"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                card_role_cache.clone(),
                track_area_cache.clone(),
            ),
        )),
        // Point `codex_bin` at the fake app-server fixture so the boot succeeds without a real codex on PATH.
        Arc::new(common::fake_codex_client()),
        Some(card_role_cache.clone()),
        Some(track_area_cache.clone()),
    );

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());

    Boot {
        app,
        area_id: area.id.to_string(),
        _daemon_data_dir: daemon_data_dir,
        _tmp: tmp,
    }
}

/// Returns `(status, json_or_null, raw_text)`. Axum's 422 from a serde-rejected `Json<T>` is `text/plain`,
/// not JSON, so the raw text is kept for the missing-theme substring match.
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

/// A track-create body without `theme` is rejected at the deserialize layer (422).
#[tokio::test]
async fn track_create_without_theme_is_rejected() {
    let boot = boot().await;

    // Body includes every other required field so the 422 fires on the missing `theme` and not some other field.
    let (status, _body, text) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "no theme track",
            "cwd": "/tmp/issue-250-pr2-test",
            "attach_folder": true,
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "track-create without theme must be rejected (422); got status={status}, body={text}"
    );
    assert!(
        text.contains("theme"),
        "422 must name `theme` as the rejected field (so a future \
         regression to `theme: Option<>` is caught); got body={text}"
    );
}
