use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::config::Config;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_with_codex_create_tx};
use calm_server::event::Event;
use calm_server::event::EventBus;
use calm_server::ids::WaveId;
use calm_server::model::{Card, CardPatch, CardRole, NewCard, NewCove, NewWave, Terminal, new_id};
use calm_server::pending_codex_threads::PendingThreadStartRegistry;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::routes::theme::RequestTheme;
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
    wave_id: String,
    tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "spec-card-reset".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "reset route auth".into(),
            sort: None,
            cwd: "/tmp/spec-card-reset".into(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_card_role_cache(&card_role_cache).await.unwrap();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        events,
        Arc::new(DaemonClient {
            data_dir: tmp.path().join("terminals"),
            proc_supervisor_sock: None,
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-spec-card-reset"),
            Vec::new(),
            EventBus::new(),
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        )),
        Arc::new(common::fake_codex_client()),
        Some(card_role_cache),
        Some(wave_cove_cache),
    );
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());

    Boot {
        app,
        state,
        repo,
        wave_id: wave.id.to_string(),
        tmp,
    }
}

async fn boot_shared() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "spec-card-reset-shared".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "shared reset goal".into(),
            sort: None,
            cwd: "/tmp/spec-card-reset-shared".into(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_card_role_cache(&card_role_cache).await.unwrap();
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
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        )),
        Arc::new(common::fake_codex_client()),
        Some(card_role_cache),
        Some(wave_cove_cache),
    )
    .with_shared_codex_spec_cards_enabled(true);

    let cfg = Config::parse_from([
        "calm-server",
        "--data-dir",
        tmp.path().to_str().unwrap(),
        "--codex-bin",
        common::fake_codex_bin().as_str(),
        "--shared-codex-appserver-restart-initial-delay-ms",
        "10",
        "--shared-codex-appserver-restart-max-delay-ms",
        "50",
    ]);
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed_from(None).unwrap();
    let pending = Arc::new(PendingThreadStartRegistry::new(repo.clone(), events));
    let shared = SharedCodexAppServer::new_with_pending(
        &cfg,
        Arc::new(home),
        repo.clone(),
        Some(pending.clone()),
    );
    shared.start_or_takeover().await.unwrap();
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
        wave_id: wave.id.to_string(),
        tmp,
    }
}

async fn post_empty(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

fn stage_fake_codex_on_path(tmp: &TempDir) -> String {
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("create fake codex bin dir");
    let dest = bin_dir.join("codex");
    if dest.exists() {
        std::fs::remove_file(&dest).expect("remove old fake codex symlink");
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(common::fake_codex_bin(), &dest)
        .expect("symlink fake codex fixture");
    let prev = std::env::var("PATH").unwrap_or_default();
    format!("{}:{prev}", bin_dir.display())
}

fn spec_env(boot: &Boot, card_id: &str, complete_turns: bool) -> (Value, PathBuf) {
    let capture = boot.tmp.path().join(format!("{card_id}-requests.ndjson"));
    let result = boot.tmp.path().join(format!("{card_id}-osc.txt"));
    let mut env = serde_json::Map::new();
    env.insert(
        "CODEX_HOME".into(),
        json!(
            boot.tmp
                .path()
                .join("codex-home")
                .join(card_id)
                .to_string_lossy()
        ),
    );
    env.insert("NEIGE_CARD_ID".into(), json!(card_id));
    env.insert("NEIGE_CALM_BASE_URL".into(), json!("http://127.0.0.1:0"));
    env.insert(
        "NEIGE_OSC_RESULT_PATH".into(),
        json!(result.to_string_lossy()),
    );
    env.insert("NEIGE_OSC_EXPECTED_BG".into(), json!("15,20,24"));
    env.insert(
        "FAKE_CODEX_CAPTURE_REQUESTS".into(),
        json!(capture.to_string_lossy()),
    );
    env.insert("PATH".into(), json!(stage_fake_codex_on_path(&boot.tmp)));
    if complete_turns {
        env.insert("FAKE_CODEX_TURN_COMPLETED_DELAY_MS".into(), json!("25"));
    }
    (Value::Object(env), capture)
}

async fn seed_spec_card(
    boot: &Boot,
    watermark: i64,
    complete_turns: bool,
) -> (Card, Terminal, PathBuf) {
    let card_id = new_id();
    let (env, capture) = spec_env(boot, &card_id, complete_turns);
    let mut tx = boot.repo.pool().begin().await.expect("begin tx");
    let (card, terminal, _token) = card_with_codex_create_tx(
        &mut tx,
        card_id.clone(),
        WaveId::from(boot.wave_id.clone()),
        None,
        "/tmp/spec-card-reset".into(),
        env,
        None,
        None,
        None,
        CardRole::Spec,
        false,
        &boot.state.card_role_cache,
        RequestTheme::default_dark(),
    )
    .await
    .expect("create spec card + terminal");
    tx.commit().await.expect("commit spec card");
    boot.repo
        .spec_card_set_appserver_after_reset(
            card.id.as_str(),
            "thread-old",
            12345,
            "/tmp/old-appserver.sock",
            Some(67890),
            Some("boot-old"),
            false,
        )
        .await
        .expect("seed old runtime fields");
    boot.repo
        .spec_card_set_push_watermark(card.id.as_str(), watermark)
        .await
        .expect("seed old watermark");
    (card, terminal, capture)
}

async fn seed_shared_spec_card(boot: &Boot, watermark: i64) -> (Card, Terminal, PathBuf) {
    let (card, terminal, capture) = seed_spec_card(boot, watermark, false).await;
    let mut payload = boot
        .repo
        .card_get(card.id.as_str())
        .await
        .unwrap()
        .unwrap()
        .payload;
    payload["codex_source"] = json!("shared");
    payload["codex_thread_id"] = json!("thread-old");
    payload["appserver_sock"] = json!(boot.state.shared_codex_appserver.remote_uri());
    payload["appserver_pgid"] = Value::Null;
    payload["appserver_start_time"] = Value::Null;
    payload["appserver_boot_id"] = Value::Null;
    payload
        .as_object_mut()
        .unwrap()
        .remove("appserver_needs_initial_prompt");
    boot.repo
        .card_update(
            card.id.as_str(),
            CardPatch {
                kind: None,
                sort: None,
                payload: Some(payload),
                deletable: None,
            },
        )
        .await
        .expect("mark shared spec card");
    boot.repo
        .card_codex_thread_upsert(
            card.id.as_str(),
            "thread-old",
            CardRole::Spec,
            Some(boot.wave_id.as_str()),
        )
        .await
        .expect("seed old shared thread mapping");
    (
        boot.repo.card_get(card.id.as_str()).await.unwrap().unwrap(),
        terminal,
        capture,
    )
}

async fn request_lines(path: &PathBuf) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(raw) = std::fs::read_to_string(path) {
            let rows = raw
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .collect::<Vec<_>>();
            if !rows.is_empty() {
                return rows;
            }
        }
        if Instant::now() >= deadline {
            return Vec::new();
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn turn_start_contains(rows: &[Value], needle: &str) -> bool {
    rows.iter().any(|row| {
        row.get("method").and_then(Value::as_str) == Some("turn/start")
            && row.to_string().contains(needle)
    })
}

#[tokio::test]
async fn reset_spec_card_returns_404_for_unknown_card() {
    let boot = boot().await;

    let (status, body) = post_empty(boot.app, "/api/cards/card_does_not_exist/spec/reset").await;

    assert_eq!(status, StatusCode::NOT_FOUND, "body={body}");
}

#[tokio::test]
async fn reset_spec_card_rejects_plain_codex_card() {
    let boot = boot().await;
    let card = boot
        .repo
        .card_create(NewCard {
            wave_id: boot.wave_id.clone().into(),
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .expect("plain codex card");
    boot.state.card_role_cache.insert(
        card.id.clone(),
        CardRole::Plain,
        WaveId::from(boot.wave_id.clone()),
    );

    let (status, body) = post_empty(boot.app, &format!("/api/cards/{}/spec/reset", card.id)).await;

    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
}

#[tokio::test]
async fn reset_spec_card_rejects_wrong_kind_card() {
    let boot = boot().await;
    let card = boot
        .repo
        .card_create(NewCard {
            wave_id: boot.wave_id.clone().into(),
            kind: "report".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .expect("report card");
    boot.state.card_role_cache.insert(
        card.id.clone(),
        CardRole::Plain,
        WaveId::from(boot.wave_id.clone()),
    );

    let (status, body) = post_empty(boot.app, &format!("/api/cards/{}/spec/reset", card.id)).await;

    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
}

#[tokio::test]
async fn shared_reset_swaps_thread_mapping_and_persists_new_thread_id() {
    let _guard = ENV_LOCK.lock().await;
    let capture = TempDir::new().unwrap();
    let capture_file = capture.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture_file);
    }
    let boot = boot_shared().await;
    let (card, terminal, _capture) = seed_shared_spec_card(&boot, 42).await;

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/spec/reset", card.id),
    )
    .await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
    }

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["card_id"], json!(card.id.as_str()));
    assert_eq!(body["terminal_id"], json!(terminal.id.as_str()));
    assert_eq!(body["new_thread_id"], json!("fake-thread-0001"));

    let mapping = boot
        .repo
        .card_codex_thread_get_by_card(card.id.as_str())
        .await
        .unwrap()
        .expect("new shared mapping");
    assert_eq!(mapping.thread_id, "fake-thread-0001");
    assert_eq!(mapping.role, CardRole::Spec);
    let got = boot.repo.card_get(card.id.as_str()).await.unwrap().unwrap();
    assert_eq!(got.payload["codex_source"], json!("shared"));
    assert_eq!(got.payload["codex_thread_id"], json!("fake-thread-0001"));
    assert_eq!(got.payload["push_watermark"], json!(42));
    assert!(got.payload["appserver_pgid"].is_null());

    let entry = boot
        .state
        .terminal_renderer
        .get(terminal.id.as_str())
        .expect("respawned terminal renderer");
    let shell_line = &entry.config().args[1];
    assert!(
        shell_line.contains("codex resume 'fake-thread-0001' --remote 'unix://"),
        "shared reset TUI should resume the new shared thread: {shell_line}",
    );

    calm_server::terminal_sweeper::reap_spec_push(&boot.state, &WaveId::from(boot.wave_id.clone()))
        .await;
}

#[tokio::test]
async fn shared_reset_replaces_spec_push_handle_with_new_thread_id_consumer() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_shared().await;
    let (card, _terminal, _capture) = seed_shared_spec_card(&boot, 0).await;

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/spec/reset", card.id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    let handle = boot
        .state
        .spec_push
        .remove(&WaveId::from(boot.wave_id.clone()))
        .expect("new parked shared handle");
    assert!(handle.is_shared());
    assert_eq!(handle.thread_id.as_deref(), Some("fake-thread-0001"));
}

#[tokio::test]
async fn shared_reset_with_goal_sends_title_via_turn_start() {
    let _guard = ENV_LOCK.lock().await;
    let capture = TempDir::new().unwrap();
    let capture_file = capture.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture_file);
    }
    let boot = boot_shared().await;
    let (card, _terminal, _capture) = seed_shared_spec_card(&boot, 0).await;

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/spec/reset", card.id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let rows = request_lines(&capture_file).await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
    }
    assert!(
        turn_start_contains(&rows, "shared reset goal"),
        "shared reset must send the wave title via turn/start: {rows:?}",
    );

    calm_server::terminal_sweeper::reap_spec_push(&boot.state, &WaveId::from(boot.wave_id.clone()))
        .await;
}

#[tokio::test]
async fn shared_reset_preserves_push_watermark() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_shared().await;
    let (card, _terminal, _capture) = seed_shared_spec_card(&boot, 88).await;

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/spec/reset", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    let got = boot.repo.card_get(card.id.as_str()).await.unwrap().unwrap();
    assert_eq!(got.payload["push_watermark"], json!(88));

    calm_server::terminal_sweeper::reap_spec_push(&boot.state, &WaveId::from(boot.wave_id.clone()))
        .await;
}

#[tokio::test]
async fn shared_reset_turn_start_failure_does_not_swap_mapping() {
    let _guard = ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var("FAKE_CODEX_FAIL_TURN_START", "1");
    }
    let boot = boot_shared().await;
    let (card, _terminal, _capture) = seed_shared_spec_card(&boot, 17).await;

    let (status, _body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/spec/reset", card.id),
    )
    .await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_FAIL_TURN_START");
    }

    assert!(status.is_server_error(), "expected 5xx, got {status}");
    let mapping = boot
        .repo
        .card_codex_thread_get_by_card(card.id.as_str())
        .await
        .unwrap()
        .expect("old mapping restored");
    assert_eq!(mapping.thread_id, "thread-old");
    let got = boot.repo.card_get(card.id.as_str()).await.unwrap().unwrap();
    assert_eq!(got.payload["codex_thread_id"], json!("thread-old"));
    assert_eq!(got.payload["push_watermark"], json!(17));
}

#[tokio::test]
async fn reset_spec_card_happy_path_preserves_identity_and_emits_card_updated_only() {
    let boot = boot().await;
    let (card, terminal, _capture) = seed_spec_card(&boot, 42, false).await;
    let queue_id = boot
        .repo
        .spec_card_enqueue_observation(card.id.as_str(), 43, "pending while wedged")
        .await
        .expect("seed queued observation");

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/spec/reset", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["card_id"], json!(card.id.as_str()));
    assert_eq!(body["terminal_id"], json!(terminal.id.as_str()));
    assert_ne!(body["new_thread_id"], json!("thread-old"));

    let got = boot
        .repo
        .card_get(card.id.as_str())
        .await
        .unwrap()
        .expect("card after reset");
    assert_eq!(got.id, card.id, "card id must not change");
    assert_eq!(got.payload["terminal_id"], json!(terminal.id.as_str()));
    assert_eq!(got.payload["push_watermark"], json!(42));
    assert_ne!(got.payload["codex_thread_id"], json!("thread-old"));

    let term_after = boot
        .repo
        .terminal_get_by_card(card.id.as_str())
        .await
        .unwrap()
        .expect("terminal survives reset");
    assert_eq!(
        term_after.id, terminal.id,
        "terminal row must not be replaced"
    );

    let queued = boot
        .repo
        .spec_card_queued_observations(card.id.as_str())
        .await
        .expect("queued observations after reset");
    assert_eq!(
        queued,
        vec![(queue_id, 43, "pending while wedged".into())],
        "reset must not delete durable queue rows as part of runtime replacement",
    );

    assert!(
        boot.state
            .spec_push
            .contains(&WaveId::from(boot.wave_id.clone())),
        "new app-server handle should be parked after successful reset"
    );

    let events = boot.repo.events_since(0, None).await.expect("events");
    let card_updated = events
        .iter()
        .filter(|(_, _, _, ev)| matches!(ev, Event::CardUpdated(c) if c.id == card.id))
        .count();
    let card_added = events
        .iter()
        .filter(|(_, _, _, ev)| matches!(ev, Event::CardAdded(c) if c.id == card.id))
        .count();
    let wave_updated = events
        .iter()
        .filter(
            |(_, _, _, ev)| matches!(ev, Event::WaveUpdated(w) if w.id.as_str() == boot.wave_id),
        )
        .count();
    assert_eq!(card_updated, 1, "reset should emit exactly one CardUpdated");
    assert_eq!(card_added, 0, "reset must not emit CardAdded");
    assert_eq!(wave_updated, 0, "reset must not emit WaveUpdated");

    calm_server::terminal_sweeper::reap_spec_push(&boot.state, &WaveId::from(boot.wave_id.clone()))
        .await;
}

#[tokio::test]
async fn reset_spec_card_rehydrates_queue_and_flushes_pending_observation() {
    let boot = boot().await;
    let (card, _terminal, capture) = seed_spec_card(&boot, 0, true).await;
    boot.repo
        .spec_card_enqueue_observation(card.id.as_str(), 77, "queued reset observation")
        .await
        .expect("seed queued observation");

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/spec/reset", card.id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let rows = request_lines(&capture).await;
        if turn_start_contains(&rows, "queued reset observation") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "new reset-created thread never received the rehydrated queued observation; rows={rows:?}",
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    calm_server::terminal_sweeper::reap_spec_push(&boot.state, &WaveId::from(boot.wave_id.clone()))
        .await;
}

#[tokio::test]
async fn reset_spec_card_rereads_watermark_after_push_lock_entry() {
    let boot = boot().await;
    let (card, _terminal, _capture) = seed_spec_card(&boot, 5, false).await;
    let wave_id = WaveId::from(boot.wave_id.clone());
    let card_id = card.id.clone();

    let (held_tx, held_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let dispatcher = boot.state.dispatcher.clone();
    let wave_for_lock = wave_id.clone();
    let lock_task = tokio::spawn(async move {
        dispatcher
            .with_push_lock(&wave_for_lock, async move {
                let _ = held_tx.send(());
                let _ = release_rx.await;
            })
            .await;
    });
    held_rx.await.expect("lock held");

    let app = boot.app.clone();
    let reset_card_id = card_id.clone();
    let reset_task = tokio::spawn(async move {
        post_empty(app, &format!("/api/cards/{reset_card_id}/spec/reset")).await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    boot.repo
        .spec_card_set_push_watermark(card.id.as_str(), 88)
        .await
        .expect("advance durable watermark while reset waits on lock");
    release_tx.send(()).expect("release lock");
    lock_task.await.expect("lock task joins");
    let (_status, _body) = reset_task.await.expect("reset joins");

    assert_eq!(
        boot.state.dispatcher.push_cursor_for_test(&card.id),
        88,
        "reset must seed the in-memory cursor from the watermark read inside the push lock",
    );

    calm_server::terminal_sweeper::reap_spec_push(&boot.state, &wave_id).await;
}

#[tokio::test]
async fn reset_spec_card_appserver_spawn_failure_clears_stale_terminal_exit_and_pid() {
    let boot = boot().await;
    let (card, terminal, _capture) = seed_spec_card(&boot, 9, false).await;
    boot.repo
        .terminal_set_pid(terminal.id.as_str(), Some(4242))
        .await
        .expect("seed terminal pid");
    boot.repo
        .terminal_set_exit(terminal.id.as_str(), Some(7), false)
        .await
        .expect("seed terminal exit");
    let mut codex = common::fake_codex_client();
    codex.codex_bin = "/definitely/not/a/codex".into();
    let failed_state = AppState::from_parts(
        boot.repo.clone(),
        EventBus::new(),
        boot.state.daemon.clone(),
        boot.state.plugin.clone(),
        Arc::new(codex),
        Some(boot.state.card_role_cache.clone()),
        Some(boot.state.wave_cove_cache.clone()),
    );
    let failed_app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(failed_state);

    let (status, _body) =
        post_empty(failed_app, &format!("/api/cards/{}/spec/reset", card.id)).await;
    assert!(status.is_server_error(), "expected 5xx, got {status}");

    let term_after = boot
        .repo
        .terminal_get_by_card(card.id.as_str())
        .await
        .unwrap()
        .expect("terminal row survives failed reset");
    assert_eq!(term_after.id, terminal.id);
    assert_eq!(term_after.pid, None, "stale pid must be cleared");
    assert_eq!(
        term_after.exit_code, None,
        "stale exit code must be cleared"
    );
    assert!(
        !term_after.signal_killed,
        "stale signal flag must be cleared"
    );

    let got = boot
        .repo
        .card_get(card.id.as_str())
        .await
        .unwrap()
        .expect("card survives failed reset");
    assert_eq!(got.payload["codex_thread_id"], json!("thread-old"));
    assert_eq!(got.payload["push_watermark"], json!(9));
}

#[tokio::test]
async fn reset_spec_card_terminal_spawn_failure_reaps_new_appserver_and_clears_runtime_fields() {
    let boot = boot().await;
    let (card, _terminal, _capture) = seed_spec_card(&boot, 11, false).await;
    let failed_daemon = Arc::new(DaemonClient {
        data_dir: boot.state.daemon.data_dir.clone(),
        proc_supervisor_sock: Some(boot.tmp.path().join("missing-proc-supervisor.sock")),
    });
    let failed_state = AppState::from_parts(
        boot.repo.clone(),
        EventBus::new(),
        failed_daemon,
        boot.state.plugin.clone(),
        boot.state.codex.clone(),
        Some(boot.state.card_role_cache.clone()),
        Some(boot.state.wave_cove_cache.clone()),
    );
    let failed_app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(failed_state.clone());

    let (status, _body) =
        post_empty(failed_app, &format!("/api/cards/{}/spec/reset", card.id)).await;
    assert!(status.is_server_error(), "expected 5xx, got {status}");

    let wave_id = WaveId::from(boot.wave_id.clone());
    assert!(
        !failed_state.spec_push.contains(&wave_id),
        "terminal spawn failure must explicitly reap the reset-created app-server handle"
    );
    let got = boot
        .repo
        .card_get(card.id.as_str())
        .await
        .unwrap()
        .expect("card survives failed reset");
    assert!(
        got.payload.get("codex_thread_id").is_none(),
        "partial reset failure should clear the new dead thread id"
    );
    assert_eq!(got.payload["push_watermark"], json!(11));
}
