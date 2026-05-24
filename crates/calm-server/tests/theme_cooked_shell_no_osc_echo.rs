//! Hermetic regression test for **fix B, shell direction** (terminal
//! OSC-echo bug).
//!
//! ## What fix B is and why this test exists
//!
//! "New terminal" runs an interactive shell (`zsh` / `fish`) which sits
//! at its prompt. A modern shell prompt is **not** cooked: zsh's ZLE
//! (fish's reader) drives the line via a raw-mode editor — ECHO off,
//! ICANON off, identical termios to a real TUI. What it does NOT do is
//! opt into DECSET 1004 (focus event reporting): it only enables
//! bracketed paste (`ESC[?2004h`) and never queries OSC 10/11.
//!
//! The daemon's mid-session `Effect::TerminalThemeUpdate` branch writes a
//! synthetic OSC 10/11 reply pair (+ focus-in `ESC[I`) onto the PTY. A
//! focus-aware TUI consumes that silently; a shell's ZLE treats it as
//! *input* and redraws the bytes at the prompt as syntax-highlighted
//! garbage — the OSC-echo bug seen after a theme toggle.
//!
//! The original fix B gated on the PTY master's termios `ECHO` flag,
//! which is **wrong**: ZLE turns ECHO off, so a shell at its prompt
//! looks exactly like a TUI to that probe and the gate lets the write
//! through. The corrected fix B (`crates/calm-session/src/bin/daemon.rs`,
//! `Effect::TerminalThemeUpdate` arm) instead gates on whether the child
//! enabled DECSET 1004 (read via `RenderPlane::focus_event_tracking()`):
//!   - 1004 enabled  → focus-aware TUI (codex) → write (consumed silently).
//!   - 1004 disabled → shell at prompt         → **skip** the write.
//!
//! ## Complementary to `theme_osc_roundtrip.rs`
//!
//! `theme_osc_roundtrip.rs` anchors the *opted-in* direction with the
//! `osc-probe-child` fixture (which now sends `ESC[?1004h` on startup,
//! like codex): it proves fix B does **not** over-gate — a 1004-aware
//! TUI still receives the mid-session OSC reply.
//!
//! This file anchors the *opposite* direction with the
//! `cooked-shell-child` fixture: a shell in ZLE raw mode that never
//! enabled 1004 must **not** receive (and therefore must not redraw) the
//! synthetic OSC. We assert it by collecting every broadcast
//! `RenderPatch` after the toggle and checking none carry
//! `\x1b]10;rgb:` / `\x1b]11;rgb:` — nor the `]10;rgb:` / `]11;rgb:`
//! literal the shell's line editor would surface if the bytes had
//! reached its stdin.
//!
//! Without this test, the shell direction had only manual Playwright
//! coverage; removing fix B's 1004 gate would slip past CI. (Verified
//! during development: removing the 1004 gate makes this test fail
//! because the raw-mode shell child surfaces `]10;rgb:` into a
//! `RenderPatch`.)
//!
//! ## How the cooked child is wired in
//!
//! Same mechanism as `theme_osc_roundtrip.rs`: the codex-cards endpoint
//! hard-codes the program name `"codex"` and runs `sh -c codex`, so we
//! symlink our `cooked-shell-child` fixture onto `<tmp>/bin/codex` and
//! prepend `<tmp>/bin` to PATH on the test process. The PATH propagates
//! test process → daemon → PTY child via env inheritance. We avoid the
//! host's real `$SHELL` to stay hermetic and deterministic.

#![cfg(unix)]

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

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

/// Process-wide guard around tests that mutate process env vars (`PATH`).
/// cargo runs `#[tokio::test]` in parallel by default; without this lock
/// our `prepend_path`/`restore_path` pair could race the same pair in
/// `theme_osc_roundtrip.rs`'s tests (each test crate is its own process,
/// so the race is only *within* this file — but we still serialize for
/// robustness if more cases are added here). Single test today, so the
/// lock is effectively free.
fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    match LOCK.get_or_init(|| Mutex::new(())).lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// Per-step budget. Matches `theme_osc_roundtrip.rs` — generous enough
/// to absorb daemon-readiness polling + PTY init under CI contention.
const STEP_TIMEOUT: Duration = Duration::from_secs(5);

/// Locate the real `calm-session-daemon` binary built by the workspace.
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

/// Locate the `cooked-shell-child` test fixture bin. Cargo populates
/// `CARGO_BIN_EXE_cooked-shell-child` for integration test crates.
fn locate_cooked_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cooked-shell-child"))
}

struct Boot {
    addr: std::net::SocketAddr,
    app: axum::Router,
    cove_id: String,
    /// Tempdir kept alive for the test's lifetime — drop tears down the
    /// daemon socket dir and SIGKILLs the PTY child. Dead by design (the
    /// runtime effect is its `Drop`).
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
            name: "osc-echo-cooked".into(),
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
        axum::serve(listener, serve_app).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    Boot {
        addr,
        app,
        cove_id: cove.id.to_string(),
        tmp,
    }
}

/// Stage a `<tmp>/bin/codex` symlink pointing at the cooked-shell-child
/// fixture, so the codex-cards endpoint's `sh -c codex` PATH lookup
/// resolves to it. Same hijack as `theme_osc_roundtrip.rs::stage_fake_codex`,
/// only the symlink target differs (cooked child vs OSC probe).
fn stage_fake_codex(bin_dir: &Path) {
    std::fs::create_dir_all(bin_dir).expect("mkdir fake-codex bin dir");
    let dest = bin_dir.join("codex");
    if dest.exists() {
        let _ = std::fs::remove_file(&dest);
    }
    symlink(locate_cooked_bin(), &dest).expect("symlink cooked-shell-child -> codex");
}

/// Prepend `bin_dir` to PATH on the current process so daemon spawns
/// inherit it. Returns the original PATH for cleanup.
fn prepend_path(bin_dir: &Path) -> String {
    let prev = std::env::var("PATH").unwrap_or_default();
    let new = format!("{}:{prev}", bin_dir.display());
    // SAFETY: single-threaded test setup before any background tasks
    // depend on PATH; the env_guard serializes against other writers.
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

/// Create a wave under `cove_id`. `theme` is required end-to-end (#177);
/// the value here is inert for this test (we drive the theme under test
/// via the codex card body + the mid-session toggle).
async fn create_wave(app: axum::Router, cove_id: &str) -> String {
    let (status, body) = post(
        app,
        "/api/waves",
        json!({
            "cove_id": cove_id,
            "title": "osc-echo-cooked",
            "cwd": "/tmp/osc-echo-cooked-test",
            "attach_folder": true,
            "theme": { "fg": [216, 219, 226], "bg": [15, 20, 24] },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "wave create body={body}");
    body["id"].as_str().expect("wave.id").to_string()
}

/// Shell direction of fix B: a mid-session `TerminalThemeUpdate` against
/// a child that never enabled DECSET 1004 (a shell in ZLE raw mode) must
/// NOT cause the synthetic OSC 10/11 to be written to the PTY — so the
/// shell's line editor has nothing to surface, and no OSC literal ever
/// reaches a `RenderPatch`.
//
// `env_guard()`'s `std::sync::Mutex` is intentionally held across
// `.await` points (it serializes process-global `PATH` writes). Switching
// to `tokio::sync::Mutex` would add an async dep for a conceptually
// blocking single-resource lock; allow the lint — same justification as
// `theme_osc_roundtrip.rs`.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn cooked_shell_theme_toggle_does_not_echo_osc() {
    let _guard = env_guard();
    let tmp = TempDir::new().expect("tempdir for fixture staging");
    let bin_dir = tmp.path().join("bin");

    // Wire up: fake `codex` on PATH resolves to the cooked-shell child.
    stage_fake_codex(&bin_dir);
    let prev_path = prepend_path(&bin_dir);

    let boot = boot_full().await;
    let wave_id = create_wave(boot.app.clone(), &boot.cove_id).await;

    // POST the codex card with an initial DARK theme. The daemon spawns
    // `sh -c codex` → our cooked-shell-child fixture, which enters ZLE-
    // style raw mode (ECHO off, ICANON off) but never enables DECSET 1004
    // and never emits an OSC query, then just blocks reading stdin —
    // exactly a shell sitting at its prompt.
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

    let raw_terminal_id = body["payload"]["terminal_id"]
        .as_str()
        .expect("card.payload.terminal_id present")
        .to_string();

    // Open a WS as the browser does and complete the handshake (Owner
    // role is required for the daemon to honor TerminalThemeUpdate).
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

    // Drain frames until ServerHello so we know the handshake completed
    // and Owner role was assigned (the daemon won't apply
    // TerminalThemeUpdate until then). Stash any RenderPatch bytes that
    // arrive in the meantime — there shouldn't be OSC literals, but if a
    // regression echoed pre-toggle, collecting from the handshake on is
    // strictly safer than starting after.
    let mut render_bytes = Vec::<u8>::new();
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
                match msg {
                    DaemonMsg::RenderPatch(p) => render_bytes.extend_from_slice(&p.data),
                    DaemonMsg::ServerHello { .. } => break,
                    _ => {}
                }
            }
            Ok(Some(Ok(_))) => continue,
            _ => panic!("ws stream ended before ServerHello"),
        }
    }

    // Toggle to a DIFFERENT (light) theme. The colors differ from the
    // dark create-time theme on purpose: that ensures fix A (the
    // terminal_session.rs "colors == current default → suppress" guard)
    // does NOT pre-empt this update, so the daemon actually reaches fix
    // B's `Effect::TerminalThemeUpdate` arm — the branch under test.
    let toggle = ClientMsg::TerminalThemeUpdate {
        fg: (24, 33, 41),    // light-theme fg
        bg: (247, 249, 252), // light-theme bg
    };
    ws.send(TMessage::Text(serde_json::to_string(&toggle).unwrap()))
        .await
        .unwrap();

    // Collect every RenderPatch for a fixed window after the toggle.
    // Timing matters in two directions:
    //   - Long enough that, IF fix B were broken, the synthetic OSC
    //     write → cooked-tty echo → daemon vte → broadcast RenderPatch
    //     round-trip has time to land (avoids a false PASS).
    //   - The window is a hard ceiling, not a "first patch wins": we
    //     keep reading until it elapses so a late echo can't sneak past.
    // The select-pump shape means we must keep calling `ws.next()` to
    // keep the bridge's up-arm moving (same note as
    // theme_osc_roundtrip.rs case 2) — which this drain loop does.
    let drain_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < drain_deadline {
        match tokio::time::timeout(Duration::from_millis(200), ws.next()).await {
            Ok(Some(Ok(TMessage::Text(t)))) => {
                if let Ok(DaemonMsg::RenderPatch(p)) = serde_json::from_str::<DaemonMsg>(&t) {
                    render_bytes.extend_from_slice(&p.data);
                }
            }
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(_))) | Ok(None) => break,
            Err(_) => continue, // 200ms read timeout — keep draining until the window closes
        }
    }

    let _ = ws.close(None).await;
    restore_path(prev_path);

    // Assert no OSC sequence appears in any broadcast PTY output. We
    // check both forms:
    //   - the raw sequence `\x1b]10;rgb:` / `\x1b]11;rgb:` (would appear
    //     if the daemon wrote it AND the child somehow forwarded it), and
    //   - the body `]10;rgb:` / `]11;rgb:` (a shell's line editor surfaces
    //     the leading ESC as `^[`, so the bytes that hit a RenderPatch on
    //     a broken fix B would be `^[]10;rgb:…` — the `]10;rgb:` substring
    //     is the reliable tell).
    let contains = |needle: &[u8]| render_bytes.windows(needle.len()).any(|w| w == needle);
    let preview = String::from_utf8_lossy(&render_bytes);
    assert!(
        !contains(b"\x1b]10;rgb:"),
        "shell surfaced raw OSC 10 after theme toggle (fix B regressed); \
         render bytes ({}): {preview:?}",
        render_bytes.len()
    );
    assert!(
        !contains(b"\x1b]11;rgb:"),
        "shell surfaced raw OSC 11 after theme toggle (fix B regressed); \
         render bytes ({}): {preview:?}",
        render_bytes.len()
    );
    assert!(
        !contains(b"]10;rgb:"),
        "shell surfaced caret-form OSC 10 (`^[]10;rgb:…`) after theme toggle \
         (fix B regressed); render bytes ({}): {preview:?}",
        render_bytes.len()
    );
    assert!(
        !contains(b"]11;rgb:"),
        "shell surfaced caret-form OSC 11 (`^[]11;rgb:…`) after theme toggle \
         (fix B regressed); render bytes ({}): {preview:?}",
        render_bytes.len()
    );
}
