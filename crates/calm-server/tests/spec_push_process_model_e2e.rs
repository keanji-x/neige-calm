//! Issue #293 PR3a — end-to-end verification of the two-process-per-spec-
//! card push model against a **real `codex app-server`**.
//!
//! Feature-gated behind `codex-e2e` (same convention as
//! `codex_e2e_spec_card.rs` and PR2's `codex_appserver_e2e.rs`) because CI
//! ships no `codex` binary and cannot run model turns. Run locally with:
//!
//! ```sh
//! cargo test -p calm-server --features codex-e2e \
//!   --test spec_push_process_model_e2e -- --nocapture
//! ```
//!
//! ## What it proves (push is the only path — #293 cutover)
//!
//! After `POST /api/waves`:
//!   a. the spec card payload gains a **non-empty `codex_thread_id`** (and
//!      `appserver_sock`) — the kernel booted the `codex app-server`, ran
//!      turn #1, and persisted the thread,
//!   b. the **PTY daemon argv** is `codex resume <tid> --remote
//!      unix://<sock>` (captured via the `argv-recorder-daemon` fixture
//!      standing in for `calm-session-daemon`), and
//!   c. **turn #1 started** — the kernel's app-server client observed a
//!      `turn/started` (a precondition for the 201, per DECISION A) and the
//!      parked [`SpecPushHandle`] reflects a running turn.
//!
//! There is no flag and no legacy `codex '<title>'` path anymore — the
//! cutover deleted pull entirely. Worker-spawn coverage that doesn't need a
//! real codex lives in `tests/dispatcher.rs`.
//!
//! ## Self-skip (must NOT fail when codex/auth is absent)
//!
//! Resolves the codex binary via `NEIGE_CODEX_BIN` (tilde-expanded), like
//! the sibling e2e tests. If the binary is missing — or the kernel's
//! app-server boot fails (no auth / no network) — the test prints a skip
//! marker and returns success.
//!
//! ## Proxy
//!
//! Model turns on this host go through `http://127.0.0.1:2080`. The
//! kernel-spawned `codex app-server` inherits the test process env (the
//! spawn does not `env_clear`), so we set `HTTP_PROXY`/`HTTPS_PROXY` on the
//! process from `NEIGE_CODEX_PROXY` (default `http://127.0.0.1:2080`; set
//! empty to disable) before creating the wave.

#![cfg(all(unix, feature = "codex-e2e"))]

use std::path::{Path, PathBuf};
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
use calm_server::spec_appserver::SpecPushPhase;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_cove_cache::WaveCoveCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

const DEFAULT_CODEX_BIN: &str = "~/.nvm/versions/node/v24.4.1/bin/codex";
const DEFAULT_PROXY: &str = "http://127.0.0.1:2080";

/// Resolve the codex binary the same way the sibling e2e tests do.
fn resolve_codex_bin() -> Option<PathBuf> {
    let raw = std::env::var("NEIGE_CODEX_BIN").unwrap_or_else(|_| DEFAULT_CODEX_BIN.to_string());
    let expanded = if let Some(stripped) = raw.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        PathBuf::from(home).join(stripped)
    } else {
        PathBuf::from(raw)
    };
    if !expanded.is_file() {
        return None;
    }
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(&expanded).ok()?;
    if meta.permissions().mode() & 0o111 == 0 {
        return None;
    }
    Some(expanded)
}
macro_rules! skip {
    ($($arg:tt)*) => {{
        eprintln!("[spec-push-e2e] SKIP: {}", format!($($arg)*));
        return;
    }};
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

async fn get_cards(app: axum::Router, wave_id: &str) -> Value {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/waves/{wave_id}/cards"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null)
}

/// Find the spec card on a fresh wave and return `(card_id, payload)`.
/// A fresh wave has TWO kernel-owned (`deletable = false`) cards: the spec
/// (codex) card and the `wave-report` card (#229 PR B). Only the spec card
/// carries a `payload.terminal_id` (it owns a PTY daemon), so we
/// disambiguate on that — the report card has no terminal.
fn find_spec_card(cards: &Value) -> (String, Value) {
    cards
        .as_array()
        .and_then(|arr| {
            arr.iter().find_map(|c| {
                let deletable = c.get("deletable").and_then(Value::as_bool) == Some(false);
                let payload = c.get("payload").cloned().unwrap_or(Value::Null);
                let has_terminal = payload
                    .get("terminal_id")
                    .and_then(Value::as_str)
                    .is_some_and(|t| !t.is_empty());
                if deletable && has_terminal {
                    let id = c.get("id").and_then(Value::as_str)?.to_string();
                    Some((id, payload))
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| panic!("spec card present on fresh wave; cards: {cards}"))
}

/// Read the recorder's `<sock>.argv` sidecar (one argv element per line),
/// polling briefly for the write. The PTY daemon socket lives at
/// `<terminals>/<terminal_id>.sock`; we locate it from the terminal row.
fn read_recorded_argv(sock: &str) -> Vec<String> {
    let argv_path = format!("{sock}.argv");
    let start = Instant::now();
    loop {
        if let Ok(text) = std::fs::read_to_string(&argv_path)
            && !text.is_empty()
        {
            return text.lines().map(String::from).collect();
        }
        if start.elapsed() > Duration::from_secs(5) {
            panic!("argv sidecar {argv_path:?} never appeared / stayed empty");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Build an `AppState` whose PTY daemon is the argv recorder and whose
/// `codex_bin` is the resolved real codex (for the kernel-owned
/// `app-server`). `data_dir` is the tempdir root; the app-server socket
/// lands under `<root>/appserver/<card_id>/` (parent of `terminals`).
async fn build_state(tmp: &TempDir, codex_bin: &Path) -> (AppState, Arc<dyn Repo>) {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    // DaemonClient.data_dir is `<root>/terminals`, so `parent()` is
    // `<root>` — the user-owned tempdir the app-server can 0700-chmod a
    // subdir under.
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().join("terminals"),
        proc_supervisor_sock: None,
    });
    // Real codex bin for the app-server; everything else stubbed.
    let mut codex = CodexClient::new_stub();
    codex.codex_bin = codex_bin.to_string_lossy().to_string();

    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = WaveCoveCache::new();
    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-spec-push-e2e"),
            Vec::new(),
            EventBus::new(),
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        )),
        Arc::new(codex),
        Some(card_role_cache),
        Some(wave_cove_cache),
    );
    (state, repo)
}

#[tokio::test]
async fn spec_push_two_process_model() {
    // #293 cutover: push is the ONLY path — no `NEIGE_SPEC_PUSH` flag, no
    // legacy `codex '<title>'` argv, no flag-off control. `create_wave`
    // unconditionally boots a real codex app-server, so this whole test
    // self-skips when no codex binary is present.
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("codex binary not found (set NEIGE_CODEX_BIN); push path needs it");
    };
    eprintln!("[spec-push-e2e] using codex at {codex_bin:?}");

    // The kernel-spawned `app-server` inherits the process env (no
    // env_clear), so set the proxy here so model turns reach upstream.
    let proxy = std::env::var("NEIGE_CODEX_PROXY").unwrap_or_else(|_| DEFAULT_PROXY.to_string());
    if !proxy.is_empty() {
        // SAFETY: single-threaded test.
        unsafe {
            std::env::set_var("HTTP_PROXY", &proxy);
            std::env::set_var("HTTPS_PROXY", &proxy);
            std::env::set_var("http_proxy", &proxy);
            std::env::set_var("https_proxy", &proxy);
        }
    }

    let tmp_on = TempDir::new().expect("tempdir on");
    let (state_on, repo_on) = build_state(&tmp_on, &codex_bin).await;
    let cove_on = repo_on
        .cove_create(NewCove {
            name: "spec-push-on".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let app_on = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state_on.clone());

    let (status, body) = post(
        app_on.clone(),
        "/api/waves",
        json!({
            "cove_id": cove_on.id,
            "title": "push wave",
            "cwd": "/tmp/spec-push-on",
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]}
        }),
    )
    .await;
    // A 5xx here means the app-server failed to boot/turn (no auth /
    // network): self-skip rather than fail, mirroring the sibling gates.
    if status != StatusCode::CREATED {
        skip!("wave create returned {status} (likely no codex auth / network); body={body}");
    }
    let wave_on_id = body.get("id").and_then(Value::as_str).unwrap().to_string();
    eprintln!("[spec-push-e2e] push wave created: {wave_on_id}");

    // (a) spec card payload gained a non-empty codex_thread_id + sock.
    let cards_on = get_cards(app_on.clone(), &wave_on_id).await;
    let (spec_on_id, payload_on) = find_spec_card(&cards_on);
    let thread_id = payload_on
        .get("codex_thread_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        !thread_id.is_empty(),
        "spec card must have non-empty codex_thread_id; payload: {payload_on}"
    );
    let appserver_sock = payload_on
        .get("appserver_sock")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        !appserver_sock.is_empty(),
        "spec card must have non-empty appserver_sock; payload: {payload_on}"
    );
    eprintln!(
        "[spec-push-e2e] (a) PASS codex_thread_id={thread_id} appserver_sock={appserver_sock}"
    );

    // (b) PTY daemon argv is `codex resume <tid> --remote unix://<sock>`.
    let term_on = repo_on
        .terminal_get_by_card(&spec_on_id)
        .await
        .unwrap()
        .expect("spec terminal row");
    let sock_on = state_on.daemon.sock_path(&term_on.id);
    let argv_on = read_recorded_argv(&sock_on.to_string_lossy());
    eprintln!("[spec-push-e2e] argv: {argv_on:?}");
    let program_on = argv_on.last().expect("program in argv");
    let expected = format!("codex resume '{thread_id}' --remote 'unix://{appserver_sock}'");
    assert_eq!(
        program_on, &expected,
        "PTY argv must be resume-mode; got: {program_on}"
    );
    eprintln!("[spec-push-e2e] (b) PASS resume-mode argv");

    // (c) turn #1 started: the 201 itself implies the kernel observed
    //     `turn/started` (DECISION A awaits it before returning). Confirm
    //     via the parked handle's tracked status.
    let handle_status = {
        let wave_key = wave_on_id.clone().into();
        assert!(
            state_on.spec_push.contains(&wave_key),
            "registry must hold the push handle"
        );
        // Pull the handle out to read status, then put it back so the
        // child isn't reaped early. (remove → status → re-insert)
        let handle = state_on
            .spec_push
            .remove(&wave_key)
            .expect("handle present");
        let st = handle.status().await;
        state_on.spec_push.insert(wave_key, handle);
        st
    };
    eprintln!("[spec-push-e2e] handle status: {handle_status:?}");
    assert_eq!(
        handle_status.last_thread_id.as_deref(),
        Some(thread_id),
        "tracked thread id matches the persisted one"
    );
    assert!(
        matches!(
            handle_status.phase,
            SpecPushPhase::TurnRunning | SpecPushPhase::TurnCompleted
        ),
        "turn #1 must have started (phase TurnRunning/TurnCompleted); got {:?}",
        handle_status.phase
    );
    eprintln!("[spec-push-e2e] (c) PASS turn #1 observed by kernel client");

    // Teardown: drop the registry handle to kill the app-server child.
    let _ = state_on.spec_push.remove(&wave_on_id.clone().into());
    eprintln!("[spec-push-e2e] ALL PASS");
}
