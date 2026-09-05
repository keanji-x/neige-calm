//! Issue #1505 S4-2 — `GET /api/models`.
//!
//! Every test drives the real `SharedCodexAppServer`, the real
//! `CodexAppServer` WebSocket-over-UDS client, and a fake `codex app-server`
//! child process that answers `model/list` / `config/read` from sidecar files
//! next to its listen socket (`tests/fixtures/osc-probe-child/appserver.rs`).
//! Nothing in the assertions re-implements what the handler computes.
//!
//! **Which router each test mounts, precisely.** The behaviour tests mount
//! `routes::router()` plus `actor_middleware` — that is the whole handler
//! path but NOT the session gate, so they say nothing about authentication.
//! `models_read_is_behind_the_session_gate` is the one that mounts the
//! production assembly `routes::application_router`, and it is the only thing
//! standing between `models::router()` and a future move into
//! `public_router()`.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{AuthConfig, AuthState, SESSION_COOKIE};
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
    /// `<sock>.methods` — every JSON-RPC method the fake daemon received.
    methods_path: PathBuf,
    /// The same state `app` is built over, so a test can re-mount it behind
    /// the production assembly (`routes::application_router`).
    state: AppState,
    card_id: String,
    home: Arc<SharedCodexHome>,
    _tmp: TempDir,
}

impl Boot {
    fn methods_seen(&self) -> Vec<String> {
        std::fs::read_to_string(&self.methods_path)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
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
        .with_state(state.clone());

    Boot {
        app,
        methods_path: sock.with_extension("methods"),
        state,
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
    // Upper bound, not a stopwatch: a paused clock also advances whenever the
    // runtime parks on real IO. It is meaningful here because this request
    // issues exactly one frame and then has nothing but our deadline to wait
    // on. See `config_read_shares_the_one_request_budget` for why a test with
    // two reads cannot assert on virtual elapsed at all.
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

// ---------------------------------------------------------------------------
// Design test 13 (GET half) — the endpoint sits behind the session gate.
// ---------------------------------------------------------------------------

/// UNIQUELY PINS: `GET /api/models` is reachable only with a session.
///
/// Every other test in this file mounts `routes::router()`, which merges the
/// protected, internal and public trees with no gate at all — so none of them
/// can tell whether this route is protected. This one mounts the production
/// assembly, which is the only place `require_session` exists. Without it,
/// moving `.merge(models::router())` into `public_router()` would be green.
#[tokio::test]
async fn models_read_is_behind_the_session_gate() {
    let boot = boot(false, |_sock| {}).await;
    let auth = AuthState::new(AuthConfig {
        username: Some("owner".into()),
        password: Some("pw".into()),
        dev_autologin: false,
        display_name: "owner".into(),
    });
    let app = routes::application_router(boot.state.clone(), auth);

    let anonymous = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        anonymous.status(),
        StatusCode::UNAUTHORIZED,
        "a cookieless read must be refused by the session gate, not served"
    );

    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "username": "owner", "password": "pw" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK, "login must succeed");
    let raw = login
        .headers()
        .get(header::SET_COOKIE)
        .expect("Set-Cookie present")
        .to_str()
        .expect("ascii");
    let cookie = raw.split(';').next().unwrap().to_string();
    assert!(cookie.starts_with(&format!("{SESSION_COOKIE}=")));

    let authenticated = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/models")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        authenticated.status(),
        StatusCode::OK,
        "the same read must succeed once a session exists"
    );
}

// ---------------------------------------------------------------------------
// The default source hangs on the CONNECTION, not on `model/list` succeeding.
// ---------------------------------------------------------------------------

/// UNIQUELY PINS: a live daemon whose catalog we cannot read must still have
/// its default resolved through `config/read`.
///
/// `source` collapses three facts into `unavailable` (no connection, an
/// RPC/decode failure, a timeout). Deriving the default branch from it would
/// send a *connected* installation to `config.toml` — the source the design
/// calls the wrong one, because a managed-config layer merges above the user
/// layer and can name a different model. The `config.toml` written below is
/// the decoy: if it ever shows up in the answer, we reported a model this
/// installation does not follow.
#[tokio::test]
async fn default_stays_on_config_read_when_the_catalog_page_is_undecodable() {
    let boot = boot(true, |sock| {
        // Well-formed JSON, wrong shape: `data` is not an array, so the page
        // itself fails to decode and no per-entry tolerance can rescue it.
        std::fs::write(
            sock.with_extension("model-list"),
            json!({ "data": "not-a-list", "nextCursor": null }).to_string(),
        )
        .unwrap();
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
    std::fs::write(
        boot.home.path().join("config.toml"),
        "model = \"decoy-from-config-toml\"\n",
    )
    .unwrap();

    let (status, body) = get_models(&boot.app, &format!("?card_id={}", boot.card_id)).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["source"], "unavailable",
        "the catalog really is unreadable, and that half must say so"
    );
    assert_eq!(
        body["default_source"], "config_read",
        "the daemon is connected, so the default still comes from the merged layers"
    );
    assert_eq!(body["default"]["model"], "gpt-5-codex");
    assert_ne!(
        body["default"]["model"], "decoy-from-config-toml",
        "a connected daemon must never be answered from our own config.toml layer"
    );
}

/// UNIQUELY PINS: one preset this build cannot decode costs that preset, not
/// the catalog.
///
/// An all-or-nothing page decode would render identically to a dormant daemon
/// — empty list, "codex is not running" — while codex is running turns. Codex
/// ships five `#[serde(default)]` attributes on its own `Model` precisely
/// because this catalog is expected to grow.
#[tokio::test]
async fn one_undecodable_preset_does_not_empty_the_catalog() {
    let boot = boot(true, |sock| {
        let mut page = catalog();
        // Drop a required field from the FIRST entry only.
        page["data"][0]
            .as_object_mut()
            .unwrap()
            .remove("description");
        std::fs::write(sock.with_extension("model-list"), page.to_string()).unwrap();
    })
    .await;

    let (status, body) = get_models(&boot.app, "").await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["source"], "live",
        "codex answered; a preset we cannot read is not an outage"
    );
    assert_eq!(
        body["models"].as_array().unwrap().len(),
        1,
        "the readable preset must survive its malformed neighbour"
    );
    assert_eq!(body["models"][0]["model"], "gpt-5-pro");
}

/// A complete `config/read` whose merged config sets no model is `config_read`
/// + `null`, not `unknown`: we read it successfully and the answer is "nothing
/// configured". Codex builds that member from the *effective* merged layers,
/// so an unset `model` there is a real state, not a gap in our read.
#[tokio::test]
async fn an_unset_model_in_config_read_is_a_read_default_not_unknown() {
    let boot = boot(true, |sock| {
        std::fs::write(sock.with_extension("model-list"), catalog().to_string()).unwrap();
        std::fs::write(
            sock.with_extension("config-read"),
            json!({ "config": {}, "origins": {} }).to_string(),
        )
        .unwrap();
    })
    .await;

    let (status, body) = get_models(&boot.app, &format!("?card_id={}", boot.card_id)).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["default_source"], "config_read");
    assert_eq!(body["default"]["model"], Value::Null);
    assert_eq!(body["default"]["reasoning_effort"], Value::Null);
}

// ---------------------------------------------------------------------------
// Pagination, the whole-request budget, and the 404 path.
// ---------------------------------------------------------------------------

/// UNIQUELY PINS: the pagination cursor is actually followed.
///
/// Nothing else in this suite pages — the default fixture answers
/// `"nextCursor": null` — so replacing the drain loop with "return after the
/// first page" was previously green across the whole file.
#[tokio::test]
async fn model_list_pagination_cursor_is_followed() {
    let boot = boot(true, |sock| {
        let mut first = catalog();
        let second = json!({ "data": [first["data"][1].clone()], "nextCursor": null });
        first["data"] = json!([first["data"][0].clone()]);
        first["nextCursor"] = json!("page-2");
        std::fs::write(sock.with_extension("model-list"), first.to_string()).unwrap();
        std::fs::write(sock.with_extension("model-list-page-2"), second.to_string()).unwrap();
    })
    .await;

    let (status, body) = get_models(&boot.app, "").await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["models"].as_array().unwrap().len(),
        2,
        "the second page must be fetched and appended, not dropped"
    );
    assert_eq!(body["models"][0]["model"], "gpt-5-codex");
    assert_eq!(
        body["models"][1]["model"], "gpt-5-pro",
        "the entry that only exists on page 2 must be present"
    );
}

/// UNIQUELY PINS: a peer that never clears `nextCursor` is an error, not a
/// silently truncated catalog.
///
/// Answering `Ok` with the pages collected so far would present an incomplete
/// list as a complete one — a model the account really has would just be
/// missing from the picker, with nothing in the response saying so.
#[tokio::test]
async fn endless_pagination_degrades_instead_of_truncating() {
    let boot = boot(true, |sock| {
        // Every page points at itself, so the cursor never clears.
        std::fs::write(
            sock.with_extension("model-list"),
            json!({ "data": [], "nextCursor": "forever" }).to_string(),
        )
        .unwrap();
        std::fs::write(
            sock.with_extension("model-list-forever"),
            json!({ "data": [], "nextCursor": "forever" }).to_string(),
        )
        .unwrap();
    })
    .await;

    let (status, body) = get_models(&boot.app, "").await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["source"], "unavailable",
        "a catalog we could not finish reading must not be labelled `live`"
    );
}

/// UNIQUELY PINS: `CODEX_READ_TIMEOUT` is one budget for the **whole
/// request**, not one per codex read. It is also the only coverage of the
/// `config/read` bound at all.
///
/// The evidence is deliberately NOT a stopwatch. Under `tokio::time::pause()`
/// the clock auto-advances whenever the runtime parks, and this harness parks
/// on a real socket and on the daemon supervisor's own timers — an earlier
/// version of this test measured 12.9s and then 14.0s of virtual time for an
/// 8s budget, none of it spent waiting on our deadlines. Virtual elapsed is
/// not a valid measurement here, so the assertion is on the wire instead:
/// with one shared budget the catalog read spends it and `config/read` is
/// never issued at all. Two independent budgets would put that frame on the
/// socket.
#[tokio::test]
async fn config_read_shares_the_one_request_budget() {
    let boot = boot(true, |sock| {
        std::fs::write(sock.with_extension("model-list-no-answer"), "1").unwrap();
        std::fs::write(sock.with_extension("config-read-no-answer"), "1").unwrap();
    })
    .await;
    let app = boot.app.clone();
    let query = format!("?card_id={}", boot.card_id);

    // No timing assertion at all, deliberately. A virtual-clock race against
    // this request was measured to be insensitive to the budget constant
    // (shortening `CODEX_READ_TIMEOUT` to 3s left it green), because the
    // paused clock advances on parked socket IO rather than on our deadlines.
    // An assertion that cannot fail is worse than no assertion: the evidence
    // below is on the wire.
    tokio::time::pause();
    let (status, body) = get_models(&app, &query).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["source"], "unavailable");
    assert_eq!(
        body["default_source"], "unknown",
        "a default we could not read is `unknown`, never a guess"
    );

    let methods = boot.methods_seen();
    assert!(
        methods.iter().any(|m| m == "model/list"),
        "positive control: the catalog read must have reached the daemon, got {methods:?}"
    );
    assert!(
        !methods.iter().any(|m| m == "config/read"),
        "the catalog read spent the whole request budget, so `config/read` must \
         never have been issued — seeing it means the second read was handed a \
         fresh timer and the endpoint's real bound is 16s, not 8s. Saw {methods:?}"
    );

    drop(boot);
}

/// A `card_id` naming no card is a malformed request and answers 404 — the
/// documented narrowing of the design's "always 200". Silently degrading it to
/// `unknown` would hide a caller bug.
#[tokio::test]
async fn an_unknown_card_id_is_a_404() {
    let boot = boot(false, |_sock| {}).await;

    let (status, _body) = get_models(&boot.app, "?card_id=no-such-card").await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A blank `card_id=` is what a UI sends with nothing selected. It means "no
/// card", not "a card that is missing", so it takes the cardless path rather
/// than 404ing.
#[tokio::test]
async fn a_blank_card_id_takes_the_cardless_path() {
    let boot = boot(true, |sock| {
        std::fs::write(sock.with_extension("model-list"), catalog().to_string()).unwrap();
    })
    .await;

    let (status, body) = get_models(&boot.app, "?card_id=").await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["default_source"], "unknown");
    assert_eq!(body["source"], "live");
}
