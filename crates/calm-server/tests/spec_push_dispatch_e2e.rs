//! Issue #293 PR3b — end-to-end verification of the **dispatcher push
//! path** against a **real `codex app-server`**.
//!
//! Feature-gated behind `codex-e2e` (same convention as PR3a's
//! `spec_push_process_model_e2e.rs`) because CI ships no `codex` binary and
//! cannot run model turns. Run locally with:
//!
//! ```sh
//! cargo test -p calm-server --features codex-e2e \
//!   --test spec_push_dispatch_e2e -- --nocapture
//! ```
//!
//! ## What it proves (push is the only path — #293 cutover)
//!
//! After `POST /api/waves` (spec app-server boots + runs turn #1 per PR3a):
//!   1. We attach a **SECOND** `CodexAppServer` client to the SAME thread
//!      via `thread_resume(codex_thread_id)` over the spec's listen socket.
//!   2. We wait for turn #1 to settle (so the thread is idle/between turns).
//!   3. We emit a `task.completed` event INTO the wave (via the persisted
//!      `log_pure_event` path on the same bus the dispatcher subscribes to).
//!   4. The dispatcher's push branch resolves the wave's spec card +
//!      `SpecPushHandle` and issues a `turn/start` carrying the observation.
//!      We assert the **second client observes a fresh `turn/started`**
//!      shortly after — proof the dispatcher pushed a turn.
//!   5. **Dedup**: we re-deliver the *same* persisted envelope (same
//!      `events.id`) on the bus and assert NO additional `turn/started`
//!      fires within a window (the dedicated push watermark deduped it).
//!
//! ## Self-skip
//!
//! Resolves the codex binary via `NEIGE_CODEX_BIN` (tilde-expanded). If
//! absent — or the kernel's app-server boot fails (no auth / no network) —
//! the whole test prints a skip marker and returns success. Worker-spawn
//! coverage that doesn't need a real codex lives in `tests/dispatcher.rs`.
//!
//! ## No-leak discipline
//!
//! A BEFORE/AFTER `ps` snapshot brackets the scenario and asserts zero net
//! new `codex app-server` processes survive this test's run (mirrors PR3a's
//! check).

#![cfg(all(unix, feature = "codex-e2e"))]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::codex_appserver::{ClientInfo, CodexAppServer, Notification, NotificationStream};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{ArtifactRef, BroadcastEnvelope, Event, EventBus, EventScope};
use calm_server::ids::{ActorId, CoveId, WaveId};
use calm_server::model::NewCove;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_cove_cache::WaveCoveCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

const DEFAULT_CODEX_BIN: &str = "~/.nvm/versions/node/v24.4.1/bin/codex";
const DEFAULT_PROXY: &str = "http://127.0.0.1:2080";

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
        eprintln!("[spec-push-dispatch-e2e] SKIP: {}", format!($($arg)*));
        return;
    }};
}

/// Count live `codex app-server` processes (whole-host; we only compare the
/// delta across our run, so other users' servers wash out).
fn count_codex_app_servers() -> usize {
    let out = std::process::Command::new("ps")
        .args(["-eo", "args"])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains("codex") && l.contains("app-server") && l.contains("--listen"))
        .count()
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

/// Find the spec card on a fresh wave; returns `(card_id, payload)`.
/// The spec card is the kernel-owned (`deletable = false`) card that owns a
/// PTY daemon (`payload.terminal_id`) — the report card has no terminal.
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

async fn build_state(tmp: &TempDir, codex_bin: &Path) -> (AppState, Arc<dyn Repo>) {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().join("terminals"),
        proc_supervisor_sock: None,
    });
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
            std::env::temp_dir().join("calm-plugins-data-spec-push-dispatch-e2e"),
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

/// Resolve the wave's cove id (the scope a wave event needs).
async fn cove_of_wave(repo: &Arc<dyn Repo>, wave_id: &str) -> CoveId {
    let wave = repo
        .wave_get(wave_id)
        .await
        .unwrap()
        .expect("wave row exists");
    wave.cove_id
}

/// Emit a persisted `task.completed` into a wave through the SAME bus the
/// dispatcher subscribes to. Returns the broadcast envelope (with its real
/// `events.id`) captured off a fresh subscription, so the dedup step can
/// re-deliver it verbatim.
async fn emit_task_completed(
    state: &AppState,
    repo: &Arc<dyn Repo>,
    wave_id: &str,
    idem: &str,
) -> BroadcastEnvelope {
    let cove = cove_of_wave(repo, wave_id).await;
    let scope = EventScope::Wave {
        wave: WaveId::from(wave_id.to_string()),
        cove,
    };
    // Subscribe BEFORE emitting so we capture the exact envelope (real id).
    let mut rx = state.events.subscribe();
    let ev = Event::TaskCompleted {
        idempotency_key: idem.to_string(),
        result: json!({ "note": "dispatch-e2e" }),
        artifacts: Vec::<ArtifactRef>::new(),
    };
    repo.log_pure_event(
        ActorId::User,
        scope,
        None,
        &state.events,
        &state.card_role_cache,
        &state.wave_cove_cache,
        ev,
    )
    .await
    .expect("log_pure_event task.completed");

    // Pull the matching envelope off the bus.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            Instant::now() < deadline,
            "task.completed envelope never observed on bus"
        );
        if let Ok(env) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("bus recv timeout")
            && matches!(env.event, Event::TaskCompleted { .. })
        {
            return env;
        }
    }
}

/// Drain `notifs` for up to `budget`, returning the COUNT of `turn/started`
/// observed for `thread_id`.
async fn count_turn_starts(
    notifs: &mut NotificationStream,
    thread_id: &str,
    budget: Duration,
) -> usize {
    let deadline = Instant::now() + budget;
    let mut count = 0;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return count;
        }
        match tokio::time::timeout(remaining, notifs.recv()).await {
            Ok(Some(Notification::TurnStarted { thread_id: t, .. })) if t == thread_id => {
                count += 1;
            }
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return count,
        }
    }
}

#[tokio::test]
async fn spec_push_dispatch_pushes_turn_and_dedups() {
    // ===== Push path (requires real codex + auth) =====
    // #293 cutover: push is the ONLY path now — no `NEIGE_SPEC_PUSH` flag, no
    // flag-off control. `create_wave` unconditionally boots a real codex
    // app-server, so this whole test self-skips when no codex binary is
    // present. (Worker-spawn-without-codex coverage lives in
    // `tests/dispatcher.rs`.)
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("codex binary not found (set NEIGE_CODEX_BIN); push path needs it");
    };
    eprintln!("[spec-push-dispatch-e2e] using codex at {codex_bin:?}");

    let servers_before = count_codex_app_servers();
    eprintln!("[spec-push-dispatch-e2e] codex app-server count BEFORE: {servers_before}");

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
            name: "dispatch-on".into(),
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
            "title": "dispatch push wave",
            "cwd": "/tmp/dispatch-on",
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]}
        }),
    )
    .await;
    if status != StatusCode::CREATED {
        skip!("wave create returned {status} (likely no codex auth / network); body={body}");
    }
    let wave_on_id = body.get("id").and_then(Value::as_str).unwrap().to_string();
    eprintln!("[spec-push-dispatch-e2e] push wave created: {wave_on_id}");

    // This block owns everything that must drop (the second client) BEFORE
    // we reap the registry handle + assert no-leak.
    let push_outcome = run_push_scenario(&state_on, &repo_on, app_on.clone(), &wave_on_id).await;

    // Teardown: reap the spec app-server child.
    let _ = state_on.spec_push.remove(&wave_on_id.clone().into());
    // Give the group-kill a moment to reap the native child.
    tokio::time::sleep(Duration::from_millis(800)).await;

    // No-leak: AFTER count must not exceed BEFORE (our spawned server reaped).
    let servers_after = count_codex_app_servers();
    eprintln!("[spec-push-dispatch-e2e] codex app-server count AFTER: {servers_after}");
    assert!(
        servers_after <= servers_before,
        "leaked codex app-server: before={servers_before} after={servers_after}"
    );
    eprintln!("[spec-push-dispatch-e2e] no-leak PASS");

    // Now surface the scenario result (panic AFTER teardown so we never leak
    // a server on assertion failure).
    push_outcome.expect("push scenario");
    eprintln!("[spec-push-dispatch-e2e] ALL PASS");
}

#[tokio::test]
async fn spec_push_empty_title_boots_idle_without_initial_turn() {
    use calm_server::spec_appserver::SpecPushPhase;

    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("codex binary not found (set NEIGE_CODEX_BIN); push path needs it");
    };
    eprintln!("[spec-push-dispatch-e2e] using codex at {codex_bin:?}");

    let servers_before = count_codex_app_servers();
    eprintln!("[spec-push-dispatch-e2e] codex app-server count BEFORE: {servers_before}");

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

    let tmp = TempDir::new().expect("tempdir");
    let (state, repo) = build_state(&tmp, &codex_bin).await;
    let cove = repo
        .cove_create(NewCove {
            name: "empty-title".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());

    let (status, body) = post(
        app.clone(),
        "/api/waves",
        json!({
            "cove_id": cove.id,
            "title": "",
            "cwd": "/tmp/empty-title",
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]}
        }),
    )
    .await;
    if status != StatusCode::CREATED {
        skip!("wave create returned {status} (likely no codex auth / network); body={body}");
    }
    let wave_id = body.get("id").and_then(Value::as_str).unwrap().to_string();
    let wave_key: WaveId = wave_id.clone().into();

    let Some(initial_status) = state.spec_push.status(&wave_key).await else {
        skip!("empty-title wave created but no spec push handle was parked; body={body}");
    };
    assert_eq!(initial_status.phase, SpecPushPhase::Idle);
    assert!(
        initial_status.last_thread_id.is_some(),
        "empty-title boot still starts a codex thread"
    );
    assert_eq!(
        initial_status.last_turn_id, None,
        "empty-title boot must not observe an initial turn"
    );
    assert!(
        state.spec_push.pusher(&wave_key).is_some(),
        "empty-title boot must park a pusher for later turns"
    );

    tokio::time::sleep(Duration::from_millis(800)).await;
    let settled_status = state
        .spec_push
        .status(&wave_key)
        .await
        .expect("handle remains parked");
    assert_eq!(settled_status.phase, SpecPushPhase::Idle);
    assert_eq!(
        settled_status.last_turn_id, None,
        "empty-title boot must stay turnless until a later push/user turn"
    );

    let cards = get_cards(app, &wave_id).await;
    let (_spec_id, payload) = find_spec_card(&cards);
    assert!(
        payload
            .get("codex_thread_id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty()),
        "spec card payload should persist codex_thread_id for empty-title boot; payload={payload}"
    );
    assert_eq!(
        payload
            .get("appserver_needs_initial_prompt")
            .and_then(Value::as_bool),
        Some(true),
        "empty-title boot must be marked fresh-bootable instead of resumable; payload={payload}"
    );
    assert!(
        payload.get("prompt").and_then(Value::as_str).is_none(),
        "empty title should not persist an auto-submit prompt; payload={payload}"
    );

    let _ = state.spec_push.remove(&wave_key);
    tokio::time::sleep(Duration::from_millis(800)).await;

    let servers_after = count_codex_app_servers();
    eprintln!("[spec-push-dispatch-e2e] codex app-server count AFTER: {servers_after}");
    assert!(
        servers_after <= servers_before,
        "leaked codex app-server: before={servers_before} after={servers_after}"
    );
}

/// The push scenario body, returning `Result` so the caller can run
/// teardown (reap + no-leak) regardless of pass/fail, then surface the
/// outcome. Self-skips (returns Ok) if the second client can't resume.
async fn run_push_scenario(
    state: &AppState,
    repo: &Arc<dyn Repo>,
    app: axum::Router,
    wave_id: &str,
) -> Result<(), String> {
    // Read the spec card's persisted thread id + socket.
    let cards = get_cards(app, wave_id).await;
    let (_spec_id, payload) = find_spec_card(&cards);
    let thread_id = payload
        .get("codex_thread_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let sock = payload
        .get("appserver_sock")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if thread_id.is_empty() || sock.is_empty() {
        return Err(format!(
            "spec card missing codex_thread_id/appserver_sock; payload: {payload}"
        ));
    }
    eprintln!("[spec-push-dispatch-e2e] thread_id={thread_id} sock={sock}");

    // Attach a SECOND client to the same thread.
    let (client2, mut notifs2) = CodexAppServer::connect(Path::new(&sock))
        .await
        .map_err(|e| format!("second client connect: {e}"))?;
    client2
        .initialize(ClientInfo {
            name: "spec-push-dispatch-e2e-observer".into(),
            version: "0".into(),
        })
        .await
        .map_err(|e| format!("second client initialize: {e}"))?;
    client2
        .thread_resume(&thread_id)
        .await
        .map_err(|e| format!("second client thread_resume: {e}"))?;
    eprintln!("[spec-push-dispatch-e2e] second client resumed thread");

    // Drain turn #1's activity until the thread goes idle (turn/completed).
    // Bounded — turn #1 is the create-goal turn and may run a while.
    wait_until_idle(state, wave_id, Duration::from_secs(90)).await;

    // Emit task.completed INTO the wave; capture the persisted envelope.
    let env = emit_task_completed(state, repo, wave_id, "dispatch-on-1").await;
    eprintln!(
        "[spec-push-dispatch-e2e] emitted task.completed (events.id={})",
        env.id
    );

    // The dispatcher should push a turn → the second client observes a fresh
    // turn/started. (We already drained turn #1, so any new turn/started here
    // is the pushed one.)
    let starts = count_turn_starts(&mut notifs2, &thread_id, Duration::from_secs(45)).await;
    if starts == 0 {
        return Err(
            "second client observed NO turn/started after task.completed — \
                    dispatcher did not push"
                .to_string(),
        );
    }
    eprintln!("[spec-push-dispatch-e2e] push PASS: observed {starts} new turn/started");

    // Dedup: re-deliver the SAME persisted envelope on the bus and assert no
    // ADDITIONAL turn/started fires (the dedicated push watermark deduped it).
    // First let the pushed turn settle so a *new* start would be distinct.
    wait_until_idle(state, wave_id, Duration::from_secs(90)).await;
    let redelivered = BroadcastEnvelope {
        id: env.id,
        event_version: env.event_version,
        actor: env.actor.clone(),
        scope: env.scope.clone(),
        event: env.event.clone(),
    };
    state.events.emit_envelope_for_test(redelivered);
    let extra = count_turn_starts(&mut notifs2, &thread_id, Duration::from_secs(10)).await;
    if extra != 0 {
        return Err(format!(
            "dedup FAILED: re-delivering the same events.id pushed {extra} more turn(s)"
        ));
    }
    eprintln!("[spec-push-dispatch-e2e] dedup PASS: re-delivery did not double-push");

    // Drop the second client explicitly here (before the caller reaps) so
    // its reader task + socket close before we kill the app-server child.
    drop(client2);
    let _ = notifs2;
    Ok(())
}

/// Poll the wave's spec `SpecPushHandle` status until the tracked phase is
/// neither `TurnRunning` nor `Issuing` (idle / between turns), or the budget
/// elapses. (N2) Reads the handle **non-destructively** via
/// `SpecPushRegistry::status` — NOT `remove → status → insert`, which left a
/// window with no handle registered where a concurrent dispatcher push would
/// be silently dropped, making the test timing-fragile.
async fn wait_until_idle(state: &AppState, wave_id: &str, budget: Duration) {
    use calm_server::spec_appserver::SpecPushPhase;
    let key: WaveId = wave_id.to_string().into();
    let deadline = Instant::now() + budget;
    loop {
        if Instant::now() >= deadline {
            eprintln!("[spec-push-dispatch-e2e] wait_until_idle budget elapsed (continuing)");
            return;
        }
        let phase = match state.spec_push.status(&key).await {
            Some(st) => st.phase,
            None => return,
        };
        // `Issuing` (a turn is being issued) is also "busy" — wait for it to
        // reconcile to TurnRunning/TurnCompleted before treating as idle.
        if !matches!(phase, SpecPushPhase::TurnRunning | SpecPushPhase::Issuing) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
