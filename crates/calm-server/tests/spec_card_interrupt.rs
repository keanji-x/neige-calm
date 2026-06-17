//! Issue #668 — `POST /api/cards/{id}/spec/interrupt` route tests.
//!
//! Contract under test:
//! - a running turn gets an interrupt dispatched at it (asserted via the
//!   fake shared app-server's `interrupted_turns_for_test` hook) and the
//!   route answers `200 { stopped: true }`;
//! - stopping when no turn is running is a graceful no-op:
//!   `200 { stopped: false }`, nothing dispatched;
//! - stopping while a `turn/start` is still in flight (`IssuingTurn`)
//!   answers `stopped: false` — the interrupt is dispatched best-effort
//!   (it lands only when the app-server already knows the active turn),
//!   so the route must not promise the turn was stopped;
//! - no active runtime row, or an active row with no registered harness,
//!   is the typed 409 `spec_harness_dormant` (same code as `/spec/input`).
//!
//! Also covers the read sibling `GET /api/cards/{id}/spec/run` (#668 fix):
//! same guard chain, but dormancy is a normal `{runtime_id: null,
//! phase: null}` answer instead of a 409 — the client uses it to seed its
//! initial phase when a page opens mid-turn.

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
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, HarnessState, IssuingKind, SpecHarness,
    SpecHarnessParams,
};
use calm_server::ids::WaveId;
use calm_server::model::{Card, CardRole, NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

mod common;

struct Boot {
    app: axum::Router,
    state: AppState,
    repo: Arc<SqlxRepo>,
    wave_id: String,
    _tmp: TempDir,
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
            name: "spec-card-interrupt".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "interrupt route".into(),
            sort: None,
            cwd: "/tmp/spec-card-interrupt".into(),
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
            std::env::temp_dir().join("calm-plugins-data-spec-card-interrupt"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
        )),
        Arc::new(common::fake_codex_client()),
        Some(card_role_cache),
        Some(wave_cove_cache),
    );
    // Swap in the fixture fake shared app-server: it records
    // `turn/interrupt` calls (`interrupted_turns_for_test`) instead of
    // talking to a real daemon, and `with_shared_codex_appserver` rebuilds
    // the operation runtime so the `spec-harness-interrupt` adapter shares
    // this state's harness registry.
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
        wave_id: wave.id.to_string(),
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
    let card = boot
        .repo
        .card_create(NewCard {
            wave_id: WaveId::from(boot.wave_id.clone()),
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "schemaVersion": 1,
                "spec_harness": role == CardRole::Spec
            }),
        })
        .await
        .expect("seed codex card");
    boot.state
        .card_role_cache
        .insert(card.id.clone(), role, WaveId::from(boot.wave_id.clone()));
    card
}

async fn seed_active_spec_runtime_row(boot: &Boot, card: &Card) -> (String, String) {
    let runtime_id = new_id();
    let thread_id = format!("thread-{runtime_id}");
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.clone());
    let mut tx = boot.repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        RuntimeInit {
            id: runtime_id.clone(),
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
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .expect("seed active spec harness runtime");
    tx.commit().await.unwrap();
    (runtime_id, thread_id)
}

/// Seed a live spec harness (idle) registered under an active runtime row,
/// mirroring `spec_card_reset.rs::seed_live_spec_harness`.
async fn seed_live_spec_harness(boot: &Boot) -> (Card, String, String, SpecHarness) {
    let card = seed_codex_card_with_role(boot, CardRole::Spec).await;
    let (runtime_id, thread_id) = seed_active_spec_runtime_row(boot, &card).await;

    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.clone());
    let repo_dyn: Arc<dyn Repo> = boot.repo.clone();
    let harness = SpecHarness::run(SpecHarnessParams {
        runtime_id: runtime_id.clone(),
        wave_id: card.wave_id.clone(),
        card_id: card.id.clone(),
        thread_id: Some(thread_id.clone()),
        repo: repo_dyn,
        events: boot.state.events.clone(),
        card_role_cache: boot.state.card_role_cache.clone(),
        wave_cove_cache: boot.state.wave_cove_cache.clone(),
        daemon: boot.state.shared_codex_appserver.clone(),
        config: HarnessConfig::default(),
        snapshot,
    });
    boot.state
        .harness
        .insert(runtime_id.clone(), harness.clone());
    (card, runtime_id, thread_id, harness)
}

async fn shutdown_seeded_harness(boot: &Boot, runtime_id: &String, harness: SpecHarness) {
    if let Some(handle) = boot.state.harness.remove(runtime_id) {
        handle.shutdown().await.unwrap();
    } else {
        harness.shutdown().await.unwrap();
    }
}

/// Interrupt while a turn is running: the route dispatches the
/// `spec-harness-interrupt` operation, which issues `turn/interrupt` at the
/// running turn, and answers `stopped: true`.
#[tokio::test]
async fn interrupt_running_turn_issues_interrupt() {
    let boot = boot().await;
    let (card, runtime_id, thread_id, harness) = seed_live_spec_harness(&boot).await;
    harness
        .set_state_for_test(HarnessState::TurnRunning {
            turn_id: "T1".into(),
            started_at: Instant::now(),
        })
        .await;

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/spec/interrupt", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["card_id"], json!(card.id.as_str()));
    assert_eq!(body["runtime_id"], json!(runtime_id.as_str()));
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

/// Interrupt while a `turn/start` is still in flight (`IssuingTurn`) and
/// the shared app-server does NOT yet know the active turn: the harness's
/// `issue_interrupt` resolves no target and no-ops, so the route must
/// answer `stopped: false` — a `stopped: true` here would narrate a false
/// "Turn stopped" while the turn keeps running.
#[tokio::test]
async fn interrupt_issuing_turn_window_reports_not_stopped() {
    let boot = boot().await;
    let (card, runtime_id, _thread_id, harness) = seed_live_spec_harness(&boot).await;
    harness
        .set_state_for_test(HarnessState::Issuing {
            since: Instant::now(),
            kind: IssuingKind::TurnStart,
        })
        .await;

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/spec/interrupt", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["stopped"], json!(false), "body={body}");
    assert_eq!(body["runtime_id"], json!(runtime_id.as_str()));
    assert!(
        boot.state
            .shared_codex_appserver
            .interrupted_turns_for_test()
            .is_empty(),
        "no active turn id is known yet, so nothing can be interrupted"
    );

    shutdown_seeded_harness(&boot, &runtime_id, harness).await;
}

/// Interrupt during `IssuingTurn` when the shared app-server already knows
/// the active turn: the interrupt is still dispatched best-effort, but the
/// route keeps `stopped: false` — only `TurnRunning` guarantees a target.
#[tokio::test]
async fn interrupt_issuing_turn_dispatches_best_effort() {
    let boot = boot().await;
    let (card, runtime_id, thread_id, harness) = seed_live_spec_harness(&boot).await;
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
        &format!("/api/cards/{}/spec/interrupt", card.id),
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

/// Interrupt while the harness is idle: graceful no-op — 200 with
/// `stopped: false`, no `turn/interrupt` dispatched, NOT an error. The
/// harness's own `issue_interrupt` ignores interrupts with no active turn,
/// so an error here would only punish a harmless Esc press.
#[tokio::test]
async fn interrupt_idle_harness_is_a_200_noop() {
    let boot = boot().await;
    let (card, runtime_id, _thread_id, harness) = seed_live_spec_harness(&boot).await;

    let (status, body) = post_empty(
        boot.app.clone(),
        &format!("/api/cards/{}/spec/interrupt", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["stopped"], json!(false), "body={body}");
    assert_eq!(body["runtime_id"], json!(runtime_id.as_str()));
    assert!(
        boot.state
            .shared_codex_appserver
            .interrupted_turns_for_test()
            .is_empty(),
        "idle stop must not dispatch turn/interrupt"
    );

    shutdown_seeded_harness(&boot, &runtime_id, harness).await;
}

/// No active runtime row at all → typed 409 `spec_harness_dormant`, same
/// contract as `/spec/input` (steer the user to Reset, don't 404).
#[tokio::test]
async fn interrupt_without_runtime_409_dormant() {
    let boot = boot().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Spec).await;

    let (status, body) =
        post_empty(boot.app, &format!("/api/cards/{}/spec/interrupt", card.id)).await;

    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], json!("spec_harness_dormant"), "body={body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| error.contains("reset")),
        "dormant body should point at reset: body={body}"
    );
}

/// Active runtime row but no registered harness (post-restart shape) →
/// 409 dormant too. Unlike `/spec/input` there is no lazy recovery here: a
/// freshly recovered harness has no running turn to stop.
#[tokio::test]
async fn interrupt_registry_miss_409_dormant() {
    let boot = boot().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Spec).await;
    seed_active_spec_runtime_row(&boot, &card).await;

    let (status, body) =
        post_empty(boot.app, &format!("/api/cards/{}/spec/interrupt", card.id)).await;

    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], json!("spec_harness_dormant"), "body={body}");
}

/// `GET /spec/run` with a running turn reports the live phase
/// (`turn_running`) and the active runtime id — the production wire value
/// the frontend gates Stop/typing on.
#[tokio::test]
async fn get_spec_run_running_turn_reports_phase() {
    let boot = boot().await;
    let (card, runtime_id, _thread_id, harness) = seed_live_spec_harness(&boot).await;
    harness
        .set_state_for_test(HarnessState::TurnRunning {
            turn_id: "T1".into(),
            started_at: Instant::now(),
        })
        .await;

    let (status, body) = get_json(
        boot.app.clone(),
        &format!("/api/cards/{}/spec/run", card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["card_id"], json!(card.id.as_str()));
    assert_eq!(body["runtime_id"], json!(runtime_id.as_str()));
    assert_eq!(body["phase"], json!("turn_running"), "body={body}");

    shutdown_seeded_harness(&boot, &runtime_id, harness).await;
}

/// `GET /spec/run` with no active runtime row: dormancy is not an error
/// for a read — 200 with null `runtime_id`/`phase`.
#[tokio::test]
async fn get_spec_run_without_runtime_returns_nulls() {
    let boot = boot().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Spec).await;

    let (status, body) = get_json(boot.app, &format!("/api/cards/{}/spec/run", card.id)).await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["card_id"], json!(card.id.as_str()));
    assert_eq!(body["runtime_id"], json!(null), "body={body}");
    assert_eq!(body["phase"], json!(null), "body={body}");
}

/// `GET /spec/run` with an active runtime row but no registered harness
/// (post-restart shape) is the same dormant nulls answer.
#[tokio::test]
async fn get_spec_run_registry_miss_returns_nulls() {
    let boot = boot().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Spec).await;
    seed_active_spec_runtime_row(&boot, &card).await;

    let (status, body) = get_json(boot.app, &format!("/api/cards/{}/spec/run", card.id)).await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["runtime_id"], json!(null), "body={body}");
    assert_eq!(body["phase"], json!(null), "body={body}");
}

/// `GET /spec/run` refuses non-spec codex cards with 403, mirroring the
/// write routes' guard chain.
#[tokio::test]
async fn get_spec_run_non_spec_card_403() {
    let boot = boot().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Worker).await;

    let (status, body) = get_json(boot.app, &format!("/api/cards/{}/spec/run", card.id)).await;

    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| error.contains("not a spec codex card")),
        "body={body}"
    );
}

/// Non-spec codex cards are refused with 403, mirroring `/spec/input`.
#[tokio::test]
async fn interrupt_non_spec_card_403() {
    let boot = boot().await;
    let card = seed_codex_card_with_role(&boot, CardRole::Worker).await;

    let (status, body) =
        post_empty(boot.app, &format!("/api/cards/{}/spec/interrupt", card.id)).await;

    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| error.contains("not a spec codex card")),
        "body={body}"
    );
}
