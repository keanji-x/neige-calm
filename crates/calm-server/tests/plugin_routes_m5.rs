//! Integration tests for M3-mcp-apps **Slice M5** routes:
//!
//!   * `GET /api/plugins/:id/resources/:view_id` — iframe HTML over HTTP.
//!     Resolves to a `ui://<id>/<view_id>` URI and routes through
//!     `plugin_host::read_ui_resource`. Asserts body + `Content-Type` +
//!     derived `Content-Security-Policy` header.
//!   * `POST /api/plugins/:id/tool-call` — AppBridge fan-out for
//!     `app.callServerTool({ name, arguments })`. Asserts:
//!       - `neige.*` names dispatch into the kernel callback router (the
//!         plugin process never sees the call) and 200 the result.
//!       - non-`neige.*` names return 403 `forbidden_tool` per §7.6 row 5.
//!
//! The fixtures reuse the existing echo stub binary — none of these tests
//! require the plugin to do anything beyond a clean `initialize` handshake,
//! since the iframe HTTP route reads from the manifest + on-disk HTML and
//! the tool-call route routes `neige.*` straight into `callbacks::dispatch`.

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

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Fixture {
    state: AppState,
    plugin_id: String,
    _tmp: tempfile::TempDir,
}

struct FxConfig<'a> {
    plugin_id: &'a str,
    /// Permissions block to embed in the manifest. Use `json!({})` for the
    /// "no perms" forbidden test; full perms for the overlay happy-path.
    permissions: Value,
    /// HTML body to write at `<install>/views/status.html`. None = skip the
    /// file (used by the 404-on-missing-file negative).
    view_html: Option<&'a str>,
    /// Optional CSP block on the view (mirrored under `_meta.ui.csp` in the
    /// `resources/read` response, and emitted as the
    /// `Content-Security-Policy` HTTP header).
    csp: Option<Value>,
    /// If true, spawn + wait for Running. Tests that only need the
    /// registry (iframe HTML) can skip the spawn cost.
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

    let registry = PluginRegistry::empty();
    registry.insert(manifest, Some(install_dir.clone()));
    let events = EventBus::new();
    // Shared repo so the dispatcher's writes are observable from the test.
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite repo"),
    );
    // Seed plugin row so plugin_token_set's FK is satisfied on spawn.
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
        Arc::clone(&repo),
        plugins_dir,
        plugins_data_dir,
        Vec::new(),
        events.clone(),
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

// ---------------------------------------------------------------------------
// GET /api/plugins/:id/resources/:view_id
// ---------------------------------------------------------------------------

#[tokio::test]
async fn view_html_returns_body_and_mcp_app_mime() {
    // Happy path: manifest declares a view, HTML file exists; GET returns
    // 200 + body + the MCP-app MIME profile.
    let fx = boot(FxConfig {
        plugin_id: "m5.iframe.ok",
        permissions: json!({}),
        view_html: Some("<!doctype html><html><body>hello m5</body></html>"),
        csp: None,
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
    // No CSP declared → no header on the response.
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
    // Order isn't guaranteed (HashMap iteration in the meta block), so just
    // assert the three directives are present with their expected sources.
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
    // Boot a fixture with a different plugin id — the requested id won't
    // be in the registry, so we get a clean 404.
    let fx = boot(FxConfig {
        plugin_id: "m5.iframe.installed",
        permissions: json!({}),
        view_html: Some("<html></html>"),
        csp: None,
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

// ---------------------------------------------------------------------------
// POST /api/plugins/:id/tool-call
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tool_call_dispatches_neige_overlay_set_to_kernel() {
    let fx = boot(FxConfig {
        plugin_id: "m5.tc.overlay",
        permissions: json!({
            "overlays_write": ["wave"],
        }),
        view_html: Some("<html></html>"),
        csp: None,
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
                            "entity_kind": "wave",
                            "entity_id": "wave-xyz",
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
    // dispatch returns a JSON Value — the overlay set handler responds with
    // an `{"ok": true}` shape (or similar). We assert the route round-tripped
    // _something_ rather than pinning the dispatcher's return shape.
    assert!(!body.is_null(), "expected non-null response body");

    // And the side-effect should be visible in the repo.
    let overlays = fx
        .state
        .repo
        .overlays_for("wave", "wave-xyz")
        .await
        .expect("overlay list");
    assert_eq!(overlays.len(), 1, "expected one overlay row");
    assert_eq!(overlays[0].kind, "status");
    assert_eq!(overlays[0].plugin_id, fx.plugin_id);

    fx.state.plugin.stop(&fx.plugin_id).await.ok();
}

#[tokio::test]
async fn tool_call_rejects_non_neige_namespace() {
    // Even when the plugin is running with full permissions, the iframe
    // can't reach the plugin's own server tools — §7.6 row 5.
    let fx = boot(FxConfig {
        plugin_id: "m5.tc.gated",
        permissions: json!({
            "overlays_write": ["wave"],
        }),
        view_html: Some("<html></html>"),
        csp: None,
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
    // Plugin row exists in the registry (we always seed one to boot the
    // fixture), but we ask for a different id that isn't running anywhere.
    let fx = boot(FxConfig {
        plugin_id: "m5.tc.installed-only",
        permissions: json!({}),
        view_html: Some("<html></html>"),
        csp: None,
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
