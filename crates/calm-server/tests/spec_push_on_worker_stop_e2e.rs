//! End-to-end verification that worker Stop hooks push a light observation to
//! the wave's spec app-server.
//!
//! This intentionally mirrors `spec_push_dispatch_e2e.rs`: create a real wave
//! with a spec `codex app-server`, attach a second observer client to the same
//! thread, emit persisted events into the same bus the dispatcher subscribes
//! to, and assert the observer sees `turn/started`.

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
use calm_server::event::{BroadcastEnvelope, Event, EventBus, EventScope};
use calm_server::ids::{ActorId, CardId, CoveId, WaveId};
use calm_server::model::{CardRole, NewCard, NewCove};
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
        eprintln!("[spec-push-worker-stop-e2e] SKIP: {}", format!($($arg)*));
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

async fn build_state(tmp: &TempDir, codex_bin: &Path) -> (AppState, Arc<SqlxRepo>) {
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().join("terminals"),
        proc_supervisor_sock: None,
    });
    let mut codex = CodexClient::new_stub();
    codex.codex_bin = codex_bin.to_string_lossy().to_string();

    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = WaveCoveCache::new();
    let events = EventBus::new();
    let state = AppState::from_parts(
        repo_dyn.clone(),
        events.clone(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo_dyn,
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-spec-push-worker-stop-e2e"),
            Vec::new(),
            events,
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        )),
        Arc::new(codex),
        Some(card_role_cache),
        Some(wave_cove_cache),
    );
    (state, repo)
}

async fn cove_of_wave(repo: &SqlxRepo, wave_id: &str) -> CoveId {
    let wave = repo
        .wave_get(wave_id)
        .await
        .unwrap()
        .expect("wave row exists");
    wave.cove_id
}

async fn create_worker_card(
    state: &AppState,
    repo: &SqlxRepo,
    wave_id: &str,
    kind: &str,
) -> CardId {
    let card = repo
        .card_create(NewCard {
            wave_id: WaveId::from(wave_id.to_string()),
            kind: kind.to_string(),
            sort: None,
            payload: json!({}),
        })
        .await
        .expect("create worker test card");
    sqlx::query("UPDATE cards SET role = 'worker' WHERE id = ?1")
        .bind(card.id.as_str())
        .execute(repo.pool())
        .await
        .expect("mark worker test card");
    state.card_role_cache.insert(
        card.id.clone(),
        CardRole::Worker,
        WaveId::from(wave_id.to_string()),
    );
    card.id
}

async fn emit_hook(
    state: &AppState,
    repo: &SqlxRepo,
    wave_id: &str,
    card_id: &CardId,
    event: Event,
) -> BroadcastEnvelope {
    let cove = cove_of_wave(repo, wave_id).await;
    let scope = EventScope::Card {
        card: card_id.clone(),
        wave: WaveId::from(wave_id.to_string()),
        cove,
    };
    let mut rx = state.events.subscribe();
    let actor = match &event {
        Event::CodexHook { .. } => ActorId::AiCodex(card_id.clone()),
        Event::ClaudeHook { .. } => ActorId::AiClaude(card_id.clone()),
        other => panic!("emit_hook got non-hook event: {other:?}"),
    };
    repo.log_pure_event(
        actor,
        scope,
        None,
        &state.events,
        &state.card_role_cache,
        &state.wave_cove_cache,
        event,
    )
    .await
    .expect("log_pure_event hook");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            Instant::now() < deadline,
            "hook envelope never observed on bus"
        );
        if let Ok(env) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("bus recv timeout")
            && matches!(
                env.event,
                Event::CodexHook { .. } | Event::ClaudeHook { .. }
            )
        {
            return env;
        }
    }
}

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
async fn worker_stop_hooks_push_turn_filter_and_dedup() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("codex binary not found (set NEIGE_CODEX_BIN); push path needs it");
    };
    eprintln!("[spec-push-worker-stop-e2e] using codex at {codex_bin:?}");

    let servers_before = count_codex_app_servers();
    eprintln!("[spec-push-worker-stop-e2e] codex app-server count BEFORE: {servers_before}");

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
            name: "worker-stop".into(),
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
            "title": "worker stop push wave",
            "cwd": "/tmp/worker-stop",
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]}
        }),
    )
    .await;
    if status != StatusCode::CREATED {
        skip!("wave create returned {status} (likely no codex auth / network); body={body}");
    }
    let wave_id = body.get("id").and_then(Value::as_str).unwrap().to_string();
    eprintln!("[spec-push-worker-stop-e2e] wave created: {wave_id}");

    let outcome = run_worker_stop_scenario(&state, repo.as_ref(), app.clone(), &wave_id).await;

    let _ = state.spec_push.remove(&wave_id.clone().into());
    tokio::time::sleep(Duration::from_millis(800)).await;

    let servers_after = count_codex_app_servers();
    eprintln!("[spec-push-worker-stop-e2e] codex app-server count AFTER: {servers_after}");
    assert!(
        servers_after <= servers_before,
        "leaked codex app-server: before={servers_before} after={servers_after}"
    );

    outcome.expect("worker stop push scenario");
    eprintln!("[spec-push-worker-stop-e2e] ALL PASS");
}

async fn run_worker_stop_scenario(
    state: &AppState,
    repo: &SqlxRepo,
    app: axum::Router,
    wave_id: &str,
) -> Result<(), String> {
    let cards = get_cards(app, wave_id).await;
    let (spec_id, payload) = find_spec_card(&cards);
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

    let (client2, mut notifs2) = CodexAppServer::connect(Path::new(&sock))
        .await
        .map_err(|e| format!("second client connect: {e}"))?;
    client2
        .initialize(ClientInfo {
            name: "spec-push-worker-stop-e2e-observer".into(),
            version: "0".into(),
        })
        .await
        .map_err(|e| format!("second client initialize: {e}"))?;
    client2
        .thread_resume(&thread_id)
        .await
        .map_err(|e| format!("second client thread_resume: {e}"))?;

    wait_until_idle(state, wave_id, Duration::from_secs(90)).await;

    let codex_worker = create_worker_card(state, repo, wave_id, "codex").await;
    let claude_worker = create_worker_card(state, repo, wave_id, "claude").await;

    let codex_stop = emit_hook(
        state,
        repo,
        wave_id,
        &codex_worker,
        Event::CodexHook {
            card_id: codex_worker.clone(),
            kind: "hook.codex.stop".into(),
            payload: json!({ "hook_event_name": "Stop" }),
        },
    )
    .await;
    expect_one_push(&mut notifs2, &thread_id, "codex worker stop").await?;

    wait_until_idle(state, wave_id, Duration::from_secs(90)).await;
    state.events.emit_envelope_for_test(BroadcastEnvelope {
        id: codex_stop.id,
        event_version: codex_stop.event_version,
        actor: codex_stop.actor.clone(),
        scope: codex_stop.scope.clone(),
        event: codex_stop.event.clone(),
    });
    expect_no_push(&mut notifs2, &thread_id, "duplicate codex worker stop").await?;

    let _ = emit_hook(
        state,
        repo,
        wave_id,
        &claude_worker,
        Event::ClaudeHook {
            card_id: claude_worker.clone(),
            kind: "hook.claude.stop".into(),
            payload: json!({ "hook_event_name": "Stop" }),
        },
    )
    .await;
    expect_one_push(&mut notifs2, &thread_id, "claude worker stop").await?;

    wait_until_idle(state, wave_id, Duration::from_secs(90)).await;
    let _ = emit_hook(
        state,
        repo,
        wave_id,
        &codex_worker,
        Event::CodexHook {
            card_id: codex_worker.clone(),
            kind: "hook.codex.post_tool_use".into(),
            payload: json!({ "hook_event_name": "PostToolUse" }),
        },
    )
    .await;
    expect_no_push(&mut notifs2, &thread_id, "codex non-stop hook").await?;

    let spec_card = CardId::from(spec_id);
    let _ = emit_hook(
        state,
        repo,
        wave_id,
        &spec_card,
        Event::CodexHook {
            card_id: spec_card.clone(),
            kind: "hook.codex.stop".into(),
            payload: json!({ "hook_event_name": "Stop" }),
        },
    )
    .await;
    expect_no_push(&mut notifs2, &thread_id, "spec card codex stop").await?;

    drop(client2);
    let _ = notifs2;
    Ok(())
}

async fn expect_one_push(
    notifs: &mut NotificationStream,
    thread_id: &str,
    label: &str,
) -> Result<(), String> {
    let starts = count_turn_starts(notifs, thread_id, Duration::from_secs(45)).await;
    if starts == 0 {
        return Err(format!("observed no turn/started after {label}"));
    }
    eprintln!("[spec-push-worker-stop-e2e] push PASS ({label}): {starts} turn/started");
    Ok(())
}

async fn expect_no_push(
    notifs: &mut NotificationStream,
    thread_id: &str,
    label: &str,
) -> Result<(), String> {
    let starts = count_turn_starts(notifs, thread_id, Duration::from_secs(10)).await;
    if starts != 0 {
        return Err(format!("unexpected {starts} turn/started after {label}"));
    }
    eprintln!("[spec-push-worker-stop-e2e] no-push PASS ({label})");
    Ok(())
}

async fn wait_until_idle(state: &AppState, wave_id: &str, budget: Duration) {
    use calm_server::spec_push::SpecPushPhase;
    let key: WaveId = wave_id.to_string().into();
    let deadline = Instant::now() + budget;
    loop {
        if Instant::now() >= deadline {
            eprintln!("[spec-push-worker-stop-e2e] wait_until_idle budget elapsed (continuing)");
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
