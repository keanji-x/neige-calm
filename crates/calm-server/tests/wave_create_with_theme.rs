//! Integration test (#177): `POST /api/waves` threads `theme: { fg, bg }`
//! through to the auto-minted spec card's `calm-session-daemon` spawn
//! argv as `--terminal-fg=r,g,b --terminal-bg=r,g,b`.
//!
//! Pre-#177 the wave-create route auto-minted a spec card and spawned
//! its codex daemon via `spawn_daemon_for` (the no-opts shim that
//! ignored theme). That meant codex's OSC 10/11 startup probe got no
//! answer from the daemon, so the composer painted against codex's
//! built-in default and visually clashed with the surrounding card.
//! PR #193 had already fixed the user-created codex-card path but
//! missed the spec-card spawn — this test is the regression guard.
//!
//! Strategy: point `DaemonClient::session_daemon_bin` at the
//! `argv-recorder-daemon` fixture which records its argv to a sidecar
//! file + binds the unix socket the kernel polls. Fire the wave-create
//! POST with a `theme` body, wait for the argv file to land
//! (background `seed_and_spawn_spec_daemon` task is fire-and-forget),
//! and assert `--terminal-fg` / `--terminal-bg` are present with the
//! exact RGB.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// Locate the argv-recorder fake daemon — Cargo drops it next to the
/// test binary (`target/<profile>/argv-recorder-daemon`).
fn locate_recorder_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_argv-recorder-daemon"))
}

struct Boot {
    app: axum::Router,
    cove_id: String,
    daemon_data_dir: PathBuf,
    repo: Arc<dyn Repo>,
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
            name: "wave-theme-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();

    let daemon_data_dir = tmp.path().to_path_buf();
    let daemon = Arc::new(DaemonClient {
        data_dir: daemon_data_dir.clone(),
        session_daemon_bin: locate_recorder_bin(),
    });
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-wave-theme-test"),
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
        daemon_data_dir,
        repo,
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

/// Wait up to `timeout` for any `*.argv` file under `data_dir`. The
/// daemon-spawn background task is fire-and-forget so we poll for the
/// sidecar to land. Returns the contents (lines).
async fn wait_for_argv_file(data_dir: &PathBuf, timeout: Duration) -> Vec<String> {
    let start = Instant::now();
    loop {
        if let Ok(read) = std::fs::read_dir(data_dir) {
            for entry in read.flatten() {
                let p = entry.path();
                if p.extension().and_then(|s| s.to_str()) == Some("argv") {
                    // Give the recorder a moment to finish writing (it
                    // writes argv before binding the socket, so by the
                    // time the kernel sees ready the file is complete).
                    let content = std::fs::read_to_string(&p).expect("read argv file");
                    return content.lines().map(String::from).collect();
                }
            }
        }
        if start.elapsed() > timeout {
            panic!(
                "no *.argv file landed under {data_dir:?} within {timeout:?} — \
                 spec-card daemon spawn never ran (or recorder fixture failed)"
            );
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

/// Happy path: wave-create body carries `theme: { fg, bg }` — the
/// spec card's daemon argv must contain `--terminal-fg=216,219,226`
/// + `--terminal-bg=15,20,24` (the dark-theme RGB the web client
/// stamps for a dark host browser).
#[tokio::test]
async fn wave_create_with_theme_stamps_terminal_fg_bg_args() {
    let boot = boot().await;

    // POST /api/waves with the dark-theme RGB the web client uses
    // (`DARK_THEME_RGB` in `web/src/shared/themeRgb.ts`).
    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": boot.cove_id,
            "title": "theme wave",
            "theme": {
                "fg": [216, 219, 226],
                "bg": [15, 20, 24]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    // Background task: wait for the daemon's argv sidecar to appear.
    let argv = wait_for_argv_file(&boot.daemon_data_dir, Duration::from_secs(5)).await;

    // The kernel passes `--terminal-fg` / `--terminal-bg` as two args
    // each (the flag, then the value) — the recorder logs them
    // line-per-arg.
    let pairs: Vec<(String, String)> = argv
        .windows(2)
        .map(|w| (w[0].clone(), w[1].clone()))
        .collect();
    assert!(
        pairs
            .iter()
            .any(|(k, v)| k == "--terminal-fg" && v == "216,219,226"),
        "daemon argv must contain `--terminal-fg 216,219,226`; got: {argv:?}"
    );
    assert!(
        pairs
            .iter()
            .any(|(k, v)| k == "--terminal-bg" && v == "15,20,24"),
        "daemon argv must contain `--terminal-bg 15,20,24`; got: {argv:?}"
    );

    // #177 PR2 — the spawn should have ALSO persisted the theme onto
    // the terminal row so the WS auto-revive path picks it up. Locate
    // the spec card's terminal via the cove → wave → card chain. The
    // `--sock <path>` arg the recorder logged maps 1:1 to a terminal
    // id (`<data_dir>/<id>.sock`), so we extract the id from there.
    let mut sock_arg: Option<String> = None;
    let mut it = argv.iter().peekable();
    while let Some(a) = it.next() {
        if a == "--sock"
            && let Some(v) = it.peek()
        {
            sock_arg = Some((*v).clone());
            break;
        }
    }
    let sock = sock_arg.expect("recorder must have seen --sock");
    let terminal_id = PathBuf::from(&sock)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(String::from)
        .expect("derive terminal id from sock path");
    // #177 PR2 persistence happens AFTER the daemon socket binds —
    // since the recorder writes the argv sidecar BEFORE binding, the
    // file appearing doesn't mean the kernel's `terminal_set_theme`
    // call has landed yet. Poll briefly for the row's theme cols to
    // flip non-NULL.
    let start = Instant::now();
    let term = loop {
        let row = boot
            .repo
            .terminal_get(&terminal_id)
            .await
            .expect("read terminal row")
            .expect("terminal row must exist");
        if row.theme_fg.is_some() && row.theme_bg.is_some() {
            break row;
        }
        if start.elapsed() > Duration::from_secs(5) {
            panic!(
                "spec-card terminal theme cols stayed NULL; fg={:?}, bg={:?}",
                row.theme_fg, row.theme_bg
            );
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    };
    assert_eq!(
        term.theme_fg.as_deref(),
        Some("216,219,226"),
        "spec-card terminal row must remember its host theme fg"
    );
    assert_eq!(
        term.theme_bg.as_deref(),
        Some("15,20,24"),
        "spec-card terminal row must remember its host theme bg"
    );
}

/// Back-compat: wave-create body without `theme` (older clients,
/// scripted callers, tests) must not stamp the args — the daemon
/// stays silent on OSC 10/11 and codex falls back to its built-in
/// default. Regression guard so a future refactor doesn't accidentally
/// hard-code a theme default for the wave-create path.
#[tokio::test]
async fn wave_create_without_theme_omits_terminal_fg_bg_args() {
    let boot = boot().await;

    let (status, _body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": boot.cove_id,
            "title": "no theme wave"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let argv = wait_for_argv_file(&boot.daemon_data_dir, Duration::from_secs(5)).await;
    assert!(
        !argv.iter().any(|a| a == "--terminal-fg"),
        "no `--terminal-fg` should appear without theme; got: {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "--terminal-bg"),
        "no `--terminal-bg` should appear without theme; got: {argv:?}"
    );

    // #177 PR2 — the row's theme columns must remain NULL when the
    // wave-create body carries no theme. Belt-and-braces: a future
    // refactor that accidentally hard-codes a default would trip
    // this regression alongside the argv assertions above.
    let mut sock_arg: Option<String> = None;
    let mut it = argv.iter().peekable();
    while let Some(a) = it.next() {
        if a == "--sock"
            && let Some(v) = it.peek()
        {
            sock_arg = Some((*v).clone());
            break;
        }
    }
    let sock = sock_arg.expect("recorder must have seen --sock");
    let terminal_id = PathBuf::from(&sock)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(String::from)
        .expect("derive terminal id from sock path");
    let term = boot
        .repo
        .terminal_get(&terminal_id)
        .await
        .expect("read terminal row")
        .expect("terminal row must exist");
    assert!(
        term.theme_fg.is_none() && term.theme_bg.is_none(),
        "untheme wave-create must leave theme columns NULL; got fg={:?}, bg={:?}",
        term.theme_fg,
        term.theme_bg
    );
}
