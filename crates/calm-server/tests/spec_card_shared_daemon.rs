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
use calm_server::model::{CardRole, NewCard, NewCove, NewWave};
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

async fn boot(start_shared: bool) -> Boot {
    boot_with_proc_supervisor(start_shared, None).await
}

async fn boot_with_proc_supervisor(
    start_shared: bool,
    proc_supervisor_sock: Option<PathBuf>,
) -> Boot {
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
            proc_supervisor_sock,
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            tmp.path().join("plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(role_cache.clone(), wave_cove_cache.clone()),
        )),
        Arc::new(common::fake_codex_client()),
        Some(role_cache),
        Some(wave_cove_cache),
    );

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
    // Use seed_from(None) to skip reading host ~/.codex, which can be
    // concurrently mutated by other tests / the dev's live codex session
    // and trigger ENOENT in fs::read_dir mid-copy. The fake codex bin
    // doesn't need a populated CODEX_HOME — only the dir itself.
    home.seed_from(None).unwrap();
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
        .with_pending_codex_threads(pending);
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

async fn runtime_status_for_card(repo: &SqlxRepo, card_id: &str) -> String {
    sqlx::query_scalar(
        "SELECT status FROM runtimes WHERE card_id = ?1 ORDER BY updated_at_ms DESC LIMIT 1",
    )
    .bind(card_id)
    .fetch_one(repo.pool())
    .await
    .unwrap()
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
            calm_server::state::WriteContext::new(role_cache.clone(), wave_cove_cache.clone()),
        )),
        Arc::new(common::fake_codex_client()),
        Some(role_cache),
        Some(wave_cove_cache),
    )
    .with_shared_codex_appserver(boot.state.shared_codex_appserver.clone())
    .with_pending_codex_threads(boot.state.pending_codex_threads.clone())
}

#[tokio::test]
async fn non_empty_wave_routes_spec_card_to_shared_daemon() {
    let _guard = ENV_LOCK.lock().await;
    let capture = TempDir::new().unwrap();
    let capture_file = capture.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture_file);
    }
    let boot = boot(true).await;
    let (status, wave) = post_wave(boot.app.clone(), &boot.cove_id, "shared spec goal").await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
    }
    assert_eq!(status, StatusCode::CREATED, "body={wave:?}");

    let spec = spec_card(&boot.repo, wave["id"].as_str().unwrap()).await;
    assert_eq!(spec.payload["codex_source"], "shared");
    assert_eq!(spec.payload["codex_thread_id"], "fake-thread-0001");
    assert!(spec.payload.get("appserver_pgid").is_none());
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
async fn non_empty_shared_spec_turn_start_failure_marks_runtime_failed_and_rolls_back() {
    let _guard = ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var("FAKE_CODEX_FAIL_TURN_START", "1");
    }
    let boot = boot(true).await;
    let (status, wave) = post_wave(boot.app.clone(), &boot.cove_id, "turn start fails").await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_FAIL_TURN_START");
    }
    assert_eq!(status, StatusCode::CREATED, "body={wave:?}");

    let wave_id = wave["id"].as_str().unwrap().to_string();
    let spec = spec_card(&boot.repo, &wave_id).await;
    assert!(spec.payload.get("codex_source").is_none());
    assert!(spec.payload.get("codex_thread_id").is_none());
    assert!(
        boot.repo
            .card_codex_thread_get_by_card(spec.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        runtime_status_for_card(&boot.repo, spec.id.as_str()).await,
        "failed"
    );
    assert!(!boot.state.spec_push.contains(&wave_id.into()));
}

#[tokio::test]
async fn empty_wave_registers_pending_spec_thread_without_thread_id() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(true).await;
    let (status, wave) = post_wave(boot.app.clone(), &boot.cove_id, "").await;
    assert_eq!(status, StatusCode::CREATED, "body={wave:?}");
    let wave_id = wave["id"].as_str().unwrap().to_string();
    let spec = spec_card(&boot.repo, wave["id"].as_str().unwrap()).await;
    assert_eq!(spec.payload["codex_source"], "shared");
    assert!(spec.payload.get("codex_thread_id").is_none());
    assert!(spec.payload.get("appserver_needs_initial_prompt").is_none());
    assert!(
        boot.repo
            .card_codex_thread_get_by_card(spec.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(boot.state.pending_codex_threads.pending_count().await, 1);
    let terminal_id = spec.payload["terminal_id"].as_str().unwrap();
    let entry = boot
        .state
        .terminal_renderer
        .get(terminal_id)
        .expect("renderer entry");
    let shell_line = &entry.config().args[1];
    assert!(
        shell_line.contains("codex -c 'developer_instructions=\""),
        "shared empty spec TUI must pass spec developer_instructions: {shell_line}"
    );
    assert!(
        shell_line.contains(&format!("You are the spec agent for wave `{wave_id}`.")),
        "developer_instructions must be rendered for this wave: {shell_line}"
    );
    assert!(
        shell_line.contains("calm.update_wave_state"),
        "developer_instructions must include the spec MCP contract: {shell_line}"
    );
    assert!(
        shell_line.contains("--remote 'unix://"),
        "shared empty spec TUI must still fresh-start over remote app-server: {shell_line}"
    );
}

#[tokio::test]
async fn empty_shared_spec_respawns_daemon_when_proxy_changed() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(true).await;
    let old_pid = boot
        .state
        .shared_codex_appserver
        .status_snapshot()
        .runtime
        .expect("shared daemon runtime")
        .pid;
    boot.repo
        .settings_upsert("http_proxy", "http://proxy-after-empty-spec.local:3128")
        .await
        .unwrap();
    boot.state.shared_codex_appserver.mark_needs_respawn();

    let (status, wave) = post_wave(boot.app.clone(), &boot.cove_id, "").await;
    assert_eq!(status, StatusCode::CREATED, "body={wave:?}");

    let snapshot = boot.state.shared_codex_appserver.status_snapshot();
    assert_eq!(snapshot.restart_count, 1);
    assert_ne!(
        snapshot.runtime.as_ref().map(|runtime| runtime.pid),
        Some(old_pid),
        "empty spec path must respawn before the TUI uses the shared remote"
    );
    assert!(
        !boot
            .state
            .shared_codex_appserver
            .needs_respawn_on_next_thread_start_for_test()
    );
    assert_eq!(boot.state.pending_codex_threads.pending_count().await, 1);
}

#[tokio::test]
async fn empty_shared_spec_respawn_failure_does_not_leave_card_stamped() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(true).await;
    unsafe {
        std::env::set_var("FAKE_CODEX_FAIL_INITIALIZE", "1");
    }
    boot.state.shared_codex_appserver.mark_needs_respawn();

    let (status, wave) = post_wave(boot.app.clone(), &boot.cove_id, "").await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_FAIL_INITIALIZE");
    }

    assert_eq!(status, StatusCode::CREATED, "body={wave:?}");
    let wave_id = wave["id"].as_str().unwrap().to_string();
    let spec = spec_card(&boot.repo, &wave_id).await;
    assert!(spec.payload.get("codex_source").is_none());
    assert!(spec.payload.get("appserver_needs_initial_prompt").is_none());
    assert_eq!(
        runtime_status_for_card(&boot.repo, spec.id.as_str()).await,
        "failed"
    );
    assert_eq!(boot.state.pending_codex_threads.pending_count().await, 0);
    assert!(!boot.state.spec_push.contains(&wave_id.clone().into()));
}

#[tokio::test]
async fn empty_shared_spec_pending_register_waits_for_spawn_serial_lock() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(true).await;
    let serial_guard = boot.state.pending_codex_threads_spawn_serial.lock().await;
    let pending_post = post_wave(boot.app.clone(), &boot.cove_id, "");
    tokio::pin!(pending_post);

    tokio::select! {
        biased;
        _ = &mut pending_post => panic!("shared empty spec create completed while spawn-serial lock was held"),
        _ = tokio::time::sleep(Duration::from_millis(50)) => {}
    }

    assert_eq!(
        boot.state.pending_codex_threads.pending_count().await,
        0,
        "pending spec registration must be inside the spawn-serial critical section"
    );

    drop(serial_guard);
    let (status, wave) = pending_post.await;
    assert_eq!(status, StatusCode::CREATED, "body={wave:?}");
    assert_eq!(boot.state.pending_codex_threads.pending_count().await, 1);
}

#[tokio::test]
async fn empty_shared_spec_boot_takeover_reparks_pending_without_legacy_bootstrap() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(true).await;
    let (status, wave) = post_wave(boot.app.clone(), &boot.cove_id, "").await;
    assert_eq!(status, StatusCode::CREATED, "body={wave:?}");
    let wave_id = wave["id"].as_str().unwrap().to_string();
    let spec = spec_card(&boot.repo, &wave_id).await;
    // Force the terminal row "alive" — under CI's daemon spawn pipeline the
    // PTY TUI exits quickly and reconcile/sweeper races mark the terminal
    // dead before the test asserts. The takeover SQL (intentionally) skips
    // dead terminals; this test exercises the *alive-terminal* re-park
    // path, so reset exit_code+signal_killed here. terminal_set_exit with
    // (None, false) writes UPDATE terminals SET exit_code=NULL,
    // signal_killed=0 — effectively "resurrecting" the row for the test.
    let terminal_id = spec.payload["terminal_id"].as_str().unwrap();
    boot.repo
        .terminal_set_exit(terminal_id, None, false)
        .await
        .unwrap();
    let pending = &boot.state.pending_codex_threads;
    assert!(pending.remove_by_card(spec.id.as_str()).await);
    drop(boot.state.spec_push.remove(&wave_id.clone().into()));

    let reloaded = reloaded_state_from_boot(&boot);
    assert_eq!(reloaded.pending_codex_threads.pending_count().await, 0);
    calm_server::takeover_shared_spec_cards_on_boot(&reloaded).await;
    assert!(reloaded.spec_push.contains(&wave_id.clone().into()));
    assert_eq!(reloaded.pending_codex_threads.pending_count().await, 1);

    assert_eq!(
        reloaded
            .pending_codex_threads
            .on_thread_started("T-spec-reloaded")
            .await
            .unwrap()
            .as_deref(),
        Some(spec.id.as_str())
    );
    assert_eq!(
        boot.repo
            .card_codex_thread_get_by_card(spec.id.as_str())
            .await
            .unwrap()
            .unwrap()
            .thread_id,
        "T-spec-reloaded"
    );
}

#[tokio::test]
async fn empty_shared_spec_tui_spawn_failure_rolls_back_pending_state() {
    let _guard = ENV_LOCK.lock().await;
    let bad_supervisor_sock = PathBuf::from(format!(
        "/tmp/neige-calm-missing-supervisor-{}.sock",
        calm_server::model::new_id()
    ));
    let boot = boot_with_proc_supervisor(true, Some(bad_supervisor_sock)).await;
    let (status, wave) = post_wave(boot.app.clone(), &boot.cove_id, "").await;
    assert_eq!(status, StatusCode::CREATED, "body={wave:?}");
    let wave_id = wave["id"].as_str().unwrap().to_string();
    let spec = spec_card(&boot.repo, &wave_id).await;

    assert_eq!(boot.state.pending_codex_threads.pending_count().await, 0);
    assert!(!boot.state.spec_push.contains(&wave_id.clone().into()));
    assert!(
        boot.state
            .pending_codex_threads
            .on_thread_started("T-orphan")
            .await
            .unwrap()
            .is_none(),
        "rolled-back shared spec must not consume later thread/started events"
    );
    let spec = boot.repo.card_get(spec.id.as_str()).await.unwrap().unwrap();
    assert!(spec.payload.get("appserver_needs_initial_prompt").is_none());
}

#[tokio::test]
async fn empty_shared_spec_persist_failure_rolls_back_pending_entry() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(true).await;
    let wave = boot
        .repo
        .wave_create(NewWave {
            cove_id: boot.cove_id.clone().into(),
            title: "".into(),
            sort: None,
            cwd: "/tmp/spec-shared".into(),
            attach_folder: true,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let spec = boot
        .repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!("not-an-object"),
        })
        .await
        .unwrap();
    let theme = calm_server::routes::theme::RequestTheme::default_dark();
    let terminal_id = format!("term-{}", calm_server::model::new_id());
    sqlx::query(
        r#"INSERT INTO terminals
               (id, card_id, program, cwd, env, pid, theme_fg, theme_bg, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8)"#,
    )
    .bind(&terminal_id)
    .bind(spec.id.as_str())
    .bind("bash")
    .bind("/tmp/spec-shared")
    .bind("{}")
    .bind(theme.fg_arg())
    .bind(theme.bg_arg())
    .bind(0_i64)
    .execute(boot.repo.pool())
    .await
    .unwrap();

    let result = calm_server::routes::waves::spawn_push_via_shared_daemon_for_test(
        &boot.state,
        spec.id.as_str(),
        &wave,
    )
    .await;

    assert!(result.is_err());
    let pending = &boot.state.pending_codex_threads;
    assert_eq!(pending.pending_count().await, 0);
    assert!(
        pending
            .on_thread_started("T-unrelated")
            .await
            .unwrap()
            .is_none(),
        "failed persist must not consume a later thread/started"
    );
    assert!(
        boot.repo
            .card_codex_thread_get_by_card(spec.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    let terminal = boot
        .repo
        .terminal_get(&terminal_id)
        .await
        .unwrap()
        .expect("terminal row remains");
    assert!(terminal.exit_code.is_none());
    assert!(!terminal.signal_killed);
}

#[tokio::test]
async fn shared_daemon_stopped_leaves_inert_spec_card() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(false).await;
    let (status, wave) = post_wave(boot.app.clone(), &boot.cove_id, "shared daemon stopped").await;
    assert_eq!(status, StatusCode::CREATED, "body={wave:?}");
    let spec = spec_card(&boot.repo, wave["id"].as_str().unwrap()).await;
    assert!(spec.payload.get("codex_source").is_none());
    assert!(spec.payload.get("appserver_pgid").is_none());
    assert!(
        boot.repo
            .card_codex_thread_get_by_card(spec.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
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
    let boot = boot(true).await;
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
