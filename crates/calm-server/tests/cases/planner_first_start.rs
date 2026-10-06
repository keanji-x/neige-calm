//! #2184 — a harness card with no thread to preserve starts on a person's send, unless its creator
//! still owns the first start. The daily Track is
//! created model-free (#2024), and an ordinary Track's create-time start can fail; the first
//! `POST /planner/input` must start the conversation and queue the message, not answer 409
//! `planner_harness_dormant`. A carrier holding a thread, or a transcript, keeps the dormant answer.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::FromRef;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::event::EventBus;
use calm_server::model::{NewArea, new_id, now_ms};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::session_projection_repo::{AgentProvider, WorkerSessionInit, WorkerSessionState};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient, RouteState, WriteContext};
use calm_server::test_seams::{
    PLANNER_FIRST_START, PLANNER_INPUT_REPLAY_MISSED, PausePoint,
    TRACK_CREATE_BEFORE_PLANNER_START, install_pause_for_test,
};
use calm_server::track_area_cache::TrackAreaCache;
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::Notify;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    state: AppState,
    repo: Arc<SqlxRepo>,
    planner_card_id: String,
    _tmp: TempDir,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AppServer {
    Running,
    Down,
}

fn today() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-04T01:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn router(state: &AppState) -> axum::Router {
    routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone())
}

async fn app_state(app_server: AppServer) -> (TempDir, Arc<SqlxRepo>, AppState) {
    let tmp = TempDir::new().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let dyn_repo: Arc<dyn Repo> = repo.clone();
    let roles = CardRoleCache::new();
    let tracks = TrackAreaCache::new();
    let events = EventBus::new();
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().join("data"),
        proc_supervisor_sock: None,
    });
    std::fs::create_dir_all(&daemon.data_dir).unwrap();
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        dyn_repo.clone(),
        PathBuf::new(),
        tmp.path().join("plugins"),
        Vec::new(),
        events.clone(),
        WriteContext::new(roles.clone(), tracks.clone()),
    ));
    let mut state = AppState::from_parts(
        dyn_repo,
        events,
        daemon,
        plugin,
        Arc::new(CodexClient::new_stub()),
        Some(roles),
        Some(tracks),
    )
    .with_workspace_root(tmp.path().join("workspaces"));
    if app_server == AppServer::Running {
        state = with_running_app_server(state, &repo);
    }
    (tmp, repo, state)
}

/// The production daily lifecycle creates today's Track: the Planner card exists and nothing has
/// ever started it.
async fn boot(app_server: AppServer) -> Boot {
    let (tmp, repo, state) = app_state(app_server).await;
    let track = calm_server::daily_planner::reconcile(&RouteState::from_ref(&state), today())
        .await
        .expect("daily reconcile creates today's Track");
    let planner_card_id: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id = ?1 AND role = 'planner'")
            .bind(track.id.as_str())
            .fetch_one(repo.pool())
            .await
            .expect("the daily Track has one Planner card");
    let boot = Boot {
        app: router(&state),
        state,
        repo,
        planner_card_id,
        _tmp: tmp,
    };
    assert_eq!(
        boot.session_rows().await,
        0,
        "premise: creation started no model"
    );
    assert_eq!(boot.start_ops().await, 0, "premise: no start was submitted");
    boot
}

fn with_running_app_server(state: AppState, repo: &Arc<SqlxRepo>) -> AppState {
    state.with_shared_codex_appserver(SharedCodexAppServer::new_fake_running_with_pending(
        repo.clone(),
        None,
    ))
}

impl Boot {
    fn input_uri(&self) -> String {
        format!("/api/cards/{}/planner/input", self.planner_card_id)
    }

    async fn scalar(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .bind(&self.planner_card_id)
            .fetch_one(self.repo.pool())
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    async fn session_rows(&self) -> i64 {
        self.scalar("SELECT COUNT(*) FROM worker_sessions WHERE card_id = ?1")
            .await
    }

    async fn active_session_rows(&self) -> i64 {
        self.scalar(
            "SELECT COUNT(*) FROM worker_sessions WHERE card_id = ?1 \
               AND state IN ('starting','running','idle','turn_pending')",
        )
        .await
    }

    async fn start_ops(&self) -> i64 {
        self.scalar(
            "SELECT COUNT(*) FROM operations WHERE kind = 'planner-harness-start' \
               AND json_extract(payload_json, '$.spec_card_id') = ?1",
        )
        .await
    }

    async fn enqueued_audits(&self) -> i64 {
        self.scalar(
            "SELECT COUNT(*) FROM events WHERE kind = 'harness.user_message.enqueued' \
               AND json_extract(payload, '$.card_id') = ?1",
        )
        .await
    }

    async fn bindings(&self) -> i64 {
        self.scalar("SELECT COUNT(*) FROM planner_input_idempotency WHERE card_id = ?1")
            .await
    }

    /// Whether `text` reached the card's live harness: still queued, or already handed to the
    /// model as a turn or a steer.
    async fn delivered(&self, text: &str) -> bool {
        let runtime = self
            .repo
            .session_projection_active_for_card(&self.planner_card_id)
            .await
            .unwrap();
        let queued = match runtime.and_then(|runtime| self.state.harness.get(&runtime.id)) {
            Some(harness) => {
                serde_json::to_string(&harness.snapshot().await.pending_observations())
                    .unwrap()
                    .contains(text)
            }
            None => false,
        };
        let daemon = &self.state.shared_codex_appserver;
        queued
            || serde_json::to_string(&daemon.started_turns_for_test())
                .unwrap()
                .contains(text)
            || format!("{:?}", daemon.steered_turns_for_test()).contains(text)
    }

    async fn wait_delivered(&self, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.delivered(text).await {
            assert!(
                Instant::now() < deadline,
                "accepted text {text:?} never reached the harness"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn shutdown(self) {
        if let Some(runtime) = self
            .repo
            .session_projection_active_for_card(&self.planner_card_id)
            .await
            .unwrap()
            && let Some(harness) = self.state.harness.remove(&runtime.id)
        {
            harness.shutdown().await.unwrap();
        }
    }
}

async fn post_input(app: axum::Router, uri: &str, text: &str, key: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .header("idempotency-key", key)
                .body(Body::from(json!({ "text": text }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn the_first_send_to_a_never_started_daily_planner_starts_it_and_queues_the_message() {
    let boot = boot(AppServer::Running).await;

    let (status, body) = post_input(
        boot.app.clone(),
        &boot.input_uri(),
        "plan my day",
        &new_id(),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["card_id"], json!(boot.planner_card_id));
    let active = boot
        .repo
        .session_projection_active_for_card(&boot.planner_card_id)
        .await
        .unwrap()
        .expect("the send started a session");
    assert_eq!(body["worker_session_id"], json!(active.id));
    assert_eq!(boot.start_ops().await, 1, "one start for the first send");
    assert_eq!(boot.session_rows().await, 1, "one session row");
    assert_eq!(boot.active_session_rows().await, 1, "one active session");
    assert_eq!(boot.bindings().await, 1, "the message was stored once");
    assert_eq!(boot.enqueued_audits().await, 1, "and enqueued once");
    boot.wait_delivered("plan my day").await;
    boot.shutdown().await;
}

#[tokio::test]
async fn a_retried_first_send_under_its_key_starts_once_and_enqueues_once() {
    let boot = boot(AppServer::Running).await;
    let key = new_id();

    let (status, first) = post_input(boot.app.clone(), &boot.input_uri(), "only once", &key).await;
    assert_eq!(status, StatusCode::OK, "body={first}");
    let (status, retried) =
        post_input(boot.app.clone(), &boot.input_uri(), "only once", &key).await;
    assert_eq!(status, StatusCode::OK, "body={retried}");

    assert_eq!(retried, first, "the retry replays the first answer");
    assert_eq!(boot.start_ops().await, 1);
    assert_eq!(boot.session_rows().await, 1);
    assert_eq!(boot.bindings().await, 1);
    assert_eq!(
        boot.enqueued_audits().await,
        1,
        "a replay is not a second send"
    );
    boot.shutdown().await;
}

#[tokio::test]
async fn a_never_started_card_with_the_app_server_down_is_503_and_starts_later() {
    let mut boot = boot(AppServer::Down).await;

    let (status, body) =
        post_input(boot.app.clone(), &boot.input_uri(), "too early", &new_id()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body={body}");
    assert_eq!(
        boot.session_rows().await,
        0,
        "a refused start writes no row"
    );
    assert_eq!(boot.start_ops().await, 0, "nor an operation");
    assert_eq!(boot.bindings().await, 0, "nor the message");

    boot.state = with_running_app_server(boot.state.clone(), &boot.repo);
    boot.app = router(&boot.state);
    let (status, body) = post_input(
        boot.app.clone(),
        &boot.input_uri(),
        "now it runs",
        &new_id(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(boot.start_ops().await, 1);
    assert_eq!(boot.active_session_rows().await, 1);
    assert_eq!(boot.enqueued_audits().await, 1);
    boot.wait_delivered("now it runs").await;
    boot.shutdown().await;
}

/// Two first sends under different keys: the first holds the card's lock at the start, the
/// second is shown queued on that same lock, and the card still gets one start and one session
/// with both messages.
#[tokio::test]
async fn two_concurrent_first_sends_start_once_and_keep_both_messages() {
    let boot = boot(AppServer::Running).await;
    let card_id = boot.planner_card_id.clone();
    let first_pause = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(PLANNER_FIRST_START, &card_id, first_pause.clone());
    let first = spawn_input(&boot, "first message");
    within(
        first_pause.entered.notified(),
        "the first send reaches its start",
    )
    .await;
    let held = boot.state.planner_recovery_lock_handles_for_test(&card_id);
    assert!(held > 0, "premise: the first send holds the card's lock");

    let second_pause = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(PLANNER_INPUT_REPLAY_MISSED, &card_id, second_pause.clone());
    let second = spawn_input(&boot, "second message");
    within(
        second_pause.entered.notified(),
        "the second send is in flight",
    )
    .await;
    second_pause.release.notify_one();
    let deadline = Instant::now() + Duration::from_secs(10);
    while boot.state.planner_recovery_lock_handles_for_test(&card_id) <= held {
        assert!(
            Instant::now() < deadline,
            "the second send never queued on the card's lock"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !second.is_finished(),
        "the second send waits behind the first"
    );

    first_pause.release.notify_one();
    let (status, first) = first.await.unwrap();
    assert_eq!(status, StatusCode::OK, "body={first}");
    let (status, second) = second.await.unwrap();
    assert_eq!(status, StatusCode::OK, "body={second}");

    assert_eq!(
        first["worker_session_id"], second["worker_session_id"],
        "both messages went to the one session"
    );
    assert_eq!(boot.start_ops().await, 1, "one start for two first sends");
    assert_eq!(boot.session_rows().await, 1, "one session row");
    assert_eq!(boot.bindings().await, 2, "both messages stored");
    assert_eq!(boot.enqueued_audits().await, 2, "both messages enqueued");
    boot.wait_delivered("first message").await;
    boot.wait_delivered("second message").await;
    boot.shutdown().await;
}

fn spawn_input(boot: &Boot, text: &'static str) -> tokio::task::JoinHandle<(StatusCode, Value)> {
    let (app, uri) = (boot.app.clone(), boot.input_uri());
    tokio::spawn(async move { post_input(app, &uri, text, &new_id()).await })
}

async fn within(future: impl std::future::Future<Output = ()>, what: &str) {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .unwrap_or_else(|_| panic!("timed out: {what}"));
}

/// An ordinary Track whose create-time start failed ("planner agent is inert"): compensation
/// leaves a `failed`, completed row with no thread, which preserves nothing, so the first send
/// starts the session.
#[tokio::test]
async fn an_ordinary_track_whose_create_time_start_failed_starts_on_the_first_send() {
    let (tmp, repo, state) = app_state(AppServer::Running).await;
    let area = repo
        .area_create(NewArea {
            name: "inert".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let app = router(&state);
    let (status, body) = create_ordinary_track(app.clone(), area.id.to_string(), "inert").await;
    assert!(
        status.is_success(),
        "the create answers success with an inert Planner: {status} {body}"
    );
    let track_id = body["id"].as_str().expect("created track id").to_string();
    let planner_card_id: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id = ?1 AND role = 'planner'")
            .bind(&track_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    let boot = Boot {
        app,
        state,
        repo,
        planner_card_id,
        _tmp: tmp,
    };
    assert_eq!(
        boot.scalar(
            "SELECT COUNT(*) FROM worker_sessions WHERE card_id = ?1 AND state = 'failed' \
               AND completed_at_ms IS NOT NULL AND thread_id IS NULL"
        )
        .await,
        1,
        "premise: the failed start left its compensated row"
    );
    assert_eq!(boot.active_session_rows().await, 0, "premise: inert");

    let (status, body) =
        post_input(boot.app.clone(), &boot.input_uri(), "wake up", &new_id()).await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(boot.active_session_rows().await, 1);
    assert_eq!(
        boot.start_ops().await,
        2,
        "the failed create-time start and this one"
    );
    assert_eq!(boot.enqueued_audits().await, 1);
    boot.wait_delivered("wake up").await;
    boot.shutdown().await;
}

/// A failed row is a conversation as soon as it names a thread, on the row or in its snapshot.
/// The same card, with the thread taken away, starts: the thread is the only difference.
#[tokio::test]
async fn a_failed_row_that_names_a_thread_stays_dormant() {
    let boot = boot(AppServer::Running).await;
    let mut tx = boot.repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit::shared_planner(
            new_id(),
            boot.planner_card_id.clone(),
            AgentProvider::Codex,
            WorkerSessionState::Failed,
            Some("thread-kept".into()),
            json!({}),
            now_ms(),
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("UPDATE worker_sessions SET completed_at_ms = 1 WHERE card_id = ?1")
        .bind(&boot.planner_card_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();

    for (shape, sql) in [
        (
            "a thread on the row",
            "UPDATE worker_sessions SET thread_id = 'thread-kept' WHERE card_id = ?1",
        ),
        (
            "a thread only in the snapshot",
            "UPDATE worker_sessions SET thread_id = NULL, \
               handle_state_json = '{\"last_thread_id\":\"thread-kept\"}' WHERE card_id = ?1",
        ),
    ] {
        sqlx::query(sql)
            .bind(&boot.planner_card_id)
            .execute(boot.repo.pool())
            .await
            .unwrap();
        let (status, body) =
            post_input(boot.app.clone(), &boot.input_uri(), "hello?", &new_id()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{shape}: body={body}");
        assert_eq!(body["code"], json!("planner_harness_dormant"), "{shape}");
        assert_eq!(boot.start_ops().await, 0, "{shape}: no start was submitted");
    }

    sqlx::query("UPDATE worker_sessions SET handle_state_json = '{}' WHERE card_id = ?1")
        .bind(&boot.planner_card_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let (status, body) = post_input(boot.app.clone(), &boot.input_uri(), "now?", &new_id()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "without the thread it starts: body={body}"
    );
    assert_eq!(boot.start_ops().await, 1);
    boot.shutdown().await;
}

/// `POST /api/tracks`, message-less: the production create, whose Planner start is its own.
async fn create_ordinary_track(
    app: axum::Router,
    area_id: String,
    title: &'static str,
) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tracks")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "planner_provider": "codex",
                        "area_id": area_id,
                        "title": title,
                        "cwd": null,
                        "attach_folder": false,
                        "theme": {"fg": [216, 219, 226], "bg": [15, 20, 24]},
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Codex review (#2184): an ordinary create commits its cards, then submits its own start. A send
/// in that window must not start the card, or the create's start would supersede the session the
/// send's message went to. The never-started card of a non-managed Track stays 409.
#[tokio::test]
async fn a_send_between_a_creates_commit_and_its_start_does_not_pre_empt_the_creator() {
    let (tmp, repo, state) = app_state(AppServer::Running).await;
    let area = repo
        .area_create(NewArea {
            name: "racing create".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let app = router(&state);
    let parked = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(
        TRACK_CREATE_BEFORE_PLANNER_START,
        area.id.as_str(),
        parked.clone(),
    );
    let create = tokio::spawn(create_ordinary_track(
        app.clone(),
        area.id.to_string(),
        "racing",
    ));
    within(
        parked.entered.notified(),
        "the create parks before its start",
    )
    .await;
    let planner_card_id: String = sqlx::query_scalar(
        "SELECT c.id FROM cards c JOIN tracks t ON t.id = c.track_id \
          WHERE t.area_id = ?1 AND c.role = 'planner'",
    )
    .bind(area.id.as_str())
    .fetch_one(repo.pool())
    .await
    .expect("the create committed its Planner card before its start");
    let boot = Boot {
        app,
        state,
        repo,
        planner_card_id,
        _tmp: tmp,
    };

    let (status, body) =
        post_input(boot.app.clone(), &boot.input_uri(), "too soon", &new_id()).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        body["code"],
        json!("planner_harness_dormant"),
        "body={body}"
    );
    assert_eq!(boot.start_ops().await, 0, "the send submitted no start");
    assert_eq!(boot.session_rows().await, 0);

    parked.release.notify_one();
    let (status, created) = create.await.unwrap();
    assert!(status.is_success(), "{status} {created}");
    let active = boot
        .repo
        .session_projection_active_for_card(&boot.planner_card_id)
        .await
        .unwrap()
        .expect("the create's own start ran");

    let (status, body) = post_input(boot.app.clone(), &boot.input_uri(), "now", &new_id()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        body["worker_session_id"],
        json!(active.id),
        "the creator's session"
    );
    assert_eq!(boot.start_ops().await, 1, "one start: the creator's");
    assert_eq!(boot.active_session_rows().await, 1);
    boot.wait_delivered("now").await;
    boot.shutdown().await;
}

/// A start of the card that has not finished answers 503, and the send submits no second one.
#[tokio::test]
async fn a_send_while_a_start_is_in_flight_is_503_and_starts_nothing() {
    let boot = boot(AppServer::Running).await;
    // Leased far ahead by another owner, so no driver of this server picks it up.
    sqlx::query(
        "INSERT INTO operations (id, operation_key, kind, payload_hash, target_type, \
           target_json, payload_json, phase, lease_owner, lease_until_ms, created_at_ms, \
           updated_at_ms) \
         VALUES ('in-flight', 'in-flight', 'planner-harness-start', 'h', 'track', '{}', ?1, \
           'tx_committed', 'elsewhere', 9223372036854775807, 0, 0)",
    )
    .bind(json!({ "spec_card_id": boot.planner_card_id }).to_string())
    .execute(boot.repo.pool())
    .await
    .unwrap();
    assert_eq!(boot.start_ops().await, 1, "premise: one start in flight");

    let (status, body) = post_input(boot.app.clone(), &boot.input_uri(), "hurry", &new_id()).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body={body}");
    assert_eq!(boot.start_ops().await, 1, "no second start was submitted");
    assert_eq!(boot.session_rows().await, 0);
    assert_eq!(boot.bindings().await, 0, "the message was not stored");
}

/// A retired row is history: the send must not mint a second conversation over it.
#[tokio::test]
async fn a_card_whose_only_session_is_retired_stays_dormant() {
    let boot = boot(AppServer::Running).await;
    let (status, body) = post_input(boot.app.clone(), &boot.input_uri(), "first", &new_id()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    boot.shutdown_runtime_as_superseded().await;
    let starts_before = boot.start_ops().await;

    let (status, body) = post_input(boot.app.clone(), &boot.input_uri(), "again", &new_id()).await;

    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        body["code"],
        json!("planner_harness_dormant"),
        "body={body}"
    );
    assert_eq!(
        boot.start_ops().await,
        starts_before,
        "no start was submitted"
    );
    assert_eq!(boot.active_session_rows().await, 0);
}

/// A transcript without any session row (rows deleted, items kept) is history too.
#[tokio::test]
async fn a_card_with_transcript_items_but_no_session_row_stays_dormant() {
    let boot = boot(AppServer::Running).await;
    sqlx::query(
        "INSERT INTO harness_items \
           (worker_session_id, card_id, track_id, thread_id, method, params, created_at_ms) \
         SELECT 'gone-runtime', id, track_id, 'gone-thread', 'item/completed', '{}', 0 \
           FROM cards WHERE id = ?1",
    )
    .bind(&boot.planner_card_id)
    .execute(boot.repo.pool())
    .await
    .unwrap();

    let (status, body) = post_input(boot.app.clone(), &boot.input_uri(), "hello?", &new_id()).await;

    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        body["code"],
        json!("planner_harness_dormant"),
        "body={body}"
    );
    assert_eq!(boot.start_ops().await, 0, "no start was submitted");
    assert_eq!(boot.session_rows().await, 0);
}

impl Boot {
    /// Retire the card's runtime the way a superseding writer leaves it: the row stays, as
    /// `superseded`, and no harness serves it.
    async fn shutdown_runtime_as_superseded(&self) {
        let runtime = self
            .repo
            .session_projection_active_for_card(&self.planner_card_id)
            .await
            .unwrap()
            .expect("an active runtime to retire");
        if let Some(harness) = self.state.harness.remove(&runtime.id) {
            harness.shutdown().await.unwrap();
        }
        sqlx::query("UPDATE worker_sessions SET state = 'superseded' WHERE id = ?1")
            .bind(&runtime.id)
            .execute(self.repo.pool())
            .await
            .unwrap();
        assert_eq!(self.session_rows().await, 1, "premise: the row is kept");
    }
}
