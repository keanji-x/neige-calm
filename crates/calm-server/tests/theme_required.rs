//! Issue #177 (PR1 of split / closes #256) — `theme` is a required
//! field on every card-creation DTO and on `NewWave`. A body missing
//! `theme` is rejected at the deserialize step (422). This is the
//! root-cause defence against the "spawn without theme" bug observed in
//! PR #193: forcing the field at the type layer means a forgetful
//! caller fails at the request boundary instead of silently producing
//! a daemon that doesn't answer codex's OSC 10/11 probe.
//!
//! These tests are intentionally lightweight — they don't need a real
//! daemon, they assert the route's 422 surface BEFORE any DB or spawn
//! work runs. Boot uses a stub `CodexClient` and a non-existent daemon
//! path; the deserialize gate runs before we'd hit either.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewCove, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    cove_id: String,
    wave_id: String,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "theme-required-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "theme-required-test".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        session_daemon_bin: PathBuf::from("/nonexistent/calm-session-daemon"),
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
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::wave_cove_cache::WaveCoveCache::new(),
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
        cove_id: cove.id.to_string(),
        wave_id: wave.id.to_string(),
        _tmp: tmp,
    }
}

/// Returns `(status, json_or_null, raw_text)`. Axum's 422 surface from a
/// serde-rejected `Json<T>` is `text/plain` like
/// `"Failed to deserialize the JSON body into the target type: missing field 'theme' at line X column Y"` —
/// not JSON. We keep both shapes: the JSON value for happy-path tests
/// that want to drill into a structured response, and the raw text so
/// the missing-field substring assertion below can pin `theme` as the
/// rejected field even though the body itself isn't JSON.
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

/// `POST /api/waves` with a body that omits the `theme` field must
/// return 422 BEFORE any DB work happens. The 422 is the kernel's
/// fail-loud signal to clients that PR1-#177 made theme required end-
/// to-end. If this test starts succeeding (200 / 201) the type-layer
/// guard has regressed and the original "spawn-without-theme" bug
/// can resurface.
#[tokio::test]
async fn post_waves_without_theme_is_rejected_with_422() {
    let boot = boot().await;
    // Body includes every other required field (cwd, attach_folder) so
    // the 422 fires on the missing `theme` and not some other field. The
    // body-substring assertion pins `theme` as the rejected field — if
    // someone later turns `theme: Option<>`, this test starts failing
    // even if 422 still happens (for a different reason).
    let (status, _body, text) = post(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": boot.cove_id,
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

/// `POST /api/waves` with `theme: null` must also be rejected — JSON
/// `null` should NOT deserialize into `RequestTheme` (no `Option`,
/// no `#[serde(default)]`). Companion to the missing-field test
/// above; together they pin the root-cause defence.
#[tokio::test]
async fn post_waves_with_null_theme_is_rejected_with_422() {
    let boot = boot().await;
    // Body includes every other required field so the 422 fires on
    // `theme: null` and not a missing field. The body-substring assertion
    // pins `theme` as the rejected field.
    let (status, _body, text) = post(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": boot.cove_id,
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

/// `POST /api/waves/:wave_id/codex-cards` without theme must 422.
/// Codex cards are the primary user-facing card-create route — this
/// is the path the original bug shipped through (`useTodayTerminal.ts`
/// calling `createCodexCard({})`).
#[tokio::test]
async fn post_codex_cards_without_theme_is_rejected_with_422() {
    let boot = boot().await;
    let (status, _body, text) = post(
        boot.app.clone(),
        &format!("/api/waves/{}/codex-cards", boot.wave_id),
        json!({ "cwd": "/tmp" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "expected 422 on `codex-cards` body missing `theme`; body={text}",
    );
}

/// `POST /api/waves/:wave_id/terminal-cards` without theme must 422.
/// Plain terminal-card route — same fail-loud contract.
#[tokio::test]
async fn post_terminal_cards_without_theme_is_rejected_with_422() {
    let boot = boot().await;
    let (status, _body, text) = post(
        boot.app.clone(),
        &format!("/api/waves/{}/terminal-cards", boot.wave_id),
        json!({ "program": "/bin/sh" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "expected 422 on `terminal-cards` body missing `theme`; body={text}",
    );
}
