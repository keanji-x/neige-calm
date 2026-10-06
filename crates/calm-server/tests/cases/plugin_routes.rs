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
    let src_dir = write_stub_plugin(src_root.path(), "testinstall");

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
    assert_eq!(body["id"], "testinstall");
    assert_eq!(body["enabled"], false);
    assert_eq!(body["state"], "disabled");

    assert!(
        plugins_dir.join("testinstall").exists(),
        "plugins_dir entry should exist"
    );

    let resp = get_path(app(state.clone()), "/api/plugins").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list = body_to_json(resp).await;
    let arr = list.as_array().expect("list should be array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], "testinstall");
    assert_eq!(arr[0]["manifest_name"], "Echo Stub");

    let resp = get_path(app(state.clone()), "/api/plugins/testinstall").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let det = body_to_json(resp).await;
    assert_eq!(det["id"], "testinstall");
    assert!(det["manifest"]["views"].is_array());
}

#[tokio::test]
async fn enable_transitions_to_running() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "testenable");

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/testenable/enable",
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "enable should 200");
    let det = body_to_json(resp).await;
    assert_eq!(det["enabled"], true);

    // The state can be `spawning` momentarily; poll until `running`.
    let det = wait_for_state(&state, "testenable", "running", Duration::from_secs(3)).await;
    assert_eq!(det["enabled"], true);

    let _ = post_json(
        app(state.clone()),
        "/api/plugins/testenable/disable",
        json!({}),
    )
    .await;
}

#[tokio::test]
async fn disable_transitions_to_disabled() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "testdisable");
    post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;
    post_json(
        app(state.clone()),
        "/api/plugins/testdisable/enable",
        json!({}),
    )
    .await;
    wait_for_state(&state, "testdisable", "running", Duration::from_secs(3)).await;

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/testdisable/disable",
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
    let src_dir = write_stub_plugin(src_root.path(), "testlog");
    post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;
    post_json(app(state.clone()), "/api/plugins/testlog/enable", json!({})).await;
    wait_for_state(&state, "testlog", "running", Duration::from_secs(3)).await;

    let resp = get_path(app(state.clone()), "/api/plugins/testlog/log?n=10").await;
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
        "/api/plugins/testlog/disable",
        json!({}),
    )
    .await;
}

#[tokio::test]
async fn uninstall_cascades_satellites() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "testuninstall");
    post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;

    state
        .repo
        .plugin_kv_set("testuninstall", "foo", &json!("bar"))
        .await
        .unwrap();
    state
        .raw_repo()
        .overlay_upsert(NewOverlay {
            plugin_id: "testuninstall".into(),
            entity_kind: "track".into(),
            entity_id: "w1".into(),
            kind: "status".into(),
            payload: json!({"x": 1}),
        })
        .await
        .unwrap();

    let resp = delete_path(app(state.clone()), "/api/plugins/testuninstall").await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = get_path(app(state.clone()), "/api/plugins/testuninstall").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    assert!(
        state
            .repo
            .plugin_token_get("testuninstall")
            .await
            .unwrap()
            .is_none()
    );
    let kv = state
        .repo
        .plugin_kv_list("testuninstall", "")
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
    let src_dir = write_stub_plugin(src_root.path(), "testviews");

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
        "/api/plugins/testviews/enable",
        json!({}),
    )
    .await;
    wait_for_state(&state, "testviews", "running", Duration::from_secs(3)).await;

    let resp = get_path(app(state.clone()), "/api/plugins/views").await;
    let arr = body_to_json(resp).await;
    let entries = arr.as_array().expect("array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["resource_uri"], "ui://testviews/main");
    assert_eq!(entries[0]["scope"], "card");
    assert_eq!(entries[0]["default_size"]["w"], 4);
    assert!(entries[0].get("plugin_id").is_none());
    assert!(entries[0].get("view_id").is_none());

    let _ = post_json(
        app(state.clone()),
        "/api/plugins/testviews/disable",
        json!({}),
    )
    .await;
}

#[tokio::test]
async fn install_rejects_track_scope_manifest() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_bad_scope_plugin(src_root.path(), "testbadscope");

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
    let src_dir = write_stub_plugin(src_root.path(), "testdup");

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

/// #2087 §6: every built-in manifest id, and every id the install route accepts for a new plugin,
/// is one word or a grandfathered legacy id. `dev.new-plugin` is refused before a tree or a row is
/// written; `newplugin` installs, and so does a fresh `dev-neige-market` (§9's closed list).
#[tokio::test]
async fn plugin_ids_are_words() {
    let word = |id: &str| {
        (2..=32).contains(&id.len()) && id.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9'))
    };
    let mut builtins: Vec<String> = calm_server::builtin_plugins::catalog()
        .iter()
        .map(|component| component.manifest().id.clone())
        .collect();
    builtins.sort();
    assert_eq!(builtins, ["calendar", "gitforge"], "the built-in catalog");
    assert!(builtins.iter().all(|id| word(id)), "{builtins:?}");

    let (state, _tmp, plugins_dir, repo) = boot_state_with_repo().await;
    let src_root = tempfile::tempdir().unwrap();
    let dotted = write_stub_plugin(src_root.path(), "dev.new-plugin");
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": dotted.to_string_lossy() } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_to_text(resp).await;
    assert!(
        body.contains("manifest id `dev.new-plugin` must be one word: ^[a-z0-9]{2,32}$"),
        "got: {body}"
    );
    assert!(!plugins_dir.join("dev.new-plugin").exists());
    assert!(
        repo.plugin_get_by_id("dev.new-plugin")
            .await
            .unwrap()
            .is_none()
    );

    let src = write_stub_plugin(src_root.path(), "newplugin");
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src.to_string_lossy() } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = body_to_json(resp).await;
    assert!(word(body["id"].as_str().unwrap()), "{body}");

    let legacy = write_stub_plugin(src_root.path(), "dev-neige-market");
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": legacy.to_string_lossy() } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(body_to_json(resp).await["id"], "dev-neige-market");
    assert!(
        calm_server::plugin_host::manifest::LEGACY_PLUGIN_IDS
            .iter()
            .all(|id| !word(id)),
        "the legacy list holds only ids that are not words"
    );
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
        write_stub_plugin_with_config(src_root.path(), "testconfig", stub_config_schema());
    install(&state, &src_dir).await;

    let resp = patch_config(&state, "testconfig", json!({ "theme": "light" })).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let det = body_to_json(resp).await;
    assert_eq!(det["user_config"]["theme"], "light");
    assert_eq!(det["effective_config"]["theme"], "light");
}

#[tokio::test]
async fn patch_config_on_a_plugin_without_a_schema_is_400() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "testnoschema");
    install(&state, &src_dir).await;

    let resp = patch_config(&state, "testnoschema", json!({ "theme": "dark" })).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_to_text(resp).await;
    assert!(body.contains("config_schema"), "got: {body}");

    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/testnoschema").await).await;
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
    let src_dir = write_stub_plugin_with_config(src_root.path(), "testpatch", stub_config_schema());
    install(&state, &src_dir).await;

    let det = body_to_json(
        patch_config(
            &state,
            "testpatch",
            json!({ "theme": "light", "label": "a" }),
        )
        .await,
    )
    .await;
    assert_eq!(
        det["user_config"],
        json!({ "theme": "light", "label": "a" })
    );

    let det = body_to_json(patch_config(&state, "testpatch", json!({ "label": "b" })).await).await;
    assert_eq!(
        det["user_config"],
        json!({ "theme": "light", "label": "b" }),
        "an absent key must keep its stored value, not be dropped"
    );

    let det = body_to_json(patch_config(&state, "testpatch", json!({ "theme": null })).await).await;
    assert_eq!(det["user_config"], json!({ "label": "b" }));
    assert_eq!(
        det["effective_config"]["theme"], "dark",
        "a cleared key falls back to its default, not to absent"
    );

    let det = body_to_json(patch_config(&state, "testpatch", json!({ "label": null })).await).await;
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
    let src_dir = write_stub_plugin_with_config(src_root.path(), "testbad", stub_config_schema());
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
        let resp = patch_config(&state, "testbad", body).await;
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

    let resp = patch_config(&state, "testbad", json!({ "retries": 3, "theme": "light" })).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = patch_config(&state, "testbad", json!({ "theme": "neon" })).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/testbad").await).await;
    assert_eq!(det["user_config"]["theme"], "light");
}

/// The exact wire body of a config refusal (#2154): a one-key violation carries the key's path in
/// `field` and its reason alone in `error`; a refusal of the patch as a whole carries no `field`.
/// `fe/core/domain/plugins.test.ts` feeds these same bodies to `configWriteError`.
#[tokio::test]
async fn a_config_violation_answers_its_field_apart_from_its_reason() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin_with_config(src_root.path(), "testfield", stub_config_schema());
    install(&state, &src_dir).await;

    let cases = [
        (
            json!({ "retries": "three" }),
            json!({
                "error": "expected type `integer` (an integer-encoded JSON number; float-encoded \
                          values such as `1.0` are rejected)",
                "code": "bad_request",
                "field": "config.retries",
            }),
        ),
        (
            json!({ "theme": "neon" }),
            json!({
                "error": "expected one of [\"dark\", \"light\"]",
                "code": "bad_request",
                "field": "config.theme",
            }),
        ),
        // Judged before `null` means delete, through the other entry point.
        (
            json!({ "ghost": null }),
            json!({
                "error": "unknown field (schema declares additionalProperties: false)",
                "code": "bad_request",
                "field": "config.ghost",
            }),
        ),
        (
            json!(["retries"]),
            json!({
                "error": "config patch must be a JSON object of the keys being edited",
                "code": "bad_request",
            }),
        ),
        (
            json!({ "label": "x".repeat(9000) }),
            json!({
                "error": "config: must serialize to at most 8192 bytes",
                "code": "bad_request",
            }),
        ),
    ];
    for (patch, expected) in cases {
        let resp = patch_config(&state, "testfield", patch.clone()).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{patch}");
        assert_eq!(body_to_json(resp).await, expected, "{patch}");
    }
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
    let src_dir = write_stub_plugin_with_config(src_root.path(), "testcap", two_strings);
    install(&state, &src_dir).await;

    let chunk = "x".repeat(5000);

    let resp = patch_config(&state, "testcap", json!({ "a": chunk })).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "5000 bytes is under the cap: {}",
        body_to_text(resp).await
    );

    let resp = patch_config(&state, "testcap", json!({ "b": chunk.clone() })).await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "the cap is on the merged storage state, and 5000 + 5000 > 8192"
    );
    let text = body_to_text(resp).await;
    assert!(text.contains("8192"), "got: {text}");

    let row = repo.plugin_get_by_id("testcap").await.unwrap().unwrap();
    assert_eq!(row.user_config.as_object().unwrap().len(), 1);

    // Reverse: a row already over the cap (only a direct write can produce one) still accepts a patch that shrinks it.
    repo.plugin_update_user_config("testcap", json!({ "a": chunk.clone(), "b": chunk.clone() }))
        .await
        .unwrap();
    let resp = patch_config(&state, "testcap", json!({ "b": "small" })).await;
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

    let det = body_to_json(patch_config(&state, "testcap", json!({ "a": null })).await).await;
    assert_eq!(det["user_config"], json!({ "b": "small" }));
}

/// The schema needs `manifest_version: 3`, the only place this suite exercises that version end to end through install.
#[tokio::test]
async fn patch_config_does_not_enforce_required_keys() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin_with_config(
        src_root.path(),
        "testrequired",
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

    let resp = patch_config(&state, "testrequired", json!({ "token": "t" })).await;
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
        body_to_json(patch_config(&state, "testrequired", json!({ "secondary": "s" })).await).await;
    assert_eq!(
        det["user_config"],
        json!({ "token": "t", "secondary": "s" })
    );

    let resp = patch_config(&state, "testrequired", json!({ "token": null })).await;
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

    let resp = patch_config(&state, "testrequired", json!({ "token": 7 })).await;
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
        write_stub_plugin_with_config(src_root.path(), "testghostnull", stub_config_schema());
    install(&state, &src_dir).await;

    let resp = patch_config(&state, "testghostnull", json!({ "ghost": null })).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let text = body_to_text(resp).await;
    assert!(text.contains("config.ghost"), "got: {text}");

    let resp = patch_config(
        &state,
        "testghostnull",
        json!({ "label": "keep", "ghost": null }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/testghostnull").await).await;
    assert_eq!(det["user_config"], json!({}), "nothing was written");

    let det =
        body_to_json(patch_config(&state, "testghostnull", json!({ "label": "x" })).await).await;
    assert_eq!(det["user_config"], json!({ "label": "x" }));
    let det =
        body_to_json(patch_config(&state, "testghostnull", json!({ "label": null })).await).await;
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
    let src_dir = write_stub_plugin_with_config(src_root.path(), "testnarrow", wide);
    install(&state, &src_dir).await;

    let det = body_to_json(
        patch_config(
            &state,
            "testnarrow",
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
        "/api/plugins/testnarrow/reload",
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "reload failed");

    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/testnarrow").await).await;
    assert_eq!(det["user_config"]["old"], "residue");
    assert!(
        det["effective_config"].get("old").is_none(),
        "…but nothing runs with it: {det:?}"
    );

    let resp = patch_config(&state, "testnarrow", json!({ "keep": "b" })).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "an invisible key must not reject a legal request: {}",
        body_to_text(resp).await
    );
    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/testnarrow").await).await;
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
        "/api/plugins/testnarrow/reload",
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "re-widening reload failed");
    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/testnarrow").await).await;
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
        "/api/plugins/testnarrow/reload",
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "re-narrowing reload failed");

    let resp = patch_config(&state, "testnarrow", json!({ "old": "again" })).await;
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
        write_stub_plugin_with_config(src_root.path(), "testupgrade", stub_config_schema());
    install(&state, &src_dir).await;

    // Rewrite the persisted blob to what an older kernel would have stored.
    let row = repo.plugin_get_by_id("testupgrade").await.unwrap().unwrap();
    let mut blob = row.manifest.clone();
    assert!(
        blob.as_object_mut()
            .unwrap()
            .remove("config_schema")
            .is_some(),
        "fixture precondition: the blob carried the schema"
    );
    repo.plugin_update_manifest("testupgrade", blob)
        .await
        .unwrap();

    let rows = body_to_json(get_path(app(state.clone()), "/api/plugins").await).await;
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "testupgrade")
        .unwrap()
        .clone();
    assert_eq!(row["has_config"], json!(true), "got {row:?}");

    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/testupgrade").await).await;
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

    let resp = patch_config(&state, "testupgrade", json!({ "theme": "light" })).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "an upgraded install must not need a manual reload: {}",
        body_to_text(resp).await
    );
    let resp = patch_config(&state, "testupgrade", json!({ "theme": "neon" })).await;
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
    let src_dir = write_stub_plugin_with_config(src_root.path(), "testgap", stub_config_schema());
    install(&state, &src_dir).await;

    // Reproduce the window: drop the registry entry, keep the row.
    let cs = calm_server::state::CodexShellState::from_ref(&state);
    let guard = cs.plugin.try_lock_lifecycle("testgap").expect("lock free");
    assert!(cs.plugin.registry_remove(&guard).is_some());
    drop(guard);

    let resp = patch_config(&state, "testgap", json!({ "theme": "light" })).await;
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
        .find(|r| r["id"] == "testgap")
        .unwrap()
        .clone();
    assert_eq!(row["has_config"], json!(false), "got {row:?}");
    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/testgap").await).await;
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
    let src_dir = write_stub_plugin(src_root.path(), "testgapnoschema");
    install(&state, &src_dir).await;

    let resp = patch_config(&state, "testgapnoschema", json!({ "x": 1 })).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let cs = calm_server::state::CodexShellState::from_ref(&state);
    let guard = cs
        .plugin
        .try_lock_lifecycle("testgapnoschema")
        .expect("lock free");
    assert!(cs.plugin.registry_remove(&guard).is_some());
    drop(guard);

    let resp = patch_config(&state, "testgapnoschema", json!({ "x": 1 })).await;
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
        write_stub_plugin_with_config(src_root.path(), "testcorrupt", stub_config_schema());
    install(&state, &src_dir).await;
    repo.plugin_update_user_config("testcorrupt", json!("theme=light"))
        .await
        .unwrap();

    let resp = patch_config(&state, "testcorrupt", json!({ "theme": "light" })).await;
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

    let row = repo.plugin_get_by_id("testcorrupt").await.unwrap().unwrap();
    assert_eq!(row.user_config, json!("theme=light"));

    let resp = patch_config_query(
        &state,
        "testcorrupt",
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

    let resp = patch_config(&state, "testcorrupt", json!({ "label": "x" })).await;
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
    let src_dir = write_stub_plugin_with_config(src_root.path(), "testreset", stub_config_schema());
    install(&state, &src_dir).await;

    let det = body_to_json(
        patch_config(
            &state,
            "testreset",
            json!({ "theme": "light", "label": "a" }),
        )
        .await,
    )
    .await;
    assert_eq!(
        det["user_config"],
        json!({ "theme": "light", "label": "a" })
    );

    let det = body_to_json(patch_config(&state, "testreset", json!({})).await).await;
    assert_eq!(
        det["user_config"],
        json!({ "theme": "light", "label": "a" }),
        "an empty Save must not be a reset"
    );

    let det =
        body_to_json(patch_config_query(&state, "testreset", "?reset=true", json!({})).await).await;
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
        "testgrow",
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
                "/api/plugins/testgrow/reload",
                json!({}),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::OK, "reload failed on {round}");
        }

        let resp = patch_config(&state, "testgrow", one(&key, &chunk)).await;
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
                body_to_json(get_path(app(state.clone()), "/api/plugins/testgrow").await).await;
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
    let resp = patch_config_query(&state, "testgrow", "?reset=true", one(&key, &chunk)).await;
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
    let src_dir = write_stub_plugin_with_config(src_root.path(), "testbusy", stub_config_schema());
    install(&state, &src_dir).await;
    let det =
        body_to_json(patch_config(&state, "testbusy", json!({ "label": "before" })).await).await;
    assert_eq!(det["user_config"], json!({ "label": "before" }));

    let cs = calm_server::state::CodexShellState::from_ref(&state);
    let guard = cs.plugin.try_lock_lifecycle("testbusy").expect("lock free");

    let resp = patch_config(&state, "testbusy", json!({ "label": "during" })).await;
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

    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/testbusy").await).await;
    assert_eq!(det["user_config"], json!({ "label": "before" }));

    drop(guard);
    let resp = patch_config(&state, "testbusy", json!({ "label": "during" })).await;
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
    let src_dir = write_stub_plugin_with_config(src_root.path(), "testbody", stub_config_schema());
    install(&state, &src_dir).await;
    let det =
        body_to_json(patch_config(&state, "testbody", json!({ "label": "keep" })).await).await;
    assert_eq!(det["user_config"], json!({ "label": "keep" }));

    for body in [json!(["not", "an", "object"]), json!("nope"), json!(7)] {
        let resp = patch_config(&state, "testbody", body.clone()).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "body {body}");
        let text = body_to_text(resp).await;
        assert!(text.contains("must be a JSON object"), "got: {text}");
    }

    let resp = patch_config(&state, "testbody", json!({})).await;
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
    install(&state, &write_stub_plugin(src_root.path(), "testnoschema2")).await;

    for body in [json!({}), json!({ "theme": null })] {
        let resp = patch_config(&state, "testnoschema2", body.clone()).await;
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
        &write_stub_plugin_with_config(src_root.path(), "testwith", stub_config_schema()),
    )
    .await;
    install(&state, &write_stub_plugin(src_root.path(), "testwithout")).await;

    let rows = body_to_json(get_path(app(state.clone()), "/api/plugins").await).await;
    let rows = rows.as_array().expect("list is an array");
    let find = |id: &str| {
        rows.iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("row {id} missing from {rows:?}"))
            .clone()
    };
    assert_eq!(find("testwith")["has_config"], json!(true));
    assert_eq!(find("testwithout")["has_config"], json!(false));

    let with = body_to_json(get_path(app(state.clone()), "/api/plugins/testwith").await).await;
    assert_eq!(with["config_schema"], stub_config_schema());
    let without =
        body_to_json(get_path(app(state.clone()), "/api/plugins/testwithout").await).await;
    assert!(
        without.get("config_schema").is_none(),
        "no schema declared ⇒ none published: {without:?}"
    );
}

#[tokio::test]
async fn detail_carries_effective_config_without_persisting_defaults() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin_with_config(src_root.path(), "testeff", stub_config_schema());
    install(&state, &src_dir).await;

    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/testeff").await).await;
    assert_eq!(det["user_config"], json!({}), "nothing persisted yet");
    assert_eq!(det["effective_config"], json!({ "theme": "dark" }));

    let det = body_to_json(patch_config(&state, "testeff", json!({ "retries": 2 })).await).await;
    assert_eq!(det["user_config"], json!({ "retries": 2 }));
    assert_eq!(
        det["effective_config"],
        json!({ "theme": "dark", "retries": 2 })
    );
}

const GHOST: &str = "testnosuchplugin";

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
    let src_dir = write_stub_plugin(src_root.path(), "testreloaddisabled");
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert!(
        state.plugin.status("testreloaddisabled").await.is_none(),
        "freshly installed plugin must not be running"
    );

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/testreloaddisabled/reload",
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
        connector_body("testzhibao", json!({})),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "connector install should 201"
    );
    let body = body_to_json(resp).await;
    assert_eq!(body["id"], "testzhibao");
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

    let dir = plugins_dir.join("testzhibao");
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
    assert_eq!(arr[0]["id"], "testzhibao");
    assert_eq!(arr[0]["manifest_name"], "Zhibao");
}

/// Compatibility is keyed on presence, not array length: an explicit `tools_allow: []` asked for zero tools.
#[tokio::test]
async fn connector_install_preserves_explicit_empty_and_named_allowlists() {
    let (state, _tmp, _plugins_dir) = boot_state().await;

    for (id, allow) in [
        ("testnone", json!([])),
        ("testnamed", json!(["search", "fetch-detail"])),
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
            "testnulltools",
            Value::Null,
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        ("testbadtool", json!(["two words"]), StatusCode::BAD_REQUEST),
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
        connector_body("testretired", json!({ "api_key_in": "query:api_key" })),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "`query:<name>` was retired by #1194 and must not be reachable from the UI"
    );
    assert!(
        !plugins_dir.join("testretired").exists(),
        "a refused install must leave nothing on disk"
    );
}

#[tokio::test]
async fn connector_install_rejects_a_non_http_url_without_writing_a_tree() {
    let (state, _tmp, plugins_dir) = boot_state().await;
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        connector_body("testbadurl", json!({ "url": "file:///etc/passwd" })),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(
        !plugins_dir.join("testbadurl").exists(),
        "a refused install must leave nothing on disk"
    );
}

#[tokio::test]
async fn connector_reinstall_conflicts_without_touching_the_installed_tree() {
    let (state, _tmp, plugins_dir) = boot_state().await;
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        connector_body("testdup", json!({})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        connector_body(
            "testdup",
            json!({ "api_key": "sk-second-attempt-credential" }),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT, "duplicate id is a 409");
    assert_eq!(
        body_to_json(resp).await["code"],
        "plugin_conflict",
        "an id already installed is the one 409 a retried install may read as its own"
    );

    let secrets = plugins_dir.join("testdup").join("secrets.json");
    assert_eq!(
        serde_json::from_str::<Value>(&std::fs::read_to_string(&secrets).unwrap()).unwrap()["api_key"],
        TEST_KEY,
        "the refused install must not have rewritten the live plugin's credential"
    );
}

#[tokio::test]
async fn connector_install_refuses_a_directory_the_kernel_did_not_write() {
    let (state, _tmp, plugins_dir) = boot_state().await;
    let dir = plugins_dir.join("testoccupied");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("work.txt"), "operator's own file").unwrap();

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        connector_body("testoccupied", json!({})),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "an occupied directory is a conflict, not an overwrite"
    );
    assert_eq!(
        body_to_json(resp).await["code"],
        "plugin_dir_occupied",
        "not `plugin_conflict`: nothing is installed, so a retried install must not read it as done"
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
        connector_body("testgone", json!({})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let dir = plugins_dir.join("testgone");
    assert!(dir.join("secrets.json").is_file());

    let resp = delete_path(app(state.clone()), "/api/plugins/testgone").await;
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
    let src_dir = write_stub_plugin(src_root.path(), "testoperator");

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_str().unwrap() } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = delete_path(app(state.clone()), "/api/plugins/testoperator").await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(
        src_dir.join("manifest.json").is_file(),
        "uninstall must never delete a directory the operator supplied"
    );
    assert!(
        std::fs::symlink_metadata(plugins_dir.join("testoperator")).is_ok(),
        "and the link into plugins_dir is left alone too, as before #1480"
    );
}

/// A `local_path` install may not adopt a marked tree: uninstall would then delete a directory the kernel never wrote.
#[tokio::test]
async fn local_path_install_refuses_a_source_carrying_the_managed_marker() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "testmarked");
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
        connector_body("testrotate", json!({})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = delete_path(app(state.clone()), "/api/plugins/testrotate").await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let mut body = connector_body("testrotate", json!({}));
    body["source"]["api_key"] = Value::Null;
    body["source"]["api_key_in"] = Value::Null;
    let resp = post_json(app(state.clone()), "/api/plugins/install", body).await;
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "keyless connector is legal"
    );

    let dir = plugins_dir.join("testrotate");
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

#[tokio::test]
async fn disabled_install_and_uninstall_publish_completed_catalog_changes() {
    let (state, _tmp, _plugins_dir, repo) = boot_state_with_repo().await;
    let source = tempfile::tempdir().unwrap();
    let dir = write_stub_plugin(source.path(), "testcatalogchange");
    let installed = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({
            "source": { "kind": "local_path", "path": dir.to_string_lossy() }
        }),
    )
    .await;
    assert_eq!(installed.status(), StatusCode::CREATED);
    let events = repo.events_since(0, i64::MAX).await.unwrap();
    let installed_event = events.iter().rfind(|(_,_,_,event)| matches!(event,
        calm_server::event::Event::PluginState { id, state, .. } if id == "testcatalogchange" && state == "disabled")).expect("a disabled install must notify other clients").0;
    assert!(
        repo.plugin_get_by_id("testcatalogchange")
            .await
            .unwrap()
            .is_some()
    );
    let removed = delete_path(app(state), "/api/plugins/testcatalogchange").await;
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    assert!(
        repo.plugin_get_by_id("testcatalogchange")
            .await
            .unwrap()
            .is_none()
    );
    let events = repo.events_since(installed_event, i64::MAX).await.unwrap();
    assert!(events.iter().any(|(_,_,_,event)| matches!(event,
        calm_server::event::Event::PluginState { id, state, .. } if id == "testcatalogchange" && state == "disabled")),
        "an already-disabled uninstall must notify clients after removing the row");
}
