//! #1505 — one live planner card with a registered harness, for the queue
//! slices.
//!
//! PR1's read-path cases and PR2's mutation cases both need the same six-object
//! setup (area, track, planner card, worker-session row, app state, harness),
//! and a second copy of it would be a second thing to keep in step with the
//! production boot path. It lives here so both drive the same one.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::auth::Principal;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_create_with_id_tx, session_start_runtime_tx};
use calm_server::event::EventBus;
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, PlannerHarness, PlannerHarnessParams,
    QueueEntry,
};
use calm_server::model::{Card, CardRole, NewArea, NewCard, NewTrack, new_id};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

pub const SEED_THREAD_ID: &str = "thread-pending-queue";

async fn insert_owner_principal(
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    request.extensions_mut().insert(Principal {
        user_id: "owner".into(),
        display_name: "owner".into(),
        role: "owner".into(),
        session_id: "planner-queue-fixture".into(),
    });
    next.run(request).await
}

pub struct Boot {
    pub app: axum::Router,
    pub harness: PlannerHarness,
    pub planner_card: Card,
    pub worker_session_id: String,
    pub daemon: Arc<SharedCodexAppServer>,
    pub repo: Arc<SqlxRepo>,
}

impl Boot {
    /// The persisted payloads of every event of one kind, oldest first.
    ///
    /// Read from the `events` table rather than from a bus subscription: the
    /// row is what an audit, a replay and the websocket fan-out all read, and
    /// it is committed before the handler answers.
    pub async fn event_payloads(&self, kind: &str) -> Vec<Value> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT payload FROM events WHERE kind = ?1 ORDER BY id ASC")
                .bind(kind)
                .fetch_all(self.repo.pool())
                .await
                .expect("event rows");
        rows.into_iter()
            .map(|(payload,)| serde_json::from_str(&payload).expect("event payload json"))
            .collect()
    }
}

/// A planner card with a live, registered harness seeded from `snapshot`.
///
/// The debounce windows are pushed out to a minute so the run loop cannot
/// drain the queue out from under an assertion — every test here is about
/// what is IN the queue.
/// The default: issuance paused, so the queue is whatever the test put in it.
pub async fn boot_with(snapshot: HarnessSnapshot) -> Boot {
    boot_with_issuance(snapshot, Issuance::Paused).await
}

/// Whether the harness is allowed to drain the queue into `turn/start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Issuance {
    /// The run loop's 50ms tick will pick a hard-fire entry up and issue it.
    /// Only for the tests whose subject IS that race.
    Live,
    Paused,
}

/// Boot with the `events` table renamed away just before the harness starts,
/// so every event insert it attempts fails.
///
/// Fault injection, not a knob that omits an invariant: the production write
/// path is the one under test, and what changes is only whether the database
/// accepts it. Renaming BEFORE `PlannerHarness::run` is the point — doing it
/// afterwards races the run loop's own early flush, which under load wins and
/// leaves nothing outstanding for the assertion to observe.
pub async fn boot_with_broken_event_writes(snapshot: HarnessSnapshot) -> Boot {
    boot_inner(snapshot, Issuance::Paused, EventWrites::Broken).await
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EventWrites {
    Working,
    Broken,
}

pub async fn boot_with_issuance(snapshot: HarnessSnapshot, issuance: Issuance) -> Boot {
    boot_inner(snapshot, issuance, EventWrites::Working).await
}

async fn boot_inner(
    snapshot: HarnessSnapshot,
    issuance: Issuance,
    event_writes: EventWrites,
) -> Boot {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "pending-queue".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "pending queue".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let role_cache = CardRoleCache::new();
    let track_area_cache = TrackAreaCache::new();
    track_area_cache.insert(track.id.clone(), area.id);

    let mut tx = repo.pool().begin().await.unwrap();
    let planner_card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "planner_harness": true}),
        },
        CardRole::Planner,
        false,
        &role_cache,
    )
    .await
    .unwrap();

    let worker_session_id = new_id();
    session_start_runtime_tx(
        &mut tx,
        calm_server::session_projection_repo::WorkerSessionInit {
            id: worker_session_id.clone(),
            card_id: planner_card.id.to_string(),
            kind: calm_server::session_projection_repo::WorkerSessionKind::SharedPlanner,
            agent_provider: Some(calm_server::session_projection_repo::AgentProvider::Codex),
            status: calm_server::session_projection_repo::WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(SEED_THREAD_ID.to_string()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            spawn_op_id: None,
            now_ms: calm_server::model::now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-pending-queue"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(role_cache.clone(), track_area_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(role_cache.clone()),
        Some(track_area_cache.clone()),
    );

    if event_writes == EventWrites::Broken {
        sqlx::query("ALTER TABLE events RENAME TO events_hidden")
            .execute(repo.pool())
            .await
            .expect("hide the events table");
    }

    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let daemon_handle = Arc::clone(&daemon);
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let harness = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: worker_session_id.clone(),
        track_id: planner_card.track_id.clone(),
        card_id: planner_card.id.clone(),
        thread_id: Some(SEED_THREAD_ID.to_string()),
        repo: repo_dyn,
        events,
        card_role_cache: role_cache,
        track_area_cache,
        daemon,
        config: HarnessConfig {
            debounce_min_idle: Duration::from_secs(60),
            debounce_max_wait: Duration::from_secs(60),
            ..HarnessConfig::default()
        },
        snapshot,
    });
    // Every test in this file asserts on what is IN the queue, and a user
    // message hard-fires: it bypasses the debounce windows above entirely and
    // would be drained by the first 50ms tick. Pausing issuance is what makes
    // these assertions deterministic rather than a race against that tick.
    if issuance == Issuance::Paused {
        harness.pause_issuance_for_dev();
    }
    state
        .harness
        .insert(worker_session_id.clone(), harness.clone());

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        // Stand-in for `auth::require_session`, which is layered over the whole
        // protected REST subtree in `application_router` and is what puts a
        // `Principal` in the extensions. Inserting one directly is the same
        // post-condition without a cookie jar; a handler that takes the
        // `Principal` extractor answers 401 without it, which is how this
        // arrived in the fixture.
        .layer(axum::middleware::from_fn(insert_owner_principal))
        .with_state(state);

    Boot {
        app,
        harness,
        planner_card,
        worker_session_id,
        daemon: daemon_handle,
        repo,
    }
}

pub fn idle_snapshot(entries: Vec<QueueEntry>) -> HarnessSnapshot {
    let mut snapshot = HarnessSnapshot::initial(0, entries);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(SEED_THREAD_ID.to_string());
    snapshot
}

pub async fn get(app: axum::Router, uri: String) -> (StatusCode, Value) {
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

/// One JSON request with a body, as the human actor unless told otherwise.
pub async fn send_json(
    app: axum::Router,
    method: &str,
    uri: String,
    actor: &str,
    body: Value,
) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("x-calm-actor", actor)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, parsed)
}

pub async fn post_input(app: axum::Router, card_id: &str, text: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{card_id}/planner/input"))
                .header("content-type", "application/json")
                .header("x-calm-actor", "user")
                .body(Body::from(json!({"text": text}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}
