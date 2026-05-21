//! Integration test for the codex hands-free spawn primitive's
//! v2-protocol interaction with the calm-session-daemon.
//!
//! What we're actually validating
//! ------------------------------
//!
//! The hands-free path has four layers (managed hooks, composer pre-
//! fill, trust silencer, auto-submit). Layers 1-3 are pure file/string
//! plumbing covered by unit tests. Layer 4 is the only one that touches
//! the live daemon protocol, and that's where v2 compatibility could
//! break. This file pins it.
//!
//! Specifically:
//!
//!   * `DaemonClient::inject_stdin` opens a fresh Unix-socket
//!     connection to a live PTY-backed daemon, frames a `ClientHello`
//!     with `capabilities.kernel_originated_input = true` +
//!     `role_hint = Observer`, then an `Input` frame with our bytes,
//!     then drops. The daemon's owner-only gate on `Input` must accept
//!     the write because of the capability (the post-PR-87 v2 contract).
//!
//!   * The browser's Owner WS connection must remain Owner — no
//!     `OwnerChanged` frame fires when the kernel injects. This is what
//!     keeps the user able to keep typing while a hands-free spawn
//!     finishes; if our injection accidentally stole Owner, the
//!     browser's next keystroke would land in nowhere.
//!
//!   * The injected bytes flow through the PTY just like browser-typed
//!     bytes — they show up in subsequent `RenderPatch` frames. (We use
//!     `/bin/sh` and inject `printf hello\n` because that's the
//!     tightest substring-match we can do without exit-code timing
//!     races.)
//!
//! What we're explicitly NOT testing here
//! --------------------------------------
//!
//! Codex itself is not on PATH in CI. The interaction with the actual
//! codex CLI (its composer + auto-submit-on-`\r`) is covered manually
//! and at the unit-test level (`shell_single_quote`, `build_config_toml`,
//! `should_auto_submit`). This test stands in for the wire-level
//! contract between `inject_stdin` and the daemon, which is the only
//! place the v2 protocol can regress invisibly.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewCove, NewWave};
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

const STEP_TIMEOUT: Duration = Duration::from_secs(5);

/// Mirror of `tests/ws_terminal_e2e.rs::locate_daemon_bin`. Kept local
/// rather than refactored into a shared `tests/common/` module because
/// the shared-module song-and-dance ranges from one-off-helper to
/// `cargo` quirks (`Cargo.toml` `[[test]]` declarations vs. `mod`
/// inclusion); a 10-line copy is easier to read than the alternative.
fn locate_daemon_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop();
    p.pop();
    p.push("calm-session-daemon");
    assert!(
        p.exists(),
        "calm-session-daemon not found at {p:?}; run \
         `cargo build -p calm-session --bin calm-session-daemon` first, or \
         use `cargo test --workspace` which builds workspace bins"
    );
    p
}

/// Boot a real daemon-capable fixture. The `events` field on the returned
/// `AppState`-equivalent tuple is exposed (`bus`) so the new subscriber-
/// chain tests can publish synthetic `Event::CodexHook` envelopes; the
/// `repo` is exposed so they can read back the `card.payload` /
/// `terminal.program` the production route stamps.
async fn boot_full() -> (
    std::net::SocketAddr,
    axum::Router,
    Arc<DaemonClient>,
    String,
    TempDir,
    EventBus,
    Arc<dyn Repo>,
) {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");

    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );

    let cove = repo
        .cove_create(NewCove {
            name: "cf".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "cf".into(),
            sort: None,
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        session_daemon_bin: locate_daemon_bin(),
    });
    // Single shared bus so handler emissions, the auto-submit subscriber,
    // and the test's `bus.emit(...)` calls all see the same channel. The
    // pre-existing test never subscribed itself, so this is purely
    // additive — the v2-contract test below still calls `boot_full()` and
    // simply ignores the extra return values.
    let bus = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        bus.clone(),
        daemon.clone(),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
        )),
        Arc::new(CodexClient::new_stub()),
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

    (addr, app, daemon, wave.id, tmp, bus, repo)
}

async fn rest_post(app: axum::Router, uri: String, body: Value) -> (StatusCode, Value) {
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

/// Drain frames until `pred` returns Some or we hit the budget. Tolerant
/// of `RenderPatch` / `RenderSnapshot` / `ChildReady` noise.
async fn wait_for_some<T>(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    label: &str,
    budget: Duration,
    mut pred: impl FnMut(&DaemonMsg) -> Option<T>,
) -> T {
    let deadline = tokio::time::Instant::now() + budget;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            panic!(
                "timed out waiting for {label}; saw {} other frames: {seen:?}",
                seen.len()
            );
        }
        let remaining = deadline - now;
        let msg = match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(TMessage::Text(t)))) => {
                serde_json::from_str::<DaemonMsg>(&t).expect("decode DaemonMsg from ws text frame")
            }
            Ok(Some(Ok(TMessage::Close(_)))) => panic!("ws closed before {label}; saw {seen:?}"),
            Ok(Some(Ok(_other))) => continue,
            Ok(Some(Err(e))) => panic!("ws read error waiting for {label}: {e}"),
            Ok(None) => panic!("ws stream ended before {label}; saw {seen:?}"),
            Err(_) => panic!(
                "timed out waiting for {label}; saw {} other frames: {seen:?}",
                seen.len()
            ),
        };
        if let Some(out) = pred(&msg) {
            return out;
        }
        seen.push(match &msg {
            DaemonMsg::RenderPatch(p) => format!("RenderPatch(rev={})", p.render_rev),
            DaemonMsg::RenderSnapshot(s) => format!("RenderSnapshot(rev={})", s.render_rev),
            DaemonMsg::ChildReady { .. } => "ChildReady".into(),
            DaemonMsg::OwnerChanged { .. } => "OwnerChanged".into(),
            other => format!("{other:?}"),
        });
    }
}

/// Spawn a `/bin/sh` terminal, attach as Owner via WS, inject bytes
/// via the kernel's privileged path, observe them echo, and assert the
/// browser Owner connection stays Owner throughout.
#[tokio::test]
async fn inject_stdin_writes_via_kernel_capability_without_stealing_owner() {
    let (addr, app, daemon, wave_id, _tmp, _bus, _repo) = boot_full().await;

    // ---- 1. Card + terminal via the real REST path. -------------------
    let (status, card) = rest_post(
        app.clone(),
        format!("/api/waves/{wave_id}/cards"),
        json!({ "kind": "terminal", "payload": null, "sort": 1.0 }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create card: body={card:?}");
    let card_id = card["id"].as_str().unwrap().to_string();

    let (status, term) = rest_post(
        app.clone(),
        format!("/api/cards/{card_id}/terminal"),
        json!({ "program": "/bin/sh", "cwd": "", "env": {} }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "create terminal: body={term:?}"
    );
    let raw_terminal_id = term["id"].as_str().unwrap().to_string();
    // `model::new_id` returns the *simple* (no-dashes) UUID form via the
    // API. Production callers (`codex_auto_submit`) read this exact form
    // off `card.payload.terminal_id` and hand it to BOTH `sock_path`
    // and `inject_stdin`; the latter normalizes internally to the
    // hyphenated form the daemon's handshake match expects. That double-
    // duty is the contract this test pins.
    let hyphenated_terminal_id = Uuid::parse_str(&raw_terminal_id)
        .expect("terminal id is a uuid")
        .to_string();

    // ---- 2. Browser-style WS attach as Owner. -------------------------
    let ws_url = format!("ws://{addr}/api/terminals/{raw_terminal_id}");
    let (mut ws, _resp) =
        tokio::time::timeout(STEP_TIMEOUT, tokio_tungstenite::connect_async(&ws_url))
            .await
            .expect("ws connect timed out")
            .expect("ws connect failed");

    let browser_client_id = Uuid::new_v4();
    let hello = ClientMsg::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id: raw_terminal_id.clone(),
        client_id: browser_client_id,
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
            // The WS bridge unconditionally strips this to `false` (see
            // ws/terminal.rs §SECURITY); a hostile browser CANNOT spoof
            // kernel-originated input. The kernel-side `inject_stdin`
            // path, by contrast, sets it to `true` because it speaks
            // directly to the daemon over the kernel-private Unix socket
            // — that's exactly what this test verifies works.
            kernel_originated_input: false,
        },
    };
    ws.send(TMessage::Text(serde_json::to_string(&hello).unwrap()))
        .await
        .unwrap();

    // ServerHello — confirms Owner role for the browser client.
    let server_hello = wait_for_some(&mut ws, "ServerHello", STEP_TIMEOUT, |msg| match msg {
        DaemonMsg::ServerHello {
            client_role,
            terminal_id,
            owner_client_id,
            ..
        } => Some((*client_role, terminal_id.clone(), *owner_client_id)),
        _ => None,
    })
    .await;
    assert!(
        matches!(server_hello.0, Role::Owner),
        "first attach should be Owner, got {:?}",
        server_hello.0
    );
    assert_eq!(
        server_hello.1, hyphenated_terminal_id,
        "daemon stamps hyphenated terminal_id in ServerHello"
    );
    assert_eq!(
        server_hello.2,
        Some(browser_client_id),
        "daemon ServerHello should report the browser client as Owner"
    );

    // ---- 3. Inject from the kernel-private path. ----------------------
    // `printf hello\n` is the smallest thing that lands a deterministic
    // substring in the PTY output without depending on a prompt redraw
    // or exit timing. Newline (not `\r`) because the test isn't trying
    // to mimic codex's submit behavior — `\r` would also work but the
    // shell's response to it is more variable across shells.
    //
    // We deliberately use `raw_terminal_id` (the simple no-dashes form
    // returned by the API and the exact bytes a production caller would
    // read from `card.payload.terminal_id`) for BOTH the sock-path
    // lookup and the inject_stdin id argument. The first is correct
    // because `spawn_daemon_for` created the socket file at
    // `daemon.sock_path(&term.id)`; the second exercises
    // `inject_stdin`'s internal normalization to hyphenated form for
    // the daemon handshake. If the normalization were ever removed,
    // this test would regress to a `BadHandshake`-driven
    // connection-closed error.
    // Wait a beat for the /bin/sh child to finish initializing under the
    // PTY. The daemon's handshake returns the moment it accepts the
    // socket connection, but the shell prompt / stdin readiness happens
    // separately on another channel. 100ms is empirically enough on dev
    // machines without padding the happy-path test budget.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let sock_path = daemon.sock_path(&raw_terminal_id);
    daemon
        .inject_stdin(&sock_path, &raw_terminal_id, b"printf hello\n")
        .await
        .expect("inject_stdin failed");
    // Give the daemon a moment to process the Input frame, forward
    // through the PTY, and broadcast the resulting RenderPatch to all
    // attached clients (the kernel connection has closed by now, but the
    // browser is still listening). Without this, fast machines can race
    // the close-vs-process and we'd false-alarm before the
    // PTY round-trip lands.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // ---- 4. Drain frames until we see "hello" in the rendered output. -
    // RenderPatch carries the raw VT chunk. Multiple chunks may interleave
    // with ChildReady / RenderSnapshot — collect across all of them.
    let mut concat: Vec<u8> = Vec::new();
    let deadline = tokio::time::Instant::now() + STEP_TIMEOUT;
    let mut owner_changed_seen = false;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let msg = match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(TMessage::Text(t)))) => {
                serde_json::from_str::<DaemonMsg>(&t).expect("decode")
            }
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(e))) => panic!("ws read error: {e}"),
            Ok(None) => break,
            Err(_) => break,
        };
        match &msg {
            DaemonMsg::RenderPatch(p) => concat.extend_from_slice(&p.data),
            DaemonMsg::RenderSnapshot(s) => {
                // A snapshot is the daemon's authoritative view at one
                // point in time — append for the substring search rather
                // than `clear()`-ing, so a snapshot triggered by our
                // kernel-client's hello ResizePty doesn't drop earlier
                // RenderPatches that already carried the injected bytes.
                concat.extend_from_slice(&s.data);
            }
            DaemonMsg::OwnerChanged { .. } => {
                owner_changed_seen = true;
            }
            _ => {}
        }
        if concat.windows(b"hello".len()).any(|w| w == b"hello") {
            break;
        }
    }

    assert!(
        concat.windows(b"hello".len()).any(|w| w == b"hello"),
        "expected 'hello' to appear in PTY output after inject_stdin; \
         got {} bytes: {:?}",
        concat.len(),
        String::from_utf8_lossy(&concat),
    );

    // ---- 5. The browser is still Owner. -------------------------------
    //
    // The whole point of `role_hint = Observer` + the
    // `kernel_originated_input` capability is that the kernel can write
    // without stealing Owner. If we saw an `OwnerChanged` frame in the
    // drain above, that contract is broken.
    assert!(
        !owner_changed_seen,
        "kernel inject_stdin must NOT trigger OwnerChanged \
         (would demote the browser's Owner connection)"
    );

    // Belt-and-suspenders: send an `Input` from the browser to prove it
    // still has write authority. A NotOwner ProtocolError here would
    // mean the kernel inject stole Owner silently.
    ws.send(TMessage::Text(
        serde_json::to_string(&ClientMsg::Input(b"\n".to_vec())).unwrap(),
    ))
    .await
    .unwrap();
    let post_check_deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    while tokio::time::Instant::now() < post_check_deadline {
        let remaining = post_check_deadline - tokio::time::Instant::now();
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(TMessage::Text(t)))) => {
                let msg: DaemonMsg = serde_json::from_str(&t).unwrap();
                if let DaemonMsg::ProtocolError { code, message, .. } = msg {
                    panic!(
                        "browser Input was rejected with ProtocolError {code:?} {message:?} \
                         — kernel inject_stdin must have stolen Owner"
                    );
                }
            }
            Ok(Some(_)) => continue,
            _ => break,
        }
    }
}

// ===========================================================================
// Regression-guard tests for the `codex_auto_submit` subscriber + the
// `POST /api/cards/:id/codex` payload-stamp contract.
//
// The v2-protocol test above directly calls `daemon.inject_stdin(...)` and
// pins one slice of the wire-level contract. These three tests close the
// two remaining gaps the PR's diff actually opens:
//
//   1. Bus subscriber chain — production path is: codex hook bridge POSTs
//      → server emits `Event::CodexHook` → `codex_auto_submit::spawn`'s
//      subscriber filters on `kind == "hook.codex.session_start"` and
//      `payload.auto_submit == true`, looks up `terminal_id` on the
//      card, sleeps SUBMIT_DELAY, then calls `inject_stdin`. The
//      `auto_submit_subscriber_chain_emits_input_on_session_start` test
//      below exercises that entire chain. The negative companion
//      `auto_submit_subscriber_skips_when_payload_opt_out` proves the
//      gate is honoured.
//
//   2. Route → payload stamp — `POST /api/cards/:id/codex` is the only
//      caller that decides what lands in `card.payload.auto_submit`
//      (`routes/codex.rs::spawn_codex_for`, "only stamp when true"
//      comment). It also assembles the daemon program — `codex` for the
//      no-prompt case, `codex '<prompt>'` for the prompt case. Renaming
//      either field on the wire or breaking the conditional stamp would
//      silently take hands-free codex out of service.
//      `post_codex_stamps_payload_and_program_from_prompt_and_auto_submit`
//      pins both.
//
// Codex itself is not on PATH in CI, so the subscriber tests install a
// per-tempdir shell shim at `<tmp>/codex` that prints a sentinel then
// `exec cat`s its stdin. The shim's stdout flows back through the PTY
// just like real codex would, and `cat`'s `\r` echo gives us a clean
// substring to assert on. PATH is mutated *only* through a fresh
// `Command::env_clear`-equivalent in the shim layer — see comment on
// `install_codex_shim` for why the test process inherits the change.
// ---------------------------------------------------------------------------

/// Drop guard for a PATH override. Restoring on Drop keeps a panic in one
/// test from leaking a fake `codex` shim onto sibling tests; cargo runs
/// `#[tokio::test]`s on a shared process, so PATH is genuinely shared.
struct PathGuard {
    prev: Option<std::ffi::OsString>,
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        // SAFETY: see `install_codex_shim` — the same single-test serialization
        // rationale applies to the restore.
        unsafe {
            match self.prev.take() {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
        }
    }
}

/// Install a fake `codex` shell shim in a tempdir and prepend that dir to
/// `PATH` so the daemon (spawned by `spawn_daemon_for` via
/// `/bin/sh -c codex ...`) picks it up. The daemon process inherits the
/// test-process env at spawn time, so the PATH override must be in place
/// **before** the `POST /api/cards/:id/codex` call lands in
/// `spawn_daemon_for`.
///
/// The shim prints a sentinel (`FAKE_CODEX_READY\n`) so the test can sync
/// on PTY readiness without depending on `ChildReady` plumbing, then
/// `exec cat`s its stdin so the injected `\r` bytes flow back through the
/// PTY and become observable as `RenderPatch`/`RenderSnapshot` content.
///
/// Returns the shim's `TempDir` (the caller must keep it alive for the
/// test's duration — dropping it removes the shim out from under any
/// still-respawning daemon) and a `PathGuard` that restores `PATH` on drop.
fn install_codex_shim() -> (TempDir, PathGuard) {
    use std::os::unix::fs::PermissionsExt;
    let shim_dir = TempDir::new().expect("tempdir for codex shim");
    let shim_path = shim_dir.path().join("codex");
    std::fs::write(
        &shim_path,
        "#!/bin/sh\n\
         # Fake codex shim for codex_hands_free integration tests. Real codex\n\
         # is not on PATH in CI; this stand-in lets us observe `\\r` injection\n\
         # via the PTY echo path without needing the upstream binary.\n\
         echo FAKE_CODEX_READY\n\
         exec cat\n",
    )
    .expect("write codex shim");
    let mut perms = std::fs::metadata(&shim_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&shim_path, perms).unwrap();

    let prev = std::env::var_os("PATH");
    let new_path = match prev.as_ref() {
        Some(p) => {
            let mut s = shim_dir.path().as_os_str().to_owned();
            s.push(":");
            s.push(p);
            s
        }
        None => shim_dir.path().as_os_str().to_owned(),
    };
    // SAFETY: `std::env::set_var` is racy across threads. Cargo runs tests
    // in parallel on the same process, so in principle two of these tests
    // running concurrently could clobber each other's PATH. We mitigate by
    // serializing the two subscriber-chain tests through `SHIM_LOCK` (see
    // its call sites below) — `set_var` only runs while the lock is held.
    unsafe {
        std::env::set_var("PATH", new_path);
    }
    (shim_dir, PathGuard { prev })
}

/// Single shared mutex to serialize the two subscriber-chain tests so their
/// PATH mutations don't race. Cheap, async-friendly, and only held for the
/// duration of the test body.
static SHIM_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Drive the production POST /api/cards/:id/codex route and return the
/// updated `Card` (with `payload.terminal_id` stamped). Asserts the route
/// returned 202; on failure the body is printed so a regression surfaces
/// with a useful error message rather than a bare status mismatch.
async fn post_codex(app: axum::Router, card_id: &str, body: Value) -> Value {
    let (status, json) = rest_post(app, format!("/api/cards/{card_id}/codex"), body).await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "POST /api/cards/{card_id}/codex failed: body={json:?}",
    );
    json
}

/// Attach a browser-style Owner WS and consume the initial `ServerHello`.
/// Returns the open stream + the bytes from the ServerHello's embedded
/// snapshot (so the caller can search through pre-attach render state
/// such as the shim's sentinel `echo`).
async fn attach_owner_ws(
    addr: std::net::SocketAddr,
    terminal_id: &str,
) -> (
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    Vec<u8>,
) {
    let ws_url = format!("ws://{addr}/api/terminals/{terminal_id}");
    let (mut ws, _resp) =
        tokio::time::timeout(STEP_TIMEOUT, tokio_tungstenite::connect_async(&ws_url))
            .await
            .expect("ws connect timed out")
            .expect("ws connect failed");

    let hello = ClientMsg::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id: terminal_id.to_string(),
        client_id: Uuid::new_v4(),
        desired_size: PtySize {
            cols: 80,
            rows: 24,
            pixel_width: None,
            pixel_height: None,
        },
        cell_size: None,
        // `All` (not `None`) because the shim's `FAKE_CODEX_READY` sentinel
        // and any pre-attach PTY output (which is the common case here —
        // the route blocks on `spawn_daemon_for`, by which time the shim
        // has already run) need to replay onto our just-attached client.
        // The v2-contract test above uses `None` because it controls the
        // injection timing relative to attach; we don't.
        initial_scrollback: InitialScrollback::All,
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
    let initial_bytes = wait_for_some(&mut ws, "ServerHello", STEP_TIMEOUT, |msg| match msg {
        // The ServerHello carries an embedded `snapshot` with the current
        // screen contents (per `InitialScrollback::All`); pull those bytes
        // out so the caller's substring search can match against text the
        // shim wrote BEFORE we attached. Without this the snapshot lives
        // only inside the ServerHello frame and our render-only drain
        // would miss it entirely.
        DaemonMsg::ServerHello { snapshot, .. } => Some(snapshot.data.clone()),
        _ => None,
    })
    .await;
    (ws, initial_bytes)
}

/// Drain frames into a single concatenated render buffer until `pred`
/// returns true on the cumulative buffer or the deadline elapses. Mirrors
/// the substring-search loop in the v2-contract test above but factored
/// out for the new tests' reuse. The bool return distinguishes "saw the
/// expected bytes" from "deadline elapsed without seeing them".
async fn drain_until_render_contains(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    seed: Vec<u8>,
    budget: Duration,
    mut pred: impl FnMut(&[u8]) -> bool,
) -> (bool, Vec<u8>) {
    let mut concat: Vec<u8> = seed;
    if pred(&concat) {
        return (true, concat);
    }
    let deadline = tokio::time::Instant::now() + budget;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let msg = match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(TMessage::Text(t)))) => {
                serde_json::from_str::<DaemonMsg>(&t).expect("decode")
            }
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(_))) | Ok(None) | Err(_) => break,
        };
        match &msg {
            DaemonMsg::RenderPatch(p) => concat.extend_from_slice(&p.data),
            DaemonMsg::RenderSnapshot(s) => concat.extend_from_slice(&s.data),
            _ => {}
        }
        if pred(&concat) {
            return (true, concat);
        }
    }
    (false, concat)
}

/// Production-shape happy path: card created via the REST route stamps
/// `payload.auto_submit == true`, the auto-submit subscriber sees the
/// synthetic `session_start` event, sleeps SUBMIT_DELAY, then injects
/// `\r` through the per-terminal daemon socket. The injected byte must
/// echo back through the PTY's cooked-mode input loop (our shim is
/// `exec cat`, which mirrors stdin to stdout — same observation surface
/// the v2-contract test uses) within the test's budget.
///
/// This is the only test in this crate that wires up
/// `codex_auto_submit::spawn` against a real daemon end-to-end; the unit
/// tests in `codex_auto_submit.rs::tests` only cover the
/// `should_auto_submit` predicate, not the subscriber loop, the bus
/// envelope unwrapping, the card lookup, the terminal_id resolution, or
/// the `inject_stdin` invocation.
#[tokio::test]
async fn auto_submit_subscriber_chain_emits_input_on_session_start() {
    let _lock = SHIM_LOCK.lock().await;
    let (_shim_dir, _path_guard) = install_codex_shim();

    let (addr, app, daemon, wave_id, _tmp, bus, repo) = boot_full().await;

    // Spawn the production subscriber against the same bus the route uses.
    // `boot_full` doesn't do this because the v2-contract test above
    // doesn't need it — it's the chain this test exists to pin.
    calm_server::codex_auto_submit::spawn(repo.clone(), daemon.clone(), bus.clone());

    // 1. Create a codex card via the standard cards route.
    let (status, card) = rest_post(
        app.clone(),
        format!("/api/waves/{wave_id}/cards"),
        json!({ "kind": "codex", "payload": null, "sort": 1.0 }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create codex card: {card:?}");
    let card_id = card["id"].as_str().unwrap().to_string();

    // 2. Bind a real codex terminal via the production route. `cwd = /tmp`
    //    keeps the per-spawn `config.toml`'s `[projects."/tmp"]` table
    //    valid. `auto_submit = true` is the opt-in this test cares about.
    let updated_card = post_codex(
        app.clone(),
        &card_id,
        json!({ "prompt": "noop", "auto_submit": true, "cwd": "/tmp" }),
    )
    .await;
    assert_eq!(
        updated_card["payload"]["auto_submit"], true,
        "route must stamp auto_submit on payload for subscriber to pick up"
    );
    let terminal_id = updated_card["payload"]["terminal_id"]
        .as_str()
        .expect("terminal_id stamped by route")
        .to_string();

    // 3. Attach as Owner and wait for the shim's readiness sentinel. The
    //    sentinel proves the PTY child is past its `echo` and into `cat`,
    //    ready to echo whatever lands on stdin — including our soon-to-
    //    arrive `\r`. The sentinel may already be in the ServerHello
    //    snapshot (route blocks on `spawn_daemon_for`, by which time the
    //    shim has typically run); `attach_owner_ws` returns those bytes
    //    so the substring search can hit either path.
    let (mut ws, initial) = attach_owner_ws(addr, &terminal_id).await;
    let (saw_ready, prefix) =
        drain_until_render_contains(&mut ws, initial, Duration::from_secs(3), |buf| {
            buf.windows(b"FAKE_CODEX_READY".len())
                .any(|w| w == b"FAKE_CODEX_READY")
        })
        .await;
    assert!(
        saw_ready,
        "shim never printed sentinel within budget; got {} bytes: {:?}",
        prefix.len(),
        String::from_utf8_lossy(&prefix),
    );

    // 4. Emit the synthetic session_start exactly the way the hook bridge
    //    would — same `kind` discriminator the subscriber matches on,
    //    same envelope shape `log_pure_event` would have produced. The
    //    subscriber's SUBMIT_DELAY (600 ms) plus a generous round-trip
    //    cushion = ~2 s budget below.
    bus.emit(
        "ai:codex",
        calm_server::event::Event::CodexHook {
            card_id: card_id.clone(),
            kind: "hook.codex.session_start".to_string(),
            payload: json!({}),
        },
    );

    // 5. Drain until we see the echoed `\r` (0x0d). `cat` in cooked-mode
    //    PTY echoes input even before its own stdout pipeline kicks in,
    //    so both the line-discipline echo and the cat-pipeline output
    //    paths satisfy the predicate. We start the search past the
    //    sentinel-byte offset so the `\r` baked into `\r\n` rendering of
    //    the sentinel's newline doesn't false-positive us.
    let after_sentinel = prefix.windows(b"FAKE_CODEX_READY".len())
        .position(|w| w == b"FAKE_CODEX_READY")
        .map(|i| i + b"FAKE_CODEX_READY".len())
        .unwrap_or(prefix.len());
    let (saw_cr, full) =
        drain_until_render_contains(&mut ws, prefix.clone(), Duration::from_secs(2), |buf| {
            buf.len() > after_sentinel
                && buf[after_sentinel..].contains(&b'\r')
        })
        .await;
    assert!(
        saw_cr,
        "auto_submit subscriber did not inject `\\r` within 2s of session_start; \
         got {} bytes (after-sentinel slice: {:?})",
        full.len(),
        String::from_utf8_lossy(&full[after_sentinel.min(full.len())..]),
    );
}

/// Negative case: when the card's payload does NOT opt in to
/// `auto_submit`, the subscriber MUST silently drop the `session_start`
/// event. Pins the `should_auto_submit` gate is actually wired to the
/// bus path (a refactor that moves the gate but forgets to re-wire it
/// would regress hands-free safety in the opposite direction:
/// auto-submitting every codex card).
#[tokio::test]
async fn auto_submit_subscriber_skips_when_payload_opt_out() {
    let _lock = SHIM_LOCK.lock().await;
    let (_shim_dir, _path_guard) = install_codex_shim();

    let (addr, app, daemon, wave_id, _tmp, bus, repo) = boot_full().await;
    calm_server::codex_auto_submit::spawn(repo.clone(), daemon.clone(), bus.clone());

    let (status, card) = rest_post(
        app.clone(),
        format!("/api/waves/{wave_id}/cards"),
        json!({ "kind": "codex", "payload": null, "sort": 1.0 }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create codex card: {card:?}");
    let card_id = card["id"].as_str().unwrap().to_string();

    // Note: omit `auto_submit` entirely — the route's "only stamp when
    // true" branch means `payload.auto_submit` will be absent, exactly
    // what `should_auto_submit` interprets as "no".
    let updated_card = post_codex(
        app.clone(),
        &card_id,
        json!({ "prompt": "noop", "cwd": "/tmp" }),
    )
    .await;
    assert!(
        updated_card["payload"].get("auto_submit").is_none(),
        "route must NOT stamp auto_submit when caller omitted it; got payload={:?}",
        updated_card["payload"],
    );
    let terminal_id = updated_card["payload"]["terminal_id"]
        .as_str()
        .expect("terminal_id stamped by route")
        .to_string();

    let (mut ws, initial) = attach_owner_ws(addr, &terminal_id).await;
    let (saw_ready, prefix) =
        drain_until_render_contains(&mut ws, initial, Duration::from_secs(3), |buf| {
            buf.windows(b"FAKE_CODEX_READY".len())
                .any(|w| w == b"FAKE_CODEX_READY")
        })
        .await;
    assert!(saw_ready, "shim sentinel never appeared");

    bus.emit(
        "ai:codex",
        calm_server::event::Event::CodexHook {
            card_id: card_id.clone(),
            kind: "hook.codex.session_start".to_string(),
            payload: json!({}),
        },
    );

    // Wait past SUBMIT_DELAY (600 ms) + a generous slack window. If the
    // subscriber gates correctly, NO `\r` shows up after the sentinel.
    let after_sentinel = prefix.windows(b"FAKE_CODEX_READY".len())
        .position(|w| w == b"FAKE_CODEX_READY")
        .map(|i| i + b"FAKE_CODEX_READY".len())
        .unwrap_or(prefix.len());
    let (saw_cr, full) =
        drain_until_render_contains(&mut ws, prefix.clone(), Duration::from_millis(1500), |buf| {
            buf.len() > after_sentinel
                && buf[after_sentinel..].contains(&b'\r')
        })
        .await;
    assert!(
        !saw_cr,
        "auto_submit subscriber injected `\\r` for an opt-out card; \
         after-sentinel bytes: {:?}",
        String::from_utf8_lossy(&full[after_sentinel.min(full.len())..]),
    );
}

/// Routes-layer regression guard: the `NewCodexBody` wire fields
/// (`prompt`, `auto_submit`) flow into `card.payload` via `spawn_codex_for`
/// (`routes/codex.rs:312-321`), and `prompt` gets shell-quoted onto the
/// daemon `program` string (`routes/codex.rs:279-284`). No existing test
/// covers either: a wire rename (e.g. `auto_submit` → `autoSubmit`) or a
/// quoting break (positional arg dropped, or unsafely interpolated) would
/// take hands-free codex out of service silently.
///
/// This test pins both contracts via the production REST route. It does
/// NOT inject anything over the daemon socket — the subscriber-chain
/// tests above already cover that path — so it doesn't need the codex
/// shim. It still needs a real daemon because `spawn_daemon_for` blocks
/// on a socket-ready handshake, so `boot_full` is the right scaffold.
#[tokio::test]
async fn post_codex_stamps_payload_and_program_from_prompt_and_auto_submit() {
    // No PATH shim — we deliberately let the daemon fail to find `codex`.
    // `spawn_daemon_for` returns Ok as long as the socket comes up; the
    // shell's "codex: not found" exit happens on the PTY side and is
    // invisible to the REST handler. That's exactly the behavior the
    // test wants: we're pinning the stamp, not the spawn.
    //
    // Verified empirically that even without a real codex on PATH, the
    // daemon's poll-until-ready loop succeeds because the socket binds
    // before the child exec attempt finishes — so the route can return
    // 202 and the stamp lands on the card payload. If a future daemon
    // refactor tightens that ordering and breaks this assumption, the
    // test will start failing with a `daemon for terminal ... did not
    // become ready` error from the REST 500 path, surfacing the change.
    let (_addr, app, _daemon, wave_id, _tmp, _bus, repo) = boot_full().await;

    // ---- Case A: prompt + auto_submit=true ---------------------------
    let (status, card_a) = rest_post(
        app.clone(),
        format!("/api/waves/{wave_id}/cards"),
        json!({ "kind": "codex", "payload": null, "sort": 1.0 }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create card A: {card_a:?}");
    let card_a_id = card_a["id"].as_str().unwrap().to_string();

    let updated_a = post_codex(
        app.clone(),
        &card_a_id,
        json!({ "prompt": "echo hello", "auto_submit": true, "cwd": "/tmp" }),
    )
    .await;
    assert_eq!(
        updated_a["payload"]["auto_submit"], true,
        "auto_submit=true must be stamped on payload"
    );
    let term_a_id = updated_a["payload"]["terminal_id"]
        .as_str()
        .expect("terminal_id stamped");
    assert!(
        !term_a_id.is_empty(),
        "terminal_id must be a non-empty string"
    );

    let term_a = repo
        .terminal_get(term_a_id)
        .await
        .unwrap()
        .expect("terminal row exists");
    assert!(
        term_a.program.starts_with("codex "),
        "with prompt set, program must start with `codex ` (note trailing space \
         confirms positional arg was shell-quoted onto the command); \
         got program={:?}",
        term_a.program,
    );
    // The prompt should appear (single-quoted) in the program string. This
    // doubles as a `shell_single_quote` smoke check at the route boundary
    // — `routes/codex.rs::tests::shell_single_quote_round_trip_under_sh`
    // covers the function in isolation, but only this test confirms it's
    // actually called on the prompt at spawn time.
    assert!(
        term_a.program.contains("'echo hello'"),
        "shell-quoted prompt missing from program; got program={:?}",
        term_a.program,
    );

    // ---- Case B: prompt absent, auto_submit absent -------------------
    let (status, card_b) = rest_post(
        app.clone(),
        format!("/api/waves/{wave_id}/cards"),
        json!({ "kind": "codex", "payload": null, "sort": 2.0 }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create card B: {card_b:?}");
    let card_b_id = card_b["id"].as_str().unwrap().to_string();

    let updated_b = post_codex(app.clone(), &card_b_id, json!({ "cwd": "/tmp" })).await;
    assert!(
        updated_b["payload"].get("auto_submit").is_none(),
        "auto_submit must be ABSENT (not false) when caller omits it — see \
         routes/codex.rs `if p.auto_submit` stamp branch; got payload={:?}",
        updated_b["payload"],
    );
    let term_b_id = updated_b["payload"]["terminal_id"].as_str().unwrap();
    let term_b = repo.terminal_get(term_b_id).await.unwrap().unwrap();
    assert_eq!(
        term_b.program, "codex",
        "without prompt, program must be exactly `codex` (no positional arg, \
         no trailing space)"
    );

    // ---- Case C: auto_submit=false is also absent --------------------
    // Explicit `false` from a caller is semantically identical to omitting
    // the field — the route's `if p.auto_submit` branch is the gate, and
    // a `false` payload key would just be noise the subscriber would have
    // to interpret anyway.
    let (status, card_c) = rest_post(
        app.clone(),
        format!("/api/waves/{wave_id}/cards"),
        json!({ "kind": "codex", "payload": null, "sort": 3.0 }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create card C: {card_c:?}");
    let card_c_id = card_c["id"].as_str().unwrap().to_string();

    let updated_c = post_codex(
        app.clone(),
        &card_c_id,
        json!({ "auto_submit": false, "cwd": "/tmp" }),
    )
    .await;
    assert!(
        updated_c["payload"].get("auto_submit").is_none(),
        "explicit auto_submit=false must NOT stamp the payload; got payload={:?}",
        updated_c["payload"],
    );
}
