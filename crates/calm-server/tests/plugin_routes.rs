//! Integration tests for `/api/plugins/*` (Slice D).
//!
//! Boots a minimal Axum app with the plugins router + AppState (MockRepo,
//! EventBus, stub DaemonClient, real PluginHost rooted in a tempdir), then
//! drives the REST surface via an in-process HTTP client. We re-use the
//! `plugin-host-stub-echo` binary from Slice B as the spawnable plugin
//! payload — it answers `initialize` and idles until SIGTERM, which is all
//! the supervisor + the routes layer care about.
//!
//! What we cover (eight scenarios per Slice D's binding spec):
//!
//!   1. install + list flow
//!   2. enable spawns the process
//!   3. disable stops the process
//!   4. log endpoint returns stderr
//!   5. uninstall cascades tokens / kv / overlays
//!   6. views catalog reflects the installed manifest
//!   7. install rejects manifest with disallowed `scope`
//!   8. install rejects reinstall with 409

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::NewOverlay;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::{Instant, sleep};
use tower::ServiceExt;

const ECHO_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-echo");

// ---------------------------------------------------------------------------
// Test fixture: build a plugin directory on disk containing a valid manifest
// and a symlink to the echo stub binary at `bin/stub`. The manifest carries
// one view so the views-catalog test has a non-empty payload to assert on.
// ---------------------------------------------------------------------------

fn write_stub_plugin(plugins_dir: &Path, id: &str) -> PathBuf {
    let plugin_dir = plugins_dir.join(id);
    let bin_dir = plugin_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::os::unix::fs::symlink(Path::new(ECHO_BIN), bin_dir.join("stub")).unwrap();
    let manifest = json!({
        "manifest_version": 1,
        "id": id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "Echo Stub",
        "description": "test fixture",
        "entrypoint": { "command": "bin/stub" },
        "views": [
            {
                "view_id": "main",
                "title": "Echo View",
                "scope": "card",
                "default_size": { "w": 4, "h": 3 }
            }
        ],
        "permissions": {
            "overlays_write": ["wave", "card"],
            "cards_create": true,
            "kv_quota_bytes": 65536
        }
    });
    std::fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    plugin_dir
}

fn write_bad_scope_plugin(plugins_dir: &Path, id: &str) -> PathBuf {
    let plugin_dir = plugins_dir.join(id);
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let manifest = json!({
        "manifest_version": 1,
        "id": id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "Bad Scope",
        "entrypoint": { "command": "bin/stub" },
        "views": [
            { "view_id": "wide", "title": "Wide", "scope": "wave" }
        ]
    });
    std::fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    plugin_dir
}

/// Build an `AppState` rooted in a fresh tempdir, an empty `PluginRegistry`,
/// and an in-memory `SqlxRepo`. Returns the state, a holding TempDir (drops
/// cleanup), and the resolved `plugins_dir` so tests can drop fixtures into it.
async fn boot_state() -> (AppState, TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite repo"),
    );
    let events = EventBus::new();
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        repo.clone(),
        plugins_dir.clone(),
        plugins_data_dir,
        Vec::new(),
        events.clone(),
    ));
    let state = AppState::from_parts(
        repo,
        events,
        Arc::new(DaemonClient::new_stub()),
        plugin,
        Arc::new(calm_server::state::CodexClient::new_stub()),
    );
    (state, tmp, plugins_dir)
}

fn app(state: AppState) -> axum::Router {
    axum::Router::new()
        .merge(routes::plugins::router())
        .with_state(state)
}

async fn body_to_json(resp: axum::http::Response<Body>) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn body_to_text(resp: axum::http::Response<Body>) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

async fn post_json(app: axum::Router, path: &str, body: Value) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn get_path(app: axum::Router, path: &str) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("GET")
            .uri(path)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn delete_path(app: axum::Router, path: &str) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("DELETE")
            .uri(path)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

/// Poll `GET /api/plugins/:id` until `state` matches `expected` (wire string)
/// or `deadline` is exceeded. Returns the final detail JSON so the caller can
/// assert further.
async fn wait_for_state(state: &AppState, id: &str, expected: &str, timeout: Duration) -> Value {
    let start = Instant::now();
    loop {
        let resp = get_path(app(state.clone()), &format!("/api/plugins/{id}")).await;
        let json = body_to_json(resp).await;
        if json.get("state").and_then(|v| v.as_str()) == Some(expected) {
            return json;
        }
        if start.elapsed() > timeout {
            panic!(
                "timeout waiting for state `{expected}` (got {:?}, elapsed {:?})",
                json.get("state"),
                start.elapsed()
            );
        }
        sleep(Duration::from_millis(50)).await;
    }
}

// ===========================================================================
// 1. install + list flow
// ===========================================================================

#[tokio::test]
async fn install_lists_and_details_round_trip() {
    let (state, _tmp, plugins_dir) = boot_state().await;
    // Source path lives OUTSIDE plugins_dir so install must materialize a
    // copy/link into plugins_dir/<id> — the realistic flow.
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.install");

    // POST install.
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({
            "source": { "kind": "local_path", "path": src_dir.to_string_lossy() }
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED, "install should 201");
    let body = body_to_json(resp).await;
    assert_eq!(body["id"], "test.install");
    assert_eq!(body["enabled"], false);
    assert_eq!(body["state"], "disabled");

    // The install path the host knows about should now exist as a symlink
    // (unix) or directory (windows) under plugins_dir.
    assert!(
        plugins_dir.join("test.install").exists(),
        "plugins_dir entry should exist"
    );

    // GET list.
    let resp = get_path(app(state.clone()), "/api/plugins").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list = body_to_json(resp).await;
    let arr = list.as_array().expect("list should be array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], "test.install");
    assert_eq!(arr[0]["manifest_name"], "Echo Stub");

    // GET detail.
    let resp = get_path(app(state.clone()), "/api/plugins/test.install").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let det = body_to_json(resp).await;
    assert_eq!(det["id"], "test.install");
    assert!(det["manifest"]["views"].is_array());
}

// ===========================================================================
// 2. enable spawns the process
// ===========================================================================

#[tokio::test]
async fn enable_transitions_to_running() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.enable");

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/test.enable/enable",
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "enable should 200");
    let det = body_to_json(resp).await;
    assert_eq!(det["enabled"], true);

    // The state can be `spawning` momentarily; poll until `running`.
    let det = wait_for_state(&state, "test.enable", "running", Duration::from_secs(3)).await;
    assert_eq!(det["enabled"], true);

    // Cleanup.
    let _ = post_json(
        app(state.clone()),
        "/api/plugins/test.enable/disable",
        json!({}),
    )
    .await;
}

// ===========================================================================
// 3. disable stops the process
// ===========================================================================

#[tokio::test]
async fn disable_transitions_to_disabled() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.disable");
    post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;
    post_json(
        app(state.clone()),
        "/api/plugins/test.disable/enable",
        json!({}),
    )
    .await;
    wait_for_state(&state, "test.disable", "running", Duration::from_secs(3)).await;

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/test.disable/disable",
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let det = body_to_json(resp).await;
    assert_eq!(det["enabled"], false);
    assert_eq!(det["state"], "disabled");
}

// ===========================================================================
// 4. log endpoint returns stderr from the running stub
// ===========================================================================

#[tokio::test]
async fn log_tail_returns_stub_stderr() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.log");
    post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;
    post_json(
        app(state.clone()),
        "/api/plugins/test.log/enable",
        json!({}),
    )
    .await;
    wait_for_state(&state, "test.log", "running", Duration::from_secs(3)).await;

    // Stub writes a startup line to stderr; the ring should pick it up.
    let resp = get_path(app(state.clone()), "/api/plugins/test.log/log?n=10").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let lines = body_to_json(resp).await;
    let arr = lines.as_array().expect("array");
    assert!(
        arr.iter()
            .any(|s| s.as_str().unwrap_or("").contains("stub-echo")),
        "expected stderr to contain stub line, got {:?}",
        arr
    );

    let _ = post_json(
        app(state.clone()),
        "/api/plugins/test.log/disable",
        json!({}),
    )
    .await;
}

// ===========================================================================
// 5. uninstall cascades tokens / kv / overlays
// ===========================================================================

#[tokio::test]
async fn uninstall_cascades_satellites() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.uninstall");
    post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;

    // Seed satellite data so we can verify the cascade.
    state
        .repo
        .plugin_kv_set("test.uninstall", "foo", &json!("bar"))
        .await
        .unwrap();
    state
        .raw_repo()
        .overlay_upsert(NewOverlay {
            plugin_id: "test.uninstall".into(),
            entity_kind: "wave".into(),
            entity_id: "w1".into(),
            kind: "status".into(),
            payload: json!({"x": 1}),
        })
        .await
        .unwrap();

    let resp = delete_path(app(state.clone()), "/api/plugins/test.uninstall").await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Subsequent GET should 404.
    let resp = get_path(app(state.clone()), "/api/plugins/test.uninstall").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Token, kv, overlays — all gone.
    assert!(
        state
            .repo
            .plugin_token_get("test.uninstall")
            .await
            .unwrap()
            .is_none()
    );
    let kv = state
        .repo
        .plugin_kv_list("test.uninstall", "")
        .await
        .unwrap();
    assert!(kv.is_empty(), "kv should be empty after uninstall");
    let overlays = state.repo.overlays_for("wave", "w1").await.unwrap();
    assert!(
        overlays.is_empty(),
        "overlays should be cleared on uninstall"
    );
}

// ===========================================================================
// 6. views catalog reflects the installed + enabled manifest
// ===========================================================================

#[tokio::test]
async fn views_catalog_lists_enabled_plugin_views() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.views");

    // Before install: empty catalog.
    let resp = get_path(app(state.clone()), "/api/plugins/views").await;
    let arr = body_to_json(resp).await;
    assert!(arr.as_array().unwrap().is_empty());

    post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;

    // Disabled plugin: still empty (only enabled plugins surface views).
    let resp = get_path(app(state.clone()), "/api/plugins/views").await;
    let arr = body_to_json(resp).await;
    assert!(
        arr.as_array().unwrap().is_empty(),
        "disabled plugin should not surface views"
    );

    post_json(
        app(state.clone()),
        "/api/plugins/test.views/enable",
        json!({}),
    )
    .await;
    wait_for_state(&state, "test.views", "running", Duration::from_secs(3)).await;

    let resp = get_path(app(state.clone()), "/api/plugins/views").await;
    let arr = body_to_json(resp).await;
    let entries = arr.as_array().expect("array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["plugin_id"], "test.views");
    assert_eq!(entries[0]["view_id"], "main");
    // M3 (mcp-apps migration): `resource_uri` is the canonical ui:// form.
    // `plugin_id`/`view_id` stay alongside it during the M3→M4 transition.
    assert_eq!(entries[0]["resource_uri"], "ui://test.views/main");
    assert_eq!(entries[0]["scope"], "card");
    assert_eq!(entries[0]["default_size"]["w"], 4);

    let _ = post_json(
        app(state.clone()),
        "/api/plugins/test.views/disable",
        json!({}),
    )
    .await;
}

// ===========================================================================
// 7. install rejects manifest with disallowed `scope: "wave"`
// ===========================================================================

#[tokio::test]
async fn install_rejects_wave_scope_manifest() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_bad_scope_plugin(src_root.path(), "test.badscope");

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_to_text(resp).await;
    assert!(
        body.contains("scope") || body.contains("wave"),
        "error should mention scope/wave, got {body}"
    );
}

// ===========================================================================
// 8. install rejects reinstall with 409
// ===========================================================================

#[tokio::test]
async fn install_twice_returns_409() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.dup");

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = body_to_text(resp).await;
    assert!(body.contains("already installed"), "got: {body}");
}

// ===========================================================================
// Bonus: install rejects unsupported source kind with 400.
// ===========================================================================

#[tokio::test]
async fn install_rejects_unsupported_source() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let resp = post_json(
        app(state),
        "/api/plugins/install",
        json!({ "source": { "kind": "tarball", "url": "https://example.com/x.tar" } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ===========================================================================
// Bonus: PATCH config writes user_config.
// ===========================================================================

#[tokio::test]
async fn patch_config_writes_user_config() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.config");
    post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;

    let app_in = app(state.clone());
    let resp = app_in
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/plugins/test.config/config")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "theme": "dark" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let det = body_to_json(resp).await;
    assert_eq!(det["user_config"]["theme"], "dark");
}

// The pre-M5 `iframe_write_without_cookie_returns_401` test exercised the
// `iframe-write` REST surface, which was deleted in M5 alongside the cookie
// cache (see migration doc §3.3). M5's replacement gate is the `neige.*`
// prefix check on `POST /api/plugins/:id/tool-call`, covered by
// `plugin_routes_m5.rs::tool_call_rejects_non_neige_namespace`.
