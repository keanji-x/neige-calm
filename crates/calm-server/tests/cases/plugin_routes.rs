//! Integration tests for `/api/plugins/*`: a minimal Axum app over a real
//! `PluginHost`, with `plugin-host-stub-echo` as the spawnable payload.

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
            "overlays_write": ["track", "card"],
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
            { "view_id": "wide", "title": "Wide", "scope": "track" }
        ]
    });
    std::fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    plugin_dir
}

async fn boot_state() -> (AppState, TempDir, PathBuf) {
    let (state, tmp, plugins_dir, _repo) = boot_state_with_repo().await;
    (state, tmp, plugins_dir)
}

/// `boot_state`, plus the `Repo` handle, for tests that must put the DB into a state no route can produce.
async fn boot_state_with_repo() -> (AppState, TempDir, PathBuf, Arc<dyn Repo>) {
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
        calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
        ),
    ));
    let state = AppState::from_parts(
        repo.clone(),
        events,
        Arc::new(DaemonClient::new_stub()),
        plugin,
        Arc::new(calm_server::state::CodexClient::new_stub()),
        None,
        None,
    );
    (state, tmp, plugins_dir, repo)
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

#[tokio::test]
async fn install_lists_and_details_round_trip() {
    let (state, _tmp, plugins_dir) = boot_state().await;
    // Source path lives outside plugins_dir so install must materialize a copy/link into plugins_dir/<id>.
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.install");

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

    assert!(
        plugins_dir.join("test.install").exists(),
        "plugins_dir entry should exist"
    );

    let resp = get_path(app(state.clone()), "/api/plugins").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list = body_to_json(resp).await;
    let arr = list.as_array().expect("list should be array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], "test.install");
    assert_eq!(arr[0]["manifest_name"], "Echo Stub");

    let resp = get_path(app(state.clone()), "/api/plugins/test.install").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let det = body_to_json(resp).await;
    assert_eq!(det["id"], "test.install");
    assert!(det["manifest"]["views"].is_array());
}

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

    let _ = post_json(
        app(state.clone()),
        "/api/plugins/test.enable/disable",
        json!({}),
    )
    .await;
}

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

    state
        .repo
        .plugin_kv_set("test.uninstall", "foo", &json!("bar"))
        .await
        .unwrap();
    state
        .raw_repo()
        .overlay_upsert(NewOverlay {
            plugin_id: "test.uninstall".into(),
            entity_kind: "track".into(),
            entity_id: "w1".into(),
            kind: "status".into(),
            payload: json!({"x": 1}),
        })
        .await
        .unwrap();

    let resp = delete_path(app(state.clone()), "/api/plugins/test.uninstall").await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = get_path(app(state.clone()), "/api/plugins/test.uninstall").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

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
    let overlays = state.repo.overlays_for("track", "w1").await.unwrap();
    assert!(
        overlays.is_empty(),
        "overlays should be cleared on uninstall"
    );
}

#[tokio::test]
async fn views_catalog_lists_enabled_plugin_views() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.views");

    let resp = get_path(app(state.clone()), "/api/plugins/views").await;
    let arr = body_to_json(resp).await;
    assert!(arr.as_array().unwrap().is_empty());

    post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;

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
    assert_eq!(entries[0]["resource_uri"], "ui://test.views/main");
    assert_eq!(entries[0]["scope"], "card");
    assert_eq!(entries[0]["default_size"]["w"], 4);
    assert!(entries[0].get("plugin_id").is_none());
    assert!(entries[0].get("view_id").is_none());

    let _ = post_json(
        app(state.clone()),
        "/api/plugins/test.views/disable",
        json!({}),
    )
    .await;
}

#[tokio::test]
async fn install_rejects_track_scope_manifest() {
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
        body.contains("scope") || body.contains("track"),
        "error should mention scope/track, got {body}"
    );
}

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

/// All keys optional (legal at `manifest_version: 2`), one with a `default` so the read-time merge has something to do.
fn stub_config_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "theme": { "type": "string", "enum": ["dark", "light"], "default": "dark" },
            "retries": { "type": "integer" },
            "label": { "type": "string" }
        },
        "additionalProperties": false
    })
}

fn write_stub_plugin_with_config(plugins_dir: &Path, id: &str, config_schema: Value) -> PathBuf {
    let plugin_dir = write_stub_plugin(plugins_dir, id);
    let path = plugin_dir.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    manifest["config_schema"] = config_schema;
    std::fs::write(&path, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
    plugin_dir
}

async fn install(state: &AppState, src_dir: &Path) {
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "install failed: {}",
        body_to_text(resp).await
    );
}

async fn patch_config(state: &AppState, id: &str, body: Value) -> axum::http::Response<Body> {
    patch_config_query(state, id, "", body).await
}

/// Same, with a raw query string (`"?reset=true"`).
async fn patch_config_query(
    state: &AppState,
    id: &str,
    query: &str,
    body: Value,
) -> axum::http::Response<Body> {
    app(state.clone())
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/plugins/{id}/config{query}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn patch_config_writes_user_config() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir =
        write_stub_plugin_with_config(src_root.path(), "test.config", stub_config_schema());
    install(&state, &src_dir).await;

    let resp = patch_config(&state, "test.config", json!({ "theme": "light" })).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let det = body_to_json(resp).await;
    assert_eq!(det["user_config"]["theme"], "light");
    assert_eq!(det["effective_config"]["theme"], "light");
}

#[tokio::test]
async fn patch_config_on_a_plugin_without_a_schema_is_400() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.noschema");
    install(&state, &src_dir).await;

    let resp = patch_config(&state, "test.noschema", json!({ "theme": "dark" })).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_to_text(resp).await;
    assert!(body.contains("config_schema"), "got: {body}");

    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/test.noschema").await).await;
    assert_eq!(det["user_config"], json!({}), "got {det:?}");
}

#[tokio::test]
async fn patch_config_unknown_id_is_still_404() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let resp = patch_config(&state, GHOST, json!({ "theme": "dark" })).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn patch_config_leaves_absent_keys_alone_and_deletes_on_explicit_null() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir =
        write_stub_plugin_with_config(src_root.path(), "test.patch", stub_config_schema());
    install(&state, &src_dir).await;

    let det = body_to_json(
        patch_config(
            &state,
            "test.patch",
            json!({ "theme": "light", "label": "a" }),
        )
        .await,
    )
    .await;
    assert_eq!(
        det["user_config"],
        json!({ "theme": "light", "label": "a" })
    );

    let det = body_to_json(patch_config(&state, "test.patch", json!({ "label": "b" })).await).await;
    assert_eq!(
        det["user_config"],
        json!({ "theme": "light", "label": "b" }),
        "an absent key must keep its stored value, not be dropped"
    );

    let det =
        body_to_json(patch_config(&state, "test.patch", json!({ "theme": null })).await).await;
    assert_eq!(det["user_config"], json!({ "label": "b" }));
    assert_eq!(
        det["effective_config"]["theme"], "dark",
        "a cleared key falls back to its default, not to absent"
    );

    let det =
        body_to_json(patch_config(&state, "test.patch", json!({ "label": null })).await).await;
    assert_eq!(det["user_config"], json!({}));
    assert!(
        det["effective_config"].get("label").is_none(),
        "got {:?}",
        det["effective_config"]
    );
}

#[tokio::test]
async fn patch_config_rejects_values_that_violate_the_schema() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin_with_config(src_root.path(), "test.bad", stub_config_schema());
    install(&state, &src_dir).await;

    let cases = [
        (
            "wrong type",
            json!({ "retries": "three" }),
            "config.retries",
        ),
        ("outside enum", json!({ "theme": "neon" }), "config.theme"),
        ("undeclared key", json!({ "ghost": "x" }), "config.ghost"),
        (
            "oversized value",
            json!({ "label": "x".repeat(9000) }),
            "config",
        ),
    ];
    for (label, body, expected_path) in cases {
        let resp = patch_config(&state, "test.bad", body).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{label}");
        let text = body_to_text(resp).await;
        assert!(
            text.contains(expected_path),
            "{label}: expected `{expected_path}` in the error, got: {text}"
        );
        // Errors must name the CONFIG field, never the track-input one whose validator this reuses.
        assert!(
            !text.contains("template_input") && !text.contains("input_schema"),
            "{label}: error leaked the other root path: {text}"
        );
    }

    let resp = patch_config(
        &state,
        "test.bad",
        json!({ "retries": 3, "theme": "light" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = patch_config(&state, "test.bad", json!({ "theme": "neon" })).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/test.bad").await).await;
    assert_eq!(det["user_config"]["theme"], "light");
}

/// Two patches of ~5000 bytes on different keys: each is under 8192, their merge is not.
#[tokio::test]
async fn the_byte_cap_is_measured_on_the_merged_config_not_the_request_body() {
    let (state, _tmp, _plugins_dir, repo) = boot_state_with_repo().await;
    let src_root = tempfile::tempdir().unwrap();
    let two_strings = json!({
        "type": "object",
        "properties": {
            "a": { "type": "string" },
            "b": { "type": "string" }
        },
        "additionalProperties": false
    });
    let src_dir = write_stub_plugin_with_config(src_root.path(), "test.cap", two_strings);
    install(&state, &src_dir).await;

    let chunk = "x".repeat(5000);

    let resp = patch_config(&state, "test.cap", json!({ "a": chunk })).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "5000 bytes is under the cap: {}",
        body_to_text(resp).await
    );

    let resp = patch_config(&state, "test.cap", json!({ "b": chunk.clone() })).await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "the cap is on the merged storage state, and 5000 + 5000 > 8192"
    );
    let text = body_to_text(resp).await;
    assert!(text.contains("8192"), "got: {text}");

    let row = repo.plugin_get_by_id("test.cap").await.unwrap().unwrap();
    assert_eq!(row.user_config.as_object().unwrap().len(), 1);

    // Reverse: a row already over the cap (only a direct write can produce one) still accepts a patch that shrinks it.
    repo.plugin_update_user_config(
        "test.cap",
        json!({ "a": chunk.clone(), "b": chunk.clone() }),
    )
    .await
    .unwrap();
    let resp = patch_config(&state, "test.cap", json!({ "b": "small" })).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "an oversized row must be shrinkable through the API: {}",
        body_to_text(resp).await
    );
    let det = body_to_json(resp).await;
    assert_eq!(det["user_config"]["b"], "small");
    assert_eq!(
        det["user_config"]["a"], chunk,
        "and the untouched key kept its value"
    );

    let det = body_to_json(patch_config(&state, "test.cap", json!({ "a": null })).await).await;
    assert_eq!(det["user_config"], json!({ "b": "small" }));
}

/// The schema needs `manifest_version: 3`, the only place this suite exercises that version end to end through install.
#[tokio::test]
async fn patch_config_does_not_enforce_required_keys() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin_with_config(
        src_root.path(),
        "test.required",
        json!({
            "type": "object",
            "properties": {
                "token": { "type": "string" },
                "secondary": { "type": "string" },
                "region": { "type": "string", "default": "eu" }
            },
            "required": ["token", "secondary", "region"],
            "additionalProperties": false
        }),
    );
    let path = src_dir.join("manifest.json");
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    m["manifest_version"] = json!(3);
    std::fs::write(&path, serde_json::to_string_pretty(&m).unwrap()).unwrap();
    install(&state, &src_dir).await;

    let resp = patch_config(&state, "test.required", json!({ "token": "t" })).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a partial Save must not be refused for the keys it deliberately omits"
    );
    let det = body_to_json(resp).await;
    assert_eq!(
        det["user_config"],
        json!({ "token": "t" }),
        "no default stored"
    );
    assert_eq!(det["effective_config"]["region"], "eu");

    let det =
        body_to_json(patch_config(&state, "test.required", json!({ "secondary": "s" })).await)
            .await;
    assert_eq!(
        det["user_config"],
        json!({ "token": "t", "secondary": "s" })
    );

    let resp = patch_config(&state, "test.required", json!({ "token": null })).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "required is enforced at bring-up, not here"
    );
    let det = body_to_json(resp).await;
    assert_eq!(det["user_config"], json!({ "secondary": "s" }));
    assert!(
        det["effective_config"].get("token").is_none(),
        "and it really is gone from what would be in force: {det:?}"
    );

    let resp = patch_config(&state, "test.required", json!({ "token": 7 })).await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "types are still enforced"
    );
}

#[tokio::test]
async fn patch_config_judges_key_names_before_null_means_delete() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir =
        write_stub_plugin_with_config(src_root.path(), "test.ghostnull", stub_config_schema());
    install(&state, &src_dir).await;

    let resp = patch_config(&state, "test.ghostnull", json!({ "ghost": null })).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let text = body_to_text(resp).await;
    assert!(text.contains("config.ghost"), "got: {text}");

    let resp = patch_config(
        &state,
        "test.ghostnull",
        json!({ "label": "keep", "ghost": null }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/test.ghostnull").await).await;
    assert_eq!(det["user_config"], json!({}), "nothing was written");

    let det =
        body_to_json(patch_config(&state, "test.ghostnull", json!({ "label": "x" })).await).await;
    assert_eq!(det["user_config"], json!({ "label": "x" }));
    let det =
        body_to_json(patch_config(&state, "test.ghostnull", json!({ "label": null })).await).await;
    assert_eq!(det["user_config"], json!({}));
}

#[tokio::test]
async fn a_stored_key_the_schema_no_longer_declares_does_not_lock_the_operator_out() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let wide = json!({
        "type": "object",
        "properties": {
            "keep": { "type": "string" },
            "old": { "type": "string" }
        },
        "additionalProperties": false
    });
    let src_dir = write_stub_plugin_with_config(src_root.path(), "test.narrow", wide);
    install(&state, &src_dir).await;

    let det = body_to_json(
        patch_config(
            &state,
            "test.narrow",
            json!({ "keep": "a", "old": "residue" }),
        )
        .await,
    )
    .await;
    assert_eq!(det["user_config"], json!({ "keep": "a", "old": "residue" }));

    // Reload is how a new manifest reaches the kernel; it rewrites both the registry and the published blob.
    let path = src_dir.join("manifest.json");
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    m["config_schema"] = json!({
        "type": "object",
        "properties": { "keep": { "type": "string" } },
        "additionalProperties": false
    });
    std::fs::write(&path, serde_json::to_string_pretty(&m).unwrap()).unwrap();
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/test.narrow/reload",
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "reload failed");

    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/test.narrow").await).await;
    assert_eq!(det["user_config"]["old"], "residue");
    assert!(
        det["effective_config"].get("old").is_none(),
        "…but nothing runs with it: {det:?}"
    );

    let resp = patch_config(&state, "test.narrow", json!({ "keep": "b" })).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "an invisible key must not reject a legal request: {}",
        body_to_text(resp).await
    );
    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/test.narrow").await).await;
    assert_eq!(
        det["user_config"],
        json!({ "keep": "b", "old": "residue" }),
        "the write unlocked, and it did NOT delete the key the operator never touched"
    );
    assert!(
        det["effective_config"].get("old").is_none(),
        "…and the survivor still runs with nothing: {det:?}"
    );

    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    m["config_schema"] = json!({
        "type": "object",
        "properties": {
            "keep": { "type": "string" },
            "old": { "type": "string" }
        },
        "additionalProperties": false
    });
    std::fs::write(&path, serde_json::to_string_pretty(&m).unwrap()).unwrap();
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/test.narrow/reload",
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "re-widening reload failed");
    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/test.narrow").await).await;
    assert_eq!(
        det["effective_config"]["old"], "residue",
        "the operator's value survived the narrow/widen round trip: {det:?}"
    );

    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    m["config_schema"] = json!({
        "type": "object",
        "properties": { "keep": { "type": "string" } },
        "additionalProperties": false
    });
    std::fs::write(&path, serde_json::to_string_pretty(&m).unwrap()).unwrap();
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/test.narrow/reload",
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "re-narrowing reload failed");

    let resp = patch_config(&state, "test.narrow", json!({ "old": "again" })).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_to_text(resp).await.contains("config.old"),
        "the pruned key may not be re-set"
    );
}

/// A plugin installed by an older kernel has a `plugins.manifest` blob with no `config_schema` key
/// (serde dropped it); the registry, which boot rebuilds from disk, has the schema all along.
#[tokio::test]
async fn a_manifest_blob_written_by_an_older_kernel_still_has_a_config_surface() {
    let (state, _tmp, _plugins_dir, repo) = boot_state_with_repo().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir =
        write_stub_plugin_with_config(src_root.path(), "test.upgrade", stub_config_schema());
    install(&state, &src_dir).await;

    // Rewrite the persisted blob to what an older kernel would have stored.
    let row = repo
        .plugin_get_by_id("test.upgrade")
        .await
        .unwrap()
        .unwrap();
    let mut blob = row.manifest.clone();
    assert!(
        blob.as_object_mut()
            .unwrap()
            .remove("config_schema")
            .is_some(),
        "fixture precondition: the blob carried the schema"
    );
    repo.plugin_update_manifest("test.upgrade", blob)
        .await
        .unwrap();

    let rows = body_to_json(get_path(app(state.clone()), "/api/plugins").await).await;
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "test.upgrade")
        .unwrap()
        .clone();
    assert_eq!(row["has_config"], json!(true), "got {row:?}");

    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/test.upgrade").await).await;
    assert_eq!(det["effective_config"], json!({ "theme": "dark" }));
    assert!(
        det["manifest"].get("config_schema").is_none(),
        "and the published blob really is the old one"
    );

    assert_eq!(
        det["config_schema"],
        stub_config_schema(),
        "the form's schema comes from the registry, like every other config \
         answer in this response: {det:?}"
    );

    let resp = patch_config(&state, "test.upgrade", json!({ "theme": "light" })).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "an upgraded install must not need a manual reload: {}",
        body_to_text(resp).await
    );
    let resp = patch_config(&state, "test.upgrade", json!({ "theme": "neon" })).await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "…and it is the registry's schema doing the validating"
    );
}

/// The row can exist while the registry does not hold the manifest: during install (row before
/// registry insert) and, durably, when `manifest.json` fails to parse at boot.
#[tokio::test]
async fn a_row_whose_manifest_is_not_in_the_registry_is_refused_explicitly() {
    use axum::extract::FromRef;

    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin_with_config(src_root.path(), "test.gap", stub_config_schema());
    install(&state, &src_dir).await;

    // Reproduce the window: drop the registry entry, keep the row.
    let cs = calm_server::state::CodexShellState::from_ref(&state);
    let guard = cs.plugin.try_lock_lifecycle("test.gap").expect("lock free");
    assert!(cs.plugin.registry_remove(&guard).is_some());
    drop(guard);

    let resp = patch_config(&state, "test.gap", json!({ "theme": "light" })).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = body_to_json(resp).await;
    assert_eq!(
        body["code"], "plugin_manifest_unloaded",
        "409s are told apart by code: {body}"
    );
    let text = body.to_string();
    assert!(
        !text.contains("declares no"),
        "must not read as the permanent 'no schema' refusal: {text}"
    );
    // A `manifest.json` that fails to parse makes "reload the plugin" fail again, so the message has to name the other action too.
    assert!(
        text.contains("manifest.json"),
        "the durable cause needs naming, not just the transient one: {text}"
    );

    let rows = body_to_json(get_path(app(state.clone()), "/api/plugins").await).await;
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "test.gap")
        .unwrap()
        .clone();
    assert_eq!(row["has_config"], json!(false), "got {row:?}");
    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/test.gap").await).await;
    assert_eq!(det["effective_config"], json!({}));
    assert!(
        det.get("config_schema").is_none(),
        "and no schema is published either — the three config answers agree: {det:?}"
    );
    assert_eq!(det["user_config"], json!({}), "and nothing was written");
}

#[tokio::test]
async fn a_registry_gap_answers_409_even_for_a_plugin_that_declares_no_schema() {
    use axum::extract::FromRef;

    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.gapnoschema");
    install(&state, &src_dir).await;

    let resp = patch_config(&state, "test.gapnoschema", json!({ "x": 1 })).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let cs = calm_server::state::CodexShellState::from_ref(&state);
    let guard = cs
        .plugin
        .try_lock_lifecycle("test.gapnoschema")
        .expect("lock free");
    assert!(cs.plugin.registry_remove(&guard).is_some());
    drop(guard);

    let resp = patch_config(&state, "test.gapnoschema", json!({ "x": 1 })).await;
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "the state gate has to run before the schema gate"
    );
    let body = body_to_json(resp).await;
    assert_eq!(body["code"], "plugin_manifest_unloaded", "got {body}");

    let resp = patch_config(&state, GHOST, json!({ "x": 1 })).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn patch_config_refuses_to_overwrite_a_non_object_user_config() {
    let (state, _tmp, _plugins_dir, repo) = boot_state_with_repo().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir =
        write_stub_plugin_with_config(src_root.path(), "test.corrupt", stub_config_schema());
    install(&state, &src_dir).await;
    repo.plugin_update_user_config("test.corrupt", json!("theme=light"))
        .await
        .unwrap();

    let resp = patch_config(&state, "test.corrupt", json!({ "theme": "light" })).await;
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "a corrupt row is a state the operator can fix, not a server fault"
    );
    let body = body_to_json(resp).await;
    assert_eq!(
        body["code"], "plugin_config_corrupt",
        "the distinction lives in the code, not the prose: {body}"
    );
    let text = body.to_string();
    assert!(text.contains("not a JSON object"), "got: {text}");
    assert!(
        text.contains("reset=true"),
        "the refusal must name the recovery action: {text}"
    );
    assert!(
        !text.contains("theme=light"),
        "and it must not echo the corrupt value back: {text}"
    );

    let row = repo
        .plugin_get_by_id("test.corrupt")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.user_config, json!("theme=light"));

    let resp = patch_config_query(
        &state,
        "test.corrupt",
        "?reset=true",
        json!({ "theme": "light" }),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "the named recovery action must actually recover: {}",
        body_to_text(resp).await
    );
    let det = body_to_json(resp).await;
    assert_eq!(det["user_config"], json!({ "theme": "light" }));

    let resp = patch_config(&state, "test.corrupt", json!({ "label": "x" })).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let det = body_to_json(resp).await;
    assert_eq!(
        det["user_config"],
        json!({ "theme": "light", "label": "x" })
    );
}

#[tokio::test]
async fn reset_is_destructive_only_when_the_operator_asks_for_it() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir =
        write_stub_plugin_with_config(src_root.path(), "test.reset", stub_config_schema());
    install(&state, &src_dir).await;

    let det = body_to_json(
        patch_config(
            &state,
            "test.reset",
            json!({ "theme": "light", "label": "a" }),
        )
        .await,
    )
    .await;
    assert_eq!(
        det["user_config"],
        json!({ "theme": "light", "label": "a" })
    );

    let det = body_to_json(patch_config(&state, "test.reset", json!({})).await).await;
    assert_eq!(
        det["user_config"],
        json!({ "theme": "light", "label": "a" }),
        "an empty Save must not be a reset"
    );

    let det =
        body_to_json(patch_config_query(&state, "test.reset", "?reset=true", json!({})).await)
            .await;
    assert_eq!(det["user_config"], json!({}));
    assert_eq!(
        det["effective_config"]["theme"], "dark",
        "and the manifest default is in force again: {det:?}"
    );
}

/// Residue is excluded from the per-write cap and nothing else removes it, so
/// `declare {k} → fill k → narrow → reload → fill next` grows the row ~8 KiB per turn.
#[tokio::test]
async fn residue_cannot_grow_the_stored_config_without_bound() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin_with_config(
        src_root.path(),
        "test.grow",
        json!({
            "type": "object",
            "properties": { "k0": { "type": "string" } },
            "additionalProperties": false
        }),
    );
    install(&state, &src_dir).await;
    let manifest_path = src_dir.join("manifest.json");

    let chunk = "x".repeat(8000);
    let one = |key: &str, value: &str| {
        let mut m = serde_json::Map::new();
        m.insert(key.to_string(), json!(value));
        Value::Object(m)
    };
    let mut refusal: Option<(usize, String, String)> = None;

    for round in 0..8usize {
        let key = format!("k{round}");
        if round > 0 {
            let mut m: Value =
                serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
            let mut props = serde_json::Map::new();
            props.insert(key.clone(), json!({ "type": "string" }));
            m["config_schema"] = json!({
                "type": "object",
                "properties": Value::Object(props),
                "additionalProperties": false
            });
            std::fs::write(&manifest_path, serde_json::to_string_pretty(&m).unwrap()).unwrap();
            let resp = post_json(
                app(state.clone()),
                "/api/plugins/test.grow/reload",
                json!({}),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::OK, "reload failed on {round}");
        }

        let resp = patch_config(&state, "test.grow", one(&key, &chunk)).await;
        if resp.status() == StatusCode::BAD_REQUEST {
            let body = body_to_json(resp).await;
            refusal = Some((
                round,
                body["error"].as_str().unwrap_or_default().to_string(),
                body["code"].as_str().unwrap_or_default().to_string(),
            ));
            break;
        }
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "round {round} is a legal single write: {}",
            body_to_text(resp).await
        );
        if round >= 1 {
            let det =
                body_to_json(get_path(app(state.clone()), "/api/plugins/test.grow").await).await;
            let bytes = det["user_config"].to_string().len();
            assert!(
                bytes > 8192,
                "round {round} should already be past the per-write cap, got {bytes}"
            );
        }
    }

    let (round, text, code) = refusal.expect("the row must stop growing at some point");
    assert!(round > 1, "the cap must not refuse an ordinary first write");
    assert!(
        text.contains("32768"),
        "the refusal names the total cap: {text}"
    );
    assert!(
        text.contains("reset=true"),
        "a refusal on bytes no ordinary patch can shrink must name the exit: {text}"
    );
    assert_eq!(
        code, "plugin_config_too_large",
        "the residue refusal must be distinguishable from a schema violation without reading the prose: {text}"
    );

    let key = format!("k{round}");
    let resp = patch_config_query(&state, "test.grow", "?reset=true", one(&key, &chunk)).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "the named recovery action must actually recover: {}",
        body_to_text(resp).await
    );
    let det = body_to_json(resp).await;
    assert_eq!(
        det["user_config"],
        one(&key, &chunk),
        "reset kept the current configuration and dropped only the residue"
    );
}

#[tokio::test]
async fn a_config_write_refuses_while_another_lifecycle_operation_holds_the_plugin() {
    use axum::extract::FromRef;

    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin_with_config(src_root.path(), "test.busy", stub_config_schema());
    install(&state, &src_dir).await;
    let det =
        body_to_json(patch_config(&state, "test.busy", json!({ "label": "before" })).await).await;
    assert_eq!(det["user_config"], json!({ "label": "before" }));

    let cs = calm_server::state::CodexShellState::from_ref(&state);
    let guard = cs
        .plugin
        .try_lock_lifecycle("test.busy")
        .expect("lock free");

    let resp = patch_config(&state, "test.busy", json!({ "label": "during" })).await;
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "a config write may not interleave with another lifecycle operation"
    );
    let body = body_to_json(resp).await;
    assert_eq!(
        body["code"], "plugin_busy",
        "the §2.4 three-state table already owns this cell — no new code: {body}"
    );

    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/test.busy").await).await;
    assert_eq!(det["user_config"], json!({ "label": "before" }));

    drop(guard);
    let resp = patch_config(&state, "test.busy", json!({ "label": "during" })).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "…and the identical request succeeds once the holder is gone: {}",
        body_to_text(resp).await
    );
    assert_eq!(
        body_to_json(resp).await["user_config"],
        json!({ "label": "during" })
    );
}

#[tokio::test]
async fn patch_config_rejects_a_non_object_body_and_accepts_an_empty_one() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin_with_config(src_root.path(), "test.body", stub_config_schema());
    install(&state, &src_dir).await;
    let det =
        body_to_json(patch_config(&state, "test.body", json!({ "label": "keep" })).await).await;
    assert_eq!(det["user_config"], json!({ "label": "keep" }));

    for body in [json!(["not", "an", "object"]), json!("nope"), json!(7)] {
        let resp = patch_config(&state, "test.body", body.clone()).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "body {body}");
        let text = body_to_text(resp).await;
        assert!(text.contains("must be a JSON object"), "got: {text}");
    }

    let resp = patch_config(&state, "test.body", json!({})).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let det = body_to_json(resp).await;
    assert_eq!(
        det["user_config"],
        json!({ "label": "keep" }),
        "an empty patch changes nothing"
    );
}

#[tokio::test]
async fn patch_config_on_a_plugin_without_a_schema_is_400_even_for_an_empty_body() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    install(
        &state,
        &write_stub_plugin(src_root.path(), "test.noschema2"),
    )
    .await;

    for body in [json!({}), json!({ "theme": null })] {
        let resp = patch_config(&state, "test.noschema2", body.clone()).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "body {body}");
        assert!(body_to_text(resp).await.contains("config_schema"));
    }
}

#[tokio::test]
async fn list_reports_has_config_per_plugin() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    install(
        &state,
        &write_stub_plugin_with_config(src_root.path(), "test.with", stub_config_schema()),
    )
    .await;
    install(&state, &write_stub_plugin(src_root.path(), "test.without")).await;

    let rows = body_to_json(get_path(app(state.clone()), "/api/plugins").await).await;
    let rows = rows.as_array().expect("list is an array");
    let find = |id: &str| {
        rows.iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("row {id} missing from {rows:?}"))
            .clone()
    };
    assert_eq!(find("test.with")["has_config"], json!(true));
    assert_eq!(find("test.without")["has_config"], json!(false));

    let with = body_to_json(get_path(app(state.clone()), "/api/plugins/test.with").await).await;
    assert_eq!(with["config_schema"], stub_config_schema());
    let without =
        body_to_json(get_path(app(state.clone()), "/api/plugins/test.without").await).await;
    assert!(
        without.get("config_schema").is_none(),
        "no schema declared ⇒ none published: {without:?}"
    );
}

#[tokio::test]
async fn detail_carries_effective_config_without_persisting_defaults() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin_with_config(src_root.path(), "test.eff", stub_config_schema());
    install(&state, &src_dir).await;

    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/test.eff").await).await;
    assert_eq!(det["user_config"], json!({}), "nothing persisted yet");
    assert_eq!(det["effective_config"], json!({ "theme": "dark" }));

    let det = body_to_json(patch_config(&state, "test.eff", json!({ "retries": 2 })).await).await;
    assert_eq!(det["user_config"], json!({ "retries": 2 }));
    assert_eq!(
        det["effective_config"],
        json!({ "theme": "dark", "retries": 2 })
    );
}

const GHOST: &str = "test.no.such.plugin";

#[tokio::test]
async fn enable_unknown_id_returns_404() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let resp = post_json(
        app(state),
        &format!("/api/plugins/{GHOST}/enable"),
        json!({}),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "endpoint contract: enable of an unknown id must 404"
    );
}

#[tokio::test]
async fn disable_unknown_id_returns_404() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let resp = post_json(
        app(state),
        &format!("/api/plugins/{GHOST}/disable"),
        json!({}),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "endpoint contract: disable of an unknown id must 404"
    );
}

#[tokio::test]
async fn uninstall_unknown_id_returns_404_not_204() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let resp = delete_path(app(state), &format!("/api/plugins/{GHOST}")).await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "uninstall of an unknown id must 404; a 204 would mean `plugin_delete` \
         silently absorbed the missing row"
    );
}

#[tokio::test]
async fn reload_unknown_id_returns_404_not_manifest_read_error() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let resp = post_json(
        app(state),
        &format!("/api/plugins/{GHOST}/reload"),
        json!({}),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "reload of an unknown id must 404, not a manifest-read 400/500"
    );
}

#[tokio::test]
async fn reload_disabled_plugin_does_not_spawn() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.reload.disabled");
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert!(
        state.plugin.status("test.reload.disabled").await.is_none(),
        "freshly installed plugin must not be running"
    );

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/test.reload.disabled/reload",
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "reload should 200");
    let det = body_to_json(resp).await;
    assert_eq!(det["enabled"], false, "reload must not flip `enabled`");
    assert_eq!(
        det["state"], "disabled",
        "a disabled plugin must still read `disabled` after reload"
    );

    let running = state.plugin.list_running().await;
    assert!(
        running.is_empty(),
        "reload of a disabled plugin must not spawn anything (design §7 \
         nail 6); running: {:?}",
        running.iter().map(|s| &s.id).collect::<Vec<_>>()
    );
}

/// Long enough for `HttpCredential::parse`'s minimum, distinctive enough for a substring search over a whole tree.
const TEST_KEY: &str = "sk-1480-connector-credential";

fn connector_body(id: &str, extra: Value) -> Value {
    let mut source = json!({
        "kind": "mcp_http",
        "id": id,
        "display_name": "Zhibao",
        "url": "https://mcp.example.test/mcp",
        "api_key": TEST_KEY,
        "api_key_in": "bearer",
    });
    for (k, v) in extra.as_object().unwrap() {
        source[k] = v.clone();
    }
    json!({ "source": source })
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn files_under(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            out.extend(files_under(&path));
        } else {
            out.push((
                path.clone(),
                std::fs::read_to_string(&path).unwrap_or_default(),
            ));
        }
    }
    out
}

#[tokio::test]
async fn connector_install_writes_manifest_secrets_and_marker() {
    let (state, _tmp, plugins_dir) = boot_state().await;

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        connector_body("test.zhibao", json!({})),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "connector install should 201"
    );
    let body = body_to_json(resp).await;
    assert_eq!(body["id"], "test.zhibao");
    assert_eq!(body["enabled"], false, "install never enables");
    assert_eq!(body["manifest"]["kind"], "mcp-http");
    assert_eq!(
        body["manifest"]["mcp_http"]["url"],
        "https://mcp.example.test/mcp"
    );
    assert_eq!(
        body["manifest"]["mcp_http"]["tools_all"], false,
        "legacy API requests omitting tools_allow must not gain authority"
    );
    assert_eq!(
        body["manifest"]["mcp_http"]["api_key_secret"], "api_key",
        "the manifest names the secrets key, never the credential"
    );
    assert!(
        !body.to_string().contains(TEST_KEY),
        "the install response must not echo the credential: {body}"
    );

    let dir = plugins_dir.join("test.zhibao");
    let secrets = dir.join("secrets.json");
    assert!(dir.join("manifest.json").is_file(), "manifest.json written");
    assert!(secrets.is_file(), "secrets.json written");
    assert!(
        dir.join(".neige-managed.json").is_file(),
        "the tree is stamped as kernel-written"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&std::fs::read_to_string(&secrets).unwrap()).unwrap()["api_key"],
        TEST_KEY
    );
    assert_eq!(
        mode_of(&secrets),
        0o600,
        "secrets.json must not be group/world readable"
    );
    assert_eq!(
        mode_of(&dir),
        0o700,
        "the tree itself must not be group/world readable"
    );

    let holders: Vec<_> = files_under(&dir)
        .into_iter()
        .filter(|(_, text)| text.contains(TEST_KEY))
        .map(|(p, _)| p)
        .collect();
    assert_eq!(
        holders,
        vec![secrets.clone()],
        "the credential must live in secrets.json and nowhere else in the tree"
    );

    let arr = body_to_json(get_path(app(state.clone()), "/api/plugins").await).await;
    assert_eq!(arr.as_array().unwrap().len(), 1);
    assert_eq!(arr[0]["id"], "test.zhibao");
    assert_eq!(arr[0]["manifest_name"], "Zhibao");
}

/// Compatibility is keyed on presence, not array length: an explicit `tools_allow: []` asked for zero tools.
#[tokio::test]
async fn connector_install_preserves_explicit_empty_and_named_allowlists() {
    let (state, _tmp, _plugins_dir) = boot_state().await;

    for (id, allow) in [
        ("test.none", json!([])),
        ("test.named", json!(["search", "fetch-detail"])),
    ] {
        let resp = post_json(
            app(state.clone()),
            "/api/plugins/install",
            connector_body(id, json!({ "tools_allow": allow.clone() })),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED, "{id} install failed");
        let body = body_to_json(resp).await;
        assert_eq!(body["manifest"]["mcp_http"]["tools_all"], false, "{id}");
        assert_eq!(body["manifest"]["mcp_http"]["tools_allow"], allow, "{id}");
    }
}

#[tokio::test]
async fn connector_install_rejects_null_or_invalid_named_tool_lists() {
    let (state, _tmp, plugins_dir) = boot_state().await;
    for (id, allow, expected) in [
        (
            "test.null-tools",
            Value::Null,
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "test.bad-tool",
            json!(["two words"]),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let resp = post_json(
            app(state.clone()),
            "/api/plugins/install",
            connector_body(id, json!({ "tools_allow": allow })),
        )
        .await;
        assert_eq!(resp.status(), expected, "{id} must be refused");
        assert!(!plugins_dir.join(id).exists(), "{id} left a tree behind");
    }
}

#[tokio::test]
async fn connector_install_rejects_retired_query_placement_without_writing_a_tree() {
    let (state, _tmp, plugins_dir) = boot_state().await;
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        connector_body("test.retired", json!({ "api_key_in": "query:api_key" })),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "`query:<name>` was retired by #1194 and must not be reachable from the UI"
    );
    assert!(
        !plugins_dir.join("test.retired").exists(),
        "a refused install must leave nothing on disk"
    );
}

#[tokio::test]
async fn connector_install_rejects_a_non_http_url_without_writing_a_tree() {
    let (state, _tmp, plugins_dir) = boot_state().await;
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        connector_body("test.badurl", json!({ "url": "file:///etc/passwd" })),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(
        !plugins_dir.join("test.badurl").exists(),
        "a refused install must leave nothing on disk"
    );
}

#[tokio::test]
async fn connector_reinstall_conflicts_without_touching_the_installed_tree() {
    let (state, _tmp, plugins_dir) = boot_state().await;
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        connector_body("test.dup", json!({})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        connector_body(
            "test.dup",
            json!({ "api_key": "sk-second-attempt-credential" }),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT, "duplicate id is a 409");

    let secrets = plugins_dir.join("test.dup").join("secrets.json");
    assert_eq!(
        serde_json::from_str::<Value>(&std::fs::read_to_string(&secrets).unwrap()).unwrap()["api_key"],
        TEST_KEY,
        "the refused install must not have rewritten the live plugin's credential"
    );
}

#[tokio::test]
async fn connector_install_refuses_a_directory_the_kernel_did_not_write() {
    let (state, _tmp, plugins_dir) = boot_state().await;
    let dir = plugins_dir.join("test.occupied");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("work.txt"), "operator's own file").unwrap();

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        connector_body("test.occupied", json!({})),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "an occupied directory is a conflict, not an overwrite"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("work.txt")).unwrap(),
        "operator's own file",
        "the operator's directory must survive the refusal"
    );
}

#[tokio::test]
async fn uninstall_removes_a_kernel_written_connector_tree() {
    let (state, _tmp, plugins_dir) = boot_state().await;
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        connector_body("test.gone", json!({})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let dir = plugins_dir.join("test.gone");
    assert!(dir.join("secrets.json").is_file());

    let resp = delete_path(app(state.clone()), "/api/plugins/test.gone").await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(
        !dir.exists(),
        "the kernel wrote this tree and holds its credential; uninstall must remove it"
    );
}

#[tokio::test]
async fn uninstall_leaves_an_operator_supplied_tree_in_place() {
    let (state, _tmp, plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.operator");

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_str().unwrap() } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = delete_path(app(state.clone()), "/api/plugins/test.operator").await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(
        src_dir.join("manifest.json").is_file(),
        "uninstall must never delete a directory the operator supplied"
    );
    assert!(
        std::fs::symlink_metadata(plugins_dir.join("test.operator")).is_ok(),
        "and the link into plugins_dir is left alone too, as before #1480"
    );
}

/// A `local_path` install may not adopt a marked tree: uninstall would then delete a directory the kernel never wrote.
#[tokio::test]
async fn local_path_install_refuses_a_source_carrying_the_managed_marker() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.marked");
    std::fs::write(src_dir.join(".neige-managed.json"), "{}").unwrap();

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_str().unwrap() } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let text = body_to_text(resp).await;
    assert!(
        text.contains(".neige-managed.json"),
        "the refusal must name the marker so the operator can act on it: {text}"
    );
}

#[tokio::test]
async fn reinstalling_without_a_credential_does_not_inherit_the_previous_secret() {
    let (state, _tmp, plugins_dir) = boot_state().await;
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        connector_body("test.rotate", json!({})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = delete_path(app(state.clone()), "/api/plugins/test.rotate").await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let mut body = connector_body("test.rotate", json!({}));
    body["source"]["api_key"] = Value::Null;
    body["source"]["api_key_in"] = Value::Null;
    let resp = post_json(app(state.clone()), "/api/plugins/install", body).await;
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "keyless connector is legal"
    );

    let dir = plugins_dir.join("test.rotate");
    assert!(
        !dir.join("secrets.json").exists(),
        "a keyless install must not leave the previous credential readable"
    );
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
    assert!(
        manifest["mcp_http"].get("api_key_secret").is_none(),
        "and its manifest must not claim a secret it does not have"
    );
}
