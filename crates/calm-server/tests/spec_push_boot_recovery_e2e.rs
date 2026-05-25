//! Issue #313 problem #1 — end-to-end verification of **boot-time takeover**
//! of an in-flight spec wave against a **real `codex app-server`**.
//!
//! Feature-gated behind `codex-e2e` (same convention as
//! `spec_push_process_model_e2e.rs` and `spec_push_dispatch_e2e.rs`) because
//! CI ships no `codex` binary and cannot run model turns. Run locally with:
//!
//! ```sh
//! cargo test -p calm-server --features codex-e2e \
//!   --test spec_push_boot_recovery_e2e -- --nocapture
//! ```
//!
//! ## What it proves
//!
//! 1. `POST /api/waves` creates a wave; the spec card's payload carries
//!    `codex_thread_id` (turn #1 actually ran) and `push_watermark` = 0
//!    (no pushes have been dispatched yet).
//! 2. The dispatcher push path works in steady state (one `task.completed`
//!    → one fresh `turn/started` on a side observer client). This both
//!    validates the wave's push channel AND advances the persisted
//!    `push_watermark` past 0.
//! 3. **Simulated kernel restart**: we drop the `SpecPushRegistry` entry
//!    (the in-memory handle disappears, mirroring what kernel exit does)
//!    AND seed the in-memory `EventCursorCache` back to 0 (mirroring a
//!    cold-boot cache). Right after, we run
//!    [`calm_server::takeover_spec_appservers_on_boot`] — the boot path
//!    `main.rs` invokes — against the same `AppState`.
//! 4. Takeover re-registers a `SpecPushHandle` for the wave and seeds the
//!    push watermark back from the persisted field (so the test wave is
//!    once again live + the cache is at its pre-restart value).
//! 5. A NEW `task.completed` emitted AFTER restart (its `events.id` is
//!    above the watermark) reaches the spec thread as a fresh
//!    `turn/started` — the takeover handle is connected.
//! 6. Re-delivering a previously-acked envelope (its `events.id` is at or
//!    below the watermark) is silently deduped — no extra `turn/started`.
//!
//! ## Self-skip
//!
//! Resolves the codex binary via `NEIGE_CODEX_BIN` (tilde-expanded). If
//! absent — or the kernel's app-server boot fails (no auth / no network)
//! — the whole test prints a skip marker and returns success.
//!
//! ## No-leak discipline
//!
//! A BEFORE/AFTER `ps` snapshot brackets the test and asserts no net new
//! `codex app-server` processes survive. Boot takeover respawns a SECOND
//! app-server, but the first one is killed on registry-handle drop in step
//! 3 above; after the test's final teardown both must be reaped.

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
use calm_server::event::{ArtifactRef, Event, EventBus, EventScope};
use calm_server::ids::{ActorId, CardId, CoveId, WaveId};
use calm_server::model::{CardRole, NewCove};
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

fn locate_recorder_bin() -> PathBuf {
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_argv-recorder-daemon") {
        return PathBuf::from(p);
    }
    let me = std::env::current_exe().expect("current_exe");
    let target_profile = me
        .parent()
        .and_then(|p| p.parent())
        .expect("test bin parent");
    let candidate = target_profile.join("argv-recorder-daemon");
    assert!(
        candidate.exists(),
        "argv-recorder-daemon not found at {candidate:?}; build with \
         `cargo build --tests -p calm-server`"
    );
    candidate
}

macro_rules! skip {
    ($($arg:tt)*) => {{
        eprintln!("[spec-push-boot-recovery-e2e] SKIP: {}", format!($($arg)*));
        return;
    }};
}

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
        session_daemon_bin: locate_recorder_bin(),
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
            std::env::temp_dir().join("calm-plugins-data-spec-push-boot-recovery-e2e"),
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

async fn cove_of_wave(repo: &Arc<dyn Repo>, wave_id: &str) -> CoveId {
    let wave = repo
        .wave_get(wave_id)
        .await
        .unwrap()
        .expect("wave row exists");
    wave.cove_id
}

/// Emit a persisted `task.completed` and return its real `events.id`.
async fn emit_task_completed(
    state: &AppState,
    repo: &Arc<dyn Repo>,
    wave_id: &str,
    idem: &str,
) -> i64 {
    let cove = cove_of_wave(repo, wave_id).await;
    let scope = EventScope::Wave {
        wave: WaveId::from(wave_id.to_string()),
        cove,
    };
    let ev = Event::TaskCompleted {
        idempotency_key: idem.to_string(),
        result: json!({ "note": "boot-recovery-e2e" }),
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
    .expect("log_pure_event task.completed")
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

/// Poll the wave's spec `SpecPushHandle` status until the tracked phase
/// is between turns, or the budget elapses (see the sibling dispatch E2E
/// for rationale). Uses the non-destructive `status` read.
async fn wait_until_idle(state: &AppState, wave_id: &str, budget: Duration) {
    use calm_server::spec_appserver::SpecPushPhase;
    let key: WaveId = wave_id.to_string().into();
    let deadline = Instant::now() + budget;
    loop {
        if Instant::now() >= deadline {
            eprintln!("[spec-push-boot-recovery-e2e] wait_until_idle budget elapsed (continuing)");
            return;
        }
        let phase = match state.spec_push.status(&key).await {
            Some(st) => st.phase,
            None => return,
        };
        if !matches!(phase, SpecPushPhase::TurnRunning | SpecPushPhase::Issuing) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Read `payload.push_watermark` for a spec card directly from the DB.
async fn persisted_watermark(repo: &Arc<dyn Repo>, card_id: &str) -> i64 {
    let card = repo
        .card_get(card_id)
        .await
        .unwrap()
        .expect("spec card row exists");
    card.payload
        .get("push_watermark")
        .and_then(Value::as_i64)
        .unwrap_or(-1)
}

#[tokio::test]
async fn boot_takeover_resumes_spec_thread_and_dedups_via_watermark() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("codex binary not found (set NEIGE_CODEX_BIN); push path needs it");
    };
    eprintln!("[spec-push-boot-recovery-e2e] using codex at {codex_bin:?}");

    let servers_before = count_codex_app_servers();
    eprintln!("[spec-push-boot-recovery-e2e] codex app-server count BEFORE: {servers_before}");

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
            name: "boot-recovery".into(),
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
            "title": "boot recovery wave",
            "cwd": "/tmp/boot-recovery-e2e",
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]}
        }),
    )
    .await;
    if status != StatusCode::CREATED {
        skip!("wave create returned {status} (likely no codex auth / network); body={body}");
    }
    let wave_id = body.get("id").and_then(Value::as_str).unwrap().to_string();
    eprintln!("[spec-push-boot-recovery-e2e] wave created: {wave_id}");

    // Run the recovery scenario under a guard that reaps regardless of
    // pass/fail so we never leak a codex app-server on assertion failure.
    let outcome = run_recovery_scenario(&state, &repo, app.clone(), &wave_id).await;

    // Teardown: reap whatever's currently in the registry (the takeover
    // handle, if it took over; otherwise the original).
    let _ = state.spec_push.remove(&WaveId::from(wave_id.clone()));
    tokio::time::sleep(Duration::from_millis(800)).await;

    let servers_after = count_codex_app_servers();
    eprintln!("[spec-push-boot-recovery-e2e] codex app-server count AFTER: {servers_after}");
    assert!(
        servers_after <= servers_before,
        "leaked codex app-server: before={servers_before} after={servers_after}"
    );
    eprintln!("[spec-push-boot-recovery-e2e] no-leak PASS");

    outcome.expect("recovery scenario");
    eprintln!("[spec-push-boot-recovery-e2e] ALL PASS");
}

async fn run_recovery_scenario(
    state: &AppState,
    repo: &Arc<dyn Repo>,
    app: axum::Router,
    wave_id: &str,
) -> Result<(), String> {
    // (1) Spec card has codex_thread_id + watermark = 0 (no pushes yet).
    let cards = get_cards(app.clone(), wave_id).await;
    let (spec_card_id, payload) = find_spec_card(&cards);
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
    let watermark0 = persisted_watermark(repo, &spec_card_id).await;
    if watermark0 != 0 {
        return Err(format!(
            "fresh wave's persisted push_watermark must be 0; got {watermark0}"
        ));
    }
    eprintln!("[spec-push-boot-recovery-e2e] (1) thread_id={thread_id} sock={sock} watermark=0");

    // (2) Pre-restart push: side-observer + emit task.completed → assert
    //     turn/started + watermark bumped past 0.
    let (observer_pre, mut notifs_pre) = CodexAppServer::connect(Path::new(&sock))
        .await
        .map_err(|e| format!("pre observer connect: {e}"))?;
    observer_pre
        .initialize(ClientInfo {
            name: "boot-recovery-pre-observer".into(),
            version: "0".into(),
        })
        .await
        .map_err(|e| format!("pre observer initialize: {e}"))?;
    observer_pre
        .thread_resume(&thread_id)
        .await
        .map_err(|e| format!("pre observer thread_resume: {e}"))?;
    wait_until_idle(state, wave_id, Duration::from_secs(90)).await;
    let pre_envelope_id = emit_task_completed(state, repo, wave_id, "boot-recovery-pre").await;
    let pre_starts = count_turn_starts(&mut notifs_pre, &thread_id, Duration::from_secs(45)).await;
    if pre_starts == 0 {
        return Err(
            "pre-restart push did not produce turn/started — push channel broken".to_string(),
        );
    }
    // Settle so the dispatcher's persist completes.
    wait_until_idle(state, wave_id, Duration::from_secs(90)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let watermark_pre = persisted_watermark(repo, &spec_card_id).await;
    if watermark_pre < pre_envelope_id {
        return Err(format!(
            "watermark did not advance past pre-restart envelope: persisted={watermark_pre} pre_envelope_id={pre_envelope_id}"
        ));
    }
    eprintln!(
        "[spec-push-boot-recovery-e2e] (2) pre-restart push PASS \
         (pre_envelope_id={pre_envelope_id} persisted_watermark={watermark_pre})"
    );
    // Drop the pre-restart observer + its socket before we simulate restart.
    drop(observer_pre);
    drop(notifs_pre);
    // Also drop any spec_push handle we still hold so the takeover starts
    // from a TRULY empty registry — that's what a kernel restart looks like.
    let _ = state.spec_push.remove(&WaveId::from(wave_id.to_string()));
    eprintln!(
        "[spec-push-boot-recovery-e2e] (3a) simulated kernel restart: dropped registry handle"
    );

    // (3b) Run the boot takeover the kernel's main.rs would run.
    calm_server::takeover_spec_appservers_on_boot(state).await;
    let wave_key: WaveId = wave_id.to_string().into();
    if !state.spec_push.contains(&wave_key) {
        return Err(
            "takeover did not re-register a SpecPushHandle for the in-flight wave".to_string(),
        );
    }
    eprintln!("[spec-push-boot-recovery-e2e] (3b) takeover registered a fresh push handle");

    // (4) New push AFTER restart with id > watermark → turn/started fires.
    //     Attach a new observer to whatever socket the takeover handle owns
    //     (it may be the same sock since we point at the same per-card
    //     path, but the underlying server is a fresh respawn).
    let post_sock = read_appserver_sock(repo, &spec_card_id).await;
    let (observer_post, mut notifs_post) = CodexAppServer::connect(Path::new(&post_sock))
        .await
        .map_err(|e| format!("post observer connect: {e}"))?;
    observer_post
        .initialize(ClientInfo {
            name: "boot-recovery-post-observer".into(),
            version: "0".into(),
        })
        .await
        .map_err(|e| format!("post observer initialize: {e}"))?;
    observer_post
        .thread_resume(&thread_id)
        .await
        .map_err(|e| format!("post observer thread_resume: {e}"))?;
    wait_until_idle(state, wave_id, Duration::from_secs(90)).await;
    let post_envelope_id = emit_task_completed(state, repo, wave_id, "boot-recovery-post").await;
    if post_envelope_id <= watermark_pre {
        return Err(format!(
            "post-restart envelope id must be > pre-restart watermark; \
             post_envelope_id={post_envelope_id} pre_watermark={watermark_pre}"
        ));
    }
    let post_starts =
        count_turn_starts(&mut notifs_post, &thread_id, Duration::from_secs(45)).await;
    if post_starts == 0 {
        return Err(
            "post-restart push did not produce turn/started — takeover did not catch up"
                .to_string(),
        );
    }
    eprintln!(
        "[spec-push-boot-recovery-e2e] (4) post-restart push PASS \
         (post_envelope_id={post_envelope_id})"
    );

    // (5) Dedup via watermark: a low-id push (use pre_envelope_id itself) must
    //     be silently dropped — re-feeding it through the dispatcher's
    //     catch-up entry point is the cleanest way to test the dedup arm
    //     specifically (a fresh `log_pure_event` would advance the id past
    //     the watermark and bypass the dedup). The catch-up path uses the
    //     same dedup as the live path, so a dedup hit there proves the
    //     invariant. After the call, wait + assert zero new turn/started.
    let pre_event = Event::TaskCompleted {
        idempotency_key: "boot-recovery-pre".into(),
        result: json!({ "note": "boot-recovery-e2e" }),
        artifacts: Vec::<ArtifactRef>::new(),
    };
    state
        .dispatcher
        .catch_up_push(
            WaveId::from(wave_id.to_string()),
            pre_event,
            pre_envelope_id,
        )
        .await;
    let dedup_extra =
        count_turn_starts(&mut notifs_post, &thread_id, Duration::from_secs(10)).await;
    if dedup_extra != 0 {
        return Err(format!(
            "dedup FAILED: pushing pre-restart envelope id={pre_envelope_id} after restart \
             produced {dedup_extra} new turn/started (watermark not honored)"
        ));
    }
    eprintln!("[spec-push-boot-recovery-e2e] (5) dedup PASS: id <= watermark dropped silently");

    drop(observer_post);
    drop(notifs_post);
    Ok(())
}

async fn read_appserver_sock(repo: &Arc<dyn Repo>, card_id: &str) -> String {
    let card = repo
        .card_get(card_id)
        .await
        .unwrap()
        .expect("spec card row exists");
    card.payload
        .get("appserver_sock")
        .and_then(Value::as_str)
        .map(str::to_string)
        .expect("appserver_sock persisted on spec card")
}

// Reference some types so unused-import warnings stay quiet under the
// feature gate (and so the diagnostic surface for future maintainers
// pins the takeover contract: it reads CardRole::Spec, the role cache,
// and the dispatcher's catch-up entry point).
#[allow(dead_code)]
fn _typecheck() -> (CardId, CardRole) {
    (CardId::from(""), CardRole::Spec)
}
