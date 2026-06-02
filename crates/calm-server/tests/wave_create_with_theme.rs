//! Integration test (#177): `POST /api/waves` threads `theme: { fg, bg }`
//! through to the auto-minted spec card's terminal renderer startup
//! argv as `--terminal-fg=r,g,b --terminal-bg=r,g,b`.
//!
//! Pre-#177 the wave-create route auto-minted a spec card and spawned
//! its codex daemon via `spawn_terminal_for` (the no-opts shim that
//! ignored theme). That meant codex's OSC 10/11 startup probe got no
//! answer from the daemon, so the composer painted against codex's
//! built-in default and visually clashed with the surrounding card.
//! PR #193 had already fixed the user-created codex-card path but
//! missed the spec-card spawn — this test is the regression guard.
//!
//! Strategy: use the fixture-backed proc supervisor so the renderer
//! receives the terminal row's theme during EnsureProc. Fire the
//! wave-create POST with a `theme` body, wait for the renderer entry
//! to land, and assert its startup config carries the exact RGB.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::NewCove;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

mod common;
struct Boot {
    app: axum::Router,
    cove_id: String,
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
    let cove = repo
        .cove_create(NewCove {
            name: "wave-theme-test".into(),
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
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-wave-theme-test"),
            Vec::new(),
            EventBus::new(),
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        )),
        // #293 cutover — `POST /api/waves` now boots a kernel-owned codex
        // app-server before returning 201. Point `codex_bin` at the
        // `osc-probe-child` fake app-server fixture so the boot succeeds
        // without a real codex on PATH (see `tests/common/mod.rs`).
        Arc::new(common::fake_codex_client()),
        Some(card_role_cache.clone()),
        Some(wave_cove_cache.clone()),
    );

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());

    Boot {
        app,
        cove_id: cove.id.to_string(),
        _daemon_data_dir: daemon_data_dir,
        _tmp: tmp,
    }
}

/// Returns `(status, json_or_null, raw_text)`. Axum's 422 surface from a
/// serde-rejected `Json<T>` is `text/plain` (not JSON); we keep both
/// shapes so the missing-theme test below can substring-match against
/// the raw text while the happy-path test still drills into the JSON.
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

/// Happy path: wave-create body carries `theme: { fg, bg }` — the
/// spec card's renderer config must carry the dark-theme RGB the web
/// client stamps for a dark host browser.
/// Required-field gate (#177 followup): wave-create body without
/// `theme` must be rejected at the deserialize layer. Previously this
/// test asserted back-compat — the route silently fell back to "no
/// theme args, daemon stays silent on OSC 10/11". That fallback was
/// exactly the bug source: a web client that forgot to include theme
/// (e.g. `useTodayTerminal.ts:168` before #177's wave-create theme
/// thread-through) ended up with a mis-tinted composer with no signal
/// at any layer. Forcing the field at the API boundary means a missing
/// theme surfaces immediately as a 422 — the bug becomes a compile-
/// time / first-request failure instead of a visual artifact.
///
/// `serde` returns 422 (Unprocessable Entity) on `Json<NewWave>`
/// deserialize failures when a non-Option field is absent.
#[tokio::test]
async fn wave_create_without_theme_is_rejected() {
    let boot = boot().await;

    // Body includes every other required field (cwd, attach_folder); only
    // `theme` is missing. Without this guard the body's first missing
    // required field (e.g. `cwd`) could fire the 422 instead, leaving the
    // theme-required contract silently un-tested. The body-substring
    // assertion below pins `theme` as the rejected field.
    let (status, _body, text) = post(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": boot.cove_id,
            "title": "no theme wave",
            "cwd": "/tmp/issue-250-pr2-test",
            "attach_folder": true,
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "wave-create without theme must be rejected (422); got status={status}, body={text}"
    );
    assert!(
        text.contains("theme"),
        "422 must name `theme` as the rejected field (so a future \
         regression to `theme: Option<>` is caught); got body={text}"
    );
}
