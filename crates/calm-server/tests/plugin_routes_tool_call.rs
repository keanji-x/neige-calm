//! Integration tests for M3-mcp-apps **Slice M2**: the
//! `POST /api/waves/:wave_id/cards` route's `via_tool_call` payload variant.
//!
//! We boot a real `PluginHost`, install + spawn a `stub-plugin-toolcall`
//! configured to return a deterministic `CallToolResult`, then drive the
//! route via `tower::ServiceExt::oneshot` and assert on:
//!
//!   * Happy path — the route returns 201 and a Card row exists with
//!     `kind == "ui://stub/status"` + `payload == {"msg":"hi"}`.
//!   * Missing `_meta.ui.resourceUri` — 422 with `code: "not_a_card_tool"`.
//!   * Manifest lacks `permissions.cards_create` — 403.
//!
//! Per the migration doc §6/M2 acceptance: "a stub plugin exposes a tool
//! returning `_meta.ui.resourceUri`; hit the REST route; assert the response
//! is 200 and a Card row exists with `kind == ui://stub/status`."

#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::Repo;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewCove, NewWave};
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use calm_server::routes;
use calm_server::state::{AppState, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};
use tower::ServiceExt;

const TOOLCALL_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-toolcall");

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// Test fixture: an `AppState` wired to a real `PluginHost`, an in-memory
/// `SqlxRepo` pre-seeded with one cove + one wave, and one installed `stub-toolcall`
/// plugin with configurable env vars (mode / resource_uri / structured).
struct Fixture {
    state: AppState,
    wave_id: String,
    plugin_id: String,
    _tmp: tempfile::TempDir,
}

struct StubConfig<'a> {
    plugin_id: &'a str,
    mode: &'a str,
    cards_create: bool,
}

async fn boot(cfg: StubConfig<'_>) -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let install_dir = plugins_dir.join(cfg.plugin_id);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();
    std::os::unix::fs::symlink(Path::new(TOOLCALL_BIN), bin_dir.join("stub")).unwrap();

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

    // Pass STUB_TOOLCALL_MODE via manifest env so the stub child reads it.
    let perms = if cfg.cards_create {
        json!({
            "overlays_write": [],
            "cards_create": true,
            "cards_read_all": false,
            "events_subscribe": []
        })
    } else {
        // cards_create defaults to false — used by the 403 test.
        json!({})
    };
    let manifest_json = json!({
        "manifest_version": 1,
        "id": cfg.plugin_id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "Tool-call stub",
        "entrypoint": {
            "command": "bin/stub",
            "env": { "STUB_TOOLCALL_MODE": cfg.mode }
        },
        "permissions": perms
    });
    let manifest: Manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest");

    let registry = PluginRegistry::empty();
    registry.insert(manifest, Some(install_dir.clone()));
    let events = EventBus::new();
    // Seed plugin row so plugin_token_set's FK is satisfied at spawn time.
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

    plugin_host.spawn(cfg.plugin_id).await.expect("spawn");
    wait_for_running(&plugin_host, cfg.plugin_id).await;

    let state = AppState {
        repo,
        events,
        daemon: Arc::new(DaemonClient::new_stub()),
        plugin: plugin_host,
        codex: Arc::new(calm_server::state::CodexClient::new_stub()),
    };

    Fixture {
        state,
        wave_id: wave.id,
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
    // Scope G: the cards router pulls `Actor` from extensions, so the
    // middleware that populates it must be present. Mirror main.rs.
    axum::Router::new()
        .merge(routes::cards::router())
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state)
}

async fn body_to_json(resp: axum::http::Response<Body>) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn post_create(app: axum::Router, wave_id: &str, body: Value) -> axum::http::Response<Body> {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn via_tool_call_creates_card_with_ui_resource_uri() {
    let fx = boot(StubConfig {
        plugin_id: "test.toolcall.ok",
        mode: "card",
        cards_create: true,
    })
    .await;

    let resp = post_create(
        app(fx.state.clone()),
        &fx.wave_id,
        json!({
            "via_tool_call": {
                "plugin_id": fx.plugin_id,
                "tool_name": "make_status_card",
                "arguments": { "ignored": true }
            }
        }),
    )
    .await;

    assert_eq!(resp.status(), StatusCode::CREATED, "expected 201 Created");
    let body = body_to_json(resp).await;
    assert_eq!(body["kind"], "ui://stub/status");
    assert_eq!(body["wave_id"], fx.wave_id);
    assert_eq!(body["payload"], json!({ "msg": "hi" }));

    // Also confirm the row landed via the repo, not just echoed back.
    let cards = fx
        .state
        .repo
        .cards_by_wave(&fx.wave_id)
        .await
        .expect("list");
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].kind, "ui://stub/status");
    assert_eq!(cards[0].payload, json!({ "msg": "hi" }));

    fx.state.plugin.stop(&fx.plugin_id).await.ok();
}

#[tokio::test]
async fn via_tool_call_returns_422_when_meta_ui_resource_uri_absent() {
    let fx = boot(StubConfig {
        plugin_id: "test.toolcall.nouri",
        mode: "no_uri",
        cards_create: true,
    })
    .await;

    let resp = post_create(
        app(fx.state.clone()),
        &fx.wave_id,
        json!({
            "via_tool_call": {
                "plugin_id": fx.plugin_id,
                "tool_name": "not_a_card_tool",
                "arguments": {}
            }
        }),
    )
    .await;

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_to_json(resp).await;
    assert_eq!(body["code"], "not_a_card_tool");

    // No row should have been inserted on the failed path.
    let cards = fx
        .state
        .repo
        .cards_by_wave(&fx.wave_id)
        .await
        .expect("list");
    assert!(cards.is_empty(), "no card should be created on 422 path");

    fx.state.plugin.stop(&fx.plugin_id).await.ok();
}

#[tokio::test]
async fn via_tool_call_returns_403_when_cards_create_not_granted() {
    let fx = boot(StubConfig {
        plugin_id: "test.toolcall.noperm",
        mode: "card",
        cards_create: false,
    })
    .await;

    let resp = post_create(
        app(fx.state.clone()),
        &fx.wave_id,
        json!({
            "via_tool_call": {
                "plugin_id": fx.plugin_id,
                "tool_name": "make_status_card",
                "arguments": {}
            }
        }),
    )
    .await;

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = body_to_json(resp).await;
    assert_eq!(body["code"], "plugin_permission");

    let cards = fx
        .state
        .repo
        .cards_by_wave(&fx.wave_id)
        .await
        .expect("list");
    assert!(cards.is_empty());

    fx.state.plugin.stop(&fx.plugin_id).await.ok();
}
