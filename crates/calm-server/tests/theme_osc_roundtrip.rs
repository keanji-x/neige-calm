//! End-to-end byte-level OSC roundtrip test (#177).
//!
//! The most surface-area test in the suite: stands up the real
//! `calm-session-daemon` binary, spawns it via the production
//! `spawn_daemon_for_with_opts` helper with theme args, and runs a
//! fixture child (`osc-probe-child`) under the daemon's PTY that
//! probes OSC 11 the same way the real codex CLI does. The fixture
//! writes "OK" / "FAIL" to a sidecar file the test driver polls.
//!
//! Every other #177 test stops short:
//!   - `wave_create_with_theme.rs`: argv-only via `argv-recorder-daemon`
//!     fixture (no real PTY, no OSC reply).
//!   - `crates/calm-session/tests/v2_*`: in-memory `RenderPlane` (no
//!     real daemon process, no PTY child).
//!   - `crates/calm-server/src/routes/terminal.rs` unit tests: argv
//!     persistence (no OSC roundtrip).
//!
//! This file closes the gap: real binary, real PTY, real OSC bytes
//! flowing both ways, real assertion against the expected RGB.
//!
//! ## Three cases
//!
//! 1. `osc_roundtrip_codex_card_create_theme` — POST a codex card
//!    with `theme: { fg, bg }`. Daemon spawns with `--terminal-fg/bg`.
//!    Fixture probes OSC 11 and asserts the reply RGB matches.
//!
//! 2. `osc_roundtrip_mid_session_theme_update` — same setup but
//!    fixture is configured for two probes. Test sends
//!    `ClientMsg::TerminalThemeUpdate` mid-session over WS. Daemon
//!    writes new OSC 10/11 to PTY. Fixture's second probe sees the
//!    new RGB.
//!
//! 3. `spec_card_path_osc_roundtrip_light_theme` — POST a **wave**
//!    with `theme: { fg, bg }`. Wave-create auto-mints a spec card
//!    and fires a background task (`spec_card::seed_and_spawn_spec_daemon`)
//!    that threads the theme into `SpawnDaemonOpts`. This is the
//!    *separate* spawn code path from case 1: a regression that
//!    drops theme on the spec-card path while leaving codex-cards
//!    intact would slip past cases 1 and 2 entirely. The light-theme
//!    RGB is the variable under test (case 1 used dark) — guards
//!    against any layer that secretly hard-codes a dark default.
//!
//! ## How the fixture is invoked
//!
//! The codex-cards endpoint hard-codes the program name as `"codex"`
//! and feeds it to `sh -c codex`. We hijack the lookup by writing
//! a symlink `<tmp>/bin/codex` → `osc-probe-child` (or, when symlinks
//! fail under the host fs, a tiny wrapper script) and prepending
//! `<tmp>/bin` to PATH on the test process. The PATH propagates
//! through kernel → daemon → PTY child via the standard env
//! inheritance chain.
//!
//! The fixture reads its parameters from env (`NEIGE_OSC_RESULT_PATH`,
//! `NEIGE_OSC_EXPECTED_BG`, etc) because we can't influence its argv
//! through the codex-cards path. Env vars flow through:
//! `std::env::set_var` on the test process → tokio::Command inherits
//! kernel env → daemon `for (k, v) in std::env::vars` → PTY child.

#![cfg(unix)]

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Process-wide guard around tests that mutate process env vars
/// (`PATH`, `NEIGE_OSC_*`). cargo runs `#[tokio::test]` in parallel
/// by default — without this lock, case 1's `restore_path` could
/// race with case 2's `prepend_path` and the fixture would see a
/// PATH from either test. Single-threaded by design.
///
/// Why not `serial_test` crate: avoiding a new dev-dep when a
/// 4-line OnceLock<Mutex> covers the same need. Why not
/// `--test-threads=1`: that demands a doc note + a CI flag, and
/// tests get run by IDE plugins that don't propagate it.
fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    // PoisonError still gives us the inner guard — we don't care
    // about the poison state because the prior test's panic already
    // surfaced; we just need exclusive access for our own env writes.
    match LOCK.get_or_init(|| Mutex::new(())).lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

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
use calm_server::ws;
use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
    RenderEncoding, Role,
};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as TMessage;
use tower::ServiceExt;
use uuid::Uuid;

/// Per-step budget. Matches `ws_terminal_e2e.rs` — generous enough to
/// absorb daemon-readiness polling (~3s worst case) and PTY init under
/// CI contention.
const STEP_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum wall-clock for the whole roundtrip (spawn → OSC query →
/// reply → result-file write). 15s leaves room on hot CI without
/// blowing the 30s per-test budget the task constraints set.
const ROUNDTRIP_BUDGET: Duration = Duration::from_secs(15);

/// Locate the real `calm-session-daemon` binary built by the workspace.
/// Same pattern as `tests/ws_terminal_e2e.rs::locate_daemon_bin`.
fn locate_daemon_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop(); // strip test name
    p.pop(); // strip "deps/"
    p.push("calm-session-daemon");
    assert!(
        p.exists(),
        "calm-session-daemon not found at {p:?}; run \
         `cargo build -p calm-session --bin calm-session-daemon` first, or \
         use `cargo test --workspace` which builds workspace bins"
    );
    p
}

/// Locate the `osc-probe-child` test fixture bin. Cargo populates
/// `CARGO_BIN_EXE_osc-probe-child` for integration test crates.
fn locate_probe_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_osc-probe-child"))
}

/// Boot a full AppState + bind a real TCP listener + spawn axum. Mirrors
/// `ws_terminal_e2e.rs::boot_full` with one twist: returns the repo
/// + daemon-data-dir so case 2 can drive the WS path.
struct Boot {
    addr: std::net::SocketAddr,
    app: axum::Router,
    cove_id: String,
    /// Tempdir kept alive for the duration of the test — drop unlinks
    /// the daemon socket directory + tears down sockets. Held by the
    /// `Boot` struct rather than the individual tests so the listener
    /// task keeps running. Field name `_tmp` would suppress dead-code
    /// warnings but the wider underscore prefix on field names
    /// confuses readers; this field IS dead by design (the runtime
    /// side effect is the Drop on `Boot`).
    #[allow(dead_code)]
    tmp: TempDir,
}

async fn boot_full() -> Boot {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");

    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "osc-e2e".into(),
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
    // #267 — `CodexClient::new_stub()` now mints its own per-instance
    // `tempfile::TempDir` for `codex_homes_dir`, so the per-card
    // codex-home subdirs (and the seeded `~/.codex` copy) get cleaned
    // up when the test's `Arc<CodexClient>` drops at teardown. The
    // previous explicit override was working around a hardcoded shared
    // `temp_dir().join("neige-codex-homes-stub")` default that has now
    // been removed at the source.
    let codex_client = CodexClient::new_stub();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            tmp.path().join("plugins-data"),
            Vec::new(),
            EventBus::new(),
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        )),
        Arc::new(codex_client),
        Some(card_role_cache.clone()),
        Some(wave_cove_cache.clone()),
    );

    let rest = routes::router().layer(axum::middleware::from_fn(
        calm_server::actor::actor_middleware,
    ));
    let app = axum::Router::new()
        .merge(rest)
        .merge(ws::router())
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve_app = app.clone();
    tokio::spawn(async move {
        axum::serve(
            listener,
            serve_app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    Boot {
        addr,
        app,
        cove_id: cove.id.to_string(),
        tmp,
    }
}

/// Stage a `<tmp>/bin/codex` symlink pointing at the probe-child
/// fixture, then prepend `<tmp>/bin` to PATH on the *current process*
/// so daemon spawns inherit it. Returns the bin dir for cleanup.
///
/// Why a symlink + PATH override: the codex-cards endpoint hard-codes
/// the program name as `"codex"` and feeds it to `sh -c codex`. We
/// can't influence the argv from the test, so we have to make `codex`
/// resolve to our fixture. The symlink keeps the fixture's argv[0]
/// stable (it sees its own path via env_var `CARGO_BIN_EXE_*` if it
/// needs it; here it just uses env vars).
fn stage_fake_codex(bin_dir: &Path) {
    std::fs::create_dir_all(bin_dir).expect("mkdir fake-codex bin dir");
    let dest = bin_dir.join("codex");
    if dest.exists() {
        let _ = std::fs::remove_file(&dest);
    }
    symlink(locate_probe_bin(), &dest).expect("symlink probe -> codex");
}

/// Prepend `bin_dir` to PATH on the current process. Daemons we spawn
/// later will inherit it. Returns the original PATH so test cleanup
/// can restore it (avoids leaking state across tests under `--test-threads=1`
/// or when the harness reuses worker threads — `set_var` is process-
/// global).
fn prepend_path(bin_dir: &Path) -> String {
    let prev = std::env::var("PATH").unwrap_or_default();
    let new = format!("{}:{prev}", bin_dir.display());
    // SAFETY: single-threaded test setup before any background tasks
    // depend on PATH. Subsequent daemon spawns read PATH on their
    // own thread but only after this set_var returns.
    unsafe {
        std::env::set_var("PATH", &new);
    }
    prev
}

fn restore_path(prev: String) {
    // SAFETY: same single-threaded contract as `prepend_path`.
    unsafe {
        std::env::set_var("PATH", prev);
    }
}

/// Set an env var on the current process for the fixture child to
/// pick up. SAFETY: same contract as `prepend_path` — tests must
/// not race here, which we enforce by running each test serially via
/// the standard `#[tokio::test]` harness (each test gets its own
/// runtime, sequential by default within a file).
fn set_env(key: &str, value: &str) {
    unsafe {
        std::env::set_var(key, value);
    }
}

fn unset_env(key: &str) {
    unsafe {
        std::env::remove_var(key);
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

/// Create a wave under `cove_id`. `theme` is required end-to-end
/// (#177); the codex card's body theme is the variable under test in
/// these cases, so pass an inert default sentinel here. The spec card
/// the wave route auto-mints does spawn with these RGBs, but its
/// daemon doesn't participate in the OSC roundtrip these cases probe.
async fn create_wave(app: axum::Router, cove_id: &str) -> String {
    let (status, body) = post(
        app,
        "/api/waves",
        json!({
            "cove_id": cove_id,
            "title": "osc-e2e",
            "cwd": "/tmp/issue-250-pr2-test",
            "attach_folder": true,
            "theme": { "fg": [216, 219, 226], "bg": [15, 20, 24] },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "wave create body={body}");
    body["id"].as_str().expect("wave.id").to_string()
}

/// Poll a result file for non-empty contents. Returns the contents
/// when at least one line is present. Panics on timeout. Used after
/// triggering the fixture to wait for its outcome.
fn wait_for_result(result_path: &Path, budget: Duration, label: &str) -> String {
    let deadline = Instant::now() + budget;
    loop {
        if let Ok(s) = std::fs::read_to_string(result_path)
            && !s.is_empty()
        {
            return s;
        }
        if Instant::now() >= deadline {
            // Include directory listing for diagnostic — when the
            // result file never lands the most likely cause is the
            // fixture child never running (PATH not propagating, sh
            // resolving codex to a different binary, ...).
            let dir = result_path.parent().unwrap_or_else(|| Path::new("."));
            let ls = std::fs::read_dir(dir)
                .map(|rd| {
                    rd.filter_map(|e| e.ok().map(|e| e.path()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            panic!(
                "result file {result_path:?} stayed empty within {budget:?} ({label}); dir entries: {ls:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// (removed `wait_for_marker` helper — case 2 inlines a custom polling
// loop because it needs to distinguish "OK\n" vs "FAIL: probe1" outcomes
// in a single pass, and reusing the helper would duplicate the deadline
// + diagnostic plumbing.)

// ---------------------------------------------------------------------------
// Case 1: theme-on-create → OSC 11 reply
// ---------------------------------------------------------------------------

/// Happy path: codex-card POST carries `theme: { fg, bg }`. The kernel
/// stamps `--terminal-fg/bg` onto the daemon argv. The daemon's
/// `RenderPlane::with_colors` pre-seeds the OSC reply colors. The
/// fixture child (running as fake `codex` under sh's PATH lookup)
/// writes OSC 11 query, reads back the reply, and asserts the RGB.
///
/// Any layer that drops the theme fails this test — the result file
/// would either stay empty (fixture never ran), report a missing
/// reply (daemon didn't synthesize one), or report a wrong RGB
/// (model carried the wrong default).
//
// The `env_guard()` `std::sync::Mutex` IS intentionally held across
// `.await` points — its job is to serialize tests that mutate process-
// global `PATH` and `NEIGE_OSC_*` env vars. Switching to `tokio::sync::Mutex`
// here would still serialize but adds an async dep for what is conceptually
// a blocking single-resource lock; allow the lint.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn osc_roundtrip_codex_card_create_theme() {
    let _guard = env_guard();
    let tmp = TempDir::new().expect("tempdir for fixture staging");
    let bin_dir = tmp.path().join("bin");
    let result_path = tmp.path().join("probe-result.txt");

    // Wire up: fake `codex` on PATH + result/expected env vars for
    // the fixture to read.
    stage_fake_codex(&bin_dir);
    let prev_path = prepend_path(&bin_dir);
    set_env("NEIGE_OSC_RESULT_PATH", &result_path.to_string_lossy());
    set_env("NEIGE_OSC_EXPECTED_BG", "15,20,24");
    // No probe-twice for case 1 — single probe is enough.
    unset_env("NEIGE_OSC_PROBE_TWICE");

    let boot = boot_full().await;
    let wave_id = create_wave(boot.app.clone(), &boot.cove_id).await;

    // POST the codex card with theme. The handler:
    //   1. Mints card + terminal rows in one txn.
    //   2. Seeds CODEX_HOME + writes hooks.json (best-effort).
    //   3. Spawns calm-session-daemon with `--terminal-fg=216,219,226
    //      --terminal-bg=15,20,24 -- /bin/sh -c codex`.
    //   4. sh's PATH lookup resolves codex → our fixture symlink.
    //   5. Fixture inherits our env vars, runs probe, writes result.
    //
    // cwd is set to the tempdir to keep codex's $HOME / cwd writes
    // (config.toml, hooks.json) out of the host $HOME.
    let cwd = tmp.path().to_string_lossy().to_string();
    let (status, body) = post(
        boot.app.clone(),
        &format!("/api/waves/{wave_id}/codex-cards"),
        json!({
            "cwd": cwd,
            "theme": { "fg": [216, 219, 226], "bg": [15, 20, 24] }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "codex card create body={body}");

    // Wait for the fixture's result file to land. The fixture runs
    // through:
    //   open /dev/tty → enter raw mode → write OSC 11 ? → poll for
    //   reply → parse RGB → compare → write result.
    // In practice this completes in <100ms once the daemon's PTY
    // reader has the query.
    let result = wait_for_result(&result_path, ROUNDTRIP_BUDGET, "probe1");
    assert!(
        result.contains("OK"),
        "OSC roundtrip failed; fixture result file says: {result:?}"
    );

    restore_path(prev_path);
}

// ---------------------------------------------------------------------------
// Case 2: mid-session theme toggle → daemon writes ONLY focus-in (no
// unsolicited OSC 10/11 RGB)
// ---------------------------------------------------------------------------

/// Mid-session toggle: start with one theme (dark), then send
/// `ClientMsg::TerminalThemeUpdate` over WS with a different theme
/// (light). Since #295 followup 1 the daemon's
/// `Effect::TerminalThemeUpdate` branch writes ONLY a focus-in
/// (`ESC[I`) to the PTY master — the unsolicited
/// `OSC 10/11;rgb:RRRR/GGGG/BBBB` pair that PR #296 used to emit
/// alongside has been dropped (the focus-in alone is enough: codex
/// re-queries on `FocusGained`, see
/// `codex/event_stream.rs::Event::FocusGained ⇒ terminal_palette::requery_default_colors`).
///
/// This case anchors the **structural** invariant: the bytes the
/// daemon writes to the PTY after a toggle contain `\x1b[I` and do
/// NOT contain any `\x1b]10;rgb:` / `\x1b]11;rgb:` opener. The
/// downstream solicited reply path is covered by
/// `osc_roundtrip_solicited_reply_after_theme_update` below.
//
// See sibling `#[allow(clippy::await_holding_lock)]` justification on
// `osc_roundtrip_codex_card_create_theme`.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn osc_roundtrip_mid_session_theme_update() {
    let _guard = env_guard();
    let tmp = TempDir::new().expect("tempdir for fixture staging");
    let bin_dir = tmp.path().join("bin");
    let result_path = tmp.path().join("probe-result.txt");

    stage_fake_codex(&bin_dir);
    let prev_path = prepend_path(&bin_dir);
    set_env("NEIGE_OSC_RESULT_PATH", &result_path.to_string_lossy());
    let trace_path = tmp.path().join("probe-trace.txt");
    set_env("NEIGE_OSC_TRACE_PATH", &trace_path.to_string_lossy());
    // Probe1 still expects dark (solicited query on startup). Probe2
    // is a passive byte drain in this case — `expected-bg-2` is
    // required by the fixture's arg layer but unused in default
    // (non-reprobe) mode, so set it to anything parseable.
    set_env("NEIGE_OSC_EXPECTED_BG", "15,20,24");
    set_env("NEIGE_OSC_EXPECTED_BG_2", "247,249,252");
    set_env("NEIGE_OSC_PROBE_TWICE", "1");
    // Default mode — drain raw bytes, dump them as
    // `PROBE2_BYTES_HEX=...` in the result file. Reprobe mode is
    // exercised by `osc_roundtrip_solicited_reply_after_theme_update`.
    unset_env("NEIGE_OSC_REPROBE");

    let boot = boot_full().await;
    let wave_id = create_wave(boot.app.clone(), &boot.cove_id).await;

    let cwd = tmp.path().to_string_lossy().to_string();
    let (status, body) = post(
        boot.app.clone(),
        &format!("/api/waves/{wave_id}/codex-cards"),
        json!({
            "cwd": cwd,
            "theme": { "fg": [216, 219, 226], "bg": [15, 20, 24] }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "codex card create body={body}");

    // CRITICAL ORDERING: wait for probe1's "OK\n" marker before
    // sending the toggle. Without this, the fixture's PTY may
    // receive the toggle's unsolicited OSC bytes BEFORE its probe1
    // read completes, causing probe1 to parse the toggle's RGB and
    // FAIL the dark-theme assertion. The race is real: both the
    // query-reply OSC and the toggle OSC funnel through the same
    // `stdin_tx` MPSC in the daemon, and the per-connection task
    // (toggle) and the pty-reader thread (query reply) can queue
    // in either order.
    //
    // The fixture writes "OK\n" *only* when probe1 saw the correct
    // RGB. Once we observe that, probe2's read loop is already
    // running and will pick up whatever the toggle path writes.
    {
        let deadline = Instant::now() + ROUNDTRIP_BUDGET;
        loop {
            let s = std::fs::read_to_string(&result_path).unwrap_or_default();
            if s.contains("OK\n") || s.contains("FAIL: probe1") {
                break;
            }
            if Instant::now() >= deadline {
                let trace =
                    std::fs::read_to_string(&trace_path).unwrap_or_else(|_| "<no trace>".into());
                panic!(
                    "probe1 outcome never landed within {ROUNDTRIP_BUDGET:?}; \
                     last result: {s:?}; trace: {trace}"
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // Probe1 must succeed before we run the toggle subpath; a
        // probe1 FAIL means the case-1 contract is broken and
        // testing the toggle on top is meaningless.
        let s = std::fs::read_to_string(&result_path).unwrap_or_default();
        assert!(
            s.contains("OK\n"),
            "probe1 must succeed before toggle subpath; result: {s:?}"
        );
    }

    // Locate the terminal id for the freshly-created card via the
    // ID extractor in the card payload. The codex card payload
    // contains `terminal_id` per `codex_cards.rs`'s schema.
    let raw_terminal_id = body["payload"]["terminal_id"]
        .as_str()
        .expect("card.payload.terminal_id present")
        .to_string();

    // Open a WS to the terminal as the browser does. Send hello
    // first (the toggle write requires an owner role to land via
    // the daemon's gate).
    let ws_url = format!("ws://{}/api/terminals/{raw_terminal_id}", boot.addr);
    let (mut ws, _resp) =
        tokio::time::timeout(STEP_TIMEOUT, tokio_tungstenite::connect_async(&ws_url))
            .await
            .expect("ws connect timed out")
            .expect("ws connect failed");

    let hello = ClientMsg::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id: raw_terminal_id.clone(),
        client_id: Uuid::new_v4(),
        desired_size: PtySize {
            cols: 80,
            rows: 24,
            pixel_width: None,
            pixel_height: None,
        },
        cell_size: None,
        initial_scrollback: InitialScrollback::None,
        resume_from: None,
        role_hint: Some(Role::Owner),
        capabilities: ClientCapabilities {
            render_encodings: vec![RenderEncoding::Vt],
            supports_scrollback: true,
            supports_sixel: false,
            supports_images: false,
            kernel_originated_input: false,
        },
    };
    ws.send(TMessage::Text(serde_json::to_string(&hello).unwrap()))
        .await
        .unwrap();

    // Drain frames until we see ServerHello so we know the handshake
    // completed and Owner role was assigned. The daemon won't apply
    // TerminalThemeUpdate until then (Owner-only gate).
    let deadline = tokio::time::Instant::now() + STEP_TIMEOUT;
    loop {
        if tokio::time::Instant::now() >= deadline {
            panic!("did not receive ServerHello within {STEP_TIMEOUT:?}");
        }
        let remaining = deadline - tokio::time::Instant::now();
        let frame = tokio::time::timeout(remaining, ws.next()).await;
        match frame {
            Ok(Some(Ok(TMessage::Text(t)))) => {
                let msg: DaemonMsg = match serde_json::from_str(&t) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if matches!(msg, DaemonMsg::ServerHello { .. }) {
                    break;
                }
            }
            Ok(Some(Ok(_))) => continue,
            _ => panic!("ws stream ended before ServerHello"),
        }
    }

    // Toggle theme. The daemon's session-state machine intercepts
    // this and emits `Effect::TerminalThemeUpdate`, which the daemon's
    // effect loop turns into an `ESC[I` write onto the PTY master
    // (#305). Our fixture's second-probe read picks it up.
    let toggle = ClientMsg::TerminalThemeUpdate {
        fg: (24, 33, 41),    // light-theme fg (dark on light bg)
        bg: (247, 249, 252), // light-theme bg
    };
    let toggle_json = serde_json::to_string(&toggle).unwrap();
    ws.send(TMessage::Text(toggle_json)).await.unwrap();

    // CRITICAL: drain a few daemon frames after the toggle to keep
    // the WS up-arm pump moving. Without this, the tokio runtime
    // never schedules the bridge's daemon-read loop and the toggle
    // frame stalls in the WS up-arm pipe before reaching the daemon
    // socket. With this loop the daemon's broadcast frames flow
    // back through the bridge, which keeps the up-arm's tokio task
    // active and pushes our toggle out the door.
    //
    // (This is a known shape with the tokio::select! pump pattern:
    // up and down arms cooperate within one task, and exhausting
    // ws.next() back-pressures the up arm. The browser hits the
    // same condition in production but masks it with constant
    // RenderPatch traffic from user typing.)
    //
    // Reads frames for up to 2s after the toggle. Either we see
    // the daemon's response traffic (proving the channel is alive)
    // or the WS closes (which would surface as Err on next()).
    let drain_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < drain_deadline {
        match tokio::time::timeout(Duration::from_millis(200), ws.next()).await {
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(_))) | Ok(None) => break,
            Err(_) => break,
        }
    }

    // Wait for probe2 to dump its byte stream. Use a longer budget
    // because we have an extra WS roundtrip + daemon-side hop on top
    // of the base PTY-write path. The fixture's drain window is 10s
    // wall-clock; ROUNDTRIP_BUDGET (15s) gives the assertion side
    // headroom over that.
    let deadline = Instant::now() + ROUNDTRIP_BUDGET;
    let result = loop {
        let s = std::fs::read_to_string(&result_path).unwrap_or_default();
        if s.contains("PROBE2_BYTES_HEX=") {
            break s;
        }
        if Instant::now() >= deadline {
            let trace =
                std::fs::read_to_string(&trace_path).unwrap_or_else(|_| "<no trace>".into());
            panic!(
                "probe2 byte-dump never landed within {ROUNDTRIP_BUDGET:?}; \
                 result so far: {s:?}; trace: {trace}"
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let trace = std::fs::read_to_string(&trace_path).unwrap_or_else(|_| "<no trace>".into());

    // Probe1 must have succeeded (asserted above already, but
    // re-affirm so this case can fail with a unified report).
    assert!(
        result.contains("OK\n"),
        "probe1 must succeed; result: {result:?}; trace: {trace:?}"
    );

    // Decode the hex byte-dump from the result file.
    let hex_line = result
        .lines()
        .find_map(|l| l.strip_prefix("PROBE2_BYTES_HEX="))
        .expect("PROBE2_BYTES_HEX= line present");
    let bytes: Vec<u8> = (0..hex_line.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex_line[i..i + 2], 16).expect("hex parse"))
        .collect();

    let contains = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    let preview = String::from_utf8_lossy(&bytes);

    // (a) Daemon MUST write the focus-in CSI. Codex re-queries OSC
    //     10/11 on `FocusGained`, so this is the trigger for the
    //     solicited path.
    assert!(
        contains(b"\x1b[I"),
        "daemon must write focus-in (ESC[I) after theme toggle; \
         got {} bytes: {preview:?}; trace: {trace}",
        bytes.len()
    );

    // (b) Daemon MUST NOT write unsolicited OSC 10/11 RGB. Anything
    //     under those openers would be the regression #295 followup
    //     1 set out to eliminate.
    assert!(
        !contains(b"\x1b]10;rgb:"),
        "daemon emitted unsolicited OSC 10 RGB after theme toggle \
         (#295 followup 1 regression); bytes={preview:?}; trace: {trace}"
    );
    assert!(
        !contains(b"\x1b]11;rgb:"),
        "daemon emitted unsolicited OSC 11 RGB after theme toggle \
         (#295 followup 1 regression); bytes={preview:?}; trace: {trace}"
    );

    let _ = ws.close(None).await;
    restore_path(prev_path);
}

// ---------------------------------------------------------------------------
// Case 3: wave-create → spec-card spawn path → OSC 11 reply (light theme)
// ---------------------------------------------------------------------------

/// Wave-create path: `POST /api/waves` with `theme: { fg, bg }`. The
/// handler atomically mints a spec card + terminal row and fires
/// `spec_card::seed_and_spawn_spec_daemon` as a background task; the
/// helper passes the theme through `SpawnDaemonOpts` to
/// `spawn_daemon_for_with_opts`. This is the **separate** spawn code
/// path from cases 1 and 2 — those exercise `routes::codex_cards`'s
/// inline spawn. A regression that drops theme inside
/// `seed_and_spawn_spec_daemon` (or the `NewWave.theme` snapshot in
/// `routes::waves::create_wave`) slips past cases 1+2 entirely.
///
/// Light-theme RGB is the variable under test on purpose: case 1
/// uses dark (`bg = 15,20,24`) so if a future refactor secretly
/// hard-codes a dark default into the spec-card path, this case
/// catches it (light bg `252,254,255` would mismatch the dark
/// default and the OSC 11 reply parse would FAIL).
///
/// Env propagation: the `osc-probe-child` fixture reads
/// `NEIGE_OSC_RESULT_PATH` and `NEIGE_OSC_EXPECTED_BG` from its
/// process env. `tokio::process::Command::spawn` inherits the parent
/// process env by default (no `env_clear` anywhere in
/// `spawn_daemon_with_parts`), so vars we set on the test process
/// reach the daemon and then the PTY child unchanged — exactly the
/// same chain cases 1 and 2 rely on. The per-card env_map built by
/// `spec_card::build_codex_env_map` only **adds** overrides
/// (`CODEX_HOME`, `NEIGE_CARD_ID`, ...); it does not clear the
/// inherited PATH / `NEIGE_OSC_*` vars.
//
// See sibling `#[allow(clippy::await_holding_lock)]` justification on
// `osc_roundtrip_codex_card_create_theme`.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn spec_card_path_osc_roundtrip_light_theme() {
    let _guard = env_guard();
    let tmp = TempDir::new().expect("tempdir for fixture staging");
    let bin_dir = tmp.path().join("bin");
    let result_path = tmp.path().join("probe-result.txt");
    let trace_path = tmp.path().join("probe-trace.txt");

    // Wire up: fake `codex` on PATH + light-theme expected bg env.
    // Light theme RGB: fg = (42,47,58), bg = (252,254,255).
    stage_fake_codex(&bin_dir);
    let prev_path = prepend_path(&bin_dir);
    set_env("NEIGE_OSC_RESULT_PATH", &result_path.to_string_lossy());
    set_env("NEIGE_OSC_TRACE_PATH", &trace_path.to_string_lossy());
    set_env("NEIGE_OSC_EXPECTED_BG", "252,254,255");
    // Single probe — wave-create has no mid-session theme toggle in
    // this test; the toggle path is already covered by case 2.
    unset_env("NEIGE_OSC_PROBE_TWICE");

    let boot = boot_full().await;

    // POST /api/waves with the light-theme RGB. The handler:
    //   1. Atomically mints wave + spec card + terminal rows in one
    //      tx and returns 201.
    //   2. Spawns `seed_and_spawn_spec_daemon` as a background task,
    //      which threads the theme into `SpawnDaemonOpts` and calls
    //      `spawn_daemon_for_with_opts`. The daemon argv carries
    //      `--terminal-fg=42,47,58 --terminal-bg=252,254,255`.
    //   3. Daemon launches `/bin/sh -c codex`; sh's PATH lookup
    //      resolves `codex` → our `<tmp>/bin/codex` symlink → the
    //      `osc-probe-child` fixture.
    //   4. Fixture probes OSC 11, reads daemon's pre-seeded reply,
    //      asserts RGB matches `NEIGE_OSC_EXPECTED_BG`.
    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": boot.cove_id,
            "title": "light-theme spec-card osc roundtrip",
            "cwd": "/tmp/issue-250-pr2-test",
            "attach_folder": true,
            "theme": { "fg": [42, 47, 58], "bg": [252, 254, 255] }
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "wave create with theme body={body}"
    );

    // The background task is fire-and-forget; poll the result file
    // until the fixture lands its outcome. Same budget as case 1 —
    // the spec-card path has an extra `seed_codex_home_for_card` hop
    // (a handful of mkdir + small writes) on top of the daemon spawn
    // but it's microseconds, well inside the 15s budget.
    // Poll asynchronously so the background `tokio::spawn` task that
    // calls `seed_and_spawn_spec_daemon` gets runtime time. Cases 1 and
    // 2 use a synchronous `std::thread::sleep` poll because their spawn
    // happens *inside* the POST handler — by the time `post().await`
    // returns, the daemon process is already running natively (no tokio
    // task needs further polling). The spec-card path differs: the
    // handler returns 201 immediately and hands the spawn to a
    // `tokio::spawn`'d future, which a sync sleep would starve under
    // the default single-threaded `#[tokio::test]` runtime.
    let result = {
        let deadline = Instant::now() + ROUNDTRIP_BUDGET;
        loop {
            if let Ok(s) = std::fs::read_to_string(&result_path)
                && !s.is_empty()
            {
                break s;
            }
            if Instant::now() >= deadline {
                // Walk both the fixture-staging tempdir AND the boot
                // tempdir (daemon data dir, codex_homes parent) — they
                // catch different failure modes: empty fixture tempdir
                // ⇒ daemon never reached `exec(codex)`; empty boot
                // tempdir ⇒ background task never ran at all.
                let mut listing = Vec::<String>::new();
                fn walk(p: &Path, out: &mut Vec<String>, depth: usize) {
                    if depth > 4 {
                        return;
                    }
                    if let Ok(rd) = std::fs::read_dir(p) {
                        for e in rd.flatten() {
                            let pp = e.path();
                            out.push(format!("{}{}", "  ".repeat(depth), pp.display()));
                            if pp.is_dir() {
                                walk(&pp, out, depth + 1);
                            }
                        }
                    }
                }
                walk(boot.tmp.path(), &mut listing, 0);
                let trace =
                    std::fs::read_to_string(&trace_path).unwrap_or_else(|_| "<no trace>".into());
                panic!(
                    "spec-card OSC roundtrip never produced a result within {ROUNDTRIP_BUDGET:?}.\n\
                     Boot tempdir tree (daemon data + codex_homes):\n{}\n\
                     Trace: {}",
                    listing.join("\n"),
                    trace
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    let trace = std::fs::read_to_string(&trace_path).unwrap_or_else(|_| "<no trace>".into());
    assert!(
        result.contains("OK"),
        "spec-card OSC roundtrip failed; fixture result: {result:?}; trace: {trace}"
    );

    restore_path(prev_path);
}

// ---------------------------------------------------------------------------
// Case 4 (#295 followup 1): solicited reply path after a mid-session
// theme update
// ---------------------------------------------------------------------------

/// Solicited OSC reply path: after the daemon writes `\x1b[I` to the
/// child following a `TerminalThemeUpdate`, a focus-aware TUI like
/// codex calls `terminal_palette::requery_default_colors()` which
/// emits `OSC 10;?` / `OSC 11;?`. The daemon's vte parser handles
/// those queries against the (now-updated) default fg/bg in the model
/// and writes a reply onto the PTY master.
///
/// This case proves the **end-to-end loop**: theme update →
/// `set_default_colors` on the model → focus-in to child → child
/// re-queries → daemon's solicited reply carries the NEW RGB. Case
/// 2 (above) asserts the no-unsolicited-RGB structural invariant;
/// this case asserts the functional outcome (the child eventually
/// learns the new color).
///
/// Fixture mode: `NEIGE_OSC_REPROBE=1` makes the fixture's probe2
/// wait for `\x1b[I`, send `\x1b]11;?\x1b\\`, then parse the reply.
/// `OK2` is written iff the reply RGB matches the post-toggle
/// expected light-theme bg.
//
// See sibling `#[allow(clippy::await_holding_lock)]` justification on
// `osc_roundtrip_codex_card_create_theme`.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn osc_roundtrip_solicited_reply_after_theme_update() {
    let _guard = env_guard();
    let tmp = TempDir::new().expect("tempdir for fixture staging");
    let bin_dir = tmp.path().join("bin");
    let result_path = tmp.path().join("probe-result.txt");
    let trace_path = tmp.path().join("probe-trace.txt");

    stage_fake_codex(&bin_dir);
    let prev_path = prepend_path(&bin_dir);
    set_env("NEIGE_OSC_RESULT_PATH", &result_path.to_string_lossy());
    set_env("NEIGE_OSC_TRACE_PATH", &trace_path.to_string_lossy());
    set_env("NEIGE_OSC_EXPECTED_BG", "15,20,24");
    set_env("NEIGE_OSC_EXPECTED_BG_2", "247,249,252");
    set_env("NEIGE_OSC_PROBE_TWICE", "1");
    set_env("NEIGE_OSC_REPROBE", "1");

    let boot = boot_full().await;
    let wave_id = create_wave(boot.app.clone(), &boot.cove_id).await;

    let cwd = tmp.path().to_string_lossy().to_string();
    let (status, body) = post(
        boot.app.clone(),
        &format!("/api/waves/{wave_id}/codex-cards"),
        json!({
            "cwd": cwd,
            "theme": { "fg": [216, 219, 226], "bg": [15, 20, 24] }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "codex card create body={body}");

    // Same ordering rationale as case 2: probe1 must complete
    // before we drive the WS toggle, else the fixture's read of
    // probe1's solicited reply could race with the toggle's
    // focus-in and confuse downstream parsing.
    {
        let deadline = Instant::now() + ROUNDTRIP_BUDGET;
        loop {
            let s = std::fs::read_to_string(&result_path).unwrap_or_default();
            if s.contains("OK\n") || s.contains("FAIL: probe1") {
                break;
            }
            if Instant::now() >= deadline {
                let trace =
                    std::fs::read_to_string(&trace_path).unwrap_or_else(|_| "<no trace>".into());
                panic!(
                    "probe1 outcome never landed within {ROUNDTRIP_BUDGET:?}; \
                     last result: {s:?}; trace: {trace}"
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let s = std::fs::read_to_string(&result_path).unwrap_or_default();
        assert!(
            s.contains("OK\n"),
            "probe1 must succeed before reprobe subpath; result: {s:?}"
        );
    }

    let raw_terminal_id = body["payload"]["terminal_id"]
        .as_str()
        .expect("card.payload.terminal_id present")
        .to_string();

    let ws_url = format!("ws://{}/api/terminals/{raw_terminal_id}", boot.addr);
    let (mut ws, _resp) =
        tokio::time::timeout(STEP_TIMEOUT, tokio_tungstenite::connect_async(&ws_url))
            .await
            .expect("ws connect timed out")
            .expect("ws connect failed");

    let hello = ClientMsg::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id: raw_terminal_id.clone(),
        client_id: Uuid::new_v4(),
        desired_size: PtySize {
            cols: 80,
            rows: 24,
            pixel_width: None,
            pixel_height: None,
        },
        cell_size: None,
        initial_scrollback: InitialScrollback::None,
        resume_from: None,
        role_hint: Some(Role::Owner),
        capabilities: ClientCapabilities {
            render_encodings: vec![RenderEncoding::Vt],
            supports_scrollback: true,
            supports_sixel: false,
            supports_images: false,
            kernel_originated_input: false,
        },
    };
    ws.send(TMessage::Text(serde_json::to_string(&hello).unwrap()))
        .await
        .unwrap();

    // Drain frames until ServerHello so we know the handshake
    // completed and Owner role was assigned (gate on
    // TerminalThemeUpdate).
    let deadline = tokio::time::Instant::now() + STEP_TIMEOUT;
    loop {
        if tokio::time::Instant::now() >= deadline {
            panic!("did not receive ServerHello within {STEP_TIMEOUT:?}");
        }
        let remaining = deadline - tokio::time::Instant::now();
        let frame = tokio::time::timeout(remaining, ws.next()).await;
        match frame {
            Ok(Some(Ok(TMessage::Text(t)))) => {
                let msg: DaemonMsg = match serde_json::from_str(&t) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if matches!(msg, DaemonMsg::ServerHello { .. }) {
                    break;
                }
            }
            Ok(Some(Ok(_))) => continue,
            _ => panic!("ws stream ended before ServerHello"),
        }
    }

    let toggle = ClientMsg::TerminalThemeUpdate {
        fg: (24, 33, 41),
        bg: (247, 249, 252),
    };
    ws.send(TMessage::Text(serde_json::to_string(&toggle).unwrap()))
        .await
        .unwrap();

    // Keep the WS up-arm pump alive (same reason as case 2 — read a
    // few daemon frames so the bridge processes our toggle).
    let drain_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < drain_deadline {
        match tokio::time::timeout(Duration::from_millis(200), ws.next()).await {
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(_))) | Ok(None) => break,
            Err(_) => break,
        }
    }

    // Wait for the reprobe outcome.
    let deadline = Instant::now() + ROUNDTRIP_BUDGET;
    let result = loop {
        let s = std::fs::read_to_string(&result_path).unwrap_or_default();
        if s.contains("OK2") || s.contains("FAIL: probe2") {
            break s;
        }
        if Instant::now() >= deadline {
            let trace =
                std::fs::read_to_string(&trace_path).unwrap_or_else(|_| "<no trace>".into());
            panic!(
                "reprobe outcome never landed within {ROUNDTRIP_BUDGET:?}; \
                 result so far: {s:?}; trace: {trace}"
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let trace = std::fs::read_to_string(&trace_path).unwrap_or_else(|_| "<no trace>".into());
    assert!(
        result.contains("OK\n") && result.contains("OK2"),
        "probe1 + reprobe must both succeed; result: {result:?}; trace: {trace:?}"
    );

    let _ = ws.close(None).await;
    restore_path(prev_path);
}
