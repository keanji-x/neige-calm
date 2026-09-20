//! `POST /api/tracks` delivers the synthesiser page's first message atomically, exactly once, as a
//! `UserMessage` from the human; one `Idempotency-Key` produces at most one track across retries, outages and re-points.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::Extension;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::auth::Principal;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::db::sqlite::{session_delete_tx, session_mark_superseded_runtime_tx};
use calm_server::db::write_in_tx_typed;
use calm_server::event::EventBus;
use calm_server::harness::Observation;
use calm_server::harness::run_loop::{
    ANY_RUNTIME, PlannerHarnessDrainRaceHook, install_planner_harness_drain_race_hook_for_test,
};
use calm_server::model::NewArea;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::routes::today_summary::TODAY_SUMMARY_BOOTSTRAP_TEXT;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::shared_codex_appserver::TurnStartReturnHook;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    state: AppState,
    area_id: String,
    repo: Arc<SqlxRepo>,
    /// `Arc` so two instances of the same database share one sandbox: `workspace_root` must be the SAME
    /// directory on both, or the loser of the primary-key race could never collide on disk.
    tmp: Arc<TempDir>,
}

/// A real git repository the user owns, the shape `PATCH /api/tracks/{id}` accepts as an attached workspace.
fn user_repo(at: &std::path::Path) -> PathBuf {
    fn git(at: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(at)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} in {at:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    std::fs::create_dir_all(at).unwrap();
    git(at, &["init", "-b", "main"]);
    git(at, &["config", "user.name", "fixture"]);
    git(at, &["config", "user.email", "fixture@example.com"]);
    git(at, &["config", "gc.auto", "0"]);
    git(at, &["config", "maintenance.auto", "false"]);
    std::fs::write(at.join("README.md"), b"the user's own work\n").unwrap();
    git(at, &["add", "-A"]);
    git(at, &["commit", "-q", "--no-verify", "-m", "user commit"]);
    at.to_path_buf()
}

async fn boot() -> Boot {
    boot_with_daemon(true).await
}

/// Same fixture, but with the shared codex app-server **not running**, which `PlannerHarnessStartAdapter::validate`
/// refuses on; with a fake installed `is_running()` short-circuits to `true` and the outage is unconstructible.
async fn boot_without_daemon() -> Boot {
    boot_with_daemon(false).await
}

fn app_for_state(state: AppState) -> axum::Router {
    routes::router()
        .layer(Extension(Principal {
            user_id: "owner".into(),
            display_name: "owner".into(),
            role: "owner".into(),
            session_id: "test".into(),
        }))
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state)
}

async fn boot_with_daemon(daemon_running: bool) -> Boot {
    let tmp = Arc::new(TempDir::new().unwrap());
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "track-create-first-message".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    instance_on(tmp, repo, area.id.to_string(), daemon_running).await
}

/// Two `AppState`s over one on-disk SQLite file, in one process: the lock maps are per-`AppState`, so nothing
/// serializes two same-key creates. `sqlite::memory:` cannot carry this (two opens are two databases).
async fn boot_two_instances_on_one_database() -> (Boot, Boot) {
    let tmp = Arc::new(TempDir::new().unwrap());
    let db_url = format!(
        "sqlite://{}?mode=rwc",
        tmp.path().join("calm.db").to_string_lossy()
    );
    let first = Arc::new(SqlxRepo::open(&db_url).await.unwrap());
    let area = first
        .area_create(NewArea {
            name: "track-create-first-message".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let second = Arc::new(SqlxRepo::open(&db_url).await.unwrap());
    let area_id = area.id.to_string();
    let winner = instance_on(tmp.clone(), first, area_id.clone(), true).await;
    let loser = instance_on(tmp, second, area_id, true).await;
    (winner, loser)
}

/// One server instance over `repo`.
async fn instance_on(
    tmp: Arc<TempDir>,
    repo: Arc<SqlxRepo>,
    area_id: String,
    daemon_running: bool,
) -> Boot {
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let events = EventBus::new();
    let roles = CardRoleCache::new();
    let tracks = TrackAreaCache::new();
    repo.seed_track_area_cache(&tracks).await.unwrap();
    let state = AppState::from_parts(
        repo_dyn.clone(),
        events.clone(),
        Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            proc_supervisor_sock: None,
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo_dyn.clone(),
            PathBuf::new(),
            tmp.path().join("plugins-data"),
            Vec::new(),
            events,
            calm_server::state::WriteContext::new(roles.clone(), tracks.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(roles),
        Some(tracks),
    )
    .with_workspace_root(tmp.path().join("workspaces"))
    .with_shared_codex_appserver(if daemon_running {
        SharedCodexAppServer::new_fake_running_with_pending(repo_dyn, None)
    } else {
        SharedCodexAppServer::new_stub_with_pending(repo_dyn, None)
    });
    let app = app_for_state(state.clone());
    Boot {
        app,
        state,
        area_id,
        repo,
        tmp,
    }
}

impl Boot {
    fn app_with_running_daemon(&self) -> axum::Router {
        let repo: Arc<dyn Repo> = self.repo.clone();
        let state = self.state.clone().with_shared_codex_appserver(
            SharedCodexAppServer::new_fake_running_with_pending(repo, None),
        );
        app_for_state(state)
    }

    /// `POST /api/tracks`; `idempotency_key` and `first_message` are both optional.
    async fn create_track(
        &self,
        idempotency_key: Option<&str>,
        first_message: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut body = json!({
            "area_id": self.area_id,
            "title": "",
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        });
        if let Some(text) = first_message {
            body["first_message"] = json!(text);
        }
        self.post_create(idempotency_key, body).await
    }

    /// `POST /api/tracks` with an **explicit** `cwd`, i.e. the attached branch — the only shape whose
    /// create-path validation reads the disk.
    async fn create_track_at(
        &self,
        idempotency_key: Option<&str>,
        first_message: Option<&str>,
        cwd: &std::path::Path,
    ) -> (StatusCode, Value) {
        let mut body = json!({
            "area_id": self.area_id,
            "title": "",
            "cwd": cwd.to_string_lossy(),
            "attach_folder": true,
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        });
        if let Some(text) = first_message {
            body["first_message"] = json!(text);
        }
        self.post_create(idempotency_key, body).await
    }

    async fn post_create(&self, idempotency_key: Option<&str>, body: Value) -> (StatusCode, Value) {
        self.post_create_on(self.app.clone(), idempotency_key, body)
            .await
    }

    async fn post_create_on(
        &self,
        app: axum::Router,
        idempotency_key: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/api/tracks")
            .header("content-type", "application/json");
        if let Some(key) = idempotency_key {
            builder = builder.header("idempotency-key", key);
        }
        let response = app
            .oneshot(builder.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// `PATCH /api/tracks/{id}` — the production route that repoints a managed workspace at a repository the user owns.
    async fn repoint_to(&self, track_id: &str, path: &std::path::Path) -> (StatusCode, Value) {
        let body = json!({"workspace": {
            "kind": "attached",
            "path": path.to_string_lossy(),
            "attach_folder": true,
        }});
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/tracks/{track_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
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

    /// `DELETE /api/tracks/{id}` — the production route, not a `DELETE FROM tracks`, so the binding row's
    /// survival is the real handler's.
    async fn delete_track(&self, track_id: &str) -> StatusCode {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/tracks/{track_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    async fn workspace_row(&self, track_id: &str) -> (String, String) {
        sqlx::query_as("SELECT workspace_kind, workspace_path FROM tracks WHERE id=?1")
            .bind(track_id)
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }

    /// The `cwd` of every `planner-harness-start` payload that carries `needle` as its `first_message`, oldest
    /// first. Filtered by `first_message` because the re-point route submits a `planner-harness-start` of its own.
    async fn first_message_payload_cwds(&self, needle: &str) -> Vec<String> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT payload_json FROM operations WHERE kind = 'planner-harness-start' \
             ORDER BY created_at_ms, id",
        )
        .fetch_all(self.repo.pool())
        .await
        .unwrap();
        rows.into_iter()
            .filter_map(|row| serde_json::from_str::<Value>(&row).ok())
            .filter(|payload| payload["first_message"].as_str() == Some(needle))
            .map(|payload| payload["cwd"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// How many `(area, Idempotency-Key)` → track bindings exist.
    async fn binding_count(&self) -> i64 {
        self.count("SELECT COUNT(*) FROM track_create_idempotency")
            .await
    }

    async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }

    async fn track_count(&self) -> i64 {
        self.count("SELECT COUNT(*) FROM tracks").await
    }

    async fn card_count(&self) -> i64 {
        self.count("SELECT COUNT(*) FROM cards").await
    }

    async fn operation_count(&self) -> i64 {
        self.count("SELECT COUNT(*) FROM operations").await
    }

    async fn user_message_event_count(&self) -> i64 {
        self.count("SELECT COUNT(*) FROM events WHERE kind = 'harness.user_message.enqueued'")
            .await
    }

    /// The `actor` column of the single `harness.user_message.enqueued` row.
    async fn user_message_actor(&self) -> String {
        sqlx::query_scalar(
            "SELECT actor FROM events WHERE kind = 'harness.user_message.enqueued' LIMIT 1",
        )
        .fetch_one(self.repo.pool())
        .await
        .unwrap()
    }

    /// How many copies of `needle` the harness has been handed: turns already started plus observations still
    /// queued, as substring occurrences (adjacent `UserMessage`s fold into one entry). Polls and returns what it saw.
    async fn copies_in_harness(&self, needle: &str, want: usize) -> usize {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let mut seen = self
                .state
                .shared_codex_appserver
                .started_turns_for_test()
                .iter()
                .map(|(_, items)| {
                    items
                        .iter()
                        .map(|item| {
                            serde_json::to_string(item)
                                .map(|s| s.matches(needle).count())
                                .unwrap_or(0)
                        })
                        .sum::<usize>()
                })
                .sum::<usize>();
            let worker_session_ids: Vec<String> =
                sqlx::query_scalar("SELECT id FROM worker_sessions")
                    .fetch_all(self.repo.pool())
                    .await
                    .unwrap();
            for id in worker_session_ids {
                if let Some(handle) = self.state.harness.get(&id) {
                    for obs in handle.pending_queue_for_test().await {
                        seen += serde_json::to_string(&obs)
                            .map(|s| s.matches(needle).count())
                            .unwrap_or(0);
                    }
                }
            }
            if seen >= want || std::time::Instant::now() >= deadline {
                return seen;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    /// How many copies of `needle` the DAEMON was actually handed — turns only. A runtime whose row was retired
    /// under it keeps its queue in memory, so `copies_in_harness` would read a double delivery.
    async fn delivered_copies(&self, needle: &str, want: usize) -> usize {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let seen = self
                .state
                .shared_codex_appserver
                .started_turns_for_test()
                .iter()
                .map(|(_, items)| {
                    items
                        .iter()
                        .map(|item| {
                            serde_json::to_string(item)
                                .map(|s| s.matches(needle).count())
                                .unwrap_or(0)
                        })
                        .sum::<usize>()
                })
                .sum::<usize>();
            if seen >= want || std::time::Instant::now() >= deadline {
                return seen;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    /// Everything the fake app-server was ever asked to run a turn on, as one JSON blob of *rendered* text.
    async fn started_turn_text(&self, needle: &str) -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let text =
                serde_json::to_string(&self.state.shared_codex_appserver.started_turns_for_test())
                    .unwrap_or_default();
            if text.contains(needle) || std::time::Instant::now() >= deadline {
                return text;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    /// Every `planner-harness-start` payload as persisted into `operations.payload_json`.
    async fn operation_payloads(&self) -> Vec<Value> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT payload_json FROM operations WHERE kind = 'planner-harness-start'",
        )
        .fetch_all(self.repo.pool())
        .await
        .unwrap();
        rows.into_iter()
            .map(|row| serde_json::from_str(&row).unwrap())
            .collect()
    }

    /// `POST /api/track-recipes`. Returns its id.
    async fn create_recipe(&self, title: &str, body: &str) -> String {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/track-recipes")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"title": title, "body": body}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let created: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        assert_eq!(status, StatusCode::CREATED, "recipe create: {created}");
        created["id"].as_str().unwrap().to_string()
    }

    /// `GET /api/tracks/{id}`, used here only to read the instantiated report back.
    async fn track_detail(&self, track_id: &str) -> Value {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/tracks/{track_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let detail: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        assert_eq!(status, StatusCode::OK, "track detail: {detail}");
        detail
    }

    /// Reject the `spawn_succeeded` phase write, so the driver's `set_phase` errors *after* `spawn_side_effect`
    /// installed a live harness: the only way to reach `OperationOutcome::Stuck` from a route test.
    async fn reject_spawn_succeeded(&self) {
        sqlx::query(
            "CREATE TRIGGER reject_spawn_succeeded BEFORE UPDATE ON operations \
             FOR EACH ROW WHEN NEW.phase = 'spawn_succeeded' \
             BEGIN SELECT RAISE(ABORT, 'injected: spawn_succeeded write rejected'); END",
        )
        .execute(self.repo.pool())
        .await
        .unwrap();
    }

    /// Park the next planner harness immediately before it can turn its pending queue into a turn. `ANY_RUNTIME`
    /// because the runtime under test does not exist yet.
    fn hold_the_next_drain(&self) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        install_planner_harness_drain_race_hook_for_test(
            ANY_RUNTIME,
            PlannerHarnessDrainRaceHook {
                entered: entered.clone(),
                release: release.clone(),
            },
        );
        (entered, release)
    }

    /// The one runtime row on this database, as `(id, card_id)`.
    async fn only_runtime(&self) -> (String, String) {
        let rows: Vec<(String, String)> = sqlx::query_as("SELECT id, card_id FROM worker_sessions")
            .fetch_all(self.repo.pool())
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "expected exactly one runtime row: {rows:?}");
        rows.into_iter().next().unwrap()
    }

    /// `queue_harvested_at_ms` for one runtime row.
    async fn harvest_stamp(&self, worker_session_id: &str) -> Option<i64> {
        sqlx::query_scalar("SELECT queue_harvested_at_ms FROM worker_sessions WHERE id = ?1")
            .bind(worker_session_id)
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }

    /// How many **persisted** runtime snapshots still hold `needle` on their `pending_queue`. The transfer is a
    /// move, so this counts owners rather than copies.
    async fn rows_holding(&self, needle: &str) -> usize {
        let rows: Vec<Option<String>> =
            sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions")
                .fetch_all(self.repo.pool())
                .await
                .unwrap();
        rows.into_iter()
            .filter(|state| {
                let Some(state) = state.as_deref() else {
                    return false;
                };
                let Ok(state) = serde_json::from_str::<Value>(state) else {
                    return false;
                };
                state
                    .get("pending_queue")
                    .map(|queue| queue.to_string().contains(needle))
                    .unwrap_or(false)
            })
            .count()
    }

    /// Poll until exactly `want` persisted snapshots still carry `needle`; waits on the successor's post-turn
    /// `persist_snapshot`, which the next restart's inherit reads.
    async fn wait_until_rows_holding(&self, needle: &str, want: usize) -> usize {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let seen = self.rows_holding(needle).await;
            if seen == want || std::time::Instant::now() >= deadline {
                return seen;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    /// Park the harness INSIDE `turn/start`, after the fake daemon has recorded the batch and before
    /// `maybe_issue_turn` writes the emptied queue back to the row.
    fn hold_the_next_turn_start(&self) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        self.state
            .shared_codex_appserver
            .install_turn_start_return_hook_for_test(TurnStartReturnHook {
                entered: entered.clone(),
                release: release.clone(),
            });
        (entered, release)
    }

    /// Retire a runtime in the database and NOTHING else — the durable half of the re-point fence without the
    /// process half; a live run loop whose row was retired under it is reachable in production.
    async fn retire_runtime_in_the_database(&self, worker_session_id: &str) {
        let worker_session_id = worker_session_id.to_string();
        write_in_tx_typed(self.repo.as_ref() as &dyn Repo, move |tx| {
            Box::pin(async move {
                session_mark_superseded_runtime_tx(tx, &worker_session_id)
                    .await
                    .map_err(calm_server::error::CalmError::from)
            })
        })
        .await
        .expect("retire runtime");
    }

    /// The `pending_queue` a runtime's PERSISTED snapshot still holds.
    async fn persisted_queue(&self, worker_session_id: &str) -> Vec<Value> {
        let state: Option<String> =
            sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id = ?1")
                .bind(worker_session_id)
                .fetch_one(self.repo.pool())
                .await
                .unwrap();
        state
            .and_then(|state| serde_json::from_str::<Value>(&state).ok())
            .and_then(|state| state.get("pending_queue").cloned())
            .and_then(|queue| queue.as_array().cloned())
            .unwrap_or_default()
    }

    /// Poll until a runtime's persisted queue reaches `want` entries; report what was actually seen.
    async fn wait_for_persisted_queue_len(&self, worker_session_id: &str, want: usize) -> usize {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let seen = self.persisted_queue(worker_session_id).await.len();
            if seen == want || std::time::Instant::now() >= deadline {
                return seen;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    /// The card's currently ACTIVE runtime id; safe on a track that also has a terminal card with a runtime.
    async fn active_runtime_of_card(&self, card_id: &str) -> String {
        sqlx::query_scalar(
            "SELECT id FROM worker_sessions WHERE card_id = ?1 \
             AND state IN ('starting','running','idle','turn_pending')",
        )
        .bind(card_id)
        .fetch_one(self.repo.pool())
        .await
        .unwrap()
    }

    /// `POST /api/today/launchpad/ensure` — the production caller of `prepare_tx`'s NON-deferred arm on its
    /// second and later calls.
    async fn ensure_launchpad(&self) -> (StatusCode, Value) {
        self.post_json("/api/today/launchpad/ensure", "{}").await
    }

    /// `POST /api/cards/{id}/planner/input` — the production send path, whose enqueue is persisted before the response.
    async fn send_planner_input(&self, card_id: &str, text: &str) -> (StatusCode, Value) {
        self.post_json(
            &format!("/api/cards/{card_id}/planner/input"),
            &json!({ "text": text }).to_string(),
        )
        .await
    }

    async fn get_json(&self, uri: &str) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
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

    async fn post_json(&self, uri: &str, body: &str) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
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
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// `POST /api/cards/{id}/planner/reset` — the production dormant-restart route (`force_new_thread: true`).
    async fn reset_planner(&self, card_id: &str) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/cards/{card_id}/planner/reset"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
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

    async fn shutdown_harnesses(&self) {
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM worker_sessions")
            .fetch_all(self.repo.pool())
            .await
            .unwrap();
        for id in ids {
            if let Some(handle) = self.state.harness.remove(&id) {
                let _ = handle.shutdown().await;
            }
        }
    }
}

/// The headline: the sentence reaches the agent, once, with the create.
#[tokio::test]
async fn the_first_message_reaches_the_agent_exactly_once() {
    let b = boot().await;
    let (status, body) = b
        .create_track(Some("idem-headline"), Some("refactor the parser"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    // `copies_in_harness` returns as soon as it has seen `want`, so "exactly once" is: wait for the delivery
    // with its own budget, then ask for a SECOND copy, which burns the full deadline before answering 1.
    assert_eq!(
        b.copies_in_harness("refactor the parser", 1).await,
        1,
        "premise: the create's first message must reach the harness"
    );
    assert_eq!(
        b.copies_in_harness("refactor the parser", 2).await,
        1,
        "…and exactly once — no second copy within the full deadline"
    );
    assert_eq!(
        b.user_message_event_count().await,
        1,
        "and it must be audited exactly once"
    );
    b.shutdown_harnesses().await;
}

/// Two independent assertions: swapping the observation type keeps the audit row but changes the rendered
/// turn text; attributing the event to a machine actor keeps the render but loses the human.
#[tokio::test]
async fn the_first_message_is_a_user_message_attributed_to_the_human() {
    let b = boot().await;
    let (status, body) = b
        .create_track(Some("idem-attrib"), Some("please rename the track"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    let turns = b.started_turn_text("User says:").await;
    assert!(
        turns.contains("User says:"),
        "the seeded observation must render as a user message, not as a bare track goal: turns={turns}"
    );
    assert!(
        turns.contains("please rename the track"),
        "…carrying the user's own text: turns={turns}"
    );
    // The persisted `actor` column, verbatim: `ActorId::User` serialises as `{"kind":"User"}`.
    assert_eq!(
        b.user_message_actor().await,
        r#"{"kind":"User"}"#,
        "the audit row must attribute the message to the human who typed it"
    );
    b.shutdown_harnesses().await;
}

/// A create with **no** `first_message` **and no `Idempotency-Key`** is unchanged: two identical creates
/// are two tracks, and the operation payload carries no `first_message` key at all (`skip_serializing_if`).
#[tokio::test]
async fn a_message_less_create_without_a_key_is_unchanged() {
    let b = boot().await;
    let (first, body) = b.create_track(None, None).await;
    assert_eq!(first, StatusCode::CREATED, "body={body}");
    let (second, _) = b.create_track(None, None).await;
    assert_eq!(second, StatusCode::CREATED);
    assert_eq!(
        b.track_count().await,
        2,
        "no key means nothing to be idempotent about: two creates are two tracks"
    );
    assert_eq!(
        b.binding_count().await,
        0,
        "and no binding row, because there is no key to bind"
    );
    assert_eq!(
        b.user_message_event_count().await,
        0,
        "nothing was typed, so nothing may be enqueued"
    );
    let payloads = b.operation_payloads().await;
    assert_eq!(payloads.len(), 2, "one start per create: {payloads:?}");
    for payload in payloads {
        assert!(
            payload.get("first_message").is_none(),
            "the message-less payload must not carry the key at all: {payload}"
        );
    }
    b.shutdown_harnesses().await;
}

/// A rejected message leaves nothing behind: the `first_message` validation runs before *any* row is minted.
#[tokio::test]
async fn a_rejected_first_message_leaves_no_track_and_no_cards() {
    let b = boot().await;
    let (blank, body) = b.create_track(Some("idem-blank"), Some("   \n  ")).await;
    assert_eq!(blank, StatusCode::BAD_REQUEST, "body={body}");
    assert_eq!(b.track_count().await, 0);
    assert_eq!(b.card_count().await, 0);

    let too_long = "x".repeat(32_769);
    let (over, body) = b.create_track(Some("idem-long"), Some(&too_long)).await;
    assert_eq!(over, StatusCode::BAD_REQUEST, "body={body}");
    assert_eq!(b.track_count().await, 0);
    assert_eq!(b.card_count().await, 0);

    // 32768 characters is the ceiling, counted in CHARACTERS not bytes.
    let at_limit = "é".repeat(32_768);
    let (ok, body) = b.create_track(Some("idem-limit"), Some(&at_limit)).await;
    assert_eq!(ok, StatusCode::CREATED, "body={body}");
    b.shutdown_harnesses().await;
}

/// A `template_id` create seeds the report inside the create transaction and then starts the harness like
/// a blank create; read from the harness, because a create that dropped the message would also answer 201.
#[tokio::test]
async fn a_template_create_delivers_the_first_message() {
    let b = boot().await;
    // A needle that appears nowhere in the `small-change` template body.
    let needle = "check the p99 on the way out";
    let (status, body) = b
        .post_create(
            Some("idem-template"),
            json!({
                "area_id": b.area_id,
                "title": "",
                "template_id": "small-change",
                "first_message": needle,
                "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track_id = body["id"].as_str().unwrap().to_string();

    // Premise: this really is the template shape.
    let template_id: Option<String> =
        sqlx::query_scalar("SELECT template_id FROM tracks WHERE id = ?1")
            .bind(&track_id)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        template_id.as_deref(),
        Some("small-change"),
        "premise: the create must have taken the template branch"
    );

    assert_eq!(
        b.copies_in_harness(needle, 1).await,
        1,
        "a template create must deliver the first message too"
    );
    assert_eq!(
        b.copies_in_harness(needle, 2).await,
        1,
        "…and exactly once — no second copy within the full deadline"
    );
    assert_eq!(
        b.user_message_event_count().await,
        1,
        "and it must be audited exactly once"
    );
    b.shutdown_harnesses().await;
}

// `first_message` × `recipe_id`: the recipe is instantiated *inside* the create transaction and the message
// is delivered by the operation submitted *after* it commits, so a bug can drop either one alone.

/// A recipe body with one task, so instantiation is checkable on a structural field.
fn recipe_body() -> String {
    format!(
        "# Rollout\n\nStage it, then watch the dashboards.\n\n{}",
        format_args!(
            "```neige-block task\n{}\n```\n",
            serde_json::to_string_pretty(&json!({
                "key": "stage",
                "goal": "stage the build",
                "kind": "codex",
                "acceptance": "the stage host serves it",
            }))
            .unwrap()
        )
    )
}

fn report_payload(detail: &Value) -> &Value {
    detail["cards"]
        .as_array()
        .expect("cards array")
        .iter()
        .find(|card| card["kind"] == "track-report")
        .map(|card| &card["payload"])
        .expect("track-report card")
}

/// Both halves asserted on the mechanism: the report is read back through `GET /api/tracks/{id}`, and the
/// delivery uses the two-step "then ask for a second copy" budget.
#[tokio::test]
async fn a_first_message_is_delivered_once_on_a_recipe_create() {
    let b = boot().await;
    let recipe_id = b.create_recipe("rollout flow", &recipe_body()).await;

    // A needle that appears nowhere in the recipe.
    let needle = "watch the p99 while it stages";
    let (status, body) = b
        .post_create(
            Some("idem-recipe"),
            json!({
                "area_id": b.area_id,
                "title": "",
                "recipe_id": recipe_id,
                "first_message": needle,
                "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track_id = body["id"].as_str().unwrap().to_string();

    // Half one: the recipe really was instantiated into this track.
    let payload = report_payload(&b.track_detail(&track_id).await).clone();
    assert_eq!(
        payload["summary"],
        json!("rollout flow"),
        "the recipe title must become the report summary: {payload}"
    );
    assert!(
        payload["body"]
            .as_str()
            .unwrap_or_default()
            .contains("Stage it, then watch the dashboards."),
        "the recipe prose must survive: {payload}"
    );
    let task_keys: Vec<Value> = payload["blocks"]
        .as_array()
        .expect("blocks snapshot")
        .iter()
        .filter(|block| block["kind"] == "task")
        .map(|block| block["payload"]["key"].clone())
        .collect();
    assert_eq!(
        task_keys,
        vec![json!("stage")],
        "the recipe's task must be on the new track's report: {payload}"
    );
    // A recipe id is not a plugin-bindable template id.
    let template_id: Option<String> =
        sqlx::query_scalar("SELECT template_id FROM tracks WHERE id = ?1")
            .bind(&track_id)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        template_id, None,
        "a recipe must not land on tracks.template_id"
    );

    // Half two: the sentence was delivered, exactly once.
    assert_eq!(
        b.copies_in_harness(needle, 1).await,
        1,
        "premise: the recipe create's first message must reach the harness"
    );
    assert_eq!(
        b.copies_in_harness(needle, 2).await,
        1,
        "…and exactly once — no second copy within the full deadline"
    );
    assert_eq!(
        b.user_message_event_count().await,
        1,
        "and it must be audited exactly once"
    );
    b.shutdown_harnesses().await;
}

/// The one refusal decided *inside* the create transaction (`TrackInit::Recipe` reads the row in the same tx
/// as the mint), so "nothing is left behind" is a rollback claim asserted over every row kind, operations included.
#[tokio::test]
async fn a_first_message_with_an_unknown_recipe_leaves_nothing_behind() {
    let b = boot().await;
    let (status, body) = b
        .post_create(
            Some("idem-missing-recipe"),
            json!({
                "area_id": b.area_id,
                "title": "",
                "recipe_id": "recipe-does-not-exist",
                "first_message": "this must not be delivered anywhere",
                "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
            }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("recipe-does-not-exist"),
        "the refusal must name the recipe rather than read as a generic failure: {body}"
    );

    assert_eq!(
        b.track_count().await,
        0,
        "the rolled-back create leaves no track"
    );
    assert_eq!(b.card_count().await, 0, "and no cards");
    assert_eq!(
        b.operation_count().await,
        0,
        "and no operation — the delivery is submitted only after the create commits"
    );
    assert_eq!(
        b.user_message_event_count().await,
        0,
        "and nothing may be audited as enqueued"
    );
}

// A harness that fails to start: with a message a failed start is a 5xx, without one the same failure is
// still a 201. `fail_next_thread_start_for_test` reaches the `Failed` branch; rejecting `spawn_succeeded` reaches `Stuck`.

/// Not a compensating handler: the create transaction has committed and the workspace is materialized, so
/// the 5xx says "the delivery promise was not kept", not "nothing happened".
#[tokio::test]
async fn a_failed_harness_start_fails_a_create_that_carried_a_first_message() {
    let b = boot().await;
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();

    let (status, body) = b
        .create_track(Some("idem-failed-start"), Some("do not lose this sentence"))
        .await;
    assert!(
        status.is_server_error(),
        "a create that promised to deliver a message must not answer 2xx when the harness that \
         would have delivered it never started: status={status} body={body}"
    );

    // Premise: the message really was not delivered, checked at the harness. The audit row IS written here
    // (`prepare_tx` commits it before `AppServerInteract` fails), so it only says a delivery was attempted.
    assert_eq!(
        b.copies_in_harness("do not lose this sentence", 1).await,
        0,
        "premise: no agent may have been handed the sentence, since no thread ever started"
    );
    // And the create is NOT undone.
    assert_eq!(
        b.track_count().await,
        1,
        "the track is already committed when the harness starts; this 5xx does not roll it back"
    );
    assert!(
        b.card_count().await > 0,
        "…nor its cards: non-201 does not mean no side effect on this handler"
    );
    b.shutdown_harnesses().await;
}

/// The control: nothing the user typed was riding on this operation, so "the track exists, its planner
/// agent is inert" is a complete and recoverable answer.
#[tokio::test]
async fn a_failed_harness_start_still_creates_a_track_without_a_first_message() {
    let b = boot().await;
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();

    let (status, body) = b.create_track(None, None).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "a message-less create keeps its documented `warn!` + 201 semantics when the harness \
         fails to start: body={body}"
    );
    assert_eq!(b.track_count().await, 1);
    assert_eq!(b.user_message_event_count().await, 0);
    b.shutdown_harnesses().await;
}

// What the 500 is allowed to SAY: the phase write can fail *after* `spawn_side_effect` installed a live
// harness, so the message may already be delivered and the text may not assert non-delivery.

/// The wording is asserted in both directions — the new text is present AND the old "was not delivered"
/// claim is absent — because asserting one alone goes vacuous the next time the sentence is rewritten.
#[tokio::test]
async fn a_stuck_start_after_spawn_has_already_delivered_the_first_message() {
    let b = boot().await;
    // Reject the `spawn_succeeded` phase write: fail the operation at the one point past the side effect.
    b.reject_spawn_succeeded().await;

    let needle = "this sentence is already on its way";
    let (status, body) = b.create_track(Some("idem-stuck"), Some(needle)).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a create whose harness start did not complete still answers 5xx: body={body}"
    );

    // The observed fact that makes a "not delivered" 500 a lie: the harness is live and holding the sentence.
    assert_eq!(
        b.copies_in_harness(needle, 1).await,
        1,
        "the spawn succeeded before the phase write failed, so the seeded observation is in a \
         live harness and the turn is out: body={body}"
    );

    let text = body.to_string();
    // Positive: the text says the outcome is unknown…
    assert!(
        text.contains("cannot tell whether the first message reached the agent"),
        "the 500 must report an unknown delivery, since it is unknown: {text}"
    );
    // ...and negative: it does not assert the delivery failed.
    assert!(
        !text.contains("not delivered"),
        "the 500 must not claim the message was not delivered — on this path it was: {text}"
    );
    assert!(
        !text.contains("send the message from the track itself"),
        "…nor instruct an unconditional resend, which duplicates it on this path: {text}"
    );
    // The two properties the server CAN prove, named in the text.
    assert!(
        text.contains("no second track"),
        "the 500 must say that retrying under the same key mints no second track — that is what \
         #1384 bought and it is the only actionable thing here: {text}"
    );
    assert!(
        text.contains("no second copy"),
        "…and that it delivers no second copy, which is the half a user is actually afraid of: \
         {text}"
    );
    // And the claim it must NOT make: that the track is fine. A replay does not repair an attached workspace
    // whose directory was deleted.
    assert!(
        text.contains("does not assert that the track is usable"),
        "the 500 must not let 'safe to retry' be read as 'the track is fine': {text}"
    );
    assert!(
        !text.contains("the create is not retryable"),
        "…and must drop the claim #1384 made false: {text}"
    );

    b.shutdown_harnesses().await;
}

/// `Idempotency-Key` is required exactly when `first_message` is present.
#[tokio::test]
async fn a_first_message_without_an_idempotency_key_is_rejected_before_any_mint() {
    let b = boot().await;
    let (status, body) = b.create_track(None, Some("no key, no track")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert_eq!(b.track_count().await, 0, "no track may survive the refusal");
    assert_eq!(b.card_count().await, 0, "and no cards either");
    assert_eq!(b.binding_count().await, 0);
}

/// A message-less create that sends an `Idempotency-Key` binds it, and the same key again returns the same
/// track through the message-less resume arm.
#[tokio::test]
async fn a_message_less_create_with_a_key_binds_and_replays() {
    let b = boot().await;
    let (first, first_body) = b.create_track(Some("idem-message-less"), None).await;
    assert_eq!(first, StatusCode::CREATED, "body={first_body}");
    assert_eq!(
        b.binding_count().await,
        1,
        "a keyed message-less create must bind its key inside the mint transaction"
    );
    let (second, second_body) = b.create_track(Some("idem-message-less"), None).await;
    assert_eq!(
        second,
        StatusCode::CREATED,
        "the same key again is still a 201: body={second_body}"
    );
    assert_eq!(
        second_body["id"], first_body["id"],
        "…and it is the SAME track, not a second one: first={first_body:?} second={second_body:?}"
    );
    assert_eq!(
        b.track_count().await,
        1,
        "one key, one track — the property #1384 bought for the first_message path and #1426 \
         extends to this one"
    );
    assert_eq!(
        b.binding_count().await,
        1,
        "and the resume writes no second binding"
    );
    assert_eq!(
        b.user_message_event_count().await,
        0,
        "nothing was typed on either request, so nothing may be enqueued"
    );
    // A different key is a different create: the mechanism is per-key.
    let (third, third_body) = b.create_track(Some("idem-message-less-2"), None).await;
    assert_eq!(third, StatusCode::CREATED);
    assert_ne!(third_body["id"], first_body["id"]);
    assert_eq!(b.track_count().await, 2);
    b.shutdown_harnesses().await;
}

/// `create_request_sha256` does not cover `first_message`, so the two bodies hash identically; only the
/// binding row's fingerprint variant tells the shapes apart.
#[tokio::test]
async fn a_key_bound_by_one_create_shape_refuses_the_other() {
    let b = boot().await;
    // A message-less create binds the key.
    let (created, body) = b.create_track(Some("idem-shape"), None).await;
    assert_eq!(created, StatusCode::CREATED, "body={body}");

    // Same key, same body, plus a sentence: the digests match, the shapes do not.
    let (with_message, body) = b.create_track(Some("idem-shape"), Some("ship it")).await;
    assert_eq!(
        with_message,
        StatusCode::CONFLICT,
        "adding a first_message under a message-less binding must be a conflict: body={body}"
    );

    // And the reverse direction, on a key a first_message create bound.
    let (created, body) = b.create_track(Some("idem-shape-2"), Some("ship it")).await;
    assert_eq!(created, StatusCode::CREATED, "body={body}");
    let (message_less, body) = b.create_track(Some("idem-shape-2"), None).await;
    assert_eq!(
        message_less,
        StatusCode::CONFLICT,
        "dropping the first_message under a message-carrying binding must be a conflict too: \
         body={body}"
    );

    // The ordinary payload-conflict half still applies: same key, no message either time, different body.
    let (renamed, body) = b
        .post_create(
            Some("idem-shape"),
            json!({
                "area_id": b.area_id,
                "title": "a different title",
                "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
            }),
        )
        .await;
    assert_eq!(
        renamed,
        StatusCode::CONFLICT,
        "a different message-less create under a bound key is a payload conflict: body={body}"
    );

    assert_eq!(
        b.track_count().await,
        2,
        "every refusal above minted nothing: only the two accepted creates exist"
    );
    b.shutdown_harnesses().await;
}

/// Replaying a successful create returns the SAME track and does not re-deliver the message.
#[tokio::test]
async fn replaying_a_successful_create_returns_the_same_track_and_delivers_once() {
    let b = boot().await;
    let (first, first_body) = b
        .create_track(Some("idem-replay"), Some("ship the thing"))
        .await;
    assert_eq!(first, StatusCode::CREATED, "body={first_body}");
    assert_eq!(b.binding_count().await, 1, "the mint wrote its binding");
    // Give the first delivery its own budget before the replay, so the "no second copy" check burns its full
    // deadline on the question it is actually asking.
    assert_eq!(
        b.copies_in_harness("ship the thing", 1).await,
        1,
        "premise: the create delivered the sentence once"
    );
    let (second, second_body) = b
        .create_track(Some("idem-replay"), Some("ship the thing"))
        .await;
    assert_eq!(second, StatusCode::CREATED, "body={second_body}");

    assert_eq!(
        first_body["id"], second_body["id"],
        "the same key must return the same track"
    );
    assert_eq!(b.track_count().await, 1, "and must not mint a second one");
    assert_eq!(
        b.copies_in_harness("ship the thing", 2).await,
        1,
        "the replay must not deliver the instruction a second time"
    );
    assert_eq!(b.user_message_event_count().await, 1);
    b.shutdown_harnesses().await;
}

/// The workspace path travels in the `planner-harness-start` payload and `submit` compares `payload_hash`
/// first, so a replay must carry the chosen operation's own `cwd`, not the repointed `track.workspace.path`.
#[tokio::test]
async fn a_replay_survives_the_track_being_repointed_in_between() {
    let b = boot().await;
    let (first, first_body) = b
        .create_track(Some("idem-repoint-replay"), Some("ship the thing"))
        .await;
    assert_eq!(first, StatusCode::CREATED, "body={first_body}");
    let track_id = first_body["id"].as_str().unwrap().to_string();
    let (kind_before, managed_path) = b.workspace_row(&track_id).await;
    assert_eq!(
        kind_before, "managed",
        "premise: the create made it managed"
    );
    assert_eq!(
        b.copies_in_harness("ship the thing", 1).await,
        1,
        "premise: the create delivered the sentence once"
    );

    let target = user_repo(&b.tmp.path().join("my-project"));
    let (patched, patch_body) = b.repoint_to(&track_id, &target).await;
    assert_eq!(
        patched,
        StatusCode::OK,
        "premise: the re-point must succeed, or this test proves nothing: body={patch_body}"
    );
    let (kind_after, path_after) = b.workspace_row(&track_id).await;
    assert_eq!(kind_after, "attached");
    assert_eq!(
        PathBuf::from(&path_after),
        target,
        "premise: the track's cwd really moved"
    );
    assert_ne!(path_after, managed_path);

    let (second, second_body) = b
        .create_track(Some("idem-repoint-replay"), Some("ship the thing"))
        .await;
    assert_eq!(
        second,
        StatusCode::CREATED,
        "a byte-identical replay must still replay after the workspace moved — the 409 here would \
         say the caller changed its message when it did not: body={second_body}"
    );
    assert_eq!(
        first_body["id"], second_body["id"],
        "and it must be the same track"
    );
    assert_eq!(b.track_count().await, 1, "no second track");
    assert_eq!(
        b.copies_in_harness("ship the thing", 2).await,
        1,
        "and the replay must not deliver the instruction a second time"
    );
    // The mechanism, not just the status: the replayed payload carries the predecessor's cwd.
    assert_eq!(
        b.first_message_payload_cwds("ship the thing").await,
        vec![managed_path.clone()],
        "the replay must resubmit the chosen operation's payload, cwd included"
    );
    b.shutdown_harnesses().await;
}

/// The counterweight: a genuine retry really starts a harness, so it must use the current cwd, not the failed
/// attempt's (which the re-point has since moved into the trash).
#[tokio::test]
async fn a_retry_after_a_failure_uses_the_repointed_workspace() {
    let b = boot().await;
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (failed, failed_body) = b
        .create_track(Some("idem-repoint-retry"), Some("second time lucky"))
        .await;
    assert!(
        !failed.is_success(),
        "the injected thread/start failure must surface: status={failed} body={failed_body}"
    );
    let track_id: String = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    let (_, managed_path) = b.workspace_row(&track_id).await;

    let target = user_repo(&b.tmp.path().join("my-project"));
    let (patched, patch_body) = b.repoint_to(&track_id, &target).await;
    assert_eq!(
        patched,
        StatusCode::OK,
        "premise: the re-point must succeed: body={patch_body}"
    );
    let (_, path_after) = b.workspace_row(&track_id).await;
    assert_eq!(PathBuf::from(&path_after), target);

    let (retry, retry_body) = b
        .create_track(Some("idem-repoint-retry"), Some("second time lucky"))
        .await;
    assert_eq!(
        retry,
        StatusCode::CREATED,
        "the same key must retry, not replay the failure: body={retry_body}"
    );
    assert_eq!(b.track_count().await, 1, "the retry reuses the track");
    assert_eq!(
        b.copies_in_harness("second time lucky", 1).await,
        1,
        "the retry delivers the message the failed attempt never did"
    );
    let cwds = b.first_message_payload_cwds("second time lucky").await;
    assert_eq!(
        cwds.len(),
        2,
        "one payload per attempt — the failed one and the retry: {cwds:?}"
    );
    assert_eq!(cwds[0], managed_path, "the failed attempt saw the old cwd");
    assert_eq!(
        cwds[1], path_after,
        "but the retry really executes, so it must start in the workspace the track has NOW — the \
         old managed directory has been recycled out from under it"
    );
    b.shutdown_harnesses().await;
}

/// The success happened on `#2`, not the base key: `retryable_operation_key` walks past the `Failed` base and
/// stops on `#2`, which already holds a succeeded operation, so this is a replay.
#[tokio::test]
async fn a_replay_of_a_success_that_happened_on_a_retry_key_survives_a_repoint() {
    let b = boot().await;
    // (1) burn the base key.
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (failed, failed_body) = b
        .create_track(Some("idem-hash-replay"), Some("ship the thing"))
        .await;
    assert!(
        !failed.is_success(),
        "premise: the injected failure must surface: status={failed} body={failed_body}"
    );

    // (2) the success lands on `#2`.
    let (created, created_body) = b
        .create_track(Some("idem-hash-replay"), Some("ship the thing"))
        .await;
    assert_eq!(
        created,
        StatusCode::CREATED,
        "premise: the retry must succeed: body={created_body}"
    );
    let track_id = created_body["id"].as_str().unwrap().to_string();
    let (_, managed_path) = b.workspace_row(&track_id).await;
    assert_eq!(
        b.copies_in_harness("ship the thing", 1).await,
        1,
        "premise: the successful `#2` attempt delivered the sentence once"
    );

    // (3) move the workspace underneath it.
    let target = user_repo(&b.tmp.path().join("my-project"));
    let (patched, patch_body) = b.repoint_to(&track_id, &target).await;
    assert_eq!(
        patched,
        StatusCode::OK,
        "premise: the re-point must succeed, or this test proves nothing: body={patch_body}"
    );
    let (_, path_after) = b.workspace_row(&track_id).await;
    assert_ne!(path_after, managed_path, "premise: the cwd really moved");

    // (4) byte-identical replay.
    let (replay, replay_body) = b
        .create_track(Some("idem-hash-replay"), Some("ship the thing"))
        .await;
    assert_eq!(
        replay,
        StatusCode::CREATED,
        "the chosen key `#2` already holds a SUCCEEDED operation, so this is a replay and must \
         resubmit that operation's payload — a 409 here tells a byte-identical caller it changed \
         its message: body={replay_body}"
    );
    assert_eq!(
        created_body["id"], replay_body["id"],
        "and it must be the same track"
    );
    assert_eq!(b.track_count().await, 1, "no second track");
    assert_eq!(
        b.copies_in_harness("ship the thing", 2).await,
        1,
        "and the replay must not deliver the instruction a second time"
    );
    b.shutdown_harnesses().await;
}

/// The same key with a different message is a conflict, not a silent replay of the first sentence.
#[tokio::test]
async fn the_same_key_with_a_different_first_message_is_a_conflict() {
    let b = boot().await;
    let (first, body) = b
        .create_track(Some("idem-edit"), Some("original draft"))
        .await;
    assert_eq!(first, StatusCode::CREATED, "body={body}");

    let (second, body) = b
        .create_track(Some("idem-edit"), Some("edited draft"))
        .await;
    assert_eq!(second, StatusCode::CONFLICT, "body={body}");
    assert_eq!(b.track_count().await, 1, "the rejected edit minted nothing");
    assert_eq!(
        b.copies_in_harness("edited draft", 1).await,
        0,
        "and delivered nothing"
    );
    b.shutdown_harnesses().await;
}

/// After a terminally failed attempt the same key genuinely RETRIES: no replay of the recorded failure, no second track.
#[tokio::test]
async fn the_same_key_after_a_failed_start_retries_against_the_same_track() {
    let b = boot().await;
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (failed, failed_body) = b
        .create_track(Some("idem-retry"), Some("second time lucky"))
        .await;
    assert!(
        !failed.is_success(),
        "the injected thread/start failure must surface: status={failed} body={failed_body}"
    );
    assert_eq!(
        b.track_count().await,
        1,
        "the failed attempt left its track"
    );

    let (retry, retry_body) = b
        .create_track(Some("idem-retry"), Some("second time lucky"))
        .await;
    assert_eq!(
        retry,
        StatusCode::CREATED,
        "the same key must retry, not replay the failure: body={retry_body}"
    );
    assert_eq!(
        b.track_count().await,
        1,
        "the retry must reuse the track, not mint a second one"
    );
    assert_eq!(
        b.copies_in_harness("second time lucky", 1).await,
        1,
        "premise: the retry delivers the message the failed attempt never did"
    );
    assert_eq!(
        b.copies_in_harness("second time lucky", 2).await,
        1,
        "…exactly once: the failed attempt's copy never reached the harness, and the retry \
         delivered one"
    );
    b.shutdown_harnesses().await;
}

/// The 409 comes from the payload hash bound to a *specific* operation key, and a terminal failure moves the
/// retry to a fresh `#N` key, so an edited sentence resent after a failure is accepted; the abandoned draft must not be delivered too.
#[tokio::test]
async fn the_same_key_after_a_failure_accepts_an_edited_first_message() {
    let b = boot().await;
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (failed, failed_body) = b
        .create_track(
            Some("idem-edit-after-failure"),
            Some("the draft that never left"),
        )
        .await;
    assert!(
        !failed.is_success(),
        "the injected thread/start failure must surface: status={failed} body={failed_body}"
    );
    assert_eq!(
        b.track_count().await,
        1,
        "the failed attempt left its track"
    );

    let (retry, retry_body) = b
        .create_track(
            Some("idem-edit-after-failure"),
            Some("the sentence I actually meant"),
        )
        .await;
    assert_eq!(
        retry,
        StatusCode::CREATED,
        "after a terminal failure the same key runs under a fresh `#N` operation key, which no \
         earlier payload hash is bound to — so the edited message is a retry, not a 409: \
         body={retry_body}"
    );
    assert_eq!(
        b.track_count().await,
        1,
        "and it still lands on the track the failed attempt created"
    );
    assert_eq!(
        b.copies_in_harness("the sentence I actually meant", 1)
            .await,
        1,
        "the edited sentence is the one that gets delivered"
    );
    assert_eq!(
        b.copies_in_harness("the draft that never left", 1).await,
        0,
        "and the abandoned draft is never delivered — the retry replaces it, it does not \
         accompany it"
    );
    // TWO audit rows for ONE delivery: the failed attempt's `prepare_tx` committed its enqueue row and only the
    // thread start afterwards failed; compensation does not roll back a committed event row.
    assert_eq!(
        b.user_message_event_count().await,
        2,
        "one delivered message, but two audit rows — the failed attempt's row survives its \
         compensation"
    );

    let (replay, replay_body) = b
        .create_track(
            Some("idem-edit-after-failure"),
            Some("the sentence I actually meant"),
        )
        .await;
    assert_eq!(
        replay,
        StatusCode::CREATED,
        "the edited message became the successful `#2` attempt, so replaying that exact attempt \
         must join it rather than comparing against the abandoned base-attempt draft: \
         body={replay_body}"
    );
    assert_eq!(
        retry_body["id"], replay_body["id"],
        "the replay returns the track already minted by the base attempt"
    );
    assert_eq!(b.track_count().await, 1, "the replay mints no second track");
    assert_eq!(
        b.copies_in_harness("the sentence I actually meant", 1)
            .await,
        1,
        "the replay must not deliver the successful retry's message twice"
    );
    assert_eq!(
        b.user_message_event_count().await,
        2,
        "joining the successful `#2` attempt writes no third audit row"
    );
    b.shutdown_harnesses().await;
}

/// A fresh `#N` operation key relaxes only the message attempt; the track's creation parameters have already
/// committed, so changing one must still conflict with the binding-level request shape.
#[tokio::test]
async fn a_failed_operation_does_not_unbind_the_create_shape() {
    let b = boot().await;
    let key = "idem-failed-shape";
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let original = json!({
        "area_id": b.area_id,
        "title": "original title",
        "first_message": "same sentence",
        "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
    });
    let (failed, failed_body) = b.post_create(Some(key), original.clone()).await;
    assert!(
        !failed.is_success(),
        "the injected first attempt must fail: status={failed} body={failed_body}"
    );

    let mut edited = original;
    edited["title"] = json!("different title");
    let (status, body) = b.post_create(Some(key), edited).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], "conflict", "body={body}");
    assert_eq!(b.track_count().await, 1);
    let title: String = sqlx::query_scalar("SELECT title FROM tracks")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(title, "original title");
    b.shutdown_harnesses().await;
}

/// 64 terminally failed attempts exhaust the key, and the 65th says so with its own code. Driven through the
/// real endpoint 64 times so the `#N` chain, payload and track-reuse branch all have to hold.
#[tokio::test]
async fn a_key_exhausted_by_64_failed_attempts_answers_409() {
    let b = boot().await;
    for attempt in 1..=64 {
        b.state
            .shared_codex_appserver
            .fail_next_thread_start_for_test();
        let (status, body) = b
            .create_track(Some("idem-burn"), Some("burn this key"))
            .await;
        assert!(
            !status.is_success(),
            "attempt {attempt} was supposed to fail: status={status} body={body}"
        );
    }
    let (status, body) = b
        .create_track(Some("idem-burn"), Some("burn this key"))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        body["code"], "idempotency_key_exhausted",
        "an exhausted key must say so — 'use a new key' is the actionable answer: body={body}"
    );
    assert_eq!(
        b.track_count().await,
        1,
        "64 failed attempts under one key must still be one track"
    );

    let different_create = json!({
        "area_id": b.area_id,
        "title": "different title",
        "first_message": "burn this key",
        "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
    });
    let (status, body) = b.post_create(Some("idem-burn"), different_create).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        body["code"], "conflict",
        "the permanent create fingerprint is checked before retry-slot exhaustion: body={body}"
    );
    b.shutdown_harnesses().await;
}

/// The arm is decided BEFORE the create path validates the request: a byte-identical replay mints nothing,
/// so `validate_attached_workspace` re-reading a deleted directory must not 400 it.
#[tokio::test]
async fn a_replay_survives_the_attached_directory_being_deleted() {
    let b = boot().await;
    let attached = user_repo(&b.tmp.path().join("my-project"));
    let (first, first_body) = b
        .create_track_at(Some("idem-deleted-dir"), Some("ship the thing"), &attached)
        .await;
    assert_eq!(first, StatusCode::CREATED, "body={first_body}");
    let track_id = first_body["id"].as_str().unwrap().to_string();
    let (kind, path) = b.workspace_row(&track_id).await;
    assert_eq!(kind, "attached", "premise: the explicit cwd attached it");
    assert_eq!(
        PathBuf::from(&path),
        attached,
        "premise: onto the directory we are about to delete"
    );
    assert_eq!(
        b.copies_in_harness("ship the thing", 1).await,
        1,
        "premise: the successful create delivered the sentence once"
    );

    // The disturbance: the user's directory goes away. The harness is deliberately left running — shutting it
    // down would drop any copy still in its pending queue.
    std::fs::remove_dir_all(&attached).unwrap();
    assert!(!attached.exists(), "premise: the directory really is gone");

    let (replay, replay_body) = b
        .create_track_at(Some("idem-deleted-dir"), Some("ship the thing"), &attached)
        .await;
    assert_eq!(
        replay,
        StatusCode::CREATED,
        "a byte-identical replay mints nothing, so the create path's disk check must not run at \
         all — a 400 here refuses a request that was already accepted, forever: body={replay_body}"
    );
    assert_eq!(
        first_body["id"], replay_body["id"],
        "and it must be the same track"
    );
    assert_eq!(b.track_count().await, 1, "no second track");
    assert_eq!(
        b.copies_in_harness("ship the thing", 2).await,
        1,
        "and the replay must not deliver the instruction a second time"
    );
    // The replay 201s and the workspace is still broken: `materialize_workspace` is a no-op for `Attached`.
    assert!(
        !attached.exists(),
        "the replay must NOT have recreated the user's directory: that is the deliberate \
         carve-out, and a test that stopped observing it would let the carve-out quietly change"
    );
    b.shutdown_harnesses().await;
}

/// Constructed with a `.git` removal rather than a whole-directory delete so the retry has a real directory
/// to run in (`PATCH` refuses to repoint an *attached* workspace).
#[tokio::test]
async fn a_retry_after_a_failure_survives_the_attached_directory_ceasing_to_validate() {
    let b = boot().await;
    let attached = user_repo(&b.tmp.path().join("my-project"));
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (failed, failed_body) = b
        .create_track_at(
            Some("idem-retry-invalid-dir"),
            Some("second time lucky"),
            &attached,
        )
        .await;
    assert!(
        !failed.is_success(),
        "premise: the injected thread/start failure must surface: status={failed} body={failed_body}"
    );
    assert_eq!(
        b.track_count().await,
        1,
        "the failed attempt left its track"
    );
    let track_id: String = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    let (_, path) = b.workspace_row(&track_id).await;
    assert_eq!(PathBuf::from(&path), attached);

    // The disturbance: the directory stops satisfying the create-path check while remaining usable.
    std::fs::remove_dir_all(attached.join(".git")).unwrap();

    let (retry, retry_body) = b
        .create_track_at(
            Some("idem-retry-invalid-dir"),
            Some("second time lucky"),
            &attached,
        )
        .await;
    assert_eq!(
        retry,
        StatusCode::CREATED,
        "the retry mints nothing either, so the create path's disk check must not stand between \
         it and the workspace the track has now: body={retry_body}"
    );
    assert_eq!(b.track_count().await, 1, "the retry reuses the track");
    let cwds = b.first_message_payload_cwds("second time lucky").await;
    assert_eq!(
        cwds.len(),
        2,
        "one payload per attempt — the failed one and the retry: {cwds:?}"
    );
    assert_eq!(
        PathBuf::from(&cwds[1]),
        attached,
        "and the retry really executes, in the workspace the track has now"
    );
    assert_eq!(
        b.copies_in_harness("second time lucky", 1).await,
        1,
        "premise: the retry delivers the message the failed attempt never did"
    );
    assert_eq!(
        b.copies_in_harness("second time lucky", 2).await,
        1,
        "…exactly once"
    );
    b.shutdown_harnesses().await;
}

/// Moving the arm decision in front of the create-path validation must not remove that validation from the
/// path that still mints.
#[tokio::test]
async fn a_create_without_a_first_message_still_runs_every_create_check() {
    let b = boot().await;
    let base = json!({
        "area_id": b.area_id,
        "title": "",
        "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
    });

    // `cwd` shape.
    let mut relative = base.clone();
    relative["cwd"] = json!("not/absolute");
    let (status, body) = b.post_create(None, relative).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");

    // Attached-workspace existence — the very check the resuming arms skip.
    let mut missing = base.clone();
    missing["cwd"] = json!(b.tmp.path().join("nope").to_string_lossy());
    missing["attach_folder"] = json!(true);
    let (status, body) = b.post_create(None, missing).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");

    // Attached workspace that exists but is not a Git work tree.
    let plain = b.tmp.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let mut not_git = base.clone();
    not_git["cwd"] = json!(plain.to_string_lossy());
    not_git["attach_folder"] = json!(true);
    let (status, body) = b.post_create(None, not_git).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");

    // Template admission.
    let mut unknown_template = base.clone();
    unknown_template["template_id"] = json!("no-such-template");
    let (status, body) = b.post_create(None, unknown_template).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");

    // `template_input` binding: no bound plugin here, so any input is refused.
    let mut unbound_input = base.clone();
    unbound_input["template_input"] = json!({"anything": 1});
    let (status, body) = b.post_create(None, unbound_input).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");

    // Area 404.
    let mut unknown_area = base.clone();
    unknown_area["area_id"] = json!("area-does-not-exist");
    let (status, body) = b.post_create(None, unknown_area).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body={body}");

    assert_eq!(
        b.track_count().await,
        0,
        "none of the refusals above may mint a track"
    );

    // And the happy legacy paths still work, plain and templated.
    let (plain_create, plain_body) = b.post_create(None, base.clone()).await;
    assert_eq!(plain_create, StatusCode::CREATED, "body={plain_body}");
    let mut templated = base.clone();
    templated["template_id"] = json!("small-change");
    let (template_create, template_body) = b.post_create(None, templated).await;
    assert_eq!(template_create, StatusCode::CREATED, "body={template_body}");
    assert_eq!(b.track_count().await, 2);
    assert_eq!(
        b.user_message_event_count().await,
        0,
        "nothing was typed on either, so nothing may be enqueued"
    );
    b.shutdown_harnesses().await;
}

/// A daemon outage adopts the track it already minted instead of minting one per retry: `validate` refuses
/// before `insert_operation`, so the binding row is the only record of which track a key created.
#[tokio::test]
async fn a_daemon_outage_adopts_the_track_it_already_minted_under_one_key() {
    let b = boot_without_daemon().await;
    let (first, first_body) = b.create_track(Some("idem-out"), Some("do the thing")).await;
    let (second, second_body) = b.create_track(Some("idem-out"), Some("do the thing")).await;
    assert_eq!(
        first,
        StatusCode::INTERNAL_SERVER_ERROR,
        "the daemon is down, so the harness start cannot succeed: body={first_body}"
    );
    assert_eq!(
        second,
        StatusCode::INTERNAL_SERVER_ERROR,
        "…and the retry fails the same way, for the same reason: body={second_body}"
    );
    // The load-bearing number.
    assert_eq!(
        b.track_count().await,
        1,
        "one key, one track — the retry must adopt the track the first attempt already minted, \
         not mint another one"
    );
    assert_eq!(
        b.card_count().await,
        2,
        "its planner and report cards, once"
    );
    assert_eq!(
        b.binding_count().await,
        1,
        "and exactly one binding row, written by the mint that committed it"
    );
    assert_eq!(
        b.operation_count().await,
        0,
        "premise: `validate` refuses before `insert_operation`, so there is still no operation \
         row — which is precisely why the operation row could never have carried this binding"
    );
    assert_eq!(b.user_message_event_count().await, 0);
    b.shutdown_harnesses().await;
}

/// The durable binding must remember the request as well as the ids: without a binding-level message digest,
/// an edited sentence is accepted on the vacant operation key and delivered to the original track.
#[tokio::test]
async fn an_operationless_binding_rejects_a_different_first_message() {
    let b = boot_without_daemon().await;
    let key = "idem-operationless-message";
    let (first, first_body) = b.create_track(Some(key), Some("original sentence")).await;
    assert_eq!(
        first,
        StatusCode::INTERNAL_SERVER_ERROR,
        "body={first_body}"
    );
    assert_eq!(b.track_count().await, 1);
    assert_eq!(b.binding_count().await, 1);
    assert_eq!(
        b.operation_count().await,
        0,
        "premise: no payload hash exists"
    );

    let running_app = b.app_with_running_daemon();
    let mut edited = json!({
        "area_id": b.area_id,
        "title": "",
        "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        "first_message": "edited sentence",
    });
    let (status, body) = b
        .post_create_on(running_app, Some(key), edited.take())
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], "conflict", "body={body}");
    assert_eq!(b.track_count().await, 1, "the rejected edit mints nothing");
    assert_eq!(
        b.operation_count().await,
        0,
        "the request fingerprint is checked before submit writes an operation"
    );
    assert_eq!(b.user_message_event_count().await, 0);
    b.shutdown_harnesses().await;
}

/// Create parameters have already taken effect once the binding exists: a vacant operation key must not make
/// a different title look like a retryable operation parameter.
#[tokio::test]
async fn an_operationless_binding_rejects_a_different_create_shape() {
    let b = boot_without_daemon().await;
    let key = "idem-operationless-shape";
    let base = json!({
        "area_id": b.area_id,
        "title": "original title",
        "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        "first_message": "same sentence",
    });
    let (first, first_body) = b.post_create(Some(key), base.clone()).await;
    assert_eq!(
        first,
        StatusCode::INTERNAL_SERVER_ERROR,
        "body={first_body}"
    );
    assert_eq!(
        b.operation_count().await,
        0,
        "premise: no payload hash exists"
    );

    let mut edited = base;
    edited["title"] = json!("different title");
    let (status, body) = b
        .post_create_on(b.app_with_running_daemon(), Some(key), edited)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], "conflict", "body={body}");
    let title: String = sqlx::query_scalar("SELECT title FROM tracks")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(title, "original title");
    assert_eq!(b.operation_count().await, 0);
    b.shutdown_harnesses().await;
}

/// Migration contract — an 0088 row has no reconstructible full request and is refused before replay side
/// effects instead of pretending NULL hashes mean a match.
#[tokio::test]
async fn a_legacy_binding_without_a_request_fingerprint_fails_closed() {
    let b = boot().await;
    let key = "idem-legacy-fingerprint";
    let (created, body) = b.create_track(Some(key), Some("same sentence")).await;
    assert_eq!(created, StatusCode::CREATED, "body={body}");
    let operations_before = b.operation_count().await;
    sqlx::query(
        "UPDATE track_create_idempotency \
         SET request_fingerprint_version = 0, \
             create_request_sha256 = NULL, \
             first_message_sha256 = NULL \
         WHERE idempotency_key = ?1",
    )
    .bind(key)
    .execute(b.repo.pool())
    .await
    .unwrap();

    let (status, body) = b.create_track(Some(key), Some("same sentence")).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], "conflict", "body={body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|message| message.contains("predates durable request fingerprints")),
        "body={body}"
    );
    assert_eq!(b.track_count().await, 1);
    assert_eq!(b.operation_count().await, operations_before);
    b.shutdown_harnesses().await;
}

/// Frozen pre-cross-area request fingerprint: seed the old persisted digest, then replay through the real
/// HTTP route on the upgraded server.
#[tokio::test]
async fn pre_cross_area_bindings_replay_after_upgrade() {
    for message in [None, Some("same sentence")] {
        let b = boot().await;
        let key = "idem-pre-cross-area";
        let (created, original) = b.create_track(Some(key), message).await;
        assert_eq!(created, StatusCode::CREATED, "body={original}");
        sqlx::query("UPDATE track_create_idempotency SET create_request_sha256 = ?1 WHERE idempotency_key = ?2")
            .bind("2e059b04225d633c402c23612df059bda03c1d064f4eac24d817061e9c9095ce")
            .bind(key)
            .execute(b.repo.pool()).await.unwrap();
        let messages_before = b.user_message_event_count().await;
        let (status, replay) = b.create_track(Some(key), message).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "message={message:?}, body={replay}"
        );
        assert_eq!(replay["id"], original["id"]);
        assert_eq!(b.track_count().await, 1);
        assert_eq!(b.user_message_event_count().await, messages_before);
        b.shutdown_harnesses().await;
    }
}

/// The control: the message-less path keeps its `warn!` + 201 during the same outage.
#[tokio::test]
async fn a_create_without_a_first_message_still_succeeds_during_a_daemon_outage() {
    let b = boot_without_daemon().await;
    let (status, body) = b.create_track(None, None).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    assert_eq!(b.track_count().await, 1);
    assert_eq!(b.binding_count().await, 0);
    b.shutdown_harnesses().await;
}

/// A binding **miss** with an **occupied** chosen key mints nothing. Unreachable by construction, so built by
/// hand; the assertion that matters is `track_count == 0`, not the status.
#[tokio::test]
async fn a_binding_miss_with_an_occupied_key_mints_nothing() {
    use sha2::{Digest, Sha256};
    let b = boot().await;
    let key = "idem-orphan-op";
    let operation_key = {
        let mut hasher = Sha256::new();
        hasher.update(format!("track-create:{}:{key}", b.area_id));
        format!("track-create-{}", hex::encode(hasher.finalize()))
    };
    // An operation under the derived key, in a non-`Failed` phase so `retryable_operation_key` stops on it, and
    // with no binding row anywhere.
    sqlx::query(
        "INSERT INTO operations \
         (id, kind, operation_key, idempotency_key, payload_hash, target_type, target_json, \
          payload_json, phase, attempt, created_at_ms, updated_at_ms) \
         VALUES ('op-orphan', 'planner-harness-start', ?1, ?1, 'hash', 'card', '{}', '{}', \
                 'succeeded', 0, 1, 1)",
    )
    .bind(&operation_key)
    .execute(b.repo.pool())
    .await
    .unwrap();
    assert_eq!(b.binding_count().await, 0, "premise: no binding row");

    let (status, body) = b
        .create_track(Some(key), Some("this must mint nothing"))
        .await;
    assert!(
        status.is_server_error(),
        "an unreachable state must fail closed rather than mint: status={status} body={body}"
    );
    assert_eq!(
        b.track_count().await,
        0,
        "and above all it must write NOTHING — a mint here commits a track and then collides on \
         the operation's unique key, leaving an orphan behind a 409"
    );
    assert_eq!(b.card_count().await, 0);
    assert_eq!(b.binding_count().await, 0);
}

/// `retryable_operation_key` stops on **any** non-`Failed` phase, so writing `Stuck` onto the operation a real
/// create produced puts the next request onto the `Replay` arm, which must answer 500 and deliver nothing.
#[tokio::test]
async fn a_replay_of_a_stuck_attempt_answers_500_and_delivers_nothing() {
    let b = boot().await;
    let key = "idem-stuck-replay";
    let (created, body) = b.create_track(Some(key), Some("the stuck sentence")).await;
    assert_eq!(created, StatusCode::CREATED, "body={body}");
    let track_id = body["id"].as_str().unwrap().to_string();
    assert_eq!(b.operation_count().await, 1, "premise: exactly one attempt");
    assert_eq!(b.binding_count().await, 1);
    assert_eq!(b.copies_in_harness("the stuck sentence", 1).await, 1);
    let deliveries_before = b.user_message_event_count().await;

    // The phase compensation could not finish on; `retryable_operation_key` deliberately does not step over it.
    let updated = sqlx::query(
        "UPDATE operations \
         SET phase = 'stuck', \
             last_error = 'compensation step failed', \
             phase_detail_json = ?1, \
             lease_owner = NULL, \
             lease_until_ms = NULL \
         WHERE kind = 'planner-harness-start'",
    )
    .bind(
        json!({
            "reason": "compensation step failed",
            "since": 1,
            "from_phase": "spawn_started",
        })
        .to_string(),
    )
    .execute(b.repo.pool())
    .await
    .unwrap()
    .rows_affected();
    assert_eq!(updated, 1, "premise: the create's own operation went stuck");

    let (status, body) = b.create_track(Some(key), Some("the stuck sentence")).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a stuck predecessor replays its recorded failure; body={body}"
    );
    let error = body["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("operation stuck in"),
        "the 500 must name the stuck operation rather than a generic failure; body={body}"
    );
    assert!(
        error.contains("creates no second track"),
        "and must carry the harness-start failure text, which is what tells the caller the \
         retry is safe; body={body}"
    );

    // What the 500 is worth: nothing new was written, and the sentence was not delivered a second time.
    assert_eq!(b.track_count().await, 1);
    assert_eq!(b.binding_count().await, 1);
    assert_eq!(
        b.operation_count().await,
        1,
        "the replay joins the stuck attempt; it does not open a `#N` one"
    );
    assert_eq!(b.user_message_event_count().await, deliveries_before);
    assert_eq!(b.copies_in_harness("the stuck sentence", 1).await, 1);
    let surviving: String = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(surviving, track_id);
    b.shutdown_harnesses().await;
}

/// The cross-instance primary-key race: two `AppState`s sharing only the database file, the loser *held* at
/// the mint rendezvous until the winner commits. The loser 500s naming the violation, leaves no orphan track, and its retry resolves to the winner's track.
#[tokio::test]
async fn a_loser_of_the_cross_instance_key_race_writes_nothing_and_retries_onto_the_winner() {
    use calm_server::routes::tracks::TrackCreateMintGate;

    let (winner, loser) = boot_two_instances_on_one_database().await;
    let key = "idem-cross-instance-race";
    let gate = Arc::new(TrackCreateMintGate::new());
    let loser_app = app_for_state(
        loser
            .state
            .clone()
            .with_track_create_mint_rendezvous(gate.clone()),
    );

    // The loser starts first and parks after its lookup 1 missed, before its create transaction opens.
    let loser_body = json!({
        "area_id": loser.area_id,
        "title": "",
        "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        "first_message": "one sentence, two instances",
    });
    let loser_request = tokio::spawn({
        let app = loser_app.clone();
        let key = key.to_string();
        async move {
            let builder = Request::builder()
                .method("POST")
                .uri("/api/tracks")
                .header("content-type", "application/json")
                .header("idempotency-key", key);
            let response = app
                .oneshot(builder.body(Body::from(loser_body.to_string())).unwrap())
                .await
                .unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            (
                status,
                serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
            )
        }
    });

    // Bounded: a loser that never arrives fails this assertion instead of hanging the runner.
    tokio::time::timeout(std::time::Duration::from_secs(30), gate.reached.wait())
        .await
        .expect("the loser must reach the mint window; without it this case is vacuous");
    assert_eq!(
        winner.binding_count().await,
        0,
        "premise: the loser passed lookup 1 with no binding row in the database, so it selected \
         the minting arm"
    );

    let (winner_status, winner_body) = winner
        .create_track(Some(key), Some("one sentence, two instances"))
        .await;
    assert_eq!(winner_status, StatusCode::CREATED, "body={winner_body}");
    let winner_track = winner_body["id"].as_str().unwrap().to_string();
    assert_eq!(winner.binding_count().await, 1);

    tokio::time::timeout(std::time::Duration::from_secs(30), gate.released.wait())
        .await
        .expect("release the loser onto the committed binding row");
    let (loser_status, loser_error) =
        tokio::time::timeout(std::time::Duration::from_secs(60), loser_request)
            .await
            .expect("the held request must finish")
            .expect("the loser task must not panic");

    // (1) the mapping.
    assert_eq!(
        loser_status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "the losing racer must fail closed, not resume and not mint; body={loser_error}"
    );
    assert!(
        loser_error["error"]
            .as_str()
            .is_some_and(|message| message.contains("claimed by a concurrent create")),
        "and must say which wall it hit; body={loser_error}"
    );

    // (2) no orphan: the loser's transaction had already minted a track row when the binding INSERT raised.
    assert_eq!(
        winner.track_count().await,
        1,
        "the loser's rolled-back mint must leave no orphan track behind its 500"
    );
    assert_eq!(winner.binding_count().await, 1);
    let surviving: String = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(winner.repo.pool())
        .await
        .unwrap();
    assert_eq!(surviving, winner_track, "and the survivor is the winner's");

    // (3) the retry, on the losing instance and with the rendezvous gone, resolves to the winner's track.
    let (retry_status, retry_body) = loser
        .post_create_on(
            loser.app.clone(),
            Some(key),
            json!({
                "area_id": loser.area_id,
                "title": "",
                "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
                "first_message": "one sentence, two instances",
            }),
        )
        .await;
    assert_eq!(retry_status, StatusCode::CREATED, "body={retry_body}");
    assert_eq!(
        retry_body["id"].as_str(),
        Some(winner_track.as_str()),
        "the loser's retry must resolve to the track that won, not mint a second one"
    );
    assert_eq!(winner.track_count().await, 1);
    assert_eq!(winner.binding_count().await, 1);
    winner.shutdown_harnesses().await;
    loser.shutdown_harnesses().await;
}

/// `Resume` re-materializes the workspace: the process can die between the COMMIT and `materialize_workspace`,
/// and a resume that only re-submitted the operation would 201 onto a directory with no `HEAD`.
#[tokio::test]
async fn a_resume_after_a_materialize_failure_materializes_the_workspace() {
    let b = boot().await;
    let (first, first_body) = b
        .create_track(Some("idem-remat"), Some("ship the thing"))
        .await;
    assert_eq!(first, StatusCode::CREATED, "body={first_body}");
    let track_id = first_body["id"].as_str().unwrap().to_string();
    let (kind, path) = b.workspace_row(&track_id).await;
    assert_eq!(kind, "managed", "premise: the create made it managed");
    let path = PathBuf::from(path);
    assert!(path.join(".git").exists(), "premise: it was materialized");
    b.shutdown_harnesses().await;

    std::fs::remove_dir_all(&path).unwrap();
    assert!(!path.exists(), "premise: the workspace really is gone");

    let (replay, replay_body) = b
        .create_track(Some("idem-remat"), Some("ship the thing"))
        .await;
    assert_eq!(replay, StatusCode::CREATED, "body={replay_body}");
    assert_eq!(first_body["id"], replay_body["id"], "the same track");
    assert_eq!(b.track_count().await, 1);
    assert!(
        path.join(".git").exists(),
        "the resume must have re-materialized the managed workspace — a 201 pointing at a \
         directory that does not exist is #1147 replayed one layer down"
    );
    let head = std::process::Command::new("git")
        .arg("-C")
        .arg(&path)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(
        head.status.success(),
        "…and its HEAD must resolve: {}",
        String::from_utf8_lossy(&head.stderr)
    );
    b.shutdown_harnesses().await;
}

/// Re-materializing a HEALTHY managed workspace is a no-op: the owner marker and the HEAD commit id are
/// compared across the replay.
#[tokio::test]
async fn a_resume_on_a_healthy_managed_workspace_is_a_no_op() {
    let b = boot().await;
    let (first, first_body) = b
        .create_track(Some("idem-noop"), Some("ship the thing"))
        .await;
    assert_eq!(first, StatusCode::CREATED, "body={first_body}");
    let track_id = first_body["id"].as_str().unwrap().to_string();
    let (_, path) = b.workspace_row(&track_id).await;
    let path = PathBuf::from(path);

    fn head_of(path: &std::path::Path) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(out.status.success(), "rev-parse HEAD must resolve");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
    fn marker_of(path: &std::path::Path) -> Vec<u8> {
        let dir = std::fs::read_dir(path.join(".git")).unwrap();
        for entry in dir.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.contains("owner") || name.contains("calm") || name.contains("neige") {
                return std::fs::read(entry.path()).unwrap();
            }
        }
        panic!("no owner marker under {path:?}/.git");
    }

    let head_before = head_of(&path);
    let marker_before = marker_of(&path);

    let (replay, replay_body) = b
        .create_track(Some("idem-noop"), Some("ship the thing"))
        .await;
    assert_eq!(replay, StatusCode::CREATED, "body={replay_body}");

    assert_eq!(
        head_of(&path),
        head_before,
        "a resume onto a healthy managed workspace must not move HEAD — re-running `git init` and \
         a fresh initial commit would rewrite the user's history under them"
    );
    assert_eq!(
        marker_of(&path),
        marker_before,
        "…and must leave the owner marker byte-identical"
    );
    b.shutdown_harnesses().await;
}

/// Process death between `create_dir_all(<path>/.git)` and the marker write leaves a directory with entries
/// and no marker, which `materialize_workspace` refuses forever; the key is poisoned and the answer is 409 `idempotency_key_exhausted`.
#[tokio::test]
async fn a_resume_onto_an_unmarked_non_empty_workspace_is_key_exhausted() {
    let b = boot().await;
    let (first, first_body) = b
        .create_track(Some("idem-brick"), Some("ship the thing"))
        .await;
    assert_eq!(first, StatusCode::CREATED, "body={first_body}");
    let track_id = first_body["id"].as_str().unwrap().to_string();
    let (_, path) = b.workspace_row(&track_id).await;
    let path = PathBuf::from(path);
    b.shutdown_harnesses().await;

    // The exact residue of the `create_dir_all(<path>/.git)` → `write` window.
    std::fs::remove_dir_all(&path).unwrap();
    std::fs::create_dir_all(path.join(".git")).unwrap();

    let (replay, replay_body) = b
        .create_track(Some("idem-brick"), Some("ship the thing"))
        .await;
    assert_eq!(
        replay,
        StatusCode::CONFLICT,
        "an un-materializable workspace must not be answered 201, and must not read as a generic \
         server fault either: body={replay_body}"
    );
    assert_eq!(
        replay_body["code"], "idempotency_key_exhausted",
        "the status alone does not tell an operator what to do; the code does: body={replay_body}"
    );
    assert_eq!(b.track_count().await, 1, "and nothing new is minted");
}

/// The poisoning is **per key**: a new `Idempotency-Key` mints a fresh track id and a managed path derived
/// from *that* id, so the poisoned directory is never revisited.
#[tokio::test]
async fn a_new_idempotency_key_recovers_from_a_poisoned_workspace() {
    let b = boot().await;
    let (first, first_body) = b
        .create_track(Some("idem-poisoned"), Some("ship the thing"))
        .await;
    assert_eq!(first, StatusCode::CREATED, "body={first_body}");
    let poisoned_track = first_body["id"].as_str().unwrap().to_string();
    let (_, poisoned_path) = b.workspace_row(&poisoned_track).await;
    let poisoned_path = PathBuf::from(poisoned_path);
    b.shutdown_harnesses().await;
    std::fs::remove_dir_all(&poisoned_path).unwrap();
    std::fs::create_dir_all(poisoned_path.join(".git")).unwrap();
    // Premise: the old key really is dead.
    let (poisoned, poisoned_body) = b
        .create_track(Some("idem-poisoned"), Some("ship the thing"))
        .await;
    assert_eq!(
        poisoned,
        StatusCode::CONFLICT,
        "premise: the old key is exhausted: body={poisoned_body}"
    );

    // A distinct sentence, so the delivery assertion below is about THIS track; the poisoned track's harness
    // was shut down, which drops anything still in its pending queue.
    let (fresh, fresh_body) = b
        .create_track(Some("idem-fresh"), Some("ship the OTHER thing"))
        .await;
    assert_eq!(
        fresh,
        StatusCode::CREATED,
        "a new Idempotency-Key must be a complete recovery — the poisoning is per key, and the \
         new track's managed path is derived from a freshly minted id: body={fresh_body}"
    );
    let fresh_track = fresh_body["id"].as_str().unwrap().to_string();
    assert_ne!(fresh_track, poisoned_track);
    let (_, fresh_path) = b.workspace_row(&fresh_track).await;
    let fresh_path = PathBuf::from(fresh_path);
    assert_ne!(
        fresh_path, poisoned_path,
        "and it must be a different directory, or the recovery would re-enter the same fence"
    );
    assert!(
        fresh_path.join(".git").exists(),
        "…which really was materialized"
    );
    assert_eq!(
        b.copies_in_harness("ship the OTHER thing", 1).await,
        1,
        "and the recovered track really received its message — a 201 alone would not prove the \
         recovery produced a WORKING track"
    );
    b.shutdown_harnesses().await;
}

/// The `Resume` arm when the track is gone: the binding row is deliberately not `ON DELETE CASCADE`, so a
/// retried create lands on a key naming a deleted track and must answer 409 `idempotency_key_exhausted` (the code the frontend rotates on), minting nothing.
#[tokio::test]
async fn a_replay_onto_a_deleted_track_is_key_exhausted() {
    let b = boot().await;
    let key = "idem-deleted-track";
    let message = "the sentence whose track went away";
    let (created, body) = b.create_track(Some(key), Some(message)).await;
    assert_eq!(created, StatusCode::CREATED, "body={body}");
    let track_id = body["id"].as_str().unwrap().to_string();
    assert_eq!(b.binding_count().await, 1, "premise: the key is bound");
    b.shutdown_harnesses().await;

    let deleted = b.delete_track(&track_id).await;
    assert!(
        deleted.is_success(),
        "premise: the production delete route really removed the track: status={deleted}"
    );
    // Premise 1 — the track is gone, so `track_get` in the arm returns `None`.
    assert_eq!(
        b.track_count().await,
        0,
        "premise: the delete committed; otherwise the replay resolves a live track"
    );
    // Premise 2 — and the binding row outlived it, which is what routes the replay into `Resume` at all.
    assert_eq!(
        b.binding_count().await,
        1,
        "premise: the binding row has no ON DELETE CASCADE, so the key still names the dead track"
    );

    let (status, body) = b.create_track(Some(key), Some(message)).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a byte-identical replay under a key whose track was deleted must fail closed, and must \
         not read as a generic server fault: body={body}"
    );
    assert_eq!(
        body["code"], "idempotency_key_exhausted",
        "the status alone does not tell the caller what to do; the code does, and it is the code \
         the frontend rotates the draft key on: body={body}"
    );
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains(&track_id),
        "…and the refusal names the dead track, so an operator reading a log knows which one: \
         body={body}"
    );

    // And it minted nothing on the way out.
    assert_eq!(
        b.track_count().await,
        0,
        "the refused replay must not mint a replacement track"
    );
    assert_eq!(b.binding_count().await, 1, "nor a second binding row");
}

/// The escape from a deleted track's poisoned key: a new `Idempotency-Key` misses the binding, takes `Mint`,
/// and gets a working track.
#[tokio::test]
async fn a_new_idempotency_key_recovers_from_a_deleted_track() {
    let b = boot().await;
    let message = "the sentence whose track went away";
    let (created, body) = b
        .create_track(Some("idem-deleted-original"), Some(message))
        .await;
    assert_eq!(created, StatusCode::CREATED, "body={body}");
    let dead_track = body["id"].as_str().unwrap().to_string();
    b.shutdown_harnesses().await;
    assert!(b.delete_track(&dead_track).await.is_success());

    // Premise: the old key really is dead, with the code the frontend rotates on.
    let (poisoned, poisoned_body) = b
        .create_track(Some("idem-deleted-original"), Some(message))
        .await;
    assert_eq!(
        poisoned,
        StatusCode::CONFLICT,
        "premise: the old key is exhausted: body={poisoned_body}"
    );
    assert_eq!(poisoned_body["code"], "idempotency_key_exhausted");

    // A distinct sentence, so the delivery assertion below is about THIS track.
    let (fresh, fresh_body) = b
        .create_track(
            Some("idem-deleted-fresh"),
            Some("a wholly different sentence"),
        )
        .await;
    assert_eq!(
        fresh,
        StatusCode::CREATED,
        "a new Idempotency-Key must be a complete recovery — it misses the dead binding, mints a \
         fresh id, and owes the deleted track nothing: body={fresh_body}"
    );
    let fresh_track = fresh_body["id"].as_str().unwrap().to_string();
    assert_ne!(
        fresh_track, dead_track,
        "and it is a NEW track, not the dead id handed back"
    );
    assert_eq!(
        b.track_count().await,
        1,
        "exactly one live track: the dead one stayed deleted and the refusal minted nothing"
    );
    assert_eq!(
        b.copies_in_harness("a wholly different sentence", 1).await,
        1,
        "…and the recovered create delivered its message exactly once"
    );
    b.shutdown_harnesses().await;
}

/// The same key with a different **create** is a conflict: `create_request_sha256` binds the complete mint
/// shape in the binding row and is carried into `payload_hash`.
#[tokio::test]
async fn the_same_key_with_a_different_title_is_a_conflict() {
    let b = boot().await;
    let base = json!({
        "area_id": b.area_id,
        "title": "the original title",
        "first_message": "ship the thing",
        "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
    });
    let (first, first_body) = b.post_create(Some("idem-title"), base.clone()).await;
    assert_eq!(first, StatusCode::CREATED, "body={first_body}");

    // Byte-identical except the title.
    let mut edited = base.clone();
    edited["title"] = json!("a completely different track");
    let (second, second_body) = b.post_create(Some("idem-title"), edited).await;
    assert_eq!(
        second,
        StatusCode::CONFLICT,
        "the same key with a different create must not silently return the original track: \
         body={second_body}"
    );
    assert_eq!(b.track_count().await, 1, "and must mint nothing");

    // The control: the SAME title still replays, so the assertion above is not satisfied by a key that 409s on every repeat.
    let (replay, replay_body) = b.post_create(Some("idem-title"), base).await;
    assert_eq!(
        replay,
        StatusCode::CREATED,
        "…while a byte-identical replay must still replay: body={replay_body}"
    );
    assert_eq!(first_body["id"], replay_body["id"]);

    // And a source field on its own key.
    let recipe_id = b.create_recipe("rollout flow", &recipe_body()).await;
    let with_recipe = json!({
        "area_id": b.area_id,
        "title": "same title",
        "recipe_id": recipe_id,
        "first_message": "ship the thing",
        "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
    });
    let (created, body) = b
        .post_create(Some("idem-source"), with_recipe.clone())
        .await;
    assert_eq!(created, StatusCode::CREATED, "body={body}");
    let mut without_recipe = with_recipe.clone();
    without_recipe.as_object_mut().unwrap().remove("recipe_id");
    let (conflict, body) = b.post_create(Some("idem-source"), without_recipe).await;
    assert_eq!(
        conflict,
        StatusCode::CONFLICT,
        "dropping `recipe_id` is a different create, not a replay: body={body}"
    );
    b.shutdown_harnesses().await;
}

/// The binding-level create fingerprint covers every request field that decides the minted track; the
/// comparison must run before replay skips create-path validation and before any side effect.
#[tokio::test]
async fn every_mint_input_is_bound_to_the_track_create_key() {
    let b = boot().await;
    let key = "idem-complete-create-shape";
    let base = json!({
        "area_id": b.area_id,
        "title": "original title",
        "first_message": "same sentence",
        "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
    });
    let (created, body) = b.post_create(Some(key), base.clone()).await;
    assert_eq!(created, StatusCode::CREATED, "body={body}");

    let mut cases = Vec::new();
    let mut edited = base.clone();
    edited["sort"] = json!(42);
    cases.push(("sort", edited));
    let mut edited = base.clone();
    edited["cwd"] = json!("/path/that/need/not/exist/on-a-replay");
    cases.push(("cwd", edited));
    let mut edited = base.clone();
    edited["attach_folder"] = json!(true);
    cases.push(("attach_folder", edited));
    let mut edited = base.clone();
    edited["template_id"] = json!("small-change");
    cases.push(("template_id", edited));
    let mut edited = base.clone();
    edited["recipe_id"] = json!("recipe-that-need-not-exist-on-a-replay");
    cases.push(("recipe_id", edited));
    let mut edited = base.clone();
    edited["theme"] = json!({"fg": [1, 2, 3], "bg": [4, 5, 6]});
    cases.push(("theme", edited));
    let mut edited = base.clone();
    edited["template_input"] = json!({"issue": 1434});
    cases.push(("template_input", edited));
    let mut edited = base;
    edited["fork_report_from"] = json!("another-track");
    cases.push(("fork_report_from", edited));

    for (field, edited) in cases {
        let (status, body) = b.post_create(Some(key), edited).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "changing {field} must not silently return the original track: body={body}"
        );
        assert_eq!(body["code"], "conflict", "field={field} body={body}");
    }
    assert_eq!(b.track_count().await, 1);
    b.shutdown_harnesses().await;
}

/// `skip_serializing_if` keeps every existing caller's `payload_hash` stable: a `null` digest would move the
/// bytes, and an operation submitted by an older binary and retried after a deploy would come back 409.
#[tokio::test]
async fn a_message_less_create_writes_byte_identical_payload_json() {
    let b = boot().await;
    let (status, body) = b.create_track(None, None).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let payloads = b.operation_payloads().await;
    assert_eq!(payloads.len(), 1, "one start: {payloads:?}");
    for payload in &payloads {
        assert!(
            payload.get("create_request_sha256").is_none(),
            "the message-less payload must not carry the key at all — not even as null: {payload}"
        );
    }

    // And the positive half: a keyed create DOES carry it.
    let (status, body) = b
        .create_track(Some("idem-digest"), Some("ship the thing"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let payloads = b.operation_payloads().await;
    let keyed: Vec<&Value> = payloads
        .iter()
        .filter(|p| p.get("first_message").is_some())
        .collect();
    assert_eq!(keyed.len(), 1, "one keyed start: {payloads:?}");
    assert!(
        keyed[0]["create_request_sha256"].is_string(),
        "a keyed create must carry the digest so operation replay keeps the same payload identity: {:?}",
        keyed[0]
    );
    b.shutdown_harnesses().await;
}

// A sentence that has not drained yet must survive the runtime that was holding it: the re-point fence
// supersedes every live runtime, and the successor must harvest the queue. `PlannerHarnessDrainRaceHook` parks the drain so the losing order is the only order.

/// Distinct from every other needle in this file so `copies_in_harness` cannot count someone else's message.
const STRANDED: &str = "reconcile the ledger before Friday";

/// THE repro. Deterministic, no load required.
#[tokio::test]
async fn a_first_message_not_yet_drained_when_the_workspace_is_repointed_still_reaches_the_agent() {
    let b = boot().await;
    let (entered, release) = b.hold_the_next_drain();

    let (status, body) = b.create_track(Some("idem-1449"), Some(STRANDED)).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track_id = body["id"].as_str().unwrap().to_string();

    // The drain is now parked with the sentence still on the queue.
    entered.notified().await;
    let (stranded_runtime, _card_id) = b.only_runtime().await;

    let target = user_repo(&b.tmp.path().join("my-project"));
    let (patched, patch_body) = b.repoint_to(&track_id, &target).await;
    assert_eq!(
        patched,
        StatusCode::OK,
        "premise: the re-point must succeed, or this test proves nothing: body={patch_body}"
    );
    release.notify_one();

    assert_eq!(
        b.copies_in_harness(STRANDED, 1).await,
        1,
        "the sentence the user typed must reach the successor the fence started — before this \
         slice it stayed on the superseded runtime's undrained queue and no path ever read it \
         again"
    );
    // "exactly once": ask for a second copy and let the deadline burn.
    assert_eq!(
        b.copies_in_harness(STRANDED, 2).await,
        1,
        "and it must arrive exactly once — the parked predecessor must not also deliver it"
    );
    assert!(
        b.harvest_stamp(&stranded_runtime).await.is_some(),
        "the mechanism, not just the outcome: the row the queue was taken from must be stamped, \
         which is what stops the next restart from taking it again"
    );
    // The transfer is a MOVE: the row it came off does not keep a copy. Asserted here because the unit test
    // drives the harvest helper with a fixture decoder, not the one production runs.
    assert_eq!(
        b.persisted_queue(&stranded_runtime).await.len(),
        0,
        "the predecessor's persisted queue must be empty after the harvest took it"
    );
    b.shutdown_harnesses().await;
}

/// The first restart INHERITS the parked runtime's whole queue and stamps it; the second restart finds a
/// `superseded` row that still carries the sentence in its snapshot and must take nothing from it.
#[tokio::test]
async fn a_harvested_sentence_is_not_delivered_again_by_a_second_restart() {
    let b = boot().await;
    let (entered, release) = b.hold_the_next_drain();

    let (status, body) = b
        .create_track(Some("idem-1449-twice"), Some(STRANDED))
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    entered.notified().await;
    let (parked_runtime, card_id) = b.only_runtime().await;

    let (reset, reset_body) = b.reset_planner(&card_id).await;
    assert_eq!(
        reset,
        StatusCode::OK,
        "premise: the first restart must succeed: body={reset_body}"
    );
    release.notify_one();
    assert_eq!(
        b.copies_in_harness(STRANDED, 1).await,
        1,
        "premise: the first restart carries the sentence forward"
    );
    assert!(
        b.harvest_stamp(&parked_runtime).await.is_some(),
        "premise: the inherit must stamp the row it emptied"
    );
    // Wait for the successor's post-turn snapshot write. The transfer is a MOVE, so at any moment at most one
    // row owes the sentence; waiting for zero is waiting for that delivery to be written down.
    assert_eq!(
        b.wait_until_rows_holding(STRANDED, 0).await,
        0,
        "premise: no row still owes the sentence once the successor has delivered it"
    );

    let (reset_again, reset_again_body) = b.reset_planner(&card_id).await;
    assert_eq!(
        reset_again,
        StatusCode::OK,
        "premise: the second restart must succeed: body={reset_again_body}"
    );
    assert_eq!(
        b.copies_in_harness(STRANDED, 2).await,
        1,
        "a second restart must NOT re-deliver a sentence an earlier restart already carried — \
         the stamp on the retired row is what makes this a construction rather than a race"
    );
    b.shutdown_harnesses().await;
}

/// Only the human's own words travel: the opening briefing is an `Observation::SystemContext` describing the
/// runtime's `cwd`, and a re-point is what makes that directory wrong. The successor writes no new one (`opening_briefing: None`).
#[tokio::test]
async fn a_repoint_does_not_carry_the_old_workspace_briefing_forward() {
    const OLD_BRIEFING: &str = "briefing about the workspace this track is leaving";

    let b = boot().await;
    let (entered, release) = b.hold_the_next_drain();

    let (status, body) = b
        .create_track(Some("idem-1449-briefing"), Some(STRANDED))
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track_id = body["id"].as_str().unwrap().to_string();
    entered.notified().await;
    let (stranded_runtime, _card_id) = b.only_runtime().await;

    // Put a briefing on the parked runtime's queue and persist it, so the retired row carries BOTH kinds of observation.
    let handle = b
        .state
        .harness
        .get(&stranded_runtime)
        .expect("the parked runtime must still be in the registry");
    handle
        .observe_for_test(
            Observation::SystemContext {
                text: OLD_BRIEFING.into(),
            },
            None,
        )
        .await;
    handle.persist_snapshot().await.unwrap();
    drop(handle);

    let target = user_repo(&b.tmp.path().join("my-project"));
    let (patched, patch_body) = b.repoint_to(&track_id, &target).await;
    assert_eq!(
        patched,
        StatusCode::OK,
        "premise: the re-point must succeed: body={patch_body}"
    );
    release.notify_one();

    assert_eq!(
        b.copies_in_harness(STRANDED, 1).await,
        1,
        "premise: the human's sentence still travels"
    );
    assert_eq!(
        b.copies_in_harness(OLD_BRIEFING, 1).await,
        0,
        "but the old workspace's briefing must NOT — the successor lives in a different directory \
         and writes its own"
    );
    b.shutdown_harnesses().await;
}

// A runtime's row can be retired while its run loop is alive, healthy and unaware; everything below orders
// events inside that gap, with `retire_runtime_in_the_database` as the durable half of the fence.

/// `maybe_issue_turn` persists "still queued", drains in memory, calls `turn/start`, and only THEN persists the
/// emptied queue; on a fenced runtime that last write is lost, so the harvest would re-deliver the batch.
#[tokio::test]
async fn a_batch_the_daemon_already_has_is_not_harvested_after_the_row_is_retired() {
    let b = boot().await;
    let (entered, release) = b.hold_the_next_turn_start();

    let (status, body) = b
        .create_track(Some("idem-1449-issued"), Some(STRANDED))
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    // Parked inside `turn/start`: the fake daemon has recorded the batch, the emptied queue is not yet written back.
    entered.notified().await;
    let (runtime, card_id) = b.only_runtime().await;
    assert_eq!(
        b.persisted_queue(&runtime).await.len(),
        1,
        "premise: the row still says the batch is queued — that is the pre-drain write"
    );

    // The fence's durable half, and only that half.
    b.retire_runtime_in_the_database(&runtime).await;
    release.notify_one();

    assert_eq!(
        b.wait_for_persisted_queue_len(&runtime, 0).await,
        0,
        "a retired runtime must still be able to write down that it delivered the batch — \
         otherwise the last word on the row is a debt it has already paid"
    );

    let (reset, reset_body) = b.reset_planner(&card_id).await;
    assert_eq!(
        reset,
        StatusCode::OK,
        "premise: the restart must succeed: body={reset_body}"
    );
    assert_eq!(
        b.delivered_copies(STRANDED, 2).await,
        1,
        "and the successor must not re-deliver a sentence the daemon already has"
    );
    b.shutdown_harnesses().await;
}

/// The mint transaction takes the queue and commits, and the predecessor's run loop knows nothing about it
/// (`shutting_down` is process memory), so it would otherwise drain the same batch too.
#[tokio::test]
async fn a_runtime_that_is_no_longer_the_cards_carrier_does_not_issue_its_queue() {
    let b = boot().await;
    let (entered, release) = b.hold_the_next_drain();

    let (status, body) = b
        .create_track(Some("idem-1449-carrier"), Some(STRANDED))
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    entered.notified().await;
    let (runtime, _card_id) = b.only_runtime().await;

    // Somebody else now owns this queue.
    b.retire_runtime_in_the_database(&runtime).await;
    release.notify_one();

    assert_eq!(
        b.delivered_copies(STRANDED, 1).await,
        0,
        "a retired runtime must leave its queue for whoever the mint handed it to; issuing it \
         anyway is how the same sentence reaches the agent twice"
    );
    // The handle stays REGISTERED and alive (`ensure_live_planner_harness` does not health-check a registered
    // handle), but a durable send through it is REFUSED: `session_set_handle_state_tx` matches no retired row.
    let handle = b
        .state
        .harness
        .get(&runtime)
        .expect("the retired handle must stay registered");
    let refused = handle
        .observe_user_message_durable("cannot be made durable here".into(), Vec::new())
        .await;
    assert!(
        refused.is_err(),
        "a send that cannot reach the row must be refused, not acknowledged: {refused:?}"
    );
    assert_eq!(
        b.persisted_queue(&runtime).await.len(),
        1,
        "and the refusal must leave the row exactly as it was"
    );
    b.shutdown_harnesses().await;
}

/// A runtime whose row is GONE does not issue either: the carrier check fails closed on a missing row.
#[tokio::test]
async fn a_runtime_whose_row_has_been_deleted_does_not_issue_its_queue() {
    let b = boot().await;
    let (entered, release) = b.hold_the_next_drain();

    let (status, body) = b
        .create_track(Some("idem-1449-deleted-row"), Some(STRANDED))
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    entered.notified().await;
    let (runtime, _card_id) = b.only_runtime().await;

    // Through the production deleter, not a raw `DELETE`: `session_delete_tx` clears `tracks.root_session_id`
    // first, and a fixture that skipped that would trip the foreign key.
    {
        let worker_session_id = runtime.clone();
        write_in_tx_typed(b.repo.as_ref() as &dyn Repo, move |tx| {
            Box::pin(async move {
                session_delete_tx(tx, &worker_session_id)
                    .await
                    .map_err(calm_server::error::CalmError::from)
            })
        })
        .await
        .expect("delete the runtime row");
    }
    release.notify_one();

    assert_eq!(
        b.delivered_copies(STRANDED, 1).await,
        0,
        "a runtime with no row cannot show that it is still the card's carrier, and a missing \
         row is reachable only in contexts where issuing a turn is wrong"
    );
    b.shutdown_harnesses().await;
}

/// A restart that fails after the harvest committed must give the queue back: compensation marks the
/// successor `failed`, a state the harvest predicate never reads, and compensation is a different transaction from the mint.
#[tokio::test]
async fn a_failed_restart_gives_the_harvested_sentence_back() {
    let b = boot().await;
    let (entered, release) = b.hold_the_next_drain();

    let (status, body) = b
        .create_track(Some("idem-1449-compensate"), Some(STRANDED))
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    entered.notified().await;
    let (stranded_runtime, card_id) = b.only_runtime().await;
    b.retire_runtime_in_the_database(&stranded_runtime).await;
    release.notify_one();

    // The restart harvests, then its `thread/start` fails and compensation runs.
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (failed, failed_body) = b.reset_planner(&card_id).await;
    assert!(
        !failed.is_success(),
        "premise: the injected thread/start failure must surface: status={failed} \
         body={failed_body}"
    );
    assert!(
        b.harvest_stamp(&stranded_runtime).await.is_none(),
        "the compensation must give the queue back: a mint that did not survive has not \
         delivered anything, so the row it took from must be harvestable again"
    );

    // And the proof that "harvestable again" means what it says.
    let (retry, retry_body) = b.reset_planner(&card_id).await;
    assert_eq!(
        retry,
        StatusCode::OK,
        "premise: the second restart must succeed: body={retry_body}"
    );
    assert_eq!(
        b.delivered_copies(STRANDED, 2).await,
        1,
        "the sentence must still reach an agent after a failed restart in between — exactly \
         once, so a compensation that gave the queue back cannot also have delivered it"
    );
    b.shutdown_harnesses().await;
}

/// The NON-deferred arm of `prepare_tx`: `ensure`'s second call starts with `force_new_thread: false`, which
/// supersedes the card's live predecessor without inheriting anything from it.
#[tokio::test]
async fn the_non_deferred_arm_carries_an_undrained_sentence_to_its_successor() {
    const LAUNCHPAD_SENTENCE: &str = "check the overnight builds";

    let b = boot().await;
    let (first, first_body) = b.ensure_launchpad().await;
    assert_eq!(
        first,
        StatusCode::CREATED,
        "premise: the launchpad must be minted: body={first_body}"
    );
    let planner_card_id = first_body["planner_card_id"].as_str().unwrap().to_string();
    let runtime = b.active_runtime_of_card(&planner_card_id).await;

    // Park the drain, THEN send: the sentence has to be durably queued and still undrained when the second ensure runs.
    let (entered, release) = b.hold_the_next_drain();
    let (sent, sent_body) = b
        .send_planner_input(&planner_card_id, LAUNCHPAD_SENTENCE)
        .await;
    assert_eq!(
        sent,
        StatusCode::OK,
        "premise: the send must be accepted: body={sent_body}"
    );
    entered.notified().await;
    assert_eq!(
        b.persisted_queue(&runtime).await.len(),
        1,
        "premise: the sentence is durably queued on the predecessor and has not drained"
    );

    // The second ensure takes the other arm.
    let (second, second_body) = b.ensure_launchpad().await;
    assert_eq!(
        second,
        StatusCode::OK,
        "premise: the second ensure must resolve the existing launchpad — a 201 here would mean \
         it minted a new one and never reached the arm under test: body={second_body}"
    );
    release.notify_one();

    assert_eq!(
        b.delivered_copies(LAUNCHPAD_SENTENCE, 2).await,
        1,
        "the non-deferred arm must hand the predecessor's undelivered sentence to the successor \
         — exactly once"
    );
    assert!(
        b.harvest_stamp(&runtime).await.is_some(),
        "and the row it was taken from must be stamped, or the next start takes it again"
    );
    b.shutdown_harnesses().await;
}

/// A deferred mint that fails must give the INHERITED queue back: the inherit is a move the harvest's undo
/// journal cannot cover (the predecessor is still `active`), so it files its own journal entry.
#[tokio::test]
async fn a_failed_deferred_mint_gives_the_inherited_sentence_back() {
    let b = boot().await;
    let (entered, release) = b.hold_the_next_drain();

    let (status, body) = b
        .create_track(Some("idem-1449-inherit"), Some(STRANDED))
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    entered.notified().await;
    let (predecessor, card_id) = b.only_runtime().await;
    assert_eq!(
        b.persisted_queue(&predecessor).await.len(),
        1,
        "premise: the sentence is durably queued and has not drained"
    );

    // The predecessor is LEFT ACTIVE, so the restart takes the inherit arm.
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (failed, failed_body) = b.reset_planner(&card_id).await;
    assert!(
        !failed.is_success(),
        "premise: the injected thread/start failure must surface: status={failed} \
         body={failed_body}"
    );
    release.notify_one();

    let holders = b.wait_until_rows_holding(STRANDED, 1).await;
    assert_eq!(
        holders, 1,
        "after a failed deferred mint exactly one row must still owe the sentence — the \
         inherited queue has to come back, or it is stranded on a `failed` runtime the \
         harvest never reads"
    );

    // And it is genuinely reachable again, not merely present somewhere.
    let (retry, retry_body) = b.reset_planner(&card_id).await;
    assert_eq!(
        retry,
        StatusCode::OK,
        "premise: the second restart must succeed: body={retry_body}"
    );
    assert_eq!(
        b.delivered_copies(STRANDED, 2).await,
        1,
        "and the sentence must reach an agent exactly once"
    );
    b.shutdown_harnesses().await;
}

/// A durable send is either ON THE ROW or REFUSED, never accepted and only in memory. Driven through the
/// HANDLE, which is what the route holds once it has resolved a runtime; the loop counter is asserted.
#[tokio::test]
async fn a_durable_send_is_on_the_row_or_refused_never_accepted_into_memory() {
    let b = boot().await;
    let (status, body) = b.create_track(Some("idem-1449-durable"), None).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let (runtime, _card_id) = b.only_runtime().await;
    let handle = b
        .state
        .harness
        .get(&runtime)
        .expect("the live handle the route would have resolved");

    // A send while the runtime is still the carrier: accepted, and on the row.
    handle
        .observe_user_message_durable("before the retirement".into(), Vec::new())
        .await
        .expect("premise: a live carrier accepts a durable send");
    assert_eq!(
        b.persisted_queue(&runtime).await.len(),
        1,
        "premise: and it lands on the row"
    );

    // Now the mint commits underneath the handle the caller is holding.
    b.retire_runtime_in_the_database(&runtime).await;

    let mut accepted = 0usize;
    let mut refused = 0usize;
    for attempt in 0..20 {
        let text = format!("after the retirement #{attempt}");
        match handle
            .observe_user_message_durable(text.clone(), Vec::new())
            .await
        {
            Ok(_ack) => {
                accepted += 1;
                let persisted = b.persisted_queue(&runtime).await;
                assert!(
                    persisted.iter().any(|e| e.to_string().contains(&text)),
                    "send #{attempt} was ACCEPTED but is not on the row — it exists only in the \
                     memory of a runtime that must not speak for this card, on a row the harvest \
                     will not read because it is stamped. Persisted queue: {persisted:?}"
                );
            }
            Err(_) => refused += 1,
        }
    }
    // No `accepted + refused == 20` here: every iteration increments one of them, so that would be a tautology.
    assert!(
        refused > 0,
        "premise: this test is worthless unless the sends actually reached a retired runtime; \
         {accepted} were accepted and none refused, so the retirement did not take"
    );
    b.shutdown_harnesses().await;
}

/// `pending_message_ids` is `#[serde(default)]`, so a sentence enqueued by an older binary decodes with no
/// id; the give-back must still return it when the mint that moved it fails.
#[tokio::test]
async fn a_pre_upgrade_sentence_survives_a_failed_mint_that_moved_it() {
    const LEGACY: &str = "typed before the upgrade";

    let b = boot().await;
    let (entered, release) = b.hold_the_next_drain();
    let (status, body) = b.create_track(Some("idem-1449-legacy"), None).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let (predecessor, card_id) = b.only_runtime().await;

    let (sent, sent_body) = b.send_planner_input(&card_id, LEGACY).await;
    assert_eq!(
        sent,
        StatusCode::OK,
        "premise: the send lands: body={sent_body}"
    );
    entered.notified().await;

    // Rewrite the row the way an older binary left it: the queue is there, the id array is not.
    let state: String =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id = ?1")
            .bind(&predecessor)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    let mut state: Value = serde_json::from_str(&state).unwrap();
    state.as_object_mut().unwrap().remove("pending_message_ids");
    sqlx::query("UPDATE worker_sessions SET handle_state_json = ?1 WHERE id = ?2")
        .bind(serde_json::to_string(&state).unwrap())
        .bind(&predecessor)
        .execute(b.repo.pool())
        .await
        .unwrap();

    // The predecessor stays ACTIVE, so the restart takes the inherit arm, and the restart fails after its
    // transaction committed.
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (failed, failed_body) = b.reset_planner(&card_id).await;
    assert!(
        !failed.is_success(),
        "premise: the injected failure must surface: status={failed} body={failed_body}"
    );
    release.notify_one();

    assert_eq!(
        b.wait_until_rows_holding(LEGACY, 1).await,
        1,
        "a sentence with no id was moved off its row by the mint; when that mint fails it has to \
         come back, or it is left on a `failed` runtime that nothing reads and nothing revives"
    );

    let (retry, retry_body) = b.reset_planner(&card_id).await;
    assert_eq!(
        retry,
        StatusCode::OK,
        "premise: the second restart must succeed: body={retry_body}"
    );
    assert_eq!(
        b.delivered_copies(LEGACY, 2).await,
        1,
        "and it reaches an agent exactly once"
    );
    b.shutdown_harnesses().await;
}

/// A move carries the sentence to the successor but leaves the evidence row naming the replaced runtime, so
/// `user_message_enqueued_on_active_runtime` answers `false` and the summary trigger sends its bootstrap again: an accepted, priced duplicate.
#[tokio::test]
async fn a_replaced_runtime_keeps_the_evidence_enqueued_against_it() {
    let b = boot().await;
    let (entered, release) = b.hold_the_next_drain();

    let (first, first_body) = b.ensure_launchpad().await;
    assert_eq!(
        first,
        StatusCode::CREATED,
        "premise: the launchpad must be minted: body={first_body}"
    );
    let planner_card_id = first_body["planner_card_id"].as_str().unwrap().to_string();
    let runtime = b.active_runtime_of_card(&planner_card_id).await;

    // A standing instruction that has not drained yet.
    let (sent, sent_body) = b
        .send_planner_input(&planner_card_id, TODAY_SUMMARY_BOOTSTRAP_TEXT)
        .await;
    assert_eq!(
        sent,
        StatusCode::OK,
        "premise: the bootstrap must be queued: body={sent_body}"
    );
    entered.notified().await;
    assert_eq!(
        b.persisted_queue(&runtime).await.len(),
        1,
        "premise: it is on the row and has not drained"
    );

    // The replacement moves it forward; the evidence row keeps naming the runtime that was replaced.
    b.retire_runtime_in_the_database(&runtime).await;
    // The successor's own drain, parked before it exists: the hook is installed BEFORE the mint that creates
    // the successor, so "not drained yet" is a held state rather than a window.
    let (successor_entered, successor_release) = b.hold_the_next_drain();
    let (second, second_body) = b.ensure_launchpad().await;
    assert_eq!(
        second,
        StatusCode::OK,
        "premise: the second ensure must resolve the existing launchpad: body={second_body}"
    );

    let successor = b.active_runtime_of_card(&planner_card_id).await;
    assert_ne!(successor, runtime, "premise: a replacement really happened");
    successor_entered.notified().await;
    let carried = b
        .persisted_queue(&successor)
        .await
        .iter()
        .filter(|entry| entry.to_string().contains("Stand by and do nothing yet"))
        .count();
    assert_eq!(
        carried, 1,
        "premise: the harvest carried the undrained standing instruction to the successor"
    );
    release.notify_one();
    successor_release.notify_one();

    // The mechanism behind the accepted duplicate: the message is on the successor, and the only evidence row
    // names the runtime that was replaced. The trigger itself is not driven here.
    let evidence_runtimes: Vec<String> = sqlx::query_scalar(
        "SELECT json_extract(payload, '$.worker_session_id') FROM events \
         WHERE kind = 'harness.user_message.enqueued'",
    )
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    assert!(
        !evidence_runtimes.is_empty(),
        "premise: the send wrote its evidence row"
    );
    assert!(
        evidence_runtimes.iter().all(|id| *id == runtime),
        "every evidence row still names the REPLACED runtime — that is why the predicate answers \
         `false` for the successor and the bootstrap is sent again. Writing one for the successor \
         is not the fix: the event records an act, and a harvest is movement nobody performed. \
         Rows: {evidence_runtimes:?}, replaced: {runtime}, successor: {successor}"
    );
    b.shutdown_harnesses().await;
}

/// The track's FIRST MESSAGE is addressable in `GET /planner/run`'s `pending`: a `QueueEntry::LegacyUser`
/// literal compiles in the adapter and would be withheld from `pending`. The drain hook parks the runtime so the queue can be read.
#[tokio::test]
async fn the_tracks_first_message_is_addressable_in_the_pending_page() {
    const SENTENCE: &str = "the first thing anybody said on this track";
    let b = boot().await;
    let (entered, release) = b.hold_the_next_drain();

    let (status, body) = b
        .create_track(Some("idem-first-addressable"), Some(SENTENCE))
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    // `POST /api/tracks` does not name the planner card in its body, so it is read from the one runtime row the create minted.
    let (_runtime, planner_card_id) = b.only_runtime().await;

    // PREMISE, not a wait: the runtime is parked at the drain hook, so what follows reads a queue that provably
    // still holds the sentence.
    tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
        .await
        .expect("the harness must reach the drain hook before it can drain");

    let (status, run) = b
        .get_json(&format!("/api/cards/{planner_card_id}/planner/run"))
        .await;
    assert_eq!(status, StatusCode::OK, "body={run}");

    let pending = run["pending"]
        .as_array()
        .expect("planner/run carries a pending page");
    assert_eq!(
        pending.len(),
        1,
        "the first message must BE on the addressable page, not merely in the queue: run={run}"
    );
    assert_eq!(pending[0]["text"], json!(SENTENCE));
    assert!(
        pending[0]["entry_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "…carrying a real id, which is the whole of what makes it editable and deletable: run={run}"
    );
    assert_eq!(
        run["pending_overflow"],
        json!(0),
        "…and not withheld from the page and counted as overflow instead, which is exactly \
         where a `LegacyUser` first message would land: run={run}"
    );

    release.notify_one();
    b.shutdown_harnesses().await;
}

#[tokio::test]
async fn create_model_selection_runs_first_message_and_binds_replay() {
    let b = boot().await;
    let body = json!({"area_id": b.area_id, "theme": {"fg": [255,255,255], "bg": [0,0,0]}, "first_message": "selected first turn", "model": "custom-create-model", "reasoning_effort": "high"});
    let (status, created) = b.post_create(Some("create-model"), body.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert!(
        b.started_turn_text("selected first turn")
            .await
            .contains("selected first turn")
    );
    let selections = b
        .state
        .shared_codex_appserver
        .started_turn_selections_for_test();
    assert_eq!(
        selections[0].1.model.as_deref(),
        Some("custom-create-model")
    );
    assert_eq!(selections[0].1.effort.as_deref(), Some("high"));
    let (status, replay) = b.post_create(Some("create-model"), body.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(created["id"], replay["id"]);
    for (field, value) in [("model", "another-model"), ("reasoning_effort", "low")] {
        let mut changed = body.clone();
        changed[field] = json!(value);
        let (status, response) = b.post_create(Some("create-model"), changed).await;
        assert_eq!(status, StatusCode::CONFLICT, "{field}: {response}");
    }
    assert_eq!(b.track_count().await, 1);
    assert_eq!(b.user_message_event_count().await, 1);
    b.shutdown_harnesses().await;
}

#[tokio::test]
async fn create_model_defaults_preserve_legacy_fingerprint_and_null_replay() {
    let b = boot_without_daemon().await;
    let body = json!({"area_id": b.area_id, "theme": {"fg": [255,255,255], "bg": [0,0,0]}});
    let (status, created) = b.post_create(Some("default-model"), body.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    // This is the exact pre-model request shape, independent of the new fields.
    let old_shape = json!({"title": "", "sort": null, "cwd": null,
        "template_id": null, "recipe_id": null, "template_input": null,
        "attach_folder": false, "theme": body["theme"], "fork_report_from": null});
    let expected = calm_server::routes::terminal_cards::stable_payload_hash(&old_shape).unwrap();
    let actual: String =
        sqlx::query_scalar("SELECT create_request_sha256 FROM track_create_idempotency")
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(actual, expected);
    let mut explicit_defaults = body;
    explicit_defaults["model"] = Value::Null;
    explicit_defaults["reasoning_effort"] = Value::Null;
    let (status, replay) = b
        .post_create(Some("default-model"), explicit_defaults)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(created["id"], replay["id"]);
    assert_eq!(b.track_count().await, 1);
}

#[tokio::test]
async fn create_model_selection_refuses_agent_before_mint() {
    let b = boot_without_daemon().await;
    for field in ["model", "reasoning_effort"] {
        let mut body = json!({"area_id": b.area_id, "theme": {"fg": [255,255,255], "bg": [0,0,0]}});
        body[field] = json!("high");
        let response = b
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tracks")
                    .header("content-type", "application/json")
                    .header("x-calm-actor", "ai:codex")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{field}");
    }
    assert_eq!(b.track_count().await, 0);
    assert_eq!(b.card_count().await, 0);
}
