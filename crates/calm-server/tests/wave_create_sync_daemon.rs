//! Issue #236 (closes) — `POST /api/waves` must spawn the spec card's
//! codex daemon **synchronously** before returning 201.
//!
//! ## Why
//!
//! Pre-fix: the route returned 201 the instant the wave + spec card +
//! terminal-row tx committed, and `seed_and_spawn_spec_daemon` was
//! fired through `tokio::spawn`. That opened a ~400 ms race window in
//! which the frontend could open the spec card's WS (which goes
//! through `ws::terminal::resolve_live_sock`), see
//! `daemon_handle = None` on the terminal row, and trigger the
//! revive-by-respawn path with the row's **baked env** — which omits
//! `NEIGE_MCP_SOCKET` / `NEIGE_MCP_TOKEN` (those are folded in only
//! at the original `spawn_daemon_for` call site). Result: two daemons
//! race on the same `--sock` path and the WS attaches to the
//! no-MCP one, breaking the codex MCP handshake.
//!
//! Post-fix: by the time 201 reaches the client, `daemon_handle` on
//! the spec card's terminal row is `Some(<sock>)`, the socket exists
//! on disk, and a subsequent WS attach never hits the respawn branch.
//!
//! ## Test design
//!
//! We use the real `calm-session-daemon` binary (the same one
//! `tests/codex_card_endpoint.rs` and `tests/ws_terminal_e2e.rs`
//! locate). The spec card's `program` is hard-coded to `"codex"` by
//! `seed_and_spawn_spec_daemon`; there's no `codex` binary in CI, so
//! `/bin/sh -c codex` will fail-fast inside the daemon child. That's
//! fine — `spawn_daemon_for` waits for the *daemon* socket to accept,
//! not for the spawned program to stay alive. The socket binds before
//! the daemon execs the child, so the wait-for-socket loop completes
//! and `terminal_set_handle` lands.
//!
//! Assertions:
//!   1. `POST /api/waves` returns 201 (synchronous spawn succeeded).
//!   2. The spec card's terminal row has `daemon_handle = Some(_)`.
//!   3. The socket file exists on disk at that path.
//!   4. A second `terminal_get` immediately after the response (the
//!      shape `ws::terminal::resolve_live_sock` would see) does NOT
//!      observe `daemon_handle = None`, i.e. the race window is
//!      closed.

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
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

/// Same daemon-locator as `codex_card_endpoint.rs` /
/// `codex_hands_free.rs` — workspace bins live one dir up from the
/// per-test `deps/` directory.
fn locate_daemon_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop();
    p.pop();
    p.push("calm-session-daemon");
    assert!(
        p.exists(),
        "calm-session-daemon not found at {p:?}; run \
         `cargo build -p calm-session --bin calm-session-daemon` first, or \
         use `cargo test --workspace` which builds workspace bins",
    );
    p
}

struct Boot {
    app: axum::Router,
    cove_id: String,
    repo: Arc<dyn Repo>,
    card_role_cache: CardRoleCache,
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
            name: "sync-daemon-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        session_daemon_bin: locate_daemon_bin(),
    });
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let state = AppState::from_parts(
        repo.clone(),
        events,
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-sync-daemon-test"),
            Vec::new(),
            EventBus::new(),
            card_role_cache.clone(),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache.clone()),
    );

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    Boot {
        app,
        cove_id: cove.id.to_string(),
        repo,
        card_role_cache,
        _tmp: tmp,
    }
}

async fn post(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
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
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// Verify: after `POST /api/waves` returns 201, the spec card's
/// terminal row has a non-None `daemon_handle` AND the socket file
/// exists. This is the post-#236 contract — no race window.
#[tokio::test]
async fn post_api_waves_spec_terminal_has_daemon_handle_before_response() {
    let boot = boot().await;

    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({"cove_id": boot.cove_id, "title": "sync-spawn wave"}),
    )
    .await;
    // Real daemon binary → spawn succeeds (the daemon binds its socket
    // before exec'ing the inner program; the inner `/bin/sh -c codex`
    // will fail because no codex in CI, but that's after the
    // daemon-side wait-for-socket completes).
    assert_eq!(
        status,
        StatusCode::CREATED,
        "wave create returns 201 when daemon spawn succeeds synchronously; body={body}",
    );

    // Drill down to the spec card the route minted.
    let waves = boot.repo.waves_by_cove(&boot.cove_id).await.unwrap();
    assert_eq!(waves.len(), 1);
    let wave = waves.into_iter().next().unwrap();
    let cards = boot.repo.cards_by_wave(wave.id.as_str()).await.unwrap();
    assert_eq!(cards.len(), 1, "exactly one spec card per wave at create");
    let spec_card_id = cards[0].id.clone();

    // Sanity: the role cache shows Spec (pre-existing PR6 invariant,
    // assertion left here so a future regression flips both signals
    // at the same site).
    assert_eq!(
        boot.card_role_cache.get(&spec_card_id),
        Some(calm_server::model::CardRole::Spec),
    );

    // The #236 contract: by the time the 201 response has reached
    // the test, the terminal row carries the daemon handle. The
    // pre-fix shape had `daemon_handle = None` here (the background
    // `tokio::spawn` task had not yet won the race against the
    // route returning), which is what `ws::terminal::resolve_live_sock`
    // was tripping on.
    let term = boot
        .repo
        .terminal_get_by_card(spec_card_id.as_str())
        .await
        .unwrap()
        .expect("spec terminal row exists");
    let handle = term.daemon_handle.as_deref().unwrap_or_else(|| {
        panic!(
            "spec card's terminal row must carry a daemon_handle by the time 201 returns \
             (issue #236 sync-spawn contract); got None on row {:?}",
            term
        )
    });

    // Socket file must exist on disk: `spawn_daemon_for`'s wait loop
    // requires `UnixStream::connect(sock)` to succeed before writing
    // the handle, so absence here would mean the row was set without
    // the underlying socket.
    let sock_path = std::path::Path::new(handle);
    assert!(
        sock_path.exists(),
        "daemon socket file must exist on disk at {sock_path:?} (per spawn_daemon_for's \
         wait-for-socket contract); row handle={handle}",
    );

    // PID also persisted (`spawn_daemon_for` set it before the
    // wait-for-socket loop). Best-effort assertion; absence would be
    // a separate degradation but not the #236 bug.
    assert!(
        term.pid.is_some(),
        "spawn_daemon_for should have persisted pid for sweeper SIGTERM fallback; \
         row = {term:?}",
    );
}

/// Regression test for the WS revive path: immediately after `POST
/// /api/waves`, the shape `ws::terminal::resolve_live_sock` sees on a
/// fresh terminal row must NOT trigger its respawn branch. Pre-#236
/// this lookup returned `daemon_handle = None` and the revive code
/// path would respawn from `term.env` (missing MCP vars). Post-#236
/// the lookup always returns `Some`.
#[tokio::test]
async fn ws_revive_path_does_not_trigger_respawn_for_freshly_created_wave() {
    let boot = boot().await;

    let (status, _) = post(
        boot.app.clone(),
        "/api/waves",
        json!({"cove_id": boot.cove_id, "title": "ws-race wave"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Mirror `resolve_live_sock`'s lookup: it does
    // `repo.terminal_get(id)` synchronously off the WS upgrade
    // handler. We resolve the spec terminal id via card_id (the WS
    // upgrade URL is /api/terminals/:id, where :id is the terminal
    // id; here we go via card-id for test ergonomics — the row is
    // the same).
    let waves = boot.repo.waves_by_cove(&boot.cove_id).await.unwrap();
    let wave = waves.into_iter().next().unwrap();
    let cards = boot.repo.cards_by_wave(wave.id.as_str()).await.unwrap();
    let spec_card_id = cards[0].id.clone();
    let term = boot
        .repo
        .terminal_get_by_card(spec_card_id.as_str())
        .await
        .unwrap()
        .expect("spec terminal row");

    // The branch in `resolve_live_sock`:
    //   if let Some(handle) = term.daemon_handle.as_ref() { ... } else {
    //       /* respawn from term.env — the #236 bug */
    //   }
    //
    // Pre-#236 the `else` branch fired with #236-shaped probability
    // (~400 ms after commit). Post-#236 `daemon_handle` is always
    // `Some` here.
    assert!(
        term.daemon_handle.is_some(),
        "freshly-created spec card's terminal must carry daemon_handle so \
         ws::terminal::resolve_live_sock never enters the respawn branch \
         (issue #236); row = {term:?}",
    );

    // Also verify the env baked into the terminal row is the
    // pre-MCP shape (matches `routes::waves::create_wave`'s comment
    // about env-augmentation happening only at spawn time). If a
    // future PR persists MCP vars into the row, this assertion
    // becomes stale and should be re-evaluated together with the
    // #236 follow-up.
    let env_obj = term.env.as_object().expect("env is an object");
    assert!(
        !env_obj.contains_key("NEIGE_MCP_TOKEN"),
        "terminal-row env is pre-MCP shape today; got: {:?}",
        env_obj.keys().collect::<Vec<_>>(),
    );
    assert!(
        !env_obj.contains_key("NEIGE_MCP_SOCKET"),
        "terminal-row env is pre-MCP shape today; got: {:?}",
        env_obj.keys().collect::<Vec<_>>(),
    );
}
