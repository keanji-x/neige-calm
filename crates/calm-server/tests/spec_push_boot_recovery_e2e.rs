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
//!    AND wipe the in-memory push cursor for the spec card via
//!    `clear_push_cursor_for_test` (mirroring a cold-boot cache). This
//!    step is load-bearing — without it, the surviving in-process bump
//!    from step 2 would mask a regression in `seed_push_cursor`. Right
//!    after, we run [`calm_server::takeover_spec_appservers_on_boot`] —
//!    the boot path `main.rs` invokes — against the same `AppState`.
//! 4. Takeover re-registers a `SpecPushHandle` for the wave and seeds the
//!    push cursor back from the persisted `payload.push_watermark`. We
//!    ASSERT the in-memory cursor reads back to the persisted value (so
//!    the test fails if `seed_push_cursor` regresses to a no-op).
//! 5. A NEW `task.completed` emitted AFTER restart (its `events.id` is
//!    above the watermark) reaches the spec thread as a fresh
//!    `turn/started` — the takeover handle is connected.
//! 6. Re-delivering a previously-acked envelope (its `events.id` is at or
//!    below the watermark) is silently deduped — no extra `turn/started`.
//!    We additionally wipe + re-seed the cursor BEFORE this probe so
//!    the dedup hit is provably keyed on the SEEDED watermark (not on a
//!    surviving in-process bump from step 5's live push).
//!
//! ## Round-2 review coverage (added on top of the above)
//!
//! 7. **B1 — queued-during-turn must not advance watermark on enqueue**.
//!    While a `turn/started` is in flight (no `turn/completed` yet) we
//!    emit a NEW `task.completed`. The dispatcher's
//!    `Inner::push_to_spec` should hit the `Enqueue` path (the
//!    SpecPushHandle's queue), NOT persist the durable watermark, and
//!    NOT issue a second `turn/start` (codex silently drops those).
//!    The persisted watermark stays at the prior value. Once the
//!    consumer's flush runs on the next `turn/completed`, the
//!    [`WatermarkSink`] callback advances the watermark past the queued
//!    envelope. This proves a kernel crash AFTER enqueue / BEFORE flush
//!    wouldn't lose the event (boot catch-up uses `id > watermark`).
//! 8. **B2 — always respawn**. The previous test asserted EITHER reuse
//!    OR respawn; round 2 dropped adoption entirely. We now require a
//!    fresh app-server pid after takeover (the persisted pgid is reaped
//!    + a new server is spawned), proving no adopt path survives.
//! 9. **B3 — per-wave lock + no-bump-without-handle keep boot catch-up
//!    honest in the race window**. Between (3a) clearing the registry
//!    and (3b) running takeover, we emit a fresh `task.completed` event
//!    (race_envelope). The dispatcher's `push_to_spec` takes the
//!    per-wave lock, finds no handle, and (round-2 fix) returns
//!    WITHOUT bumping the cursor. Then takeover takes the same lock,
//!    seeds the cursor from disk, inserts the handle, reads
//!    `events_since(watermark_pre)` (includes race_envelope), and
//!    catch_up_push_under_lock delivers it. Without the
//!    bump-after-handle-resolve move, the no-handle bump would poison
//!    the cursor and catch-up would dedup-skip → race_envelope LOST.
//!    The dispatcher unit test (`with_push_lock`) covers the lock
//!    semantics in isolation.
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

    // Teardown: use the production reap path (SIGTERM → 500ms grace →
    // SIGKILL → socket cleanup) rather than a bare `spec_push.remove`,
    // so a slow shutdown doesn't flake the no-leak assert. The bare
    // `remove` only fires `Drop` (one SIGTERM, no escalation) and was
    // the source of intermittent count drift on a loaded box.
    // #313 PR4 N7.
    calm_server::terminal_sweeper::reap_spec_push(&state, &WaveId::from(wave_id.clone())).await;
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
    // #315 round-2 (B2) — capture the pre-restart pgid. After takeover
    // (which now ALWAYS respawns, never adopts) the persisted pgid MUST
    // differ — proving the adopt path is gone.
    let pre_pgid = read_appserver_pgid(repo, &spec_card_id).await;
    if pre_pgid <= 1 {
        return Err(format!(
            "pre-restart appserver_pgid should be a real pgid (>1); got {pre_pgid}"
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

    // (2b) #315 round-3 (B2) — CREATE-WAVE SINK COVERAGE.
    //      Round-2 installed the WatermarkSink on the boot-takeover
    //      `register_and_catch_up` path but missed the symmetric
    //      `routes/waves.rs::spawn_push_appserver` create-wave path. A
    //      push enqueued mid-turn against a freshly-created (not
    //      recovered) handle would flush correctly but the durable
    //      watermark would never advance — a kernel restart would then
    //      replay already-delivered events to the spec thread.
    //
    //      To prove the round-3 fix is wired on the create-wave path
    //      WITHOUT depending on the takeover flow, we exercise an
    //      enqueue→flush cycle on the wave's ORIGINAL handle (the one
    //      `spawn_push_appserver` installed earlier in this scenario —
    //      no restart has happened yet here). Shape mirrors scenario
    //      (6): kicker → wait for TurnRunning → queued envelope →
    //      assert watermark unchanged → wait_until_idle (flush) →
    //      assert watermark advanced past queued.
    use calm_server::spec_appserver::SpecPushPhase as SpecPushPhaseB2;
    eprintln!(
        "[spec-push-boot-recovery-e2e] (2b) B2 create-wave sink check: \
         exercising enqueue→flush on the ORIGINAL (no-restart) handle"
    );
    wait_until_idle(state, wave_id, Duration::from_secs(90)).await;
    let watermark_pre_b2 = persisted_watermark(repo, &spec_card_id).await;
    let kicker_b2 =
        emit_task_completed(state, repo, wave_id, "boot-recovery-b2-create-kicker").await;
    let key_b2: WaveId = wave_id.to_string().into();
    let phase_deadline_b2 = Instant::now() + Duration::from_secs(60);
    let mut saw_running_b2 = false;
    while Instant::now() < phase_deadline_b2 {
        if let Some(st) = state.spec_push.status(&key_b2).await
            && matches!(
                st.phase,
                SpecPushPhaseB2::TurnRunning | SpecPushPhaseB2::Issuing
            )
        {
            saw_running_b2 = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if !saw_running_b2 {
        return Err(format!(
            "B2 create-wave: never observed TurnRunning/Issuing after kicker (kicker_b2={kicker_b2})"
        ));
    }
    // Snapshot watermark while the kicker turn is running — kicker
    // should have advanced it (single-issue Issued path).
    let watermark_mid_b2 = persisted_watermark(repo, &spec_card_id).await;
    if watermark_mid_b2 < kicker_b2 {
        return Err(format!(
            "B2 create-wave setup: kicker should have advanced watermark; \
             watermark_mid_b2={watermark_mid_b2} kicker_b2={kicker_b2}"
        ));
    }
    // Second event mid-turn → MUST hit Enqueue arm; durable watermark
    // MUST NOT advance past the queued id until the flush.
    let queued_b2 =
        emit_task_completed(state, repo, wave_id, "boot-recovery-b2-create-queued").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let watermark_after_enqueue_b2 = persisted_watermark(repo, &spec_card_id).await;
    if watermark_after_enqueue_b2 >= queued_b2 {
        return Err(format!(
            "B2 create-wave VIOLATION: watermark advanced past a QUEUED envelope on the \
             create-wave path: watermark_after_enqueue_b2={watermark_after_enqueue_b2} \
             queued_b2={queued_b2} (kicker_b2={kicker_b2}, \
             watermark_mid_b2={watermark_mid_b2}, watermark_pre_b2={watermark_pre_b2}) \
             — sink was installed by Issued path, not Enqueue; round-3 fix missing"
        ));
    }
    eprintln!(
        "[spec-push-boot-recovery-e2e] (2b) B2 create-wave enqueue OK: \
         watermark stayed at {watermark_after_enqueue_b2} (queued_b2={queued_b2})"
    );
    // Wait for flush via consumer task's `turn/completed` →
    // `flush_push_queue` → installed sink call.
    wait_until_idle(state, wave_id, Duration::from_secs(90)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let watermark_after_flush_b2 = persisted_watermark(repo, &spec_card_id).await;
    if watermark_after_flush_b2 < queued_b2 {
        return Err(format!(
            "B2 create-wave VIOLATION: flush did NOT advance watermark past queued \
             envelope on the create-wave path: watermark_after_flush_b2={watermark_after_flush_b2} \
             queued_b2={queued_b2} — this is the exact bug the round-3 sink-on-create \
             fix targets; without it the consumer task flushes the queue but the \
             WatermarkSink slot is None, so persistence silently no-ops"
        ));
    }
    eprintln!(
        "[spec-push-boot-recovery-e2e] (2b) B2 create-wave flush PASS: \
         watermark advanced to {watermark_after_flush_b2} after flush (queued_b2={queued_b2})"
    );
    // Refresh `watermark_pre` snapshot so downstream B3 race math
    // (race_envelope > watermark_pre) reflects the post-(2b) state.
    // (`emit_task_completed` for the kicker + queued advanced the
    // events.id sequence; without this refresh, B3's `race_envelope >
    // watermark_pre` precondition could spuriously fail if the IDs
    // overlap in a regression. A normal run satisfies it trivially.)
    let watermark_pre = persisted_watermark(repo, &spec_card_id).await;

    // Drop the pre-restart observer + its socket before we simulate restart.
    drop(observer_pre);
    drop(notifs_pre);
    // Also drop any spec_push handle we still hold so the takeover starts
    // from a TRULY empty registry — that's what a kernel restart looks like.
    let _ = state.spec_push.remove(&WaveId::from(wave_id.to_string()));
    // #313 PR4 B3 — ALSO clear the in-memory push cursor. The previous
    // version of this test left the cache populated, so the cold-boot
    // seed path (`seed_push_cursor`) was never actually exercised — a
    // regression that broke seeding would have been silently masked
    // (dedup in scenario 6 still passed via the surviving in-process
    // watermark). Wipe to mirror a true cold cache: empty → seed from
    // disk → first push observed.
    let spec_card_key: CardId = spec_card_id.clone().into();
    state.dispatcher.clear_push_cursor_for_test(&spec_card_key);
    let cursor_after_clear = state.dispatcher.push_cursor_for_test(&spec_card_key);
    if cursor_after_clear != 0 {
        return Err(format!(
            "test setup bug: clear_push_cursor_for_test left cursor at {cursor_after_clear}"
        ));
    }
    eprintln!(
        "[spec-push-boot-recovery-e2e] (3a) simulated kernel restart: dropped registry handle \
         AND cleared in-memory push cursor (cold-boot cache simulation)"
    );

    // #315 round-2 (B3) — RACE SETUP. Emit a NEW task.completed event
    // BEFORE we run boot takeover. This simulates a live event landing
    // on the bus during the boot window (between "kernel started" and
    // "takeover registered handles"). The dispatcher's `push_to_spec`
    // will:
    //   1. take the per-wave lock,
    //   2. dedup-check (envelope > 0, cursor at 0 post-clear) → pass,
    //   3. resolve handle → **MISSING** (we just dropped the registry),
    //   4. round-2 fix: log warn + return WITHOUT bumping the cursor.
    // Then takeover runs, takes the same per-wave lock, seeds cursor
    // from disk (watermark_pre, < race_envelope), inserts the handle,
    // reads events_since(watermark_pre) → includes race_envelope, then
    // catch_up_push_under_lock delivers it (cursor is still at the seed
    // value, race_envelope > seed → pass dedup → push for real).
    //
    // PRE-FIX (without the bump-after-handle-lookup move) this test would
    // fail: the bump in step 4 above would set cursor=race_envelope; then
    // catch-up's dedup would see cursor >= race_envelope and silently
    // skip → the event would be LOST. The lock alone (without moving the
    // bump) doesn't help because the live push gets the lock first when
    // it lands before takeover.
    let race_envelope = emit_task_completed(state, repo, wave_id, "boot-recovery-b3-race").await;
    if race_envelope <= watermark_pre {
        return Err(format!(
            "B3 setup bug: race_envelope must be above watermark_pre (above the seeded floor); \
             race_envelope={race_envelope} watermark_pre={watermark_pre}"
        ));
    }
    // Give the broadcast a beat to land on push_to_spec.
    // TODO(#313 round-3 N4): replace this fixed sleep with deterministic
    // synchronization — e.g., a counter/span incremented on the
    // no-handle branch, polled with a bounded retry. The 200 ms guess
    // can flake on a loaded box (broadcast → spawn → lock acquire →
    // dedup check is normally <10 ms but can spike). The invariant we
    // need is "dispatcher saw the race envelope", not "200 ms have
    // elapsed". Deferred because the bounded-retry shape needs a tiny
    // test hook on `EventBus` or `Dispatcher` to count receive ticks.
    tokio::time::sleep(Duration::from_millis(200)).await;
    // The cursor MUST still be 0 (B3 fix: no bump-without-handle).
    let cursor_after_race_emit = state.dispatcher.push_cursor_for_test(&spec_card_key);
    if cursor_after_race_emit != 0 {
        return Err(format!(
            "B3 violation (pre-takeover): cursor advanced to {cursor_after_race_emit} \
             even though no handle was registered — the dispatcher bumped the cursor on the \
             no-handle path. This silently poisons boot catch-up. \
             (race_envelope={race_envelope})"
        ));
    }
    eprintln!(
        "[spec-push-boot-recovery-e2e] (3a') B3 race-setup: emitted race_envelope={race_envelope} \
         while no handle; cursor stayed at 0 (no-handle no-bump invariant holds)"
    );

    // (3b) Run the boot takeover the kernel's main.rs would run.
    calm_server::takeover_spec_appservers_on_boot(state).await;
    let wave_key: WaveId = wave_id.to_string().into();
    if !state.spec_push.contains(&wave_key) {
        return Err(
            "takeover did not re-register a SpecPushHandle for the in-flight wave".to_string(),
        );
    }
    // B3 — explicit assert: after takeover, the in-memory cursor must
    // be at least the persisted watermark (seed_push_cursor floor) and
    // since (3a') emitted race_envelope, the catch-up replay MUST have
    // delivered it, advancing the cursor to race_envelope.
    //
    //   * cursor < watermark_pre → seed_push_cursor regressed (floor lost),
    //   * cursor < race_envelope → catch-up FAILED to deliver the racing
    //     event (B3 violation — boot catch-up didn't see/deliver it).
    //
    // The B3 race fix (cursor bump moved AFTER handle lookup +
    // register_and_catch_up under the per-wave lock) makes this pass.
    let cursor_after_seed = state.dispatcher.push_cursor_for_test(&spec_card_key);
    if cursor_after_seed < watermark_pre {
        return Err(format!(
            "takeover did not seed in-memory cursor from persisted watermark: \
             cursor_after_seed={cursor_after_seed} expected>={watermark_pre} \
             (the test wiped the cache to 0 before takeover; if seed_push_cursor \
             regressed to a no-op the cursor would still read 0 here)"
        ));
    }
    if cursor_after_seed < race_envelope {
        return Err(format!(
            "B3 violation: catch-up did NOT deliver the racing live envelope; \
             cursor_after_seed={cursor_after_seed} race_envelope={race_envelope} \
             (the bus broadcast for race_envelope landed pre-takeover with no handle, \
             then takeover ran. If the dispatcher bumps the cursor on the no-handle path, \
             catch-up dedups against the bumped cursor and silently drops the event — \
             that's the bug this scenario guards against)"
        ));
    }
    eprintln!(
        "[spec-push-boot-recovery-e2e] (3b) takeover registered a fresh push handle, \
         seeded cursor (>= watermark_pre={watermark_pre}), AND catch-up delivered the \
         racing live envelope (cursor_after_seed={cursor_after_seed} >= \
         race_envelope={race_envelope})"
    );

    // #315 round-2 (B2) — assert the persisted pgid changed across
    // takeover. Round-1 supported adopting a still-live persisted
    // app-server, which would leave the SAME pgid in place. Round-2
    // dropped adoption: every takeover MUST respawn, so the new pgid
    // is fresh (different from `pre_pgid`). If a future regression
    // re-introduces adoption, this assertion fires.
    let post_pgid = read_appserver_pgid(repo, &spec_card_id).await;
    if post_pgid <= 1 {
        return Err(format!(
            "post-takeover appserver_pgid should be a real pgid (>1); got {post_pgid}"
        ));
    }
    if post_pgid == pre_pgid {
        return Err(format!(
            "B2 violation: takeover did NOT respawn — persisted pgid unchanged across restart \
             (pre={pre_pgid} post={post_pgid}). The adopt path was supposed to be removed."
        ));
    }
    eprintln!(
        "[spec-push-boot-recovery-e2e] (3c) B2 respawn PASS: pgid changed pre={pre_pgid} \
         post={post_pgid} (always-respawn invariant holds)"
    );

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
    //     invariant.
    //
    //     #313 PR4 B3 — to prove dedup operates on the SEEDED watermark
    //     (not a surviving in-process bump from the post-restart push in
    //     scenario 4), we wipe the in-memory cursor again and re-seed
    //     from disk before the dedup probe. This makes the test fail if
    //     someone re-introduces a regression where dedup leans on the
    //     live in-process cursor instead of the persisted floor.
    state.dispatcher.clear_push_cursor_for_test(&spec_card_key);
    let persisted_now = persisted_watermark(repo, &spec_card_id).await;
    state
        .dispatcher
        .seed_push_cursor(spec_card_key.clone(), persisted_now);
    let cursor_pre_dedup = state.dispatcher.push_cursor_for_test(&spec_card_key);
    if cursor_pre_dedup != persisted_now {
        return Err(format!(
            "dedup setup: re-seed did not restore cursor to persisted watermark: \
             cursor={cursor_pre_dedup} expected={persisted_now}"
        ));
    }
    eprintln!(
        "[spec-push-boot-recovery-e2e] (5a) wiped + re-seeded cursor to {persisted_now} \
         before dedup probe (proves dedup keys on SEEDED value)"
    );

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

    // (6) Round-2 (B1) — queued-during-turn must NOT advance watermark on
    //     enqueue. Drive a fresh task.completed envelope into the dispatcher
    //     while the spec thread is mid-turn (force the tracked phase to
    //     `TurnRunning` so the new event hits the Enqueue arm). Assert:
    //       (a) the durable watermark stays at its prior value (queued, not
    //           delivered), AND
    //       (b) no extra `turn/started` is observed (codex didn't see a
    //           second concurrent `turn/start`).
    //     Then flip the phase back to `TurnCompleted`, drive a synthetic
    //     turn/completed via the consumer's flush path... actually, simpler:
    //     trigger the next live push from idle so the flush_push_queue
    //     fires naturally on the codex-emitted turn/completed. After that
    //     the watermark MUST have advanced past the queued envelope.
    eprintln!(
        "[spec-push-boot-recovery-e2e] (6) B1 enqueue-then-flush: \
         force TurnRunning, emit task.completed, expect watermark unchanged"
    );
    let watermark_pre_enqueue = persisted_watermark(repo, &spec_card_id).await;
    // Force the in-memory phase to TurnRunning via the SpecPusher's status
    // mutex — same pattern the dispatch e2e uses. We don't have direct
    // access to the SharedStatus here, but `state.spec_push.status(wave)`
    // exposes a clone of it. We need write access, so reach in via the
    // pusher's queue/status. The cleanest hook is a small helper on the
    // registry; until then we drive it through a real mid-turn by
    // attaching a fresh observer and emitting an event while the prior
    // turn from (4) might still be running. To keep the test
    // deterministic we instead skip this scenario if we can't observe
    // the phase as TurnRunning and document the limitation.
    use calm_server::spec_appserver::SpecPushPhase;
    let key: WaveId = wave_id.to_string().into();
    let phase_now = state.spec_push.status(&key).await.map(|s| s.phase);
    eprintln!(
        "[spec-push-boot-recovery-e2e] (6) current phase: {phase_now:?} \
         (watermark_pre_enqueue={watermark_pre_enqueue})"
    );

    // To deterministically exercise the enqueue path we attach an observer,
    // start a long-running turn ourselves via a new live push, then BEFORE
    // turn/completed lands, emit a second task.completed and observe that
    // it lands on the queue.
    let post2_sock = read_appserver_sock(repo, &spec_card_id).await;
    let (_obs_b1, mut notifs_b1) = CodexAppServer::connect(Path::new(&post2_sock))
        .await
        .map_err(|e| format!("b1 observer connect: {e}"))?;
    _obs_b1
        .initialize(ClientInfo {
            name: "boot-recovery-b1-observer".into(),
            version: "0".into(),
        })
        .await
        .map_err(|e| format!("b1 observer initialize: {e}"))?;
    _obs_b1
        .thread_resume(&thread_id)
        .await
        .map_err(|e| format!("b1 observer thread_resume: {e}"))?;
    // Wait until idle before kicking off the run.
    wait_until_idle(state, wave_id, Duration::from_secs(90)).await;

    // First envelope: kicks off a fresh turn (Idle → Issuing → TurnRunning).
    let kicker_envelope =
        emit_task_completed(state, repo, wave_id, "boot-recovery-b1-kicker").await;
    // Wait until the kicker turn is actually running (not yet completed).
    // Poll the phase up to a budget.
    let phase_deadline = Instant::now() + Duration::from_secs(60);
    let mut saw_running = false;
    while Instant::now() < phase_deadline {
        if let Some(st) = state.spec_push.status(&key).await
            && matches!(
                st.phase,
                SpecPushPhase::TurnRunning | SpecPushPhase::Issuing
            )
        {
            saw_running = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if !saw_running {
        return Err(format!(
            "B1 scenario: never observed TurnRunning/Issuing after kicker emit \
             (kicker_envelope={kicker_envelope})"
        ));
    }
    // Snapshot watermark while the turn is running. The kicker has been
    // delivered (`Issued { max_envelope_id = kicker }`) so watermark
    // ≥ kicker_envelope at this point.
    let watermark_mid_turn = persisted_watermark(repo, &spec_card_id).await;
    if watermark_mid_turn < kicker_envelope {
        return Err(format!(
            "B1 setup bug: kicker should have advanced watermark; \
             watermark_mid_turn={watermark_mid_turn} kicker_envelope={kicker_envelope}"
        ));
    }

    // Second envelope: emitted WHILE a turn is running → must hit the
    // Enqueue arm of push_to_spec.
    let queued_envelope =
        emit_task_completed(state, repo, wave_id, "boot-recovery-b1-queued").await;
    // Give the dispatcher a beat to process the broadcast.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let watermark_after_enqueue = persisted_watermark(repo, &spec_card_id).await;
    if watermark_after_enqueue >= queued_envelope {
        return Err(format!(
            "B1 violation: watermark advanced past a QUEUED (undelivered) envelope! \
             watermark_after_enqueue={watermark_after_enqueue} queued_envelope={queued_envelope} \
             (kicker_envelope={kicker_envelope}, watermark_mid_turn={watermark_mid_turn}) \
             — a kernel crash here would lose the queued event"
        ));
    }
    eprintln!(
        "[spec-push-boot-recovery-e2e] (6) B1 enqueue PASS: \
         watermark stayed at {watermark_after_enqueue} (queued_envelope={queued_envelope} \
         not yet persisted)"
    );

    // Now wait for the kicker's turn/completed → flush_push_queue → second
    // turn/start carrying the queued obs. The WatermarkSink should then
    // persist watermark to the queued envelope's id.
    wait_until_idle(state, wave_id, Duration::from_secs(90)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let watermark_after_flush = persisted_watermark(repo, &spec_card_id).await;
    if watermark_after_flush < queued_envelope {
        return Err(format!(
            "B1 flush did NOT advance watermark past queued envelope: \
             watermark_after_flush={watermark_after_flush} queued_envelope={queued_envelope}"
        ));
    }
    eprintln!(
        "[spec-push-boot-recovery-e2e] (6) B1 flush PASS: \
         watermark advanced to {watermark_after_flush} after flush \
         (queued_envelope={queued_envelope})"
    );

    // Drain any pending notifications on the b1 observer.
    let _ = count_turn_starts(&mut notifs_b1, &thread_id, Duration::from_millis(100)).await;
    drop(_obs_b1);
    drop(notifs_b1);

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

/// #315 round-2 (B2) — read the spec card's persisted `appserver_pgid`
/// so the test can prove a respawn happened (post != pre). Returns -1 if
/// missing so callers can sanity-check (`> 1` for a real pgid).
async fn read_appserver_pgid(repo: &Arc<dyn Repo>, card_id: &str) -> i64 {
    let card = repo
        .card_get(card_id)
        .await
        .unwrap()
        .expect("spec card row exists");
    card.payload
        .get("appserver_pgid")
        .and_then(Value::as_i64)
        .unwrap_or(-1)
}

// Reference some types so unused-import warnings stay quiet under the
// feature gate (and so the diagnostic surface for future maintainers
// pins the takeover contract: it reads CardRole::Spec, the role cache,
// and the dispatcher's catch-up entry point).
#[allow(dead_code)]
fn _typecheck() -> (CardId, CardRole) {
    (CardId::from(""), CardRole::Spec)
}
