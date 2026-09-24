//! `POST /api/cards/{id}/planner/interrupt` and `GET /api/cards/{id}/planner/run` route tests.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::event::EventBus;
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, HarnessState, IssuingKind, PlannerHarness,
    PlannerHarnessParams,
};
use calm_server::ids::TrackId;
use calm_server::model::{Card, CardRole, NewArea, NewCard, NewTrack, new_id, now_ms};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::common;

struct Boot {
    app: axum::Router,
    state: AppState,
    repo: Arc<SqlxRepo>,
    track_id: String,
    _tmp: TempDir,
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
            name: "planner-card-interrupt".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "interrupt route".into(),
            sort: None,
            cwd: "/tmp/planner-card-interrupt".into(),
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
            std::env::temp_dir().join("calm-plugins-data-planner-card-interrupt"),
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
    // The fixture fake shared app-server records `turn/interrupt` calls (`interrupted_turns_for_test`), and
    // `with_shared_codex_appserver` rebuilds the operation runtime so the interrupt adapter shares this state's harness registry.
    let shared = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let state = state.with_shared_codex_appserver(shared);
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

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
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

async fn seed_codex_card_with_role(boot: &Boot, role: CardRole) -> Card {
    let mut payload = json!({
        "schemaVersion": 1,
        "planner_harness": role == CardRole::Planner
    });
    if role == CardRole::Planner {
        payload["planner_provider"] = json!("codex");
    }
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: TrackId::from(boot.track_id.clone()),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload,
        })
        .await
        .expect("seed codex card");
    // The persisted role is what recovery reads; the cache alone is not a Planner card.
    sqlx::query("UPDATE cards SET role = ?1 WHERE id = ?2")
        .bind(role.as_db_str())
        .bind(card.id.as_str())
        .execute(boot.repo.pool())
        .await
        .expect("persist seeded card role");
    boot.state
        .card_role_cache
        .insert(card.id.clone(), role, TrackId::from(boot.track_id.clone()));
    card
}

async fn seed_active_planner_runtime_row(boot: &Boot, card: &Card) -> (String, String) {
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
    (runtime_id, thread_id)
}

/// Seed a live planner harness (idle) registered under an active runtime row.
async fn seed_live_planner_harness(boot: &Boot) -> (Card, String, String, PlannerHarness) {
    let card = seed_codex_card_with_role(boot, CardRole::Planner).await;
    let (runtime_id, thread_id) = seed_active_planner_runtime_row(boot, &card).await;

    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.clone());
    let repo_dyn: Arc<dyn Repo> = boot.repo.clone();
    let harness = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: runtime_id.clone(),
        track_id: card.track_id.clone(),
        card_id: card.id.clone(),
        thread_id: Some(thread_id.clone()),
        repo: repo_dyn,
        events: boot.state.events.clone(),
        card_role_cache: boot.state.card_role_cache.clone(),
        track_area_cache: boot.state.track_area_cache.clone(),
        backend: boot.state.shared_codex_appserver.clone().into(),
        config: HarnessConfig::default(),
        snapshot,
    });
    boot.state
        .harness
        .insert(runtime_id.clone(), harness.clone());
    (card, runtime_id, thread_id, harness)
}

async fn shutdown_seeded_harness(boot: &Boot, runtime_id: &str, harness: PlannerHarness) {
    if let Some(handle) = boot.state.harness.remove(runtime_id) {
        handle.shutdown().await.unwrap();
    } else {
        harness.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn interrupt_running_turn_issues_interrupt() {
    let boot = boot().await;
    let (card, runtime_id, thread_id, harness) = seed_live_planner_harness(&boot).await;
    harness
        .set_state_for_test(HarnessState::TurnRunning {
            turn_id: "T1".into(),
            started_at: Instant::now(),
        })
        .await;

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/interrupt", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["card_id"], json!(card.id.as_str()));
    assert_eq!(body["worker_session_id"], json!(runtime_id.as_str()));
    assert_eq!(body["stopped"], json!(true), "body={body}");
    assert!(
        boot.state
            .shared_codex_appserver
            .interrupted_turns_for_test()
            .contains(&(thread_id.clone(), "T1".to_string())),
        "expected turn/interrupt at ({thread_id}, T1); got {:?}",
        boot.state
            .shared_codex_appserver
            .interrupted_turns_for_test()
    );

    shutdown_seeded_harness(&boot, &runtime_id, harness).await;
}

/// During `IssuingTurn` the app-server does not yet know the active turn, so `issue_interrupt` no-ops; `stopped: true` would narrate a false "Turn stopped".
#[tokio::test]
async fn interrupt_issuing_turn_window_reports_not_stopped() {
    let boot = boot().await;
    let (card, runtime_id, _thread_id, harness) = seed_live_planner_harness(&boot).await;
    harness
        .set_state_for_test(HarnessState::Issuing {
            since: Instant::now(),
            kind: IssuingKind::TurnStart,
        })
        .await;

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/interrupt", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["stopped"], json!(false), "body={body}");
    assert_eq!(body["worker_session_id"], json!(runtime_id.as_str()));
    assert!(
        boot.state
            .shared_codex_appserver
            .interrupted_turns_for_test()
            .is_empty(),
        "no active turn id is known yet, so nothing can be interrupted"
    );

    shutdown_seeded_harness(&boot, &runtime_id, harness).await;
}

/// The interrupt is dispatched best-effort, but only `TurnRunning` guarantees a target, so the route keeps `stopped: false`.
#[tokio::test]
async fn interrupt_issuing_turn_dispatches_best_effort() {
    let boot = boot().await;
    let (card, runtime_id, thread_id, harness) = seed_live_planner_harness(&boot).await;
    boot.state
        .shared_codex_appserver
        .set_active_turn_for_test(&thread_id, "T2");
    harness
        .set_state_for_test(HarnessState::Issuing {
            since: Instant::now(),
            kind: IssuingKind::TurnStart,
        })
        .await;

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/interrupt", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["stopped"], json!(false), "body={body}");
    assert!(
        boot.state
            .shared_codex_appserver
            .interrupted_turns_for_test()
            .contains(&(thread_id.clone(), "T2".to_string())),
        "expected best-effort turn/interrupt at ({thread_id}, T2); got {:?}",
        boot.state
            .shared_codex_appserver
            .interrupted_turns_for_test()
    );

    shutdown_seeded_harness(&boot, &runtime_id, harness).await;
}

/// The harness's own `issue_interrupt` ignores interrupts with no active turn, so an error here would only punish a harmless Esc press.
#[tokio::test]
async fn interrupt_idle_harness_is_a_200_noop() {
    let boot = boot().await;
    let (card, runtime_id, _thread_id, harness) = seed_live_planner_harness(&boot).await;

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/interrupt", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["stopped"], json!(false), "body={body}");
    assert_eq!(body["worker_session_id"], json!(runtime_id.as_str()));
    assert!(
        boot.state
            .shared_codex_appserver
            .interrupted_turns_for_test()
            .is_empty(),
        "idle stop must not dispatch turn/interrupt"
    );

    shutdown_seeded_harness(&boot, &runtime_id, harness).await;
}

/// Same contract as `/planner/input`: steer the user to Reset, don't 404.
#[tokio::test]
async fn interrupt_without_runtime_409_dormant() {
    let boot = boot().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Planner).await;

    let (status, body) = post_empty(
        boot.app,
        &format!("/api/cards/{}/planner/interrupt", card.id),
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

/// Unlike `/planner/input` there is no lazy recovery here: a freshly recovered harness has no running turn to stop.
#[tokio::test]
async fn interrupt_registry_miss_409_dormant() {
    let boot = boot().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Planner).await;
    seed_active_planner_runtime_row(&boot, &card).await;

    let (status, body) = post_empty(
        boot.app,
        &format!("/api/cards/{}/planner/interrupt", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        body["code"],
        json!("planner_harness_dormant"),
        "body={body}"
    );
}

#[tokio::test]
async fn get_planner_run_running_turn_reports_phase() {
    let boot = boot().await;
    let (card, runtime_id, _thread_id, harness) = seed_live_planner_harness(&boot).await;
    harness
        .set_state_for_test(HarnessState::TurnRunning {
            turn_id: "T1".into(),
            started_at: Instant::now(),
        })
        .await;

    let (status, body) = get_json(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/run", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["card_id"], json!(card.id.as_str()));
    assert_eq!(body["worker_session_id"], json!(runtime_id.as_str()));
    assert_eq!(body["phase"], json!("turn_running"), "body={body}");

    shutdown_seeded_harness(&boot, &runtime_id, harness).await;
}

/// Dormancy is not an error for a read.
#[tokio::test]
async fn get_planner_run_without_runtime_returns_nulls() {
    let boot = boot().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Planner).await;

    let (status, body) = get_json(boot.app, &format!("/api/cards/{}/planner/run", card.id)).await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["card_id"], json!(card.id.as_str()));
    assert_eq!(body["worker_session_id"], json!(null), "body={body}");
    assert_eq!(body["phase"], json!(null), "body={body}");
}

#[tokio::test]
async fn get_planner_run_registry_miss_returns_nulls() {
    let boot = boot().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Planner).await;
    seed_active_planner_runtime_row(&boot, &card).await;

    let (status, body) = get_json(boot.app, &format!("/api/cards/{}/planner/run", card.id)).await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["worker_session_id"], json!(null), "body={body}");
    assert_eq!(body["phase"], json!(null), "body={body}");
}

#[tokio::test]
async fn get_planner_run_non_planner_card_403() {
    let boot = boot().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Worker).await;

    let (status, body) = get_json(boot.app, &format!("/api/cards/{}/planner/run", card.id)).await;

    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| error.contains("not a planner codex card")),
        "body={body}"
    );
}

#[tokio::test]
async fn interrupt_non_planner_card_403() {
    let boot = boot().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Worker).await;

    let (status, body) = post_empty(
        boot.app,
        &format!("/api/cards/{}/planner/interrupt", card.id),
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
