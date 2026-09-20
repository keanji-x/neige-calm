//! Integration tests for `GET /api/plugins/:id/resources/:view_id` (iframe HTML
//! over HTTP) and `POST /api/plugins/:id/tool-call` (AppBridge fan-out).

#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use calm_server::routes;
use calm_server::state::{AppState, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};
use tower::ServiceExt;

const ECHO_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-echo");

struct Fixture {
    state: AppState,
    plugin_id: String,
    _tmp: tempfile::TempDir,
}

struct FxConfig<'a> {
    plugin_id: &'a str,
    /// Permissions block to embed in the manifest.
    permissions: Value,
    /// HTML body to write at `<install>/views/status.html`; `None` skips the file.
    view_html: Option<&'a str>,
    /// Optional CSP block on the view, emitted as the `Content-Security-Policy` HTTP header.
    csp: Option<Value>,
    /// Optional per-view `permissions.tools` allow-list; `None` blocks every `tool-call` from the iframe (deny by default).
    view_tools: Option<Vec<&'a str>>,
    /// If true, spawn + wait for Running.
    run: bool,
}

async fn boot(cfg: FxConfig<'_>) -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let install_dir = plugins_dir.join(cfg.plugin_id);
    let bin_dir = install_dir.join("bin");
    let views_dir = install_dir.join("views");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&views_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();
    std::os::unix::fs::symlink(Path::new(ECHO_BIN), bin_dir.join("stub")).unwrap();
    if let Some(html) = cfg.view_html {
        std::fs::write(views_dir.join("status.html"), html).unwrap();
    }

    let mut view = json!({
        "view_id": "status",
        "title": "Status",
        "scope": "card",
    });
    if let Some(csp) = &cfg.csp {
        view["csp"] = csp.clone();
    }
    if let Some(tools) = &cfg.view_tools {
        view["permissions"] = json!({ "tools": tools });
    }
    let manifest_json = json!({
        "manifest_version": 1,
        "id": cfg.plugin_id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "M5 stub",
        "entrypoint": { "command": "bin/stub" },
        "views": [view],
        "permissions": cfg.permissions,
    });
    let manifest: Manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest");

    let registry = PluginRegistry::from_manifests([(manifest, Some(install_dir.clone()))]);
    let events = EventBus::new();
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite repo"),
    );
    // Seed the plugin row so plugin_token_set's FK is satisfied on spawn.
    repo.plugin_install(calm_server::model::NewPlugin {
        id: cfg.plugin_id.into(),
        version: "0.1.0".into(),
        install_path: install_dir.display().to_string(),
        manifest: json!({}),
        enabled: true,
        user_config: json!({}),
    })
    .await
    .expect("seed plugin row");
    let plugin_host = Arc::new(PluginHost::new_full(
        Arc::new(registry),
        repo.clone(),
        plugins_dir,
        plugins_data_dir,
        Vec::new(),
        events.clone(),
        calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
        ),
    ));

    if cfg.run {
        plugin_host.spawn(cfg.plugin_id).await.expect("spawn");
        wait_for_running(&plugin_host, cfg.plugin_id).await;
    }

    let state = AppState::from_parts(
        repo,
        events,
        Arc::new(DaemonClient::new_stub()),
        plugin_host,
        Arc::new(calm_server::state::CodexClient::new_stub()),
        None,
        None,
    );

    Fixture {
        state,
        plugin_id: cfg.plugin_id.to_string(),
        _tmp: tmp,
    }
}

async fn wait_for_running(host: &Arc<PluginHost>, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(s) = host.status(id).await
            && matches!(s.status, PluginRuntimeStatus::Running)
        {
            return;
        }
        if Instant::now() > deadline {
            panic!("plugin did not reach Running within 5s");
        }
        sleep(Duration::from_millis(25)).await;
    }
}

fn app(state: AppState) -> axum::Router {
    axum::Router::new()
        .merge(routes::plugins::router())
        .with_state(state)
}

async fn body_bytes(resp: axum::http::Response<Body>) -> Vec<u8> {
    resp.into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

async fn body_to_json(resp: axum::http::Response<Body>) -> Value {
    let bytes = body_bytes(resp).await;
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

#[tokio::test]
async fn view_html_returns_body_and_mcp_app_mime() {
    let fx = boot(FxConfig {
        plugin_id: "m5.iframe.ok",
        permissions: json!({}),
        view_html: Some("<!doctype html><html><body>hello m5</body></html>"),
        csp: None,
        view_tools: None,
        run: false,
    })
    .await;

    let app = app(fx.state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/plugins/{}/resources/status", fx.plugin_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ctype = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert_eq!(ctype, "text/html;profile=mcp-app");
    assert!(
        resp.headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .is_none()
    );
    let body = String::from_utf8(body_bytes(resp).await).unwrap();
    assert!(body.contains("<body>hello m5"), "got body: {body}");
}

#[tokio::test]
async fn view_html_emits_csp_header_when_manifest_declares_csp() {
    let fx = boot(FxConfig {
        plugin_id: "m5.iframe.csp",
        permissions: json!({}),
        view_html: Some("<html><body>csp</body></html>"),
        csp: Some(json!({
            "default_src": ["'self'"],
            "script_src": ["'self'", "'unsafe-inline'"],
            "connect_src": ["'none'"],
        })),
        view_tools: None,
        run: false,
    })
    .await;

    let resp = app(fx.state.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/plugins/{}/resources/status", fx.plugin_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let csp = resp
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    // Order isn't guaranteed (HashMap iteration in the meta block), so assert each directive separately.
    assert!(
        csp.contains("default-src 'self'"),
        "expected default-src directive, got: {csp}"
    );
    assert!(
        csp.contains("script-src 'self' 'unsafe-inline'"),
        "expected script-src directive, got: {csp}"
    );
    assert!(
        csp.contains("connect-src 'none'"),
        "expected connect-src directive, got: {csp}"
    );
}

#[tokio::test]
async fn view_html_404_when_plugin_not_installed() {
    let fx = boot(FxConfig {
        plugin_id: "m5.iframe.installed",
        permissions: json!({}),
        view_html: Some("<html></html>"),
        csp: None,
        view_tools: None,
        run: false,
    })
    .await;

    let resp = app(fx.state.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/plugins/never.installed/resources/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = body_to_json(resp).await;
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn view_html_404_when_view_id_unknown() {
    let fx = boot(FxConfig {
        plugin_id: "m5.iframe.no-view",
        permissions: json!({}),
        view_html: Some("<html></html>"),
        csp: None,
        view_tools: None,
        run: false,
    })
    .await;

    let resp = app(fx.state.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/api/plugins/{}/resources/no-such-view",
                    fx.plugin_id
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tool_call_dispatches_neige_overlay_set_to_kernel() {
    let fx = boot(FxConfig {
        plugin_id: "m5.tc.overlay",
        permissions: json!({
            "overlays_write": ["track"],
        }),
        view_html: Some("<html></html>"),
        csp: None,
        view_tools: Some(vec!["neige.overlay.set"]),
        run: true,
    })
    .await;

    let resp = app(fx.state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/plugins/{}/tool-call", fx.plugin_id))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "neige.overlay.set",
                        "arguments": {
                            "entity_kind": "track",
                            "entity_id": "track-xyz",
                            "kind": "status",
                            "payload": { "state": "running" }
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "expected 200 from tool-call");
    let body = body_to_json(resp).await;
    // Assert the route round-tripped something rather than pinning the dispatcher's return shape.
    assert!(!body.is_null(), "expected non-null response body");

    let overlays = fx
        .state
        .repo
        .overlays_for("track", "track-xyz")
        .await
        .expect("overlay list");
    assert_eq!(overlays.len(), 1, "expected one overlay row");
    assert_eq!(overlays[0].kind, "status");
    assert_eq!(overlays[0].plugin_id, fx.plugin_id);

    fx.state.plugin.stop(&fx.plugin_id).await.ok();
}

#[tokio::test]
async fn tool_call_rejects_non_neige_namespace() {
    let fx = boot(FxConfig {
        plugin_id: "m5.tc.gated",
        permissions: json!({
            "overlays_write": ["track"],
        }),
        view_html: Some("<html></html>"),
        csp: None,
        view_tools: Some(vec!["neige.overlay.set"]),
        run: true,
    })
    .await;

    let resp = app(fx.state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/plugins/{}/tool-call", fx.plugin_id))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "hello-world.some-tool",
                        "arguments": {}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = body_to_json(resp).await;
    assert_eq!(body["code"], "forbidden_tool");

    fx.state.plugin.stop(&fx.plugin_id).await.ok();
}

#[tokio::test]
async fn tool_call_404_when_plugin_not_running() {
    let fx = boot(FxConfig {
        plugin_id: "m5.tc.installed-only",
        permissions: json!({}),
        view_html: Some("<html></html>"),
        csp: None,
        view_tools: None,
        run: false,
    })
    .await;

    let resp = app(fx.state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/plugins/{}/tool-call", fx.plugin_id))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "name": "neige.overlay.set", "arguments": {} }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = body_to_json(resp).await;
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn tool_call_403_when_tool_not_in_view_allowlist() {
    let fx = boot(FxConfig {
        plugin_id: "m5.tc.toolperm.scoped",
        permissions: json!({
            "overlays_write": ["track"],
        }),
        view_html: Some("<html></html>"),
        csp: None,
        view_tools: Some(vec!["neige.overlay.set"]),
        run: true,
    })
    .await;

    let resp = app(fx.state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/plugins/{}/tool-call", fx.plugin_id))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "neige.overlay.delete",
                        "arguments": {
                            "entity_kind": "track",
                            "entity_id": "track-xyz",
                            "kind": "status"
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = body_to_json(resp).await;
    assert_eq!(body["code"], "forbidden_tool");
    let err = body["error"].as_str().unwrap_or("");
    assert!(
        err.contains("neige.overlay.delete"),
        "error should name the tool, got: {err}"
    );

    fx.state.plugin.stop(&fx.plugin_id).await.ok();
}

#[tokio::test]
async fn tool_call_403_when_view_declares_no_tools() {
    let fx = boot(FxConfig {
        plugin_id: "m5.tc.toolperm.empty",
        permissions: json!({
            "overlays_write": ["track"],
        }),
        view_html: Some("<html></html>"),
        csp: None,
        view_tools: None,
        run: true,
    })
    .await;

    let resp = app(fx.state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/plugins/{}/tool-call", fx.plugin_id))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "neige.overlay.set",
                        "arguments": {
                            "entity_kind": "track",
                            "entity_id": "track-xyz",
                            "kind": "status",
                            "payload": {}
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = body_to_json(resp).await;
    assert_eq!(body["code"], "forbidden_tool");

    fx.state.plugin.stop(&fx.plugin_id).await.ok();
}

#[tokio::test]
async fn tool_call_allows_prefix_glob_grant() {
    let fx = boot(FxConfig {
        plugin_id: "m5.tc.toolperm.glob",
        permissions: json!({
            "overlays_write": ["track"],
        }),
        view_html: Some("<html></html>"),
        csp: None,
        view_tools: Some(vec!["neige.overlay.*"]),
        run: true,
    })
    .await;

    let resp = app(fx.state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/plugins/{}/tool-call", fx.plugin_id))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "neige.overlay.set",
                        "arguments": {
                            "entity_kind": "track",
                            "entity_id": "track-glob",
                            "kind": "status",
                            "payload": { "state": "ok" }
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "prefix glob should allow neige.overlay.set"
    );

    let resp = app(fx.state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/plugins/{}/tool-call", fx.plugin_id))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "name": "neige.card.update", "arguments": {} }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = body_to_json(resp).await;
    assert_eq!(body["code"], "forbidden_tool");

    fx.state.plugin.stop(&fx.plugin_id).await.ok();
}
