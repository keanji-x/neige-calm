//! Issue #1505 S4-2 — `GET /api/models`.
//!
//! Every test here drives the production wiring end to end: the real axum
//! router, the real `SharedCodexAppServer`, the real `CodexAppServer`
//! WebSocket-over-UDS client, and a fake `codex app-server` child process
//! that answers `model/list` / `config/read` from sidecar files next to its
//! listen socket (`tests/fixtures/osc-probe-child/appserver.rs`). Nothing in
//! the assertions re-implements what the handler computes.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use calm_server::config::Config;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewArea, NewCard, NewTrack};
use calm_server::pending_codex_threads::PendingThreadStartRegistry;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::shared_codex_home::SharedCodexHome;
use calm_server::state::{AppState, DaemonClient};
use clap::Parser;
use common::{fake_codex_bin, fake_codex_client};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    card_id: String,
    home: Arc<SharedCodexHome>,
    _tmp: TempDir,
}

fn cfg(root: &TempDir) -> Config {
    Config::parse_from([
        "calm-server",
        "--data-dir",
        root.path().to_str().unwrap(),
        "--codex-bin",
        &fake_codex_bin(),
        "--shared-codex-appserver-restart-initial-delay-ms",
        "10",
        "--shared-codex-appserver-restart-max-delay-ms",
        "50",
        "--shared-codex-appserver-start-timeout-secs",
        "3",
    ])
}

/// Build the whole REST surface over a real (or deliberately unstarted)
/// shared codex daemon.
///
/// `scripted` is applied to the sidecar files **before** the daemon boots, so
/// the fixture's first `model/list` already sees them.
async fn boot(start_daemon: bool, scripted: impl FnOnce(&PathBuf)) -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let area = repo
        .area_create(NewArea {
            name: "models".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "models".into(),
            sort: None,
            cwd: "/workspace".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();

    let events = EventBus::new();
    let cfg = cfg(&tmp);
    let sock = cfg.data_dir_resolved().join("run/codex-appserver.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    scripted(&sock);

    let home = Arc::new(SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    ));
    home.seed_from(None).unwrap();

    let mut state = AppState::from_parts(
        repo_dyn.clone(),
        events.clone(),
        Arc::new(DaemonClient {
            data_dir: tmp.path().join("terminals"),
            proc_supervisor_sock: std::env::var_os("CALM_TEST_PROC_SUPERVISOR_SOCK")
                .map(PathBuf::from),
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo_dyn.clone(),
            PathBuf::new(),
            tmp.path().join("plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
        )),
        Arc::new(fake_codex_client()),
        None,
        None,
    );

    let pending = Arc::new(PendingThreadStartRegistry::new(
        repo_dyn.clone(),
        events.clone(),
    ));
    let shared =
        SharedCodexAppServer::new_with_pending(&cfg, home.clone(), repo_dyn, Some(pending.clone()));
    if start_daemon {
        shared.start_or_takeover().await.expect("boot fake daemon");
    }
    state = state.with_shared_codex_appserver(shared);
    state = state.with_pending_codex_threads(pending);

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    Boot {
        app,
        card_id: card.id.to_string(),
        home,
        _tmp: tmp,
    }
}

async fn get_models(app: &axum::Router, query: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/models{query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

/// Two entries whose `isDefault` deliberately sits on the model that
/// `config/read` does NOT name, and whose preset `id` differs from the slug.
fn catalog() -> Value {
    json!({
        "data": [
            {
                "id": "preset-codex",
                "model": "gpt-5-codex",
                "displayName": "GPT-5 Codex",
                "description": "codex preset",
                "hidden": false,
                "supportedReasoningEfforts": [
                    { "reasoningEffort": "medium", "description": "balanced" },
                    { "reasoningEffort": "brand-new-effort", "description": "not a known variant" }
                ],
                "defaultReasoningEffort": "medium",
                "isDefault": false
            },
            {
                "id": "preset-pro",
                "model": "gpt-5-pro",
                "displayName": "GPT-5 Pro",
                "description": "pro preset",
                "hidden": false,
                "supportedReasoningEfforts": [
                    { "reasoningEffort": "high", "description": "slow" }
                ],
                "defaultReasoningEffort": "high",
                "isDefault": true
            }
        ],
        "nextCursor": null
    })
}

// ---------------------------------------------------------------------------
// Design test 8 — no daemon: 200, `unavailable`, empty catalog, and a default
// that still comes back (from our own `config.toml`).
// ---------------------------------------------------------------------------

/// UNIQUELY PINS: an unreachable codex must not become an error and must not
/// become an invented catalog, and `default` / `default_source` must survive
/// the outage — S4-4 disables the picker on `unavailable` and has nothing
/// left to show if the default disappears with the list.
#[tokio::test]
async fn models_read_reports_unavailable_without_a_daemon() {
    let boot = boot(false, |_sock| {}).await;
    std::fs::write(
        boot.home.path().join("config.toml"),
        "model = \"gpt-5-codex\"\nmodel_reasoning_effort = \"high\"\n",
    )
    .unwrap();

    let (status, body) = get_models(&boot.app, "").await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["source"], "unavailable");
    assert_eq!(
        body["models"],
        json!([]),
        "an unreachable codex must yield an empty catalog, never a hardcoded one"
    );
    assert_eq!(body["default"]["model"], "gpt-5-codex");
    assert_eq!(body["default"]["reasoning_effort"], "high");
    assert_eq!(body["default_source"], "config_toml");
    assert_eq!(body["fetched_at_ms"], Value::Null);
}

// ---------------------------------------------------------------------------
// Design test 9 — `default` comes from `config/read`, never from `isDefault`.
// ---------------------------------------------------------------------------

/// UNIQUELY PINS: the two "default"s are different questions. Codex's
/// `isDefault` marks the picker's highlighted entry; the value this
/// installation actually follows is the layer-merged `config/read`. It also
/// pins the snake_case key spelling inside `config` (`ConfigReadResponse` is
/// camelCase, the `Config` it wraps is not).
#[tokio::test]
async fn models_read_takes_the_default_from_config_read_not_is_default() {
    let boot = boot(true, |sock| {
        std::fs::write(sock.with_extension("model-list"), catalog().to_string()).unwrap();
        std::fs::write(
            sock.with_extension("config-read"),
            json!({
                "config": { "model": "gpt-5-codex", "model_reasoning_effort": "medium" },
                "origins": {}
            })
            .to_string(),
        )
        .unwrap();
    })
    .await;

    let (status, body) = get_models(&boot.app, &format!("?card_id={}", boot.card_id)).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["source"], "live");
    assert_eq!(body["default_source"], "config_read");
    assert_eq!(body["default"]["model"], "gpt-5-codex");
    assert_ne!(
        body["default"]["model"], body["models"][1]["model"],
        "`default` must not be the entry codex flagged `isDefault`"
    );
    assert_eq!(body["models"][1]["is_default"], true);
    assert_eq!(
        body["default"]["reasoning_effort"], "medium",
        "`config.model_reasoning_effort` is snake_case inside a camelCase envelope"
    );
}

/// UNIQUELY PINS: what travels on our wire as a selectable model is the
/// **slug**, not the preset id, and an unknown reasoning-effort string
/// survives the round trip instead of being rejected by a closed enum.
#[tokio::test]
async fn models_read_carries_the_slug_and_passes_unknown_efforts_through() {
    let boot = boot(true, |sock| {
        std::fs::write(sock.with_extension("model-list"), catalog().to_string()).unwrap();
    })
    .await;

    let (status, body) = get_models(&boot.app, "").await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["models"][0]["model"], "gpt-5-codex");
    assert_ne!(
        body["models"][0]["model"], body["models"][0]["id"],
        "the slug must be reported separately from the preset id"
    );
    assert_eq!(body["models"][0]["id"], "preset-codex");
    assert_eq!(
        body["models"][0]["supported_reasoning_efforts"][1]["reasoning_effort"], "brand-new-effort",
        "an effort value this build has never heard of must survive verbatim"
    );
}

/// §3.3(e): a connected daemon that answers with an empty catalog is NOT the
/// same fact as an unreachable one, and the response has to say so.
#[tokio::test]
async fn models_read_distinguishes_an_empty_live_catalog_from_an_outage() {
    let boot = boot(true, |sock| {
        std::fs::write(
            sock.with_extension("model-list"),
            json!({ "data": [], "nextCursor": null }).to_string(),
        )
        .unwrap();
    })
    .await;

    let (status, body) = get_models(&boot.app, "").await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["source"], "live");
    assert_eq!(body["models"], json!([]));
}

/// Without a card there is no workspace, so there are no project layers to
/// resolve and the endpoint must say `unknown` rather than pass a global-layer
/// value off as this card's default.
#[tokio::test]
async fn models_read_reports_unknown_default_without_a_card() {
    let boot = boot(true, |sock| {
        std::fs::write(sock.with_extension("model-list"), catalog().to_string()).unwrap();
        std::fs::write(
            sock.with_extension("config-read"),
            json!({ "config": { "model": "gpt-5-codex" }, "origins": {} }).to_string(),
        )
        .unwrap();
    })
    .await;

    let (status, body) = get_models(&boot.app, "").await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["default_source"], "unknown");
    assert_eq!(body["default"]["model"], Value::Null);
}

// ---------------------------------------------------------------------------
// Design test 12 — the call-site timeout.
// ---------------------------------------------------------------------------

/// UNIQUELY PINS: `model/list` is bounded at the call site, and the bound is
/// long enough that codex's own 5 s catalog-fetch ceiling gets to answer
/// first.
///
/// The clock is paused *after* the daemon has booted, so the real socket work
/// is done under real time and only the two deadlines below are virtual.
#[tokio::test]
async fn model_list_is_bounded_at_the_call_site() {
    let boot = boot(true, |sock| {
        std::fs::write(sock.with_extension("model-list-no-answer"), "1").unwrap();
    })
    .await;
    let app = boot.app.clone();

    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let mut request = Box::pin(get_models(&app, ""));

    // Auto-advance jumps to the nearest deadline; racing the request against a
    // 5 s timer therefore asks "was it still waiting at 5 s?" without any
    // wall-clock wait.
    assert!(
        tokio::time::timeout(Duration::from_secs(5), &mut request)
            .await
            .is_err(),
        "the read must still be in flight at 5s — codex's own catalog fetch is \
         capped at 5s and must get to answer before we give up"
    );

    let (status, body) = request.await;
    let elapsed = started.elapsed();

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["source"], "unavailable");
    assert_eq!(body["models"], json!([]));
    assert!(
        elapsed <= Duration::from_millis(8_100),
        "degraded after {elapsed:?}; the call-site bound must fire by 8s \
         rather than fall through to the shared 30s per-request timeout"
    );
    assert!(
        elapsed >= Duration::from_secs(5),
        "degraded after {elapsed:?}"
    );

    drop(boot);
}
