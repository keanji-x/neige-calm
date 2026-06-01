#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::codex_appserver::InputItem;
use calm_server::config::Config;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{CardRole, NewCove};
use calm_server::pending_codex_threads::PendingThreadStartRegistry;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, DaemonClient};
use clap::Parser;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

mod common;

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Boot {
    app: axum::Router,
    state: AppState,
    repo: Arc<SqlxRepo>,
    cove_id: String,
    _tmp: TempDir,
}

async fn boot(shared_enabled: bool, start_shared: bool) -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "spec-shared".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let events = EventBus::new();
    let role_cache = CardRoleCache::new();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient {
            data_dir: tmp.path().join("terminals"),
            proc_supervisor_sock: None,
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            tmp.path().join("plugins-data"),
            Vec::new(),
            EventBus::new(),
            role_cache.clone(),
            wave_cove_cache.clone(),
        )),
        Arc::new(common::fake_codex_client()),
        Some(role_cache),
        Some(wave_cove_cache),
    )
    .with_shared_codex_spec_cards_enabled(shared_enabled);

    let fake_codex_bin = common::fake_codex_bin();
    let cfg = Config::parse_from([
        "calm-server",
        "--data-dir",
        tmp.path().to_str().unwrap(),
        "--codex-bin",
        fake_codex_bin.as_str(),
        "--shared-codex-appserver-restart-initial-delay-ms",
        "10",
        "--shared-codex-appserver-restart-max-delay-ms",
        "50",
    ]);
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed().unwrap();
    let pending = Arc::new(PendingThreadStartRegistry::new(repo.clone(), events));
    let shared = SharedCodexAppServer::new_with_pending(
        &cfg,
        Arc::new(home),
        repo.clone(),
        Some(pending.clone()),
    );
    if start_shared {
        shared.start_or_takeover().await.unwrap();
    }
    let state = state
        .with_shared_codex_appserver(shared)
        .with_pending_codex_threads(Some(pending));
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());
    Boot {
        app,
        state,
        repo,
        cove_id: cove.id.to_string(),
        _tmp: tmp,
    }
}

async fn post_wave(app: axum::Router, cove_id: &str, title: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/waves")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "cove_id": cove_id,
                        "title": title,
                        "cwd": "/tmp/spec-shared",
                        "attach_folder": true,
                        "theme": {"fg": [216,219,226], "bg": [15,20,24]}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn spec_card(repo: &SqlxRepo, wave_id: &str) -> calm_server::model::Card {
    repo.cards_by_wave(wave_id)
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "codex")
        .expect("spec card")
}

async fn wait_for_requests(path: &Path, min_count: usize) -> Vec<Value> {
    for _ in 0..50 {
        if let Ok(raw) = std::fs::read_to_string(path) {
            let rows = raw
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect::<Vec<Value>>();
            if rows.len() >= min_count {
                return rows;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for fake codex requests");
}

fn value_contains_text(value: &Value, needle: &str) -> bool {
    match value {
        Value::String(s) => s.contains(needle),
        Value::Array(items) => items.iter().any(|item| value_contains_text(item, needle)),
        Value::Object(map) => map.values().any(|item| value_contains_text(item, needle)),
        _ => false,
    }
}

fn reloaded_state_from_boot(boot: &Boot) -> AppState {
    let events = EventBus::new();
    let role_cache = CardRoleCache::new();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    AppState::from_parts(
        boot.repo.clone(),
        events.clone(),
        Arc::new(DaemonClient {
            data_dir: boot._tmp.path().join("terminals-reloaded"),
            proc_supervisor_sock: None,
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            boot.repo.clone(),
            PathBuf::new(),
            boot._tmp.path().join("plugins-data-reloaded"),
            Vec::new(),
            EventBus::new(),
            role_cache.clone(),
            wave_cove_cache.clone(),
        )),
        Arc::new(common::fake_codex_client()),
        Some(role_cache),
        Some(wave_cove_cache),
    )
    .with_shared_codex_spec_cards_enabled(true)
    .with_shared_codex_appserver(boot.state.shared_codex_appserver.clone())
}

#[test]
fn spec_card_shared_daemon_flag_defaults_to_false() {
    let tmp = TempDir::new().unwrap();
    let fake_codex_bin = common::fake_codex_bin();
    let cfg = Config::parse_from([
        "calm-server",
        "--data-dir",
        tmp.path().to_str().unwrap(),
        "--codex-bin",
        fake_codex_bin.as_str(),
    ]);
    assert!(!cfg.shared_codex_spec_cards_enabled);
}

#[tokio::test]
async fn non_empty_wave_routes_spec_card_to_shared_daemon() {
    let _guard = ENV_LOCK.lock().await;
    let capture = TempDir::new().unwrap();
    let capture_file = capture.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture_file);
    }
    let boot = boot(true, true).await;
    let (status, wave) = post_wave(boot.app.clone(), &boot.cove_id, "shared spec goal").await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
    }
    assert_eq!(status, StatusCode::CREATED, "body={wave:?}");

    let spec = spec_card(&boot.repo, wave["id"].as_str().unwrap()).await;
    assert_eq!(spec.payload["codex_source"], "shared");
    assert_eq!(spec.payload["codex_thread_id"], "fake-thread-0001");
    assert!(spec.payload["appserver_pgid"].is_null());
    let mapping = boot
        .repo
        .card_codex_thread_get_by_card(spec.id.as_str())
        .await
        .unwrap()
        .expect("mapping");
    assert_eq!(mapping.role, CardRole::Spec);
    assert_eq!(mapping.thread_id, "fake-thread-0001");
    assert!(
        boot.state
            .spec_push
            .contains(&wave["id"].as_str().unwrap().to_string().into())
    );

    let rows = wait_for_requests(&capture_file, 3).await;
    assert!(
        rows.iter()
            .any(|row| row.get("method").and_then(Value::as_str) == Some("thread/start")),
        "shared daemon should receive thread/start: {rows:?}"
    );
}

#[tokio::test]
async fn empty_wave_registers_pending_spec_thread_without_thread_id() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(true, true).await;
    let (status, wave) = post_wave(boot.app.clone(), &boot.cove_id, "").await;
    assert_eq!(status, StatusCode::CREATED, "body={wave:?}");
    let spec = spec_card(&boot.repo, wave["id"].as_str().unwrap()).await;
    assert_eq!(spec.payload["codex_source"], "shared");
    assert!(spec.payload.get("codex_thread_id").is_none());
    assert_eq!(spec.payload["appserver_needs_initial_prompt"], true);
    assert!(
        boot.repo
            .card_codex_thread_get_by_card(spec.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        boot.state
            .pending_codex_threads
            .as_ref()
            .unwrap()
            .pending_count()
            .await,
        1
    );
}

#[tokio::test]
async fn flag_on_but_shared_daemon_stopped_falls_back_to_legacy() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(true, false).await;
    let (status, wave) = post_wave(boot.app.clone(), &boot.cove_id, "legacy fallback").await;
    assert_eq!(status, StatusCode::CREATED, "body={wave:?}");
    let spec = spec_card(&boot.repo, wave["id"].as_str().unwrap()).await;
    assert_eq!(spec.payload["codex_source"], "legacy");
    assert!(spec.payload["appserver_pgid"].as_i64().is_some());
    let mapping = boot
        .repo
        .card_codex_thread_get_by_card(spec.id.as_str())
        .await
        .unwrap()
        .expect("legacy mapping");
    assert_eq!(mapping.role, CardRole::Spec);
}

#[tokio::test]
async fn shared_spec_takeover_reparks_handle_and_pushes_via_shared_daemon() {
    let _guard = ENV_LOCK.lock().await;
    let capture = TempDir::new().unwrap();
    let capture_file = capture.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture_file);
        std::env::set_var("FAKE_CODEX_TURN_COMPLETED_DELAY_MS", "25");
    }
    let boot = boot(true, true).await;
    let (status, wave) = post_wave(boot.app.clone(), &boot.cove_id, "shared takeover goal").await;
    assert_eq!(status, StatusCode::CREATED, "body={wave:?}");

    let wave_id = wave["id"].as_str().unwrap().to_string();
    let spec = spec_card(&boot.repo, &wave_id).await;
    let thread_id = spec.payload["codex_thread_id"]
        .as_str()
        .expect("shared thread id")
        .to_string();
    drop(boot.state.spec_push.remove(&wave_id.clone().into()));

    let reloaded = reloaded_state_from_boot(&boot);
    assert!(!reloaded.spec_push.contains(&wave_id.clone().into()));
    calm_server::takeover_shared_spec_cards_on_boot(&reloaded).await;
    assert!(reloaded.spec_push.contains(&wave_id.clone().into()));

    let pusher = reloaded
        .spec_push
        .pusher(&wave_id.clone().into())
        .expect("re-parked shared pusher");
    let outcome = pusher
        .push_observation(410, "after takeover observation")
        .await
        .expect("push via re-parked shared handle");
    assert_eq!(outcome, calm_server::spec_push::PushOutcome::Enqueued);

    reloaded
        .shared_codex_appserver
        .turn_start(
            &thread_id,
            vec![InputItem::text("takeover lifecycle nudge")],
        )
        .await
        .expect("lifecycle nudge turn/start");

    let rows = wait_for_requests(&capture_file, 5).await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
        std::env::remove_var("FAKE_CODEX_TURN_COMPLETED_DELAY_MS");
    }
    assert!(
        rows.iter().any(|row| {
            row.get("method").and_then(Value::as_str) == Some("turn/start")
                && value_contains_text(row, "after takeover observation")
        }),
        "re-parked shared pusher should issue turn/start through shared daemon: {rows:?}"
    );
}
