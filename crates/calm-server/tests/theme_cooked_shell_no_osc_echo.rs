//! Hermetic regression test for the **DECSET-1004 gate, shell
//! direction** (terminal OSC-echo bug, refined by #295 followup 1).
//!
//! ## What the 1004 gate is and why this test exists
//!
//! "New terminal" runs an interactive shell (`zsh` / `fish`) which sits
//! at its prompt. A modern shell prompt is **not** cooked: zsh's ZLE
//! (fish's reader) drives the line via a raw-mode editor — ECHO off,
//! ICANON off, identical termios to a real TUI. What it does NOT do is
//! opt into DECSET 1004 (focus event reporting): it only enables
//! bracketed paste (`ESC[?2004h`) and never queries OSC 10/11.
//!
//! Pre-#295, the daemon's mid-session `Effect::TerminalThemeUpdate`
//! branch wrote a synthetic OSC 10/11 reply pair plus focus-in
//! `ESC[I` onto the PTY. PR #296's fix-B gate skipped that write when
//! the child hadn't enabled 1004, since a shell's ZLE would otherwise
//! render the OSC bytes at the prompt as syntax-highlighted garbage.
//!
//! #295 followup 1 removed the unsolicited OSC 10/11 RGB write
//! entirely (the focus-in alone is enough — codex re-queries on
//! `FocusGained`). The **1004 gate is preserved** for the remaining
//! `ESC[I` byte: a shell's line editor would surface a stray focus-in
//! as input too (e.g. zsh's ZLE binds unmapped CSI sequences to
//! `self-insert-unmeta`, which displays them at the cursor).
//!
//! ## What this test still pins down
//!
//! Post-#295 the daemon writes only `\x1b[I` on theme toggle, and
//! only when the child enabled 1004. We assert two invariants here:
//!   1. The legacy OSC 10/11 RGB write paths stay closed (catches a
//!      future regression that re-introduces unsolicited RGB without
//!      thinking about the shell case).
//!   2. The `\x1b[I` write is also gated on 1004 — a cooked-shell
//!      child must see NEITHER the OSC RGB nor the focus-in. This
//!      is the load-bearing assertion post-#295 (the OSC assertions
//!      are now belt-and-braces against accidental reintroduction).
//!
//! ## Complementary to `theme_osc_roundtrip.rs`
//!
//! `theme_osc_roundtrip.rs::osc_roundtrip_mid_session_theme_update`
//! anchors the *opted-in* direction with the `osc-probe-child`
//! fixture (which sends `ESC[?1004h` on startup, like codex): it
//! proves the daemon DOES write `ESC[I` to a 1004-aware child after
//! a toggle (and proves the daemon does NOT write unsolicited OSC
//! RGB).
//!
//! This file anchors the *opposite* direction with the
//! `cooked-shell-child` fixture: a shell in ZLE raw mode that never
//! enabled 1004 must **not** receive the focus-in CSI (or any of the
//! legacy OSC RGB bytes, in case they get re-added in error). We
//! assert it by collecting every broadcast `RenderPatch` after the
//! toggle and checking none carry `\x1b[I` or the OSC literal a
//! shell's line editor would surface if the bytes had reached its
//! stdin.
//!
//! Without this test, the shell direction had only manual Playwright
//! coverage; removing the 1004 gate would slip past CI. (The focus-in
//! assertion is the post-#295 load-bearing check; removing the gate
//! today writes `ESC[I` to the cooked-shell child, which surfaces in
//! a RenderPatch as a ZLE-rendered byte and fails this test.)
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
        proc_supervisor_sock: None,
    });
    let card_role_cache = CardRoleCache::new();
    // #293 cutover: `POST /api/waves` now synchronously boots a `codex
    // app-server` via `codex_bin` (the kernel-owned spec-push channel),
    // BEFORE returning 201. Point `codex_bin` at the `osc-probe-child`
    // fixture, which answers the app-server handshake when invoked with the
    // `app-server` subcommand (see `tests/fixtures/osc-probe-child/appserver.rs`).
    // The codex-CARD spawn under test still resolves `codex` via the staged
    // PATH symlink (→ cooked-shell-child) — that path runs `sh -c codex`, not
    // `codex_bin`, so this only affects the wave-create boot.
    let mut codex_client = CodexClient::new_stub();
    codex_client.codex_bin = env!("CARGO_BIN_EXE_osc-probe-child").to_string();
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

/// Shell direction of the 1004 gate: a mid-session
/// `TerminalThemeUpdate` against a child that never enabled DECSET
/// 1004 (a shell in ZLE raw mode) must NOT cause the focus-in CSI
/// `\x1b[I` to be written to the PTY — so the shell's line editor
/// has nothing to surface, and no focus-in (and no legacy OSC literal)
/// ever reaches a `RenderPatch`.
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
    //   - Long enough that, IF the 1004 gate were broken, the
    //     `ESC[I` (or, regression-side, an unsolicited OSC RGB)
    //     write → ZLE-raw shell → daemon vte → broadcast RenderPatch
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

    // Assert nothing the daemon could write on theme toggle reaches
    // the cooked-shell child's RenderPatch stream. Post-#295 the only
    // byte sequence at risk is `\x1b[I` (focus-in); the OSC RGB
    // checks are kept as defense-in-depth against an accidental
    // re-introduction of the unsolicited path. For each sequence we
    // check both forms:
    //   - the raw sequence (would appear if the daemon wrote it AND
    //     the child somehow forwarded it), and
    //   - the caret-prefixed body (a shell's line editor surfaces
    //     the leading ESC as `^[`, so the bytes that hit a
    //     RenderPatch on a broken gate would be e.g. `^[[I` — the
    //     body substring is the reliable tell).
    let contains = |needle: &[u8]| render_bytes.windows(needle.len()).any(|w| w == needle);
    let preview = String::from_utf8_lossy(&render_bytes);

    // Post-#295 load-bearing assertion: focus-in must NOT reach a
    // non-1004 child. This is the byte the daemon still writes today.
    assert!(
        !contains(b"\x1b[I"),
        "shell surfaced raw focus-in (ESC[I) after theme toggle \
         (1004 gate regressed); render bytes ({}): {preview:?}",
        render_bytes.len()
    );

    // Belt-and-braces against an accidental re-introduction of the
    // legacy unsolicited OSC 10/11 RGB write.
    assert!(
        !contains(b"\x1b]10;rgb:"),
        "shell surfaced raw OSC 10 after theme toggle (legacy \
         unsolicited write re-introduced?); render bytes ({}): {preview:?}",
        render_bytes.len()
    );
    assert!(
        !contains(b"\x1b]11;rgb:"),
        "shell surfaced raw OSC 11 after theme toggle (legacy \
         unsolicited write re-introduced?); render bytes ({}): {preview:?}",
        render_bytes.len()
    );
    assert!(
        !contains(b"]10;rgb:"),
        "shell surfaced caret-form OSC 10 (`^[]10;rgb:…`) after theme toggle \
         (legacy unsolicited write re-introduced?); render bytes ({}): {preview:?}",
        render_bytes.len()
    );
    assert!(
        !contains(b"]11;rgb:"),
        "shell surfaced caret-form OSC 11 (`^[]11;rgb:…`) after theme toggle \
         (legacy unsolicited write re-introduced?); render bytes ({}): {preview:?}",
        render_bytes.len()
    );
}
