use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::config::Config;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_projection_by_id_tx, session_start_runtime_tx};
use calm_server::error::{CalmError, Result as CalmResult};
use calm_server::event::EventBus;
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, Observation, PlannerHarness,
    PlannerHarnessParams,
};
use calm_server::ids::TrackId;
use calm_server::model::{
    Card, CardRole, NewArea, NewCard, NewTerminal, NewTrack, RequestTheme, new_id, now_ms,
};
use calm_server::operation::planner_harness_start_adapter::PlannerHarnessStartAdapter;
use calm_server::operation::{
    AppServerInteractKind, AppServerInteractOutcome, CompensationStateVersioned, CompensationStep,
    Operation, OperationCompletionBus, OperationRuntime, PhaseTag, ProviderAdapter, SpawnCtx,
    SpawnOutcome, SqlxOperationRepo, Tx, TxOutput,
};
use calm_server::pending_codex_threads::PendingThreadStartRegistry;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, DaemonClient};
use clap::Parser;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

mod common;
mod support;

use support::git_helpers::attached_repo_fixture;

/// Serializes intra-binary tests that toggle `FAKE_CODEX_CAPTURE_REQUESTS`
/// (or any other process env read by the fake codex shim). Peer test
/// binaries keep their own `ENV_LOCK` because each test binary is a separate
/// process.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Boot {
    app: axum::Router,
    state: AppState,
    repo: Arc<SqlxRepo>,
    track_id: String,
    _tmp: TempDir,
}

async fn runtime_by_id_tx_snapshot(
    repo: &SqlxRepo,
    runtime_id: &str,
) -> Option<calm_server::session_projection_repo::WorkerSessionProjection> {
    let id = runtime_id.to_string();
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_projection_by_id_tx(&mut tx, &id).await.unwrap();
    tx.commit().await.unwrap();
    runtime
}

struct FailingSpawnPlannerHarnessStartAdapter {
    inner: PlannerHarnessStartAdapter,
}

#[async_trait]
impl ProviderAdapter for FailingSpawnPlannerHarnessStartAdapter {
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
    ) -> CalmResult<SpawnOutcome> {
        Err(CalmError::Internal(
            "test planner harness spawn failure".into(),
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
    let area = repo
        .area_create(NewArea {
            name: "planner-card-reset".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "reset route auth".into(),
            sort: None,
            cwd: "/tmp/planner-card-reset".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_card_role_cache(&card_role_cache).await.unwrap();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();
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
            std::env::temp_dir().join("calm-plugins-data-planner-card-reset"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                card_role_cache.clone(),
                track_area_cache.clone(),
            ),
        )),
        Arc::new(common::fake_codex_client()),
        Some(card_role_cache),
        Some(track_area_cache),
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
        track_id: track.id.to_string(),
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
    let area = repo
        .area_create(NewArea {
            name: "planner-card-reset-shared".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "shared reset goal".into(),
            sort: None,
            cwd: "/tmp/planner-card-reset-shared".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_card_role_cache(&card_role_cache).await.unwrap();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();
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
            calm_server::state::WriteContext::new(
                card_role_cache.clone(),
                track_area_cache.clone(),
            ),
        )),
        Arc::new(common::fake_codex_client()),
        Some(card_role_cache),
        Some(track_area_cache),
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
        track_id: track.id.to_string(),
        _tmp: tmp,
    }
}

fn install_failing_planner_start_runtime(boot: &mut Boot) {
    let route_repo: Arc<dyn RouteRepo> = boot.repo.clone();
    let operation_repo = Arc::new(SqlxOperationRepo::new(boot.repo.pool().clone()));
    let start_adapter: Arc<dyn ProviderAdapter> =
        Arc::new(FailingSpawnPlannerHarnessStartAdapter {
            inner: PlannerHarnessStartAdapter::new(
                boot.repo.clone(),
                boot.state.shared_codex_appserver.clone(),
                boot.state.harness.clone(),
                boot.state.plugin.clone(),
                boot.state.card_role_cache.clone(),
                boot.state.track_area_cache.clone(),
                None,
            ),
        });
    let completion = OperationCompletionBus::new();
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo.clone(),
        vec![start_adapter],
        boot.state.events.clone(),
        completion.clone(),
        SpawnCtx::new(
            route_repo,
            operation_repo,
            boot.state.daemon.clone(),
            boot.state.terminal_renderer.clone(),
            boot.state.events.clone(),
            completion,
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

async fn post_json_with_actor(
    app: axum::Router,
    uri: &str,
    body: Value,
    actor: &str,
) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .header("X-Calm-Actor", actor)
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

async fn seed_shared_worker_card(boot: &Boot, label: &str, thread_id: &str) -> Card {
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: TrackId::from(boot.track_id.clone()),
            title: None,
            kind: "plugin:test:worker".into(),
            sort: None,
            payload: json!({
                "label": label,
                "codex_source": "shared",
                "codex_thread_id": thread_id,
            }),
        })
        .await
        .expect("seed shared worker card");
    let mut tx = boot.repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: new_id(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Running,
            terminal_run_id: None,
            thread_id: Some(thread_id.to_string()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .expect("seed shared worker runtime");
    tx.commit().await.unwrap();
    card
}

async fn request_lines_containing(path: &PathBuf, method: &str, count: usize) -> Vec<Value> {
    request_lines_containing_within(path, method, count, Duration::from_secs(15)).await
}

/// Poll `path` (an ndjson capture file) until at least `count` lines whose
/// `method` field equals `method` are present, then return them. Panics on
/// timeout with a descriptive message — a missing line is a real failure, not
/// something to swallow. (Flaky-test #845: the old silent partial-return
/// mis-reported anomalies as `has_interrupt` failures.)
async fn request_lines_containing_within(
    path: &PathBuf,
    method: &str,
    count: usize,
    within: Duration,
) -> Vec<Value> {
    let deadline = Instant::now() + within;
    loop {
        let matching = std::fs::read_to_string(path)
            .ok()
            .map(|raw| {
                raw.lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .filter(|row| row.get("method").and_then(Value::as_str) == Some(method))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if matching.len() >= count {
            return matching;
        }
        if Instant::now() >= deadline {
            panic!(
                "request_lines_containing: captured {}/{} '{method}' lines within {within:?}; rows={matching:?}",
                matching.len(),
                count,
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
#[should_panic(expected = "captured 0/2")]
async fn request_lines_containing_panics_loudly_on_timeout() {
    // A capture file that never reaches the requested count must panic with a
    // descriptive message — not silently return a partial vector (#845).
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("never-written.ndjson");
    let _ =
        request_lines_containing_within(&missing, "turn/interrupt", 2, Duration::from_millis(50))
            .await;
}

async fn wait_for_harness_watermark(harness: &PlannerHarness, watermark: i64) {
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

async fn seed_codex_card_with_role(boot: &Boot, role: CardRole) -> Card {
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: TrackId::from(boot.track_id.clone()),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "schemaVersion": 1,
                "planner_harness": role == CardRole::Planner
            }),
        })
        .await
        .expect("seed codex card");
    boot.state
        .card_role_cache
        .insert(card.id.clone(), role, TrackId::from(boot.track_id.clone()));
    card
}

async fn seed_live_planner_harness(boot: &Boot) -> (Card, String, PlannerHarness) {
    let card = seed_codex_card_with_role(boot, CardRole::Planner).await;
    let runtime_id = new_id();
    let thread_id = format!("thread-{runtime_id}");
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.clone());
    let mut tx = boot.repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(thread_id.clone()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .expect("seed active planner harness runtime");
    tx.commit().await.unwrap();

    let repo_dyn: Arc<dyn Repo> = boot.repo.clone();
    let harness = PlannerHarness::run(PlannerHarnessParams {
        runtime_id: runtime_id.clone(),
        track_id: card.track_id.clone(),
        card_id: card.id.clone(),
        thread_id: Some(thread_id),
        repo: repo_dyn,
        events: boot.state.events.clone(),
        card_role_cache: boot.state.card_role_cache.clone(),
        track_area_cache: boot.state.track_area_cache.clone(),
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
        .insert(runtime_id.clone(), harness.clone());
    (card, runtime_id, harness)
}

async fn seed_live_plain_chat_harness(boot: &Boot) -> (Card, String, PlannerHarness) {
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: TrackId::from(boot.track_id.clone()),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "harness_profile": "plain_chat"}),
        })
        .await
        .unwrap();
    boot.state.card_role_cache.insert(
        card.id.clone(),
        CardRole::Worker,
        TrackId::from(boot.track_id.clone()),
    );
    let runtime_id = new_id();
    let thread_id = format!("thread-{runtime_id}");
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.clone());
    let mut tx = boot.repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(thread_id.clone()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let repo_dyn: Arc<dyn Repo> = boot.repo.clone();
    let harness = PlannerHarness::run(PlannerHarnessParams {
        runtime_id: runtime_id.clone(),
        track_id: card.track_id.clone(),
        card_id: card.id.clone(),
        thread_id: Some(thread_id),
        repo: repo_dyn,
        events: boot.state.events.clone(),
        card_role_cache: boot.state.card_role_cache.clone(),
        track_area_cache: boot.state.track_area_cache.clone(),
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
        .insert(runtime_id.clone(), harness.clone());
    (card, runtime_id, harness)
}

#[tokio::test]
async fn planner_input_accepts_plain_chat_but_rejects_unmarked_pty_codex() {
    let boot = boot().await;
    let pty = seed_codex_card_with_role(&boot, CardRole::Worker).await;
    let (status, _) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", pty.id),
        json!({"text": "must fail closed"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (chat, runtime_id, harness) = seed_live_plain_chat_harness(&boot).await;
    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", chat.id),
        json!({"text": "hello chat"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(harness.snapshot().await.pending_queue.len(), 1);
    boot.state.harness.remove(&runtime_id);
    harness.shutdown().await.unwrap();
}

async fn seed_inactive_planner_runtime(boot: &Boot, card: &Card) -> String {
    let runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    let mut tx = boot.repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Exited,
            terminal_run_id: None,
            thread_id: Some(format!("thread-{runtime_id}")),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .expect("seed inactive planner harness runtime");
    tx.commit().await.unwrap();
    runtime_id
}

async fn wait_for_user_message(harness: &PlannerHarness, text: &str) -> HarnessSnapshot {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let snapshot = harness.snapshot().await;
        if snapshot.pending_queue.iter().any(|obs| {
            matches!(
                obs,
                Observation::UserMessage { text: queued } if queued == text
            )
        }) {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for user message {text:?}; snapshot={snapshot:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn shutdown_seeded_harness(boot: &Boot, runtime_id: &str, harness: PlannerHarness) {
    if let Some(handle) = boot.state.harness.remove(runtime_id) {
        handle.shutdown().await.unwrap();
    } else {
        harness.shutdown().await.unwrap();
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
async fn send_planner_input_happy() {
    let boot = boot().await;
    let text = "look into Korean refiners";
    let (card, runtime_id, harness) = seed_live_planner_harness(&boot).await;

    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": text }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["card_id"], json!(card.id.as_str()));
    assert_eq!(body["runtime_id"], json!(runtime_id.as_str()));
    let snapshot = wait_for_user_message(&harness, text).await;
    assert!(
        snapshot.pending_queue.iter().any(|obs| {
            matches!(
                obs,
                Observation::UserMessage { text: queued } if queued == text
            )
        }),
        "pending_queue={:?}",
        snapshot.pending_queue
    );

    shutdown_seeded_harness(&boot, &runtime_id, harness).await;
}

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn planner_harness_user_message_folds_at_saturation() {
    let boot = boot().await;
    let (_card, runtime_id, harness) = seed_live_planner_harness(&boot).await;

    for i in 0..256 {
        harness
            .observe_for_test(
                Observation::UserMessage {
                    text: format!("msg-{i}"),
                },
                None,
            )
            .await;
    }

    assert_eq!(harness.pending_len_for_test().await, 256);

    harness
        .observe_for_test(
            Observation::UserMessage {
                text: "tail-append".into(),
            },
            None,
        )
        .await;

    assert_eq!(
        harness.pending_len_for_test().await,
        256,
        "fold must keep queue at cap"
    );
    let pending = harness.pending_queue_for_test().await;
    let Some(Observation::UserMessage { text }) = pending.last() else {
        panic!("expected UserMessage tail; got {:?}", pending.last());
    };
    assert!(
        text.contains("msg-255"),
        "tail should retain msg-255: {text}"
    );
    assert!(
        text.contains("tail-append"),
        "tail should retain new msg: {text}"
    );
    assert!(text.contains("\n\n"), "fold separator missing: {text}");

    shutdown_seeded_harness(&boot, &runtime_id, harness).await;
}

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn send_planner_input_reports_backpressure_when_the_persisted_queue_rejects_it() {
    let boot = boot().await;
    let (card, worker_session_id, harness) = seed_live_planner_harness(&boot).await;
    harness.pause_issuance_for_dev();
    for i in 0..255 {
        harness
            .observe_for_test(
                Observation::UserMessage {
                    text: format!("hard-{i}"),
                },
                None,
            )
            .await;
    }
    harness
        .observe_for_test(
            Observation::UserMessage {
                // Production's fold cap. A one-character route message cannot
                // merge into this tail, and every queued item is hard-fire, so
                // the pending queue must reject it rather than evict intent.
                text: "x".repeat(128 * 1024),
            },
            None,
        )
        .await;

    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": "y" }),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body={body}");
    assert_eq!(harness.pending_len_for_test().await, 256);

    shutdown_seeded_harness(&boot, &worker_session_id, harness).await;
}

#[tokio::test]
async fn send_planner_input_emits_audit_event() {
    let boot = boot().await;
    let text = "audit me";
    let (card, runtime_id, harness) = seed_live_planner_harness(&boot).await;

    let (status, _body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": text }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let area_id = boot
        .repo
        .track_get(card.track_id.as_str())
        .await
        .unwrap()
        .unwrap()
        .area_id;
    let events = boot.repo.events_since(0, i64::MAX).await.unwrap();
    let found = events.iter().any(|(_id, _version, scope, event)| {
        matches!(
            (scope, event),
            (
                calm_server::event::EventScope::Card {
                    card: scope_card,
                    track: scope_track,
                    area: scope_area
                },
                calm_server::event::Event::HarnessUserMessageEnqueued {
                    runtime_id: ev_rt,
                    card_id: ev_card,
                    track_id: ev_track,
                    char_count,
                },
            ) if ev_rt == &runtime_id
                && ev_card == &card.id
                && ev_track == &card.track_id
                && scope_card == &card.id
                && scope_track == &card.track_id
                && scope_area == &area_id
                && *char_count == text.chars().count() as u32
        )
    });
    assert!(found, "expected harness.user_message.enqueued: {events:?}");

    let actor_row: (String,) = sqlx::query_as(
        "SELECT actor FROM events \
         WHERE kind = 'harness.user_message.enqueued' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(boot.repo.pool())
    .await
    .unwrap();
    let actor_json: Value = serde_json::from_str(&actor_row.0).expect("events.actor is JSON");
    assert_eq!(
        actor_json,
        json!({"kind": "User"}),
        "human planner input must keep User audit attribution"
    );

    shutdown_seeded_harness(&boot, &runtime_id, harness).await;
}

#[tokio::test]
async fn send_planner_input_with_ai_codex_actor_emits_planner_session_audit_event() {
    let boot = boot().await;
    let (card, runtime_id, harness) = seed_live_planner_harness(&boot).await;

    let (status, body) = post_json_with_actor(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": "from codex" }),
        "ai:codex",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    let events = boot.repo.events_since(0, i64::MAX).await.unwrap();
    let found = events.iter().any(|(_id, _v, _scope, event)| {
        matches!(
            event,
            calm_server::event::Event::HarnessUserMessageEnqueued { card_id, .. }
                if card_id == &card.id
        )
    });
    assert!(found, "expected harness.user_message.enqueued: {events:?}");

    let actor_row: (String,) = sqlx::query_as(
        "SELECT actor FROM events \
         WHERE kind = 'harness.user_message.enqueued' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(boot.repo.pool())
    .await
    .unwrap();
    let actor_json: Value = serde_json::from_str(&actor_row.0).expect("events.actor is JSON");
    assert_eq!(
        actor_json,
        json!({"kind": "AiPlannerSession", "id": runtime_id.as_str()}),
        "active planner harness input must be attributed to the worker session"
    );

    shutdown_seeded_harness(&boot, &runtime_id, harness).await;
}

#[tokio::test]
async fn send_planner_input_track_missing_returns_404() {
    let boot = boot().await;
    let (mut card, runtime_id, harness) = seed_live_planner_harness(&boot).await;
    let missing_track_id = format!("missing-track-{}", new_id());

    let mut conn = boot.repo.pool().acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET track_id = ?1 WHERE id = ?2")
        .bind(&missing_track_id)
        .bind(card.id.as_str())
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);
    card.track_id = TrackId::from(missing_track_id);

    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": "track gone" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body={body}");
    assert!(
        body["error"].as_str().is_some_and(|e| e.contains("track")),
        "body={body}"
    );

    let events = boot.repo.events_since(0, i64::MAX).await.unwrap();
    let any_user_msg = events.iter().any(|(_id, _v, _scope, event)| {
        matches!(
            event,
            calm_server::event::Event::HarnessUserMessageEnqueued { .. }
        )
    });
    assert!(
        !any_user_msg,
        "track_get -> None path must not emit audit: {events:?}"
    );

    shutdown_seeded_harness(&boot, &runtime_id, harness).await;
}

#[tokio::test]
async fn send_planner_input_audit_char_count_counts_chars_not_bytes() {
    let boot = boot().await;
    // 200 CJK chars = ~600 bytes. If char_count regresses to len(), the
    // assertion below will catch the byte/char divergence loudly.
    let text: String = "字".repeat(200);
    let (card, runtime_id, harness) = seed_live_planner_harness(&boot).await;

    let (status, _body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": text }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let events = boot.repo.events_since(0, i64::MAX).await.unwrap();
    let count = events.iter().find_map(|(_id, _v, _scope, event)| {
        if let calm_server::event::Event::HarnessUserMessageEnqueued { char_count, .. } = event {
            Some(*char_count)
        } else {
            None
        }
    });
    assert_eq!(
        count,
        Some(200),
        "char_count must be utf-8 char count, not bytes"
    );

    shutdown_seeded_harness(&boot, &runtime_id, harness).await;
}

#[tokio::test]
async fn send_planner_input_accepts_max_chars() {
    let boot = boot().await;
    let (card, runtime_id, harness) = seed_live_planner_harness(&boot).await;

    let text: String = "a".repeat(32_768);
    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": text }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    shutdown_seeded_harness(&boot, &runtime_id, harness).await;
}

#[tokio::test]
async fn send_planner_input_accepts_max_cjk_chars() {
    let boot = boot().await;
    let (card, runtime_id, harness) = seed_live_planner_harness(&boot).await;

    // 32_768 CJK chars is about 98_304 bytes. `text.chars().count()` must
    // accept it; a regression to `body.text.len()` would reject it.
    let text: String = "字".repeat(32_768);
    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": text }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    shutdown_seeded_harness(&boot, &runtime_id, harness).await;
}

#[tokio::test]
async fn send_planner_input_rejects_over_max_chars() {
    let boot = boot().await;
    let (card, runtime_id, harness) = seed_live_planner_harness(&boot).await;

    let text: String = "a".repeat(32_769);
    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": text }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains("32768") || e.contains("at most")),
        "body={body}"
    );
    shutdown_seeded_harness(&boot, &runtime_id, harness).await;
}

#[tokio::test]
async fn send_planner_input_rejects_over_max_cjk_chars() {
    let boot = boot().await;
    let (card, runtime_id, harness) = seed_live_planner_harness(&boot).await;

    let text: String = "字".repeat(32_769);
    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": text }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    shutdown_seeded_harness(&boot, &runtime_id, harness).await;
}

#[tokio::test]
async fn send_planner_input_empty_400() {
    let boot = boot().await;
    let (card, runtime_id, harness) = seed_live_planner_harness(&boot).await;

    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": "   " }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    shutdown_seeded_harness(&boot, &runtime_id, harness).await;
}

#[tokio::test]
async fn send_planner_input_after_shutdown_returns_409() {
    let boot = boot().await;
    let (card, runtime_id, harness) = seed_live_planner_harness(&boot).await;

    harness.shutdown().await.unwrap();
    assert!(
        boot.state.harness.get(&runtime_id).is_some(),
        "seeded harness registry entry should remain after direct shutdown"
    );

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match harness.observe(Observation::TrackGoal {
            text: "closed-channel-probe".into(),
        }) {
            Err(CalmError::Conflict(message)) if message.contains("shutting down") => break,
            Ok(()) => {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for harness observation channel to close"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            other => panic!("unexpected observe result after shutdown: {other:?}"),
        }
    }

    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": "racing" }),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| { e.contains("shutting down") || e.contains("runtime") }),
        "expected shutting-down message: body={body}"
    );

    let _ = boot.state.harness.remove(&runtime_id);
}

#[tokio::test]
async fn send_planner_input_non_planner_card_403() {
    let boot = boot().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Worker).await;

    let (status, body) = post_json(
        boot.app,
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": "hello planner" }),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| error.contains("not a planner codex card")),
        "body={body}"
    );
}

/// #649 i2 trigger B — no active runtime row at all (the fire-and-forget
/// `planner-harness-start` failed at track creation): typed 409 with the
/// machine-readable `planner_harness_dormant` code, NOT a 404, so the client
/// can steer the user to Reset.
#[tokio::test]
async fn send_planner_input_no_active_runtime_409_dormant() {
    let boot = boot().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Planner).await;
    seed_inactive_planner_runtime(&boot, &card).await;

    let (status, body) = post_json(
        boot.app,
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": "wake up" }),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        body["code"],
        json!("planner_harness_dormant"),
        "body={body}"
    );
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| error.contains("reset")),
        "dormant body should point at reset: body={body}"
    );
}

/// Boot variant whose shared codex app-server reports `is_running() == true`
/// (fixture fake), so the #649 i2 lazy-recovery path is reachable — the
/// plain `boot()` stub daemon is Idle and would 503 before recovery.
async fn boot_fake_running() -> Boot {
    let mut boot = boot().await;
    let shared = SharedCodexAppServer::new_fake_running_with_pending(boot.repo.clone(), None);
    boot.state = boot.state.clone().with_shared_codex_appserver(shared);
    boot.app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(boot.state.clone());
    boot
}

/// Seed an *active* planner runtime row (status `idle`) with NO live harness
/// task and NO registry entry — the post-restart shape #649 i2 recovers
/// from (boot recovery skips done-lifecycle tracks, leaving the row live
/// and the registry empty).
async fn seed_active_planner_runtime_row(
    boot: &Boot,
    card: &Card,
    thread_id: Option<String>,
    handle_state_json: Option<Value>,
) -> String {
    seed_planner_runtime_row_with_status(
        boot,
        card,
        thread_id,
        handle_state_json,
        WorkerSessionState::Idle,
    )
    .await
}

/// Like [`seed_active_planner_runtime_row`] but with an explicit status — used
/// to model an in-flight `planner-harness-start` (`starting` row, registry
/// empty until `spawn_side_effect` lands).
async fn seed_planner_runtime_row_with_status(
    boot: &Boot,
    card: &Card,
    thread_id: Option<String>,
    handle_state_json: Option<Value>,
    status: WorkerSessionState,
) -> String {
    let runtime_id = new_id();
    let mut tx = boot.repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status,
            terminal_run_id: None,
            thread_id,
            session_id: None,
            active_turn_id: None,
            handle_state_json,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .expect("seed active planner runtime row");
    tx.commit().await.unwrap();
    runtime_id
}

fn idle_snapshot_value(thread_id: &str) -> Value {
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.to_string());
    serde_json::to_value(&snapshot).unwrap()
}

fn idle_snapshot_value_without_thread() -> Value {
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    serde_json::to_value(&snapshot).unwrap()
}

/// #649 i2 trigger A — registry miss with a recoverable active runtime row
/// (durable thread_id + valid snapshot): the route transparently re-spawns
/// the harness via `spawn_recovered_harness`, registers it, and enqueues
/// the user message as normal.
#[tokio::test]
async fn send_planner_input_registry_miss_recovers_harness_and_enqueues() {
    let boot = boot_fake_running().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Planner).await;
    let thread_id = format!("thread-{}", new_id());
    let runtime_id = seed_active_planner_runtime_row(
        &boot,
        &card,
        Some(thread_id.clone()),
        Some(idle_snapshot_value(&thread_id)),
    )
    .await;
    assert!(
        boot.state.harness.get(&runtime_id).is_none(),
        "precondition: no registry entry before the send"
    );

    let text = "recovered follow-up";
    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": text }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["card_id"], json!(card.id.as_str()));
    assert_eq!(body["runtime_id"], json!(runtime_id.as_str()));
    assert!(
        boot.state.harness.get(&runtime_id).is_some(),
        "registry must hold the lazily recovered harness handle"
    );
    // The route only emits the audit event after `observe` succeeded, so
    // this doubles as the "message actually enqueued" assertion without
    // racing the recovered harness's 250ms debounce-issued turn.
    let events = boot.repo.events_since(0, i64::MAX).await.unwrap();
    let found = events.iter().any(|(_id, _v, _scope, event)| {
        matches!(
            event,
            calm_server::event::Event::HarnessUserMessageEnqueued { runtime_id: ev_rt, card_id: ev_card, .. }
                if ev_rt == &runtime_id && ev_card == &card.id
        )
    });
    assert!(found, "expected harness.user_message.enqueued: {events:?}");

    if let Some(handle) = boot.state.harness.remove(&runtime_id) {
        handle.shutdown().await.unwrap();
    }
}

/// #649 i2 hardening 3 — an active row with NO thread anywhere (NULL row
/// `thread_id` AND no snapshot `last_thread_id`; half-failed start) must
/// NOT be recovered into a zombie harness: typed 409 dormant, registry
/// stays empty.
#[tokio::test]
async fn send_planner_input_active_runtime_null_thread_409_dormant() {
    let boot = boot_fake_running().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Planner).await;
    let runtime_id = seed_active_planner_runtime_row(
        &boot,
        &card,
        None,
        Some(idle_snapshot_value_without_thread()),
    )
    .await;

    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": "wake up" }),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        body["code"],
        json!("planner_harness_dormant"),
        "body={body}"
    );
    assert!(
        boot.state.harness.get(&runtime_id).is_none(),
        "thread-less row must not be recovered into the registry"
    );
}

/// #649 review round 3 — a `starting` row means `planner-harness-start` is
/// still in flight (row written before `spawn_side_effect` registers the
/// harness): lazy recovery must NOT race it by spawning a harness the start
/// op will shut down (dropping queued input). The route 503s with a retry
/// hint and leaves the registry empty.
#[tokio::test]
async fn send_planner_input_starting_runtime_503_no_recovery() {
    let boot = boot_fake_running().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Planner).await;
    let thread_id = format!("thread-{}", new_id());
    let runtime_id = seed_planner_runtime_row_with_status(
        &boot,
        &card,
        Some(thread_id.clone()),
        Some(idle_snapshot_value(&thread_id)),
        WorkerSessionState::Starting,
    )
    .await;

    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": "racing the in-flight start" }),
    )
    .await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body={body}");
    assert!(
        boot.state.harness.get(&runtime_id).is_none(),
        "in-flight start must not be raced by lazy recovery"
    );
}

/// #649 review round 1 (finding 2) — a NULL row `thread_id` with a snapshot
/// carrying `last_thread_id` is recoverable: the route mirrors boot
/// recovery's snapshot fallback instead of 409ing rows boot would revive.
#[tokio::test]
async fn send_planner_input_null_thread_snapshot_fallback_recovers() {
    let boot = boot_fake_running().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Planner).await;
    let thread_id = format!("thread-{}", new_id());
    let runtime_id =
        seed_active_planner_runtime_row(&boot, &card, None, Some(idle_snapshot_value(&thread_id)))
            .await;

    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": "snapshot-thread follow-up" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["runtime_id"], json!(runtime_id.as_str()));
    assert!(
        boot.state.harness.get(&runtime_id).is_some(),
        "snapshot last_thread_id fallback must recover the harness"
    );

    if let Some(handle) = boot.state.harness.remove(&runtime_id) {
        handle.shutdown().await.unwrap();
    }
}

/// #649 review round 2 — a blank/whitespace row `thread_id` must not defeat
/// the snapshot `last_thread_id` fallback: `Some("  ")` would win the `.or()`
/// chain in `spawn_recovered_harness` and the recovered harness would issue
/// turns against an empty thread. The helper normalizes blanks to `None`.
#[tokio::test]
async fn send_planner_input_blank_thread_snapshot_fallback_recovers() {
    let boot = boot_fake_running().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Planner).await;
    let thread_id = format!("thread-{}", new_id());
    let runtime_id = seed_active_planner_runtime_row(
        &boot,
        &card,
        Some("  ".to_string()),
        Some(idle_snapshot_value(&thread_id)),
    )
    .await;

    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": "blank-thread follow-up" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["runtime_id"], json!(runtime_id.as_str()));
    let handle = boot
        .state
        .harness
        .get(&runtime_id)
        .expect("blank row thread_id must fall back to the snapshot's last_thread_id");
    assert_eq!(
        handle.thread_id_for_test().await.as_deref(),
        Some(thread_id.as_str()),
        "recovered harness must use the snapshot thread, not the blank row value"
    );

    if let Some(handle) = boot.state.harness.remove(&runtime_id) {
        handle.shutdown().await.unwrap();
    }
}

/// #649 review round 1 (finding 3) — row-intrinsic dormancy outranks the
/// daemon liveness probe: an unrecoverable row 409s (Reset is the answer)
/// even while the daemon is down, instead of a misleading 503 "retry".
#[tokio::test]
async fn send_planner_input_dormant_row_daemon_down_409_not_503() {
    let boot = boot().await; // stub daemon: is_running() == false
    let card = seed_codex_card_with_role(&boot, CardRole::Planner).await;
    let runtime_id = seed_active_planner_runtime_row(
        &boot,
        &card,
        None,
        Some(idle_snapshot_value_without_thread()),
    )
    .await;

    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": "wake up" }),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        body["code"],
        json!("planner_harness_dormant"),
        "body={body}"
    );
    assert!(
        boot.state.harness.get(&runtime_id).is_none(),
        "dormant row must not be recovered even when daemon is down"
    );
}

/// #649 i2 hardening 2 — a corrupt/unknown snapshot shape degrades to the
/// typed 409 dormant instead of panicking inside `from_value_strict`.
#[tokio::test]
async fn send_planner_input_corrupt_snapshot_409_dormant() {
    let boot = boot_fake_running().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Planner).await;
    let thread_id = format!("thread-{}", new_id());
    let runtime_id = seed_active_planner_runtime_row(
        &boot,
        &card,
        Some(thread_id),
        Some(json!({ "mode": "harness", "schema_version": 999, "phase": "idle" })),
    )
    .await;

    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": "wake up" }),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        body["code"],
        json!("planner_harness_dormant"),
        "body={body}"
    );
    assert!(
        boot.state.harness.get(&runtime_id).is_none(),
        "corrupt-snapshot row must not be recovered into the registry"
    );
}

/// #649 i2 — recovery is gated on the shared codex app-server being up:
/// a registry miss while the daemon is down is a 503 (retryable), not a
/// silently-wedged recovered harness and not a dormant 409.
#[tokio::test]
async fn send_planner_input_registry_miss_daemon_down_503() {
    let boot = boot().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Planner).await;
    let thread_id = format!("thread-{}", new_id());
    let runtime_id = seed_active_planner_runtime_row(
        &boot,
        &card,
        Some(thread_id.clone()),
        Some(idle_snapshot_value(&thread_id)),
    )
    .await;

    let (status, body) = post_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({ "text": "wake up" }),
    )
    .await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body={body}");
    assert_eq!(body["code"], json!("service_unavailable"), "body={body}");
    assert!(
        boot.state.harness.get(&runtime_id).is_none(),
        "daemon-down miss must not spawn a harness"
    );
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
    let card = seed_shared_worker_card(&boot, "delete", "thread-delete").await;
    boot.state
        .shared_codex_appserver
        .set_active_turn_for_test("thread-delete", "turn-delete");

    let status = delete_empty(boot.app.clone(), &format!("/api/cards/{}", card.id)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let rows = request_lines_containing(&capture_file, "turn/interrupt", 1).await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
    }

    assert!(
        has_interrupt(&rows, "thread-delete", "turn-delete"),
        "card delete must interrupt active shared turn: {rows:?}"
    );
}

#[tokio::test]
async fn shared_track_delete_interrupts_all_child_turns() {
    let _guard = ENV_LOCK.lock().await;
    let capture = TempDir::new().unwrap();
    let capture_file = capture.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture_file);
    }
    let boot = boot_shared().await;
    let card_a = seed_shared_worker_card(&boot, "track-a", "thread-track-a").await;
    let card_b = seed_shared_worker_card(&boot, "track-b", "thread-track-b").await;
    boot.state
        .shared_codex_appserver
        .set_active_turn_for_test("thread-track-a", "turn-track-a");
    boot.state
        .shared_codex_appserver
        .set_active_turn_for_test("thread-track-b", "turn-track-b");

    let status = delete_empty(boot.app.clone(), &format!("/api/tracks/{}", boot.track_id)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let rows = request_lines_containing(&capture_file, "turn/interrupt", 2).await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
    }

    assert!(
        has_interrupt(&rows, "thread-track-a", "turn-track-a"),
        "track delete must interrupt first active shared turn: {rows:?}"
    );
    assert!(
        has_interrupt(&rows, "thread-track-b", "turn-track-b"),
        "track delete must interrupt second active shared turn: {rows:?}"
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
async fn track_delete_shuts_down_active_planner_harness() {
    let boot = boot_shared().await;
    let area = boot
        .repo
        .area_create(NewArea {
            name: "harness-track-delete".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let (status, body) = post_json(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": area.id,
            "title": "delete harness",
            "cwd": attached_repo_fixture("planner-card-reset-harness-delete"),
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    let track_id = body["id"].as_str().expect("track id").to_string();
    let cards = boot.repo.cards_by_track(&track_id).await.unwrap();
    let planner_card = cards
        .iter()
        .find(|card| card.kind == "codex" && card.payload["planner_harness"] == json!(true))
        .expect("planner harness card");
    let runtime = boot
        .repo
        .session_projection_active_for_card(&planner_card.id.to_string())
        .await
        .unwrap()
        .expect("active planner harness runtime");
    assert!(boot.state.harness.get(&runtime.id).is_some());
    assert_eq!(boot.state.harness.len_active(), 1);

    let status = delete_empty(boot.app.clone(), &format!("/api/tracks/{track_id}")).await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(boot.state.harness.len_active(), 0);
    assert!(
        boot.repo
            .session_projection_by_id(&runtime.id)
            .await
            .unwrap()
            .is_none(),
        "runtime row must cascade with the deleted planner card"
    );
}

#[tokio::test]
async fn acceptance_20_descendant_refusal_preserves_live_track_runtime_and_terminal() {
    let boot = boot_shared().await;
    let area = boot
        .repo
        .area_create(NewArea {
            name: "descendant-refusal-runtime".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let (status, body) = post_json(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": area.id,
            "title": "live parent",
            "cwd": attached_repo_fixture("descendant-refusal-runtime"),
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let parent_id = body["id"].as_str().unwrap().to_string();
    let cards = boot.repo.cards_by_track(&parent_id).await.unwrap();
    let planner_card = cards
        .iter()
        .find(|card| card.payload["planner_harness"] == json!(true))
        .expect("live planner card");
    let runtime = boot
        .repo
        .session_projection_active_for_card(&planner_card.id.to_string())
        .await
        .unwrap()
        .expect("live planner runtime");
    assert!(boot.state.harness.get(&runtime.id).is_some());

    let terminal_card = boot
        .repo
        .card_create(NewCard {
            track_id: parent_id.clone().into(),
            title: Some("live terminal".into()),
            kind: "terminal".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .unwrap();
    // #1439: socket 不能挂在 `$TMPDIR` 下的 TempDir 上 —— 自托管 runner 的
    // TMPDIR 已经 49 字节，再加 TempDir 的 `/.tmpXXXXXX` 和
    // `terminal-<uuid>.sock` 就越过 sun_path 的 107 字节上限。
    let socket_dir = calm_test_sockets::socket_dir("t");
    let socket_path =
        calm_test_sockets::socket_path(socket_dir.path(), &format!("terminal-{}.sock", new_id()));
    let listener = calm_test_sockets::bind(&socket_path);
    drop(listener);
    let mut process = tokio::process::Command::new("sh")
        .arg("-c")
        .arg("trap 'rm -f \"$1\"; exit 0' TERM; while :; do sleep 0.05; done")
        .arg("terminal-fixture")
        .arg(&socket_path)
        .spawn()
        .unwrap();
    let pid = process.id().expect("terminal fixture pid");
    let terminal = boot
        .repo
        .terminal_create(NewTerminal {
            card_id: terminal_card.id.clone(),
            program: "terminal-fixture".into(),
            cwd: "/tmp".into(),
            env: json!({}),
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    boot.repo
        .terminal_set_pid(&terminal.id, Some(pid))
        .await
        .unwrap();

    let child = boot
        .repo
        .track_create(NewTrack {
            area_id: area.id,
            title: "child".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET parent_track_id=?1 WHERE id=?2")
        .bind(&parent_id)
        .bind(child.id.as_str())
        .execute(boot.repo.pool())
        .await
        .unwrap();

    let before_counts: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM tracks), (SELECT count(*) FROM cards), \
                (SELECT count(*) FROM terminals), (SELECT count(*) FROM worker_sessions)",
    )
    .fetch_one(boot.repo.pool())
    .await
    .unwrap();
    let status = delete_empty(boot.app.clone(), &format!("/api/tracks/{parent_id}")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    tokio::time::sleep(Duration::from_millis(150)).await;

    assert!(
        process.try_wait().unwrap().is_none(),
        "terminal process changed"
    );
    assert!(socket_path.exists(), "terminal socket changed");
    assert!(
        boot.state.harness.get(&runtime.id).is_some(),
        "harness registry changed"
    );
    assert!(boot.repo.track_get(&parent_id).await.unwrap().is_some());
    assert!(
        boot.repo
            .track_get(child.id.as_str())
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        boot.repo
            .terminal_get_by_card(terminal_card.id.as_str())
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        boot.repo
            .session_projection_by_id(&runtime.id)
            .await
            .unwrap()
            .is_some()
    );
    let after_counts: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM tracks), (SELECT count(*) FROM cards), \
                (SELECT count(*) FROM terminals), (SELECT count(*) FROM worker_sessions)",
    )
    .fetch_one(boot.repo.pool())
    .await
    .unwrap();
    assert_eq!(after_counts, before_counts, "DB rows changed on refusal");

    process.kill().await.ok();
    process.wait().await.ok();
    std::fs::remove_file(&socket_path).ok();
    if let Some(harness) = boot.state.harness.remove(&runtime.id) {
        harness.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn reset_planner_card_returns_404_for_unknown_card() {
    let boot = boot().await;

    let (status, body) = post_empty(boot.app, "/api/cards/card_does_not_exist/planner/reset").await;

    assert_eq!(status, StatusCode::NOT_FOUND, "body={body}");
}

#[tokio::test]
async fn reset_planner_card_rejects_worker_codex_card() {
    let boot = boot().await;
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: boot.track_id.clone().into(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .expect("worker codex card");
    boot.state.card_role_cache.insert(
        card.id.clone(),
        CardRole::Worker,
        TrackId::from(boot.track_id.clone()),
    );

    let (status, body) =
        post_empty(boot.app, &format!("/api/cards/{}/planner/reset", card.id)).await;

    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
}

#[tokio::test]
async fn reset_planner_card_rejects_wrong_kind_card() {
    let boot = boot().await;
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: boot.track_id.clone().into(),
            title: None,
            kind: "report".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .expect("report card");
    boot.state.card_role_cache.insert(
        card.id.clone(),
        CardRole::Worker,
        TrackId::from(boot.track_id.clone()),
    );

    let (status, body) =
        post_empty(boot.app, &format!("/api/cards/{}/planner/reset", card.id)).await;

    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
}

#[tokio::test]
async fn reset_planner_card_restarts_terminal_less_harness_card() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_shared().await;
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: TrackId::from(boot.track_id.clone()),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "schemaVersion": 1,
                "planner_harness": true
            }),
        })
        .await
        .unwrap();
    boot.state.card_role_cache.insert(
        card.id.clone(),
        CardRole::Planner,
        TrackId::from(boot.track_id.clone()),
    );
    let old_runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(3, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some("thread-old".into());
    let mut tx = boot.repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: old_runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some("thread-old".into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/reset", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["card_id"], json!(card.id.as_str()));
    assert_eq!(body["terminal_id"], json!(""));
    assert_eq!(body["new_thread_id"], json!("fake-thread-0001"));
    assert_eq!(body["track"]["id"], json!(boot.track_id));
    assert!(
        boot.repo
            .terminal_get_by_card(card.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        runtime_by_id_tx_snapshot(boot.repo.as_ref(), &old_runtime_id)
            .await
            .unwrap()
            .status,
        WorkerSessionState::Superseded
    );
    let active = boot
        .repo
        .session_projection_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("new active runtime");
    assert_eq!(active.thread_id.as_deref(), Some("fake-thread-0001"));
    assert!(boot.state.harness.get(&active.id).is_some());
    if let Some(handle) = boot.state.harness.remove(&active.id) {
        handle.shutdown().await.unwrap();
    }
}

/// #649 followup (codex-review P2 on #660) — the corrupt-snapshot shape that
/// degrades `/planner/input` to the typed 409 dormant must NOT panic the
/// recommended Reset: `planner-harness-start` gates snapshot inheritance on
/// `is_harness_snapshot_value` and starts a fresh session, discarding the
/// corrupt row's queued observations.
#[tokio::test]
async fn reset_planner_card_tolerates_corrupt_dormant_snapshot() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_shared().await;
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: TrackId::from(boot.track_id.clone()),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "schemaVersion": 1,
                "planner_harness": true
            }),
        })
        .await
        .unwrap();
    boot.state.card_role_cache.insert(
        card.id.clone(),
        CardRole::Planner,
        TrackId::from(boot.track_id.clone()),
    );
    let old_runtime_id = new_id();
    let mut tx = boot.repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: old_runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some("thread-corrupt".into()),
            session_id: None,
            active_turn_id: None,
            // Same corrupt shape that 409s /planner/input: harness mode but an
            // unknown schema_version that `from_value_strict` would panic on.
            handle_state_json: Some(json!({
                "mode": "harness",
                "schema_version": 999,
                "phase": "idle"
            })),
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/reset", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["card_id"], json!(card.id.as_str()));
    assert_eq!(body["new_thread_id"], json!("fake-thread-0001"));
    assert_eq!(
        runtime_by_id_tx_snapshot(boot.repo.as_ref(), &old_runtime_id)
            .await
            .unwrap()
            .status,
        WorkerSessionState::Superseded
    );
    let active = boot
        .repo
        .session_projection_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("new active runtime");
    assert_ne!(active.id, old_runtime_id);
    assert_eq!(active.thread_id.as_deref(), Some("fake-thread-0001"));
    let new_snapshot = HarnessSnapshot::from_value_strict(
        active
            .handle_state_json
            .clone()
            .expect("new runtime snapshot"),
    );
    // Fresh session: nothing inherited from the corrupt row — watermark is 0
    // and the queue is empty. #1211 S1 removed the title→goal seeding, so this
    // asserts `is_empty()` rather than `.all(is TrackGoal)`: the latter is
    // vacuously true on an empty queue and would pass no matter what reset put
    // there.
    assert_eq!(
        new_snapshot.push_watermark, 0,
        "corrupt inherited snapshot must be discarded, not carried over"
    );
    assert!(
        new_snapshot.pending_queue.is_empty(),
        "fresh queue must be empty — reset seeds no observation: {:?}",
        new_snapshot.pending_queue
    );
    assert!(boot.state.harness.get(&active.id).is_some());
    if let Some(handle) = boot.state.harness.remove(&active.id) {
        handle.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn reset_planner_card_preserves_runtime_pending_queue_and_push_watermark() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_shared().await;
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: TrackId::from(boot.track_id.clone()),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "schemaVersion": 1,
                "planner_harness": true
            }),
        })
        .await
        .unwrap();
    boot.state.card_role_cache.insert(
        card.id.clone(),
        CardRole::Planner,
        TrackId::from(boot.track_id.clone()),
    );
    let old_runtime_id = new_id();
    let thread_id = "thread-old-watermark".to_string();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.clone());
    let mut tx = boot.repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: old_runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(thread_id.clone()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let repo_dyn: Arc<dyn Repo> = boot.repo.clone();
    let harness = PlannerHarness::run(PlannerHarnessParams {
        runtime_id: old_runtime_id.clone(),
        track_id: card.track_id.clone(),
        card_id: card.id.clone(),
        thread_id: Some(thread_id),
        repo: repo_dyn,
        events: boot.state.events.clone(),
        card_role_cache: boot.state.card_role_cache.clone(),
        track_area_cache: boot.state.track_area_cache.clone(),
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
                Observation::TrackGoal {
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
        .session_projection_by_id(&old_runtime_id)
        .await
        .unwrap()
        .unwrap();
    let old_snapshot = HarnessSnapshot::from_value_strict(old_runtime.handle_state_json.unwrap());
    assert_eq!(old_snapshot.push_watermark, 3);
    assert_eq!(old_snapshot.pending_queue.len(), 3);

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/reset", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    let active = boot
        .repo
        .session_projection_active_for_card(&card.id.to_string())
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
async fn reset_planner_card_spawn_failure_restores_old_runtime_after_old_harness_teardown() {
    let _guard = ENV_LOCK.lock().await;
    let mut boot = boot_shared().await;
    install_failing_planner_start_runtime(&mut boot);
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: TrackId::from(boot.track_id.clone()),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "schemaVersion": 1,
                "planner_harness": true
            }),
        })
        .await
        .unwrap();
    boot.state.card_role_cache.insert(
        card.id.clone(),
        CardRole::Planner,
        TrackId::from(boot.track_id.clone()),
    );
    let old_runtime_id = new_id();
    let old_thread_id = "thread-old-spawn-failure".to_string();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(old_thread_id.clone());
    let mut tx = boot.repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: old_runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(old_thread_id.clone()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let repo_dyn: Arc<dyn Repo> = boot.repo.clone();
    let old_harness = PlannerHarness::run(PlannerHarnessParams {
        runtime_id: old_runtime_id.clone(),
        track_id: card.track_id.clone(),
        card_id: card.id.clone(),
        thread_id: Some(old_thread_id.clone()),
        repo: repo_dyn,
        events: boot.state.events.clone(),
        card_role_cache: boot.state.card_role_cache.clone(),
        track_area_cache: boot.state.track_area_cache.clone(),
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
        &format!("/api/cards/{}/planner/reset", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body={body}");
    let old_after = boot
        .repo
        .session_projection_by_id(&old_runtime_id)
        .await
        .unwrap()
        .expect("old runtime row remains");
    assert_eq!(old_after.status, WorkerSessionState::Idle);
    let active = boot
        .repo
        .session_projection_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("old runtime remains active");
    assert_eq!(active.id, old_runtime_id);
    assert!(
        boot.state.harness.get(&old_runtime_id).is_none(),
        "old harness is torn down before the replacement spawn can fail"
    );

    let new_rows: Vec<(String, String)> = sqlx::query_as(
        r#"SELECT id, state
             FROM worker_sessions
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
async fn reset_planner_card_recovers_inert_harness_card_without_active_runtime() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_shared().await;
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: TrackId::from(boot.track_id.clone()),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "schemaVersion": 1,
                "planner_harness": true
            }),
        })
        .await
        .unwrap();
    boot.state.card_role_cache.insert(
        card.id.clone(),
        CardRole::Planner,
        TrackId::from(boot.track_id.clone()),
    );
    assert!(
        boot.repo
            .session_projection_active_for_card(&card.id.to_string())
            .await
            .unwrap()
            .is_none()
    );

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/reset", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["card_id"], json!(card.id.as_str()));
    assert_eq!(body["terminal_id"], json!(""));
    assert_eq!(body["new_thread_id"], json!("fake-thread-0001"));
    assert_eq!(body["track"]["id"], json!(boot.track_id));
    assert!(
        boot.repo
            .terminal_get_by_card(card.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    let active = boot
        .repo
        .session_projection_active_for_card(&card.id.to_string())
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
async fn reset_planner_card_failure_keeps_old_runtime_when_shared_daemon_down() {
    let boot = boot().await;
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: TrackId::from(boot.track_id.clone()),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "schemaVersion": 1,
                "codex_source": "shared",
                "planner_harness": true,
                "codex_thread_id": "thread-old"
            }),
        })
        .await
        .unwrap();
    boot.state.card_role_cache.insert(
        card.id.clone(),
        CardRole::Planner,
        TrackId::from(boot.track_id.clone()),
    );
    let old_runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some("thread-old".into());
    let mut tx = boot.repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: old_runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some("thread-old".into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let before = boot
        .repo
        .session_projection_by_id(&old_runtime_id)
        .await
        .unwrap()
        .unwrap();

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/reset", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body={body}");
    let after = boot
        .repo
        .session_projection_by_id(&old_runtime_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after, before);
    let active = boot
        .repo
        .session_projection_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("old runtime remains active");
    assert_eq!(active.id, old_runtime_id);
    assert_eq!(active.thread_id.as_deref(), Some("thread-old"));
}

// ---------------------------------------------------------------------------
// Issue #1211 S1 — the track title is no longer seeded as the planner agent's goal
// ---------------------------------------------------------------------------
//
// Before #1211 the single new-track input box was both the track's title and the
// statement of what the track should do, and three places turned it back into an
// intent: `routes::tracks::start_planner_harness` (the `planner-harness-start`
// payload's `goal`), the planner card's `payload.prompt` at create, and
// `/api/cards/{id}/planner/reset` for the `Planner` profile. #1211 moves the intent
// out of the title — a track created without one starts unnamed and the planner
// agent names it once the conversation has established what the work is — so
// all three must stop seeding.
//
// These tests assert on the persisted `planner-harness-start` payload rather than
// only on the live queue: the payload row is what the route decided, and it
// cannot be drained out from under the assertion by a harness that consumed the
// observation before we looked.

/// Every `planner-harness-start` operation payload recorded for `card_id`,
/// oldest first. `payload_json` is the exact `PlannerHarnessStartOperationPayload`
/// the route submitted.
async fn planner_harness_start_payloads(repo: &SqlxRepo, card_id: &str) -> Vec<Value> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT payload_json FROM operations WHERE kind = 'planner-harness-start' ORDER BY rowid",
    )
    .fetch_all(repo.pool())
    .await
    .unwrap();
    rows.into_iter()
        .filter_map(|row| serde_json::from_str::<Value>(&row).ok())
        .filter(|payload| {
            payload
                .get("request")
                .unwrap_or(payload)
                // The PERSISTED key, which #1316 S3 froze at the old spelling —
                // see `PlannerHarnessStartOperationPayload`'s doc comment for
                // why renaming it would be a permanent 409.
                .get("spec_card_id")
                .and_then(Value::as_str)
                == Some(card_id)
        })
        .collect()
}

fn assert_no_seeded_goal(payloads: &[Value], what: &str) {
    assert!(
        !payloads.is_empty(),
        "{what}: expected at least one planner-harness-start payload to inspect"
    );
    for payload in payloads {
        let request = payload.get("request").unwrap_or(payload);
        assert_eq!(
            request.get("goal"),
            Some(&Value::Null),
            "{what}: planner-harness-start must carry no goal; payload = {payload}"
        );
    }
}

async fn planner_card_of_track(boot: &Boot, track_id: &str) -> Card {
    let cards = boot.repo.cards_by_track(track_id).await.unwrap();
    cards
        .into_iter()
        .find(|card| card.kind == "codex" && card.payload["planner_harness"] == json!(true))
        .expect("planner harness card")
}

/// Assert the freshly started harness for `card_id` holds no observation at
/// all — neither in memory nor in the snapshot persisted on its session row.
async fn assert_harness_queue_empty(boot: &Boot, card_id: &str, what: &str) {
    let runtime = boot
        .repo
        .session_projection_active_for_card(&card_id.to_string())
        .await
        .unwrap()
        .expect("active planner harness runtime");
    let snapshot = HarnessSnapshot::from_value_strict(
        runtime
            .handle_state_json
            .clone()
            .expect("runtime snapshot json"),
    );
    assert!(
        snapshot.pending_queue.is_empty(),
        "{what}: persisted start snapshot must hold no observation; got {:?}",
        snapshot.pending_queue
    );
    let handle = boot
        .state
        .harness
        .get(&runtime.id)
        .expect("live harness handle");
    let queued = handle.pending_queue_for_test().await;
    assert!(
        queued.is_empty(),
        "{what}: live harness queue must hold no observation; got {queued:?}"
    );
}

/// #1211 S1 (1) — a create request may omit `title` entirely, and the harness
/// that comes up behind it starts with an empty observation queue.
#[tokio::test]
async fn track_create_without_title_seeds_no_track_goal() {
    let boot = boot_shared().await;
    let area = boot
        .repo
        .area_create(NewArea {
            name: "no-goal-untitled".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let (status, body) = post_json(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": area.id,
            "cwd": attached_repo_fixture("issue-1211-untitled"),
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    let track_id = body["id"].as_str().expect("track id").to_string();
    let planner_card = planner_card_of_track(&boot, &track_id).await;
    assert_no_seeded_goal(
        &planner_harness_start_payloads(boot.repo.as_ref(), planner_card.id.as_str()).await,
        "create without title",
    );
    assert_harness_queue_empty(&boot, planner_card.id.as_str(), "create without title").await;
}

/// #1211 S1 (2) — the deletion itself. A create carrying a perfectly good
/// non-empty title must ALSO produce no `Observation::TrackGoal`: the title is
/// simply not the track's intent any more. Restore the old
/// `goal: (!goal.is_empty()).then_some(goal)` in
/// `routes::tracks::start_planner_harness` and this test is the one that fails.
#[tokio::test]
async fn track_create_with_title_seeds_no_track_goal() {
    let boot = boot_shared().await;
    let area = boot
        .repo
        .area_create(NewArea {
            name: "no-goal-titled".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let title = "ship the rename tool";
    let (status, body) = post_json(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": area.id,
            "title": title,
            "cwd": attached_repo_fixture("issue-1211-titled"),
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    let track_id = body["id"].as_str().expect("track id").to_string();
    // The title itself is untouched — only its promotion to an intent is gone.
    assert_eq!(
        boot.repo
            .track_get(&track_id)
            .await
            .unwrap()
            .expect("track row")
            .title,
        title
    );
    let planner_card = planner_card_of_track(&boot, &track_id).await;
    assert_no_seeded_goal(
        &planner_harness_start_payloads(boot.repo.as_ref(), planner_card.id.as_str()).await,
        "create with non-empty title",
    );
    assert_harness_queue_empty(
        &boot,
        planner_card.id.as_str(),
        "create with non-empty title",
    )
    .await;
}

/// #1211 S1 (3) — `/planner/reset` restarts the `Planner` profile, which was the
/// second place that re-seeded the title. A restarted planner session must come
/// up as silent as a fresh one.
#[tokio::test]
async fn planner_reset_seeds_no_track_goal() {
    let boot = boot_shared().await;
    let area = boot
        .repo
        .area_create(NewArea {
            name: "no-goal-reset".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let (status, body) = post_json(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": area.id,
            "title": "reset keeps quiet",
            "cwd": attached_repo_fixture("issue-1211-reset"),
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track_id = body["id"].as_str().expect("track id").to_string();
    let planner_card = planner_card_of_track(&boot, &track_id).await;

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/reset", planner_card.id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    // Two payloads by now (create + reset); both must be goal-free.
    let payloads =
        planner_harness_start_payloads(boot.repo.as_ref(), planner_card.id.as_str()).await;
    assert_eq!(
        payloads.len(),
        2,
        "expected the create payload and the reset payload: {payloads:?}"
    );
    assert_no_seeded_goal(&payloads, "planner reset");
    assert_harness_queue_empty(&boot, planner_card.id.as_str(), "planner reset").await;
}
