//! Integration test (#177): `POST /api/waves` threads `theme: { fg, bg }`
//! through to the auto-minted spec card's terminal renderer startup
//! argv as `--terminal-fg=r,g,b --terminal-bg=r,g,b`.
//!
//! Pre-#177 the wave-create route auto-minted a spec card and spawned
//! its codex daemon via `spawn_terminal_for` (the no-opts shim that
//! ignored theme). That meant codex's OSC 10/11 startup probe got no
//! answer from the daemon, so the composer painted against codex's
//! built-in default and visually clashed with the surrounding card.
//! PR #193 had already fixed the user-created codex-card path but
//! missed the spec-card spawn — this test is the regression guard.
//!
//! Strategy: point `DaemonClient::session_daemon_bin` at the
//! `argv-recorder-daemon` fixture which records its argv to a sidecar,
//! binds the unix socket, and writes `ready\n`. Fire the wave-create
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
use calm_server::state::{AppState, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

mod common;
struct Boot {
    app: axum::Router,
    cove_id: String,
    _daemon_data_dir: PathBuf,
    repo: Arc<dyn Repo>,
    state: AppState,
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
        proc_supervisor_sock: None,
    });
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
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
        .with_state(state.clone());

    Boot {
        app,
        cove_id: cove.id.to_string(),
        _daemon_data_dir: daemon_data_dir,
        repo,
        state,
        _tmp: tmp,
    }
}

/// Returns `(status, json_or_null, raw_text)`. Axum's 422 surface from a
/// serde-rejected `Json<T>` is `text/plain` (not JSON); we keep both
/// shapes so the missing-theme test below can substring-match against
/// the raw text while the happy-path test still drills into the JSON.
async fn post(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value, String) {
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
    let text = String::from_utf8_lossy(&bytes).to_string();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json, text)
}

/// Wait up to `timeout` for any `*.argv` file under `data_dir`. The
/// daemon-spawn background task is fire-and-forget so we poll for the
/// sidecar to land. Returns the contents (lines).
#[allow(dead_code)]
async fn wait_for_argv_file(data_dir: &PathBuf, timeout: Duration) -> Vec<String> {
    let start = Instant::now();
    loop {
        if let Ok(read) = std::fs::read_dir(data_dir) {
            for entry in read.flatten() {
                let p = entry.path();
                if p.extension().and_then(|s| s.to_str()) == Some("argv") {
                    // Give the recorder a moment to finish writing. It
                    // writes argv before binding the socket and emitting
                    // `ready\n`, so by the time the kernel sees ready the
                    // file is complete.
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
    let (status, body, _text) = post(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": boot.cove_id,
            "title": "theme wave",
            "cwd": "/tmp/issue-250-pr2-test",
            "attach_folder": true,
            "theme": {
                "fg": [216, 219, 226],
                "bg": [15, 20, 24]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    let wave_id = body["id"].as_str().expect("wave id");
    let cards = boot.repo.cards_by_wave(wave_id).await.unwrap();
    let spec_card = cards
        .iter()
        .find(|c| c.kind == "codex")
        .expect("spec codex card");
    let terminal_id = spec_card.payload["terminal_id"]
        .as_str()
        .expect("spec payload terminal_id");
    let entry = boot
        .state
        .terminal_renderer
        .get(terminal_id)
        .expect("spec renderer entry registered");
    assert_eq!(entry.config().terminal_fg, (216, 219, 226));
    assert_eq!(entry.config().terminal_bg, (15, 20, 24));
    let term = boot
        .repo
        .terminal_get(terminal_id)
        .await
        .expect("read terminal row")
        .expect("terminal row must exist");
    assert_eq!(
        term.theme_fg, "216,219,226",
        "spec-card terminal row must remember its host theme fg"
    );
    assert_eq!(
        term.theme_bg, "15,20,24",
        "spec-card terminal row must remember its host theme bg"
    );
}

/// Required-field gate (#177 followup): wave-create body without
/// `theme` must be rejected at the deserialize layer. Previously this
/// test asserted back-compat — the route silently fell back to "no
/// theme args, daemon stays silent on OSC 10/11". That fallback was
/// exactly the bug source: a web client that forgot to include theme
/// (e.g. `useTodayTerminal.ts:168` before #177's wave-create theme
/// thread-through) ended up with a mis-tinted composer with no signal
/// at any layer. Forcing the field at the API boundary means a missing
/// theme surfaces immediately as a 422 — the bug becomes a compile-
/// time / first-request failure instead of a visual artifact.
///
/// `serde` returns 422 (Unprocessable Entity) on `Json<NewWave>`
/// deserialize failures when a non-Option field is absent.
#[tokio::test]
async fn wave_create_without_theme_is_rejected() {
    let boot = boot().await;

    // Body includes every other required field (cwd, attach_folder); only
    // `theme` is missing. Without this guard the body's first missing
    // required field (e.g. `cwd`) could fire the 422 instead, leaving the
    // theme-required contract silently un-tested. The body-substring
    // assertion below pins `theme` as the rejected field.
    let (status, _body, text) = post(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": boot.cove_id,
            "title": "no theme wave",
            "cwd": "/tmp/issue-250-pr2-test",
            "attach_folder": true,
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "wave-create without theme must be rejected (422); got status={status}, body={text}"
    );
    assert!(
        text.contains("theme"),
        "422 must name `theme` as the rejected field (so a future \
         regression to `theme: Option<>` is caught); got body={text}"
    );
}
