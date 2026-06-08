use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::config::Config;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_with_codex_create_tx, runtime_start_tx};
use calm_server::event::EventBus;
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, Observation, SpecHarness, SpecHarnessParams,
};
use calm_server::ids::WaveId;
use calm_server::model::{
    Card, CardPatch, CardRole, NewCard, NewCove, NewWave, Terminal, new_id, now_ms,
};
use calm_server::pending_codex_threads::PendingThreadStartRegistry;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::routes::theme::RequestTheme;
use calm_server::runtime_lookup::project_runtime_into_card_payload;
use calm_server::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind};
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
            calm_server::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
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
            calm_server::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
        )),
        Arc::new(common::fake_codex_client()),
        Some(card_role_cache),
        Some(wave_cove_cache),
    );

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

async fn post_json(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
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
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn delete_empty(app: axum::Router, uri: &str) -> StatusCode {
    app.oneshot(
        Request::builder()
            .method("DELETE")
            .uri(uri)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
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
        &new_id(),
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
    payload["appserver_pgid"] = json!(12345);
    payload["appserver_start_time"] = json!(67890);
    payload["appserver_boot_id"] = json!("boot-old");
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

async fn seed_shared_plain_card(boot: &Boot, label: &str, thread_id: &str) -> Card {
    let card = boot
        .repo
        .card_create(NewCard {
            wave_id: WaveId::from(boot.wave_id.clone()),
            kind: "plugin:test:plain".into(),
            sort: None,
            payload: json!({
                "label": label,
                "codex_source": "shared",
                "codex_thread_id": thread_id,
            }),
        })
        .await
        .expect("seed shared plain card");
    boot.repo
        .card_codex_thread_upsert(
            card.id.as_str(),
            thread_id,
            CardRole::Plain,
            Some(boot.wave_id.as_str()),
        )
        .await
        .expect("seed shared plain mapping");
    card
}

async fn request_lines_containing(path: &PathBuf, method: &str, count: usize) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(raw) = std::fs::read_to_string(path) {
            let matching = raw
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter(|row| row.get("method").and_then(Value::as_str) == Some(method))
                .collect::<Vec<_>>();
            if matching.len() >= count || Instant::now() >= deadline {
                return matching;
            }
        }
        if Instant::now() >= deadline {
            return Vec::new();
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_harness_watermark(harness: &SpecHarness, watermark: i64) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let snapshot = harness.snapshot().await;
        if snapshot.push_watermark == watermark {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for harness watermark {watermark}; got {}",
            snapshot.push_watermark
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn has_interrupt(rows: &[Value], thread_id: &str, turn_id: &str) -> bool {
    rows.iter().any(|row| {
        row.get("method").and_then(Value::as_str) == Some("turn/interrupt")
            && row.pointer("/params/threadId").and_then(Value::as_str) == Some(thread_id)
            && row.pointer("/params/turnId").and_then(Value::as_str) == Some(turn_id)
    })
}

#[tokio::test]
async fn shared_card_delete_interrupts_active_turn() {
    let _guard = ENV_LOCK.lock().await;
    let capture = TempDir::new().unwrap();
    let capture_file = capture.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture_file);
    }
    let boot = boot_shared().await;
    let card = seed_shared_plain_card(&boot, "delete", "thread-delete").await;
    boot.state
        .shared_codex_appserver
        .set_active_turn_for_test("thread-delete", "turn-delete");

    let status = delete_empty(boot.app.clone(), &format!("/api/cards/{}", card.id)).await;
    let rows = request_lines_containing(&capture_file, "turn/interrupt", 1).await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
    }

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        has_interrupt(&rows, "thread-delete", "turn-delete"),
        "card delete must interrupt active shared turn: {rows:?}"
    );
}

#[tokio::test]
async fn shared_wave_delete_interrupts_all_child_turns() {
    let _guard = ENV_LOCK.lock().await;
    let capture = TempDir::new().unwrap();
    let capture_file = capture.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture_file);
    }
    let boot = boot_shared().await;
    let card_a = seed_shared_plain_card(&boot, "wave-a", "thread-wave-a").await;
    let card_b = seed_shared_plain_card(&boot, "wave-b", "thread-wave-b").await;
    boot.state
        .shared_codex_appserver
        .set_active_turn_for_test("thread-wave-a", "turn-wave-a");
    boot.state
        .shared_codex_appserver
        .set_active_turn_for_test("thread-wave-b", "turn-wave-b");

    let status = delete_empty(boot.app.clone(), &format!("/api/waves/{}", boot.wave_id)).await;
    let rows = request_lines_containing(&capture_file, "turn/interrupt", 2).await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
    }

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        has_interrupt(&rows, "thread-wave-a", "turn-wave-a"),
        "wave delete must interrupt first active shared turn: {rows:?}"
    );
    assert!(
        has_interrupt(&rows, "thread-wave-b", "turn-wave-b"),
        "wave delete must interrupt second active shared turn: {rows:?}"
    );
    assert!(
        boot.repo
            .card_get(card_a.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        boot.repo
            .card_get(card_b.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn wave_delete_shuts_down_active_spec_harness() {
    let boot = boot_shared().await;
    let cove = boot
        .repo
        .cove_create(NewCove {
            name: "harness-wave-delete".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let (status, body) = post_json(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": cove.id,
            "title": "delete harness",
            "cwd": "/tmp/spec-card-reset-harness-delete",
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    let wave_id = body["id"].as_str().expect("wave id").to_string();
    let cards = boot.repo.cards_by_wave(&wave_id).await.unwrap();
    let spec_card = cards
        .iter()
        .find(|card| card.kind == "codex" && card.payload["spec_harness"] == json!(true))
        .expect("spec harness card");
    let runtime = boot
        .repo
        .runtime_get_active_for_card(&spec_card.id.to_string())
        .await
        .unwrap()
        .expect("active spec harness runtime");
    assert!(boot.state.harness.get(&runtime.id).is_some());
    assert_eq!(boot.state.harness.len_active(), 1);

    let status = delete_empty(boot.app.clone(), &format!("/api/waves/{wave_id}")).await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(boot.state.harness.len_active(), 0);
    assert!(
        boot.repo
            .runtime_get_by_id(&runtime.id)
            .await
            .unwrap()
            .is_none(),
        "runtime row must cascade with the deleted spec card"
    );
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
async fn reset_spec_card_restarts_terminal_less_harness_card() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_shared().await;
    let card = boot
        .repo
        .card_create(NewCard {
            wave_id: WaveId::from(boot.wave_id.clone()),
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "schemaVersion": 1,
                "spec_harness": true,
                "push_watermark": 3
            }),
        })
        .await
        .unwrap();
    boot.state.card_role_cache.insert(
        card.id.clone(),
        CardRole::Spec,
        WaveId::from(boot.wave_id.clone()),
    );
    let old_runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(3, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some("thread-old".into());
    let mut tx = boot.repo.pool().begin().await.unwrap();
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: old_runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: RuntimeKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Idle,
            terminal_run_id: None,
            thread_id: Some("thread-old".into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            lease_owner: None,
            lease_until_ms: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/spec/reset", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["card_id"], json!(card.id.as_str()));
    assert_eq!(body["terminal_id"], json!(""));
    assert_eq!(body["new_thread_id"], json!("fake-thread-0001"));
    assert_eq!(body["wave"]["id"], json!(boot.wave_id));
    assert!(
        boot.repo
            .terminal_get_by_card(card.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        boot.repo
            .runtime_get_by_id(&old_runtime_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        RunStatus::Superseded
    );
    let active = boot
        .repo
        .runtime_get_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("new active runtime");
    assert_eq!(active.thread_id.as_deref(), Some("fake-thread-0001"));
    assert!(boot.state.harness.get(&active.id).is_some());
    if let Some(handle) = boot.state.harness.remove(&active.id) {
        handle.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn reset_spec_card_preserves_runtime_pending_queue_and_push_watermark() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_shared().await;
    let card = boot
        .repo
        .card_create(NewCard {
            wave_id: WaveId::from(boot.wave_id.clone()),
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "schemaVersion": 1,
                "spec_harness": true,
                "push_watermark": 0
            }),
        })
        .await
        .unwrap();
    boot.state.card_role_cache.insert(
        card.id.clone(),
        CardRole::Spec,
        WaveId::from(boot.wave_id.clone()),
    );
    let old_runtime_id = new_id();
    let thread_id = "thread-old-watermark".to_string();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.clone());
    let mut tx = boot.repo.pool().begin().await.unwrap();
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: old_runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: RuntimeKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Idle,
            terminal_run_id: None,
            thread_id: Some(thread_id.clone()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            lease_owner: None,
            lease_until_ms: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let repo_dyn: Arc<dyn Repo> = boot.repo.clone();
    let harness = SpecHarness::run(SpecHarnessParams {
        runtime_id: old_runtime_id.clone(),
        wave_id: card.wave_id.clone(),
        card_id: card.id.clone(),
        thread_id: Some(thread_id),
        repo: repo_dyn,
        events: boot.state.events.clone(),
        card_role_cache: boot.state.card_role_cache.clone(),
        wave_cove_cache: boot.state.wave_cove_cache.clone(),
        daemon: boot.state.shared_codex_appserver.clone(),
        config: HarnessConfig {
            debounce_min_idle: Duration::from_secs(60),
            debounce_max_wait: Duration::from_secs(60),
            ..HarnessConfig::default()
        },
        snapshot,
    });
    boot.state
        .harness
        .insert(old_runtime_id.clone(), harness.clone());
    for envelope_id in 1_i64..=3 {
        harness
            .observe_envelope(
                Observation::WaveGoal {
                    text: format!("seeded observation {envelope_id}"),
                },
                envelope_id,
            )
            .unwrap();
    }
    wait_for_harness_watermark(&harness, 3).await;
    harness.persist_snapshot().await.unwrap();

    let old_runtime = boot
        .repo
        .runtime_get_by_id(&old_runtime_id)
        .await
        .unwrap()
        .unwrap();
    let old_snapshot = HarnessSnapshot::from_value_strict(old_runtime.handle_state_json.unwrap());
    assert_eq!(old_snapshot.push_watermark, 3);
    assert_eq!(old_snapshot.pending_queue.len(), 3);

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/spec/reset", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    let active = boot
        .repo
        .runtime_get_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("new active runtime");
    assert_ne!(active.id, old_runtime_id);
    let new_snapshot = HarnessSnapshot::from_value_strict(
        active
            .handle_state_json
            .clone()
            .expect("new runtime snapshot"),
    );
    assert_eq!(new_snapshot.push_watermark, 3);
    assert_eq!(new_snapshot.pending_queue.len(), 3);
    assert!(boot.state.harness.get(&old_runtime_id).is_none());
    if let Some(handle) = boot.state.harness.remove(&active.id) {
        handle.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn reset_spec_card_recovers_inert_harness_card_without_active_runtime() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_shared().await;
    let card = boot
        .repo
        .card_create(NewCard {
            wave_id: WaveId::from(boot.wave_id.clone()),
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "schemaVersion": 1,
                "spec_harness": true,
                "push_watermark": 0
            }),
        })
        .await
        .unwrap();
    boot.state.card_role_cache.insert(
        card.id.clone(),
        CardRole::Spec,
        WaveId::from(boot.wave_id.clone()),
    );
    assert!(
        boot.repo
            .runtime_get_active_for_card(&card.id.to_string())
            .await
            .unwrap()
            .is_none()
    );

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/spec/reset", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["card_id"], json!(card.id.as_str()));
    assert_eq!(body["terminal_id"], json!(""));
    assert_eq!(body["new_thread_id"], json!("fake-thread-0001"));
    assert_eq!(body["wave"]["id"], json!(boot.wave_id));
    assert!(
        boot.repo
            .terminal_get_by_card(card.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    let active = boot
        .repo
        .runtime_get_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("new active runtime");
    assert_eq!(active.thread_id.as_deref(), Some("fake-thread-0001"));
    assert!(boot.state.harness.get(&active.id).is_some());
    if let Some(handle) = boot.state.harness.remove(&active.id) {
        handle.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn reset_spec_card_failure_keeps_old_runtime_when_shared_daemon_down() {
    let boot = boot().await;
    let card = boot
        .repo
        .card_create(NewCard {
            wave_id: WaveId::from(boot.wave_id.clone()),
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "schemaVersion": 1,
                "codex_source": "shared",
                "spec_harness": true,
                "codex_thread_id": "thread-old",
                "push_watermark": 0
            }),
        })
        .await
        .unwrap();
    boot.state.card_role_cache.insert(
        card.id.clone(),
        CardRole::Spec,
        WaveId::from(boot.wave_id.clone()),
    );
    boot.repo
        .card_codex_thread_upsert(
            card.id.as_str(),
            "thread-old",
            CardRole::Spec,
            Some(boot.wave_id.as_str()),
        )
        .await
        .unwrap();

    let old_runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some("thread-old".into());
    let mut tx = boot.repo.pool().begin().await.unwrap();
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: old_runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: RuntimeKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Idle,
            terminal_run_id: None,
            thread_id: Some("thread-old".into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            lease_owner: None,
            lease_until_ms: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let before = boot
        .repo
        .runtime_get_by_id(&old_runtime_id)
        .await
        .unwrap()
        .unwrap();

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/spec/reset", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body={body}");
    let after = boot
        .repo
        .runtime_get_by_id(&old_runtime_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after, before);
    let active = boot
        .repo
        .runtime_get_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("old runtime remains active");
    assert_eq!(active.id, old_runtime_id);
    assert_eq!(active.thread_id.as_deref(), Some("thread-old"));
    let mapping = boot
        .repo
        .card_codex_thread_get_by_card(card.id.as_str())
        .await
        .unwrap()
        .expect("old thread mapping remains");
    assert_eq!(mapping.thread_id, "thread-old");
}

#[tokio::test]
async fn shared_reset_writes_runtime_and_projects_new_thread_id() {
    let _guard = ENV_LOCK.lock().await;
    let capture = TempDir::new().unwrap();
    let capture_file = capture.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture_file);
    }
    let boot = boot_shared().await;
    let (card, _terminal, _capture) = seed_shared_spec_card(&boot, 42).await;

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
    assert_eq!(body["terminal_id"], json!(""));
    assert_eq!(body["new_thread_id"], json!("fake-thread-0001"));

    let mapping = boot
        .repo
        .card_codex_thread_get_by_card(card.id.as_str())
        .await
        .unwrap()
        .expect("#524 reset path writes legacy mapping atomically with runtime");
    assert_eq!(mapping.thread_id, "fake-thread-0001");
    let runtime = boot
        .repo
        .runtime_get_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("active runtime");
    assert_eq!(runtime.kind, RuntimeKind::SharedSpec);
    assert_eq!(runtime.thread_id.as_deref(), Some("fake-thread-0001"));
    let mut got = boot.repo.card_get(card.id.as_str()).await.unwrap().unwrap();
    assert_eq!(got.payload["codex_source"], json!("shared"));
    assert_eq!(got.payload["codex_thread_id"], json!("fake-thread-0001"));
    assert_eq!(got.payload["push_watermark"], json!(42));
    assert!(got.payload.get("appserver_pgid").is_none());
    assert!(got.payload.get("appserver_start_time").is_none());
    assert!(got.payload.get("appserver_boot_id").is_none());
    project_runtime_into_card_payload(boot.repo.as_ref(), &mut got)
        .await
        .unwrap();
    assert_eq!(got.payload["codex_source"], json!("shared"));
    assert_eq!(got.payload["codex_thread_id"], json!("fake-thread-0001"));
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
}
