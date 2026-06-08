use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::config::Config;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, runtime_start_tx};
use calm_server::error::{CalmError, Result as CalmResult};
use calm_server::event::EventBus;
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, Observation, SpecHarness, SpecHarnessParams,
};
use calm_server::ids::WaveId;
use calm_server::model::{Card, CardRole, NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::operation::spec_harness_start_adapter::SpecHarnessStartAdapter;
use calm_server::operation::{
    AppServerInteractKind, AppServerInteractOutcome, CompensationStateVersioned, CompensationStep,
    Operation, OperationRuntime, PhaseTag, ProviderAdapter, SpawnCtx, SpawnHandle,
    SqlxOperationRepo, Tx, TxOutput,
};
use calm_server::pending_codex_threads::PendingThreadStartRegistry;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
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
    _tmp: TempDir,
}

struct FailingSpawnSpecHarnessStartAdapter {
    inner: SpecHarnessStartAdapter,
}

#[async_trait]
impl ProviderAdapter for FailingSpawnSpecHarnessStartAdapter {
    fn kind(&self) -> &'static str {
        self.inner.kind()
    }

    fn phases(&self) -> &'static [PhaseTag] {
        self.inner.phases()
    }

    fn app_server_interact_kind(
        &self,
        output: &TxOutput,
        op: &Operation,
    ) -> CalmResult<AppServerInteractKind> {
        self.inner.app_server_interact_kind(output, op)
    }

    async fn validate(&self, input: &Value) -> CalmResult<()> {
        self.inner.validate(input).await
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> CalmResult<TxOutput> {
        self.inner.prepare_tx(tx, input, op).await
    }

    async fn app_server_interact(
        &self,
        output: &mut TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> CalmResult<AppServerInteractOutcome> {
        self.inner.app_server_interact(output, op, ctx).await
    }

    async fn spawn_side_effect(
        &self,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<SpawnHandle> {
        Err(CalmError::Internal(
            "test spec harness spawn failure".into(),
        ))
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        op: &Operation,
    ) -> CalmResult<CompensationStateVersioned> {
        self.inner
            .plan_compensation(from_phase, reason, output, op)
            .await
    }

    async fn compensate_step(
        &self,
        step: &CompensationStep,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> CalmResult<()> {
        self.inner.compensate_step(step, output, op, ctx).await
    }
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
        _tmp: tmp,
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
        _tmp: tmp,
    }
}

fn install_failing_spec_start_runtime(boot: &mut Boot) {
    let route_repo: Arc<dyn RouteRepo> = boot.repo.clone();
    let operation_repo = Arc::new(SqlxOperationRepo::new(boot.repo.pool().clone()));
    let start_adapter: Arc<dyn ProviderAdapter> = Arc::new(FailingSpawnSpecHarnessStartAdapter {
        inner: SpecHarnessStartAdapter::new(
            boot.repo.clone(),
            boot.state.shared_codex_appserver.clone(),
            boot.state.harness.clone(),
            boot.state.card_role_cache.clone(),
            boot.state.wave_cove_cache.clone(),
        ),
    });
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo,
        vec![start_adapter],
        boot.state.events.clone(),
        SpawnCtx::new(
            route_repo,
            boot.state.daemon.clone(),
            boot.state.terminal_renderer.clone(),
            boot.state.events.clone(),
        ),
    ));
    boot.state = boot.state.clone().with_operation_runtime(runtime);
    boot.app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(boot.state.clone());
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
async fn reset_spec_card_spawn_failure_keeps_old_runtime_and_harness() {
    let _guard = ENV_LOCK.lock().await;
    let mut boot = boot_shared().await;
    install_failing_spec_start_runtime(&mut boot);
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
    let old_thread_id = "thread-old-spawn-failure".to_string();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(old_thread_id.clone());
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
            thread_id: Some(old_thread_id.clone()),
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
    let old_harness = SpecHarness::run(SpecHarnessParams {
        runtime_id: old_runtime_id.clone(),
        wave_id: card.wave_id.clone(),
        card_id: card.id.clone(),
        thread_id: Some(old_thread_id.clone()),
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
        .insert(old_runtime_id.clone(), old_harness);

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/spec/reset", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body={body}");
    let old_after = boot
        .repo
        .runtime_get_by_id(&old_runtime_id)
        .await
        .unwrap()
        .expect("old runtime row remains");
    assert_eq!(old_after.status, RunStatus::Idle);
    let active = boot
        .repo
        .runtime_get_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("old runtime remains active");
    assert_eq!(active.id, old_runtime_id);
    assert!(
        boot.state.harness.get(&old_runtime_id).is_some(),
        "old harness remains registered after new spawn failure"
    );

    let new_rows: Vec<(String, String)> = sqlx::query_as(
        r#"SELECT id, status
             FROM runtimes
            WHERE card_id = ?1
              AND id != ?2"#,
    )
    .bind(card.id.as_str())
    .bind(&old_runtime_id)
    .fetch_all(boot.repo.pool())
    .await
    .unwrap();
    assert_eq!(new_rows.len(), 1, "expected one replacement runtime");
    assert_eq!(new_rows[0].1, "failed");
    assert!(boot.state.harness.get(&new_rows[0].0).is_none());

    if let Some(handle) = boot.state.harness.remove(&old_runtime_id) {
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
