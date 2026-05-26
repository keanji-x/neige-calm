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
use calm_server::state::{AppState, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

mod common;

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
    // #234 (rebase) — WaveCoveCache joined the AppState/PluginHost surface
    // alongside CardRoleCache. Empty seed is fine here: no waves pre-exist
    // in the freshly-opened in-memory repo, and the wave we create through
    // `POST /api/waves` populates the cache write-through via
    // `wave_create_tx`.
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
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
        json!({"cove_id": boot.cove_id, "title": "sync-spawn wave", "cwd": "/tmp/issue-250-pr2-test", "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
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

    // Drill down to the spec card the route minted. Filter by the
    // role cache rather than asserting on `cards.len()` — main may
    // co-mint kernel-owned cards alongside the spec card (issue #229
    // PR B's wave-report card on every wave create is the first such
    // sibling; future system-owned cards are likely). The #236
    // contract is specifically about the SPEC card's terminal row, so
    // pick the spec card by role and ignore the rest of the cohort.
    let waves = boot.repo.waves_by_cove(&boot.cove_id).await.unwrap();
    assert_eq!(waves.len(), 1);
    let wave = waves.into_iter().next().unwrap();
    let cards = boot.repo.cards_by_wave(wave.id.as_str()).await.unwrap();
    let spec_cards: Vec<_> = cards
        .iter()
        .filter(|c| boot.card_role_cache.get(&c.id) == Some(calm_server::model::CardRole::Spec))
        .collect();
    assert_eq!(
        spec_cards.len(),
        1,
        "exactly one Spec-role card per wave at create (cohort = {cards:?})",
    );
    let spec_card_id = spec_cards[0].id.clone();

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

    // Socket file must exist on disk: the recorder fixture only writes
    // `ready\n` after binding, so absence here would mean the row was set
    // without the underlying socket.
    let sock_path = std::path::Path::new(handle);
    assert!(
        sock_path.exists(),
        "daemon socket file must exist on disk at {sock_path:?} (per the ready-fd \
         after-bind contract); row handle={handle}",
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
        json!({"cove_id": boot.cove_id, "title": "ws-race wave", "cwd": "/tmp/issue-250-pr2-test", "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
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
    // Filter by Spec role — see the first test for the rationale. Main
    // may co-mint kernel-owned sibling cards (e.g. wave-report) that
    // share the wave but don't carry the spec card's terminal row.
    let spec_card_id = cards
        .iter()
        .find(|c| boot.card_role_cache.get(&c.id) == Some(calm_server::model::CardRole::Spec))
        .map(|c| c.id.clone())
        .expect("at least one Spec-role card per wave");
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

/// Issue #293 / PR #311 — the spec-push app-server boot is NON-FATAL to
/// wave creation. Every codex-free environment (CI's web a11y job, the
/// chromium docker stack) has no working `codex`, so booting the
/// kernel-owned app-server fails. This MUST NOT 500 the wave create:
/// the route logs a warning and returns **201** with an inert wave (the
/// spec card has no `codex_thread_id` and no parked `spec_push` handle).
///
/// This test boots with a deterministically-broken `codex_bin` (an
/// absolute path that does not exist, so the boot fails fast regardless
/// of whether a real `codex` is on PATH) and asserts:
///   1. `POST /api/waves` returns 201 (boot failure is tolerated),
///   2. the wave + spec card rows are committed,
///   3. the spec card payload has NO `codex_thread_id` / `appserver_sock`
///      (the persist step is skipped on the failure path),
///   4. the `spec_push` registry has NO handle for the wave (inert).
#[tokio::test]
async fn post_api_waves_tolerates_broken_codex_bin_returns_201_inert_wave() {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "broken-codex-tolerant-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        session_daemon_bin: locate_daemon_bin(),
    });
    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();

    // Deterministically-broken codex bin: absolute, absent → the
    // spec-push app-server `spawn` fails fast with
    // "No such file or directory", which is exactly the codex-free CI
    // shape. We capture the `AppState` here (the shared `boot()` helper
    // doesn't expose `spec_push`) so we can probe the registry.
    let mut codex = calm_server::state::CodexClient::new_stub();
    codex.codex_bin = "/nonexistent-codex-bin-tolerant-201-test".into();

    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-broken-codex-test"),
            Vec::new(),
            EventBus::new(),
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        )),
        Arc::new(codex),
        Some(card_role_cache.clone()),
        Some(wave_cove_cache.clone()),
    );
    let spec_push = state.spec_push.clone();

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    let cove_id = cove.id.to_string();
    let (status, body) = post(
        app.clone(),
        "/api/waves",
        json!({"cove_id": cove_id, "title": "inert wave", "cwd": "/tmp/issue-293-tolerant", "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;

    // (1) Boot failure is tolerated → 201, not 500.
    assert_eq!(
        status,
        StatusCode::CREATED,
        "broken codex bin must yield 201 (inert wave), not 500 (issue #293 / PR #311); body={body}",
    );

    // (2) The wave + spec card rows committed.
    let waves = repo.waves_by_cove(&cove_id).await.unwrap();
    assert_eq!(
        waves.len(),
        1,
        "exactly one wave persisted despite boot failure"
    );
    let wave = waves.into_iter().next().unwrap();
    let cards = repo.cards_by_wave(wave.id.as_str()).await.unwrap();
    let spec_card = cards
        .iter()
        .find(|c| card_role_cache.get(&c.id) == Some(calm_server::model::CardRole::Spec))
        .expect("spec card persisted even though the spec agent didn't start");

    // (3) The spec is NOT running: no codex_thread_id / appserver_sock
    // were persisted (those writes live AFTER the boot, on the success
    // path only).
    assert!(
        spec_card
            .payload
            .get("codex_thread_id")
            .is_none_or(Value::is_null),
        "inert wave's spec card must NOT carry a codex_thread_id; payload = {}",
        spec_card.payload,
    );
    assert!(
        spec_card
            .payload
            .get("appserver_sock")
            .is_none_or(Value::is_null),
        "inert wave's spec card must NOT carry an appserver_sock; payload = {}",
        spec_card.payload,
    );

    // (4) No app-server handle parked in the registry for this wave.
    assert!(
        !spec_push.contains(&wave.id),
        "inert wave must have NO parked spec_push handle (registry len = {})",
        spec_push.len(),
    );
    assert!(
        spec_push.is_empty(),
        "no push channels should exist when every boot failed (len = {})",
        spec_push.len(),
    );
}

/// Issue #251 (closes) — the wave's title must be threaded into the
/// spec card so the codex TUI mounts with the goal pre-filled and
/// `codex_auto_submit` fires its `\r` injection.
///
/// Two surfaces under test, both of which must carry the title:
///
///   1. The spec card's `payload.prompt` field. This is what
///      `codex_auto_submit::maybe_submit` reads to decide whether to
///      inject `\r`; before the fix the field was absent and the
///      subscriber short-circuited at the gate, leaving the composer
///      empty and the spec agent waiting forever.
///
///   2. The spec card's `payload.prompt` round-trips through the same
///      `card_with_codex_create_tx` writer plain hands-free codex cards
///      use; trim normalization is part of the writer so an empty /
///      whitespace-only title still falls through to the no-prompt
///      path (the route enforces non-empty title at parse time but
///      defense-in-depth is cheap).
///
/// We do NOT assert on the codex `argv` directly here — the daemon
/// hands `sh -c "codex …"` to `Command::spawn`, and the test harness
/// would need to either ptrace the child or instrument `spawn_daemon_for`
/// to capture it. The payload assertion is the contract that matters at
/// this layer: it's the same shape `codex_hands_free.rs::auto_submit_*`
/// tests use to lock down the auto-submit gate for plain cards.
#[tokio::test]
async fn post_api_waves_threads_title_into_spec_card_prompt_payload() {
    let boot = boot().await;

    let title = "draft the design doc for #251";
    let (status, _body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({"cove_id": boot.cove_id, "title": title, "cwd": "/tmp/issue-250-pr2-test", "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Find the Spec card the route minted.
    let waves = boot.repo.waves_by_cove(&boot.cove_id).await.unwrap();
    let wave = waves.into_iter().next().unwrap();
    let cards = boot.repo.cards_by_wave(wave.id.as_str()).await.unwrap();
    let spec_card = cards
        .iter()
        .find(|c| boot.card_role_cache.get(&c.id) == Some(calm_server::model::CardRole::Spec))
        .expect("exactly one Spec-role card per wave");

    // The #251 contract: `payload.prompt` carries the wave's title
    // (trimmed). `codex_auto_submit`'s gate keys on this exact field
    // shape, so any drift here is the bug coming back.
    let prompt = spec_card
        .payload
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            panic!(
                "spec card payload.prompt must carry the wave title (issue #251); \
                 payload = {}",
                spec_card.payload
            )
        });
    assert_eq!(
        prompt, title,
        "spec card payload.prompt must equal the wave title verbatim; got {prompt:?}",
    );
}

/// Issue #251 — when a wave's title is whitespace-only the spec card
/// must NOT stamp a `payload.prompt` (would otherwise trigger an
/// `\r`-on-empty-composer auto-submit) and the codex command line must
/// fall back to a bare `codex`. The route layer rejects empty titles
/// in production, but the spec_card seed path defenses against an
/// empty title here too so a future loosening of route validation
/// doesn't quietly break the auto-submit gate.
///
/// We can't easily POST a whitespace title through the route (axum's
/// JSON serde + the `NewWave { title: String }` shape accept anything
/// non-null), so this test takes the inner path: it creates a wave row
/// with title = "   " via the repo, then asserts the resulting card
/// shape. The shape assertion uses the same payload-prompt field
/// `codex_auto_submit` keys on.
#[tokio::test]
async fn whitespace_title_does_not_stamp_prompt_on_spec_card() {
    let boot = boot().await;

    // Route accepts and trims the title; assert the post-trim shape.
    let (status, _body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({"cove_id": boot.cove_id, "title": "   ", "cwd": "/tmp/issue-250-pr2-test", "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    // The wave create may still 500 because the daemon child fails to
    // exec `codex` in CI — but the row commit is what we're testing
    // here. Tolerate either 201 (sync spawn happened to win) or 500
    // (daemon-side failure post-commit); both shapes leave the card
    // row behind.
    assert!(
        status == StatusCode::CREATED || status == StatusCode::INTERNAL_SERVER_ERROR,
        "expected 201 or 500 (daemon spawn may fail in CI without codex bin); got {status}",
    );

    let waves = boot.repo.waves_by_cove(&boot.cove_id).await.unwrap();
    let wave = waves.into_iter().next().unwrap();
    let cards = boot.repo.cards_by_wave(wave.id.as_str()).await.unwrap();
    let spec_card = cards
        .iter()
        .find(|c| boot.card_role_cache.get(&c.id) == Some(calm_server::model::CardRole::Spec))
        .expect("exactly one Spec-role card per wave");
    assert!(
        spec_card.payload.get("prompt").is_none_or(Value::is_null),
        "whitespace-only title must NOT stamp payload.prompt; got payload = {}",
        spec_card.payload,
    );
}

// ---------------------------------------------------------------------------
// Issue #250 PR 2 — wave.cwd → spec-daemon cwd contract
// ---------------------------------------------------------------------------

/// PR 2 contract: the spec daemon spawn uses `wave.cwd` (the value
/// the route validated + persisted), not the pre-#250
/// `routes::codex_cards::default_cwd()` fallback.
///
/// Three rows must observe the same cwd at commit time:
///   1. `waves.cwd`               — the wave row's column.
///   2. `terminals.cwd`           — baked into the terminal row from
///      `card_with_codex_create_tx`.
///   3. `cards.payload.cwd`       — surfaced on the spec card so the
///      frontend can render the path hint.
///
/// We can't directly observe the daemon child's `current_dir` because
/// `spawn_daemon_for` passes cwd as a `--cwd` CLI arg to
/// `calm-session-daemon` (not via `Command::current_dir`); capturing
/// argv would require ptrace or instrumenting the spawner. The
/// terminal-row cwd is the equivalent persisted contract — it's what
/// the WS revive path would re-pass on respawn (see
/// `ws::terminal::resolve_live_sock`), so any cwd drift between
/// `wave.cwd` and `terminal.cwd` would surface as a daemon spawned
/// under the wrong directory after a revive.
#[tokio::test]
async fn post_api_waves_uses_wave_cwd_for_spec_daemon() {
    let boot = boot().await;

    let cwd = "/tmp/issue-250-pr2-cwd-contract";
    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": boot.cove_id,
            "title": "cwd-contract wave",
            "cwd": cwd,
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    // Real daemon binary: spawn succeeds (the daemon binds its socket
    // before exec'ing the inner `/bin/sh -c codex`).
    assert_eq!(
        status,
        StatusCode::CREATED,
        "wave create returns 201 when daemon spawn succeeds; body={body}",
    );

    // Wave row carries cwd.
    let waves = boot.repo.waves_by_cove(&boot.cove_id).await.unwrap();
    assert_eq!(waves.len(), 1);
    let wave = waves.into_iter().next().unwrap();
    assert_eq!(wave.cwd, cwd);

    // Terminal row baked the same cwd. This is the value the WS
    // revive path would re-pass to `spawn_daemon_for` on respawn,
    // so drift here would surface as a daemon spawned under the
    // wrong cwd after a daemon death.
    let cards = boot.repo.cards_by_wave(wave.id.as_str()).await.unwrap();
    let spec_card = cards
        .iter()
        .find(|c| boot.card_role_cache.get(&c.id) == Some(calm_server::model::CardRole::Spec))
        .expect("exactly one Spec-role card per wave");
    let term = boot
        .repo
        .terminal_get_by_card(spec_card.id.as_str())
        .await
        .unwrap()
        .expect("spec terminal row");
    assert_eq!(
        term.cwd, cwd,
        "spec card's terminal row must store the wave.cwd (not the \
         pre-#250 default_cwd() = $HOME); got terminal row = {term:?}",
    );

    // Spec card payload.cwd echoes the wave.cwd. This is the field
    // the frontend reads to render a path hint on the card head.
    let payload_cwd = spec_card
        .payload
        .get("cwd")
        .and_then(Value::as_str)
        .expect("spec card payload.cwd carries wave.cwd");
    assert_eq!(payload_cwd, cwd);

    // Folder claim landed inside the same tx (attach_folder = true).
    let folders = boot.repo.cove_folders_by_cove(&boot.cove_id).await.unwrap();
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0].path, cwd);
}

/// Combined coverage: wave.cwd plumbs to the spec daemon AND the
/// wave.title still threads as codex's `[PROMPT]` arg (#251). Pins
/// both contracts on one create call so a regression touching either
/// surface fails here.
#[tokio::test]
async fn post_api_waves_threads_cwd_and_title_together() {
    let boot = boot().await;

    let cwd = "/tmp/issue-250-pr2-combined";
    let title = "do the thing for issue #250";
    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": boot.cove_id,
            "title": title,
            "cwd": cwd,
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {body}");

    let waves = boot.repo.waves_by_cove(&boot.cove_id).await.unwrap();
    let wave = waves.into_iter().next().unwrap();
    assert_eq!(wave.cwd, cwd);
    assert_eq!(wave.title, title);
    assert_eq!(
        wave.terminal_at, None,
        "fresh wave starts non-terminal (lifecycle=Draft) so terminal_at must be None"
    );

    let cards = boot.repo.cards_by_wave(wave.id.as_str()).await.unwrap();
    let spec_card = cards
        .iter()
        .find(|c| boot.card_role_cache.get(&c.id) == Some(calm_server::model::CardRole::Spec))
        .expect("exactly one Spec-role card per wave");

    // #251 contract: title lands in payload.prompt.
    let prompt = spec_card
        .payload
        .get("prompt")
        .and_then(Value::as_str)
        .expect("spec payload.prompt carries wave.title");
    assert_eq!(prompt, title);

    // #250 PR 2 contract: cwd lands on the spec card's terminal row.
    let term = boot
        .repo
        .terminal_get_by_card(spec_card.id.as_str())
        .await
        .unwrap()
        .expect("spec terminal row");
    assert_eq!(term.cwd, cwd);
}

/// Lifecycle terminal-state E2E from the route: after `POST /api/waves`
/// + walking the wave to Done via the lifecycle state machine, the
/// GET wave detail must surface `terminal_at = Some(_)`. Locks in the
/// "route → lifecycle → repo" plumbing the calendar window query
/// relies on.
#[tokio::test]
async fn post_api_waves_then_lifecycle_done_surfaces_terminal_at_in_get() {
    use calm_server::model::WaveLifecycle;
    let boot = boot().await;

    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": boot.cove_id,
            "title": "wave-to-done",
            "cwd": "/tmp/issue-250-pr2-to-done",
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {body}");
    let wave_id = body
        .get("id")
        .and_then(Value::as_str)
        .expect("wave id in response")
        .to_string();

    // March the wave through the happy path to Done. We use the repo
    // directly (which routes through `wave_update_tx`) so we don't
    // have to mint a SpecAgent actor at the route boundary; the
    // route's lifecycle validator is unit-tested in
    // `wave_lifecycle.rs`. The interesting wiring here is the
    // wave_update_tx → terminal_at column write.
    for step in [
        WaveLifecycle::Planning,
        WaveLifecycle::Dispatching,
        WaveLifecycle::Working,
        WaveLifecycle::Reviewing,
        WaveLifecycle::Done,
    ] {
        boot.repo
            .wave_update(
                &wave_id,
                calm_server::model::WavePatch {
                    lifecycle: Some(step),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    // GET /api/waves/:id must surface the terminal_at stamp.
    let resp = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/waves/{wave_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let detail: Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let terminal_at = detail
        .pointer("/wave/terminal_at")
        .expect("wave/terminal_at in WaveDetail body");
    assert!(
        terminal_at.is_i64(),
        "terminal_at must be a unix-ms integer after lifecycle → Done; got {terminal_at}",
    );
}
