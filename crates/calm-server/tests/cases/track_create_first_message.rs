//! #1299 S1 — `POST /api/tracks` delivers the synthesiser page's first message
//! atomically.
//!
//! The sentence the user types on `/area/{id}/new` used to go nowhere. These
//! tests pin the three things that had to become true for it to arrive:
//!
//! 1. it reaches the agent at all, exactly once;
//! 2. it arrives as a **`UserMessage` attributed to the human**, not as a
//!    `TrackGoal` (different render, no hard-fire, no human attribution);
//! 3. a rejected message leaves nothing behind;
//! 4. a create that promised delivery and did not complete the start says so —
//!    a harness that fails to start turns the create into a 5xx instead of a
//!    201 that quietly dropped the sentence, and the 5xx's text reports an
//!    *unknown* delivery, because on one of the four failure branches the
//!    message has in fact already arrived.
//!
//! Plus the largest regression surface: a create WITHOUT `first_message` must
//! behave exactly as it did before this slice, down to the operation payload
//! bytes — including keeping its `warn!` + 201 when the harness fails to
//! start, which is the control for (4).
//!
//! And one product with a neighbouring slice: `first_message` × `recipe_id`
//! (#1292 S2). Both fields are optional and independent, so picking a recipe
//! *and* typing a sentence became reachable the moment both shipped, with
//! nothing asserting about the pair. The two cases at the bottom of this file
//! cover it in both directions — an existing recipe and a missing one.
//!
//! # #1384 — safe retry
//!
//! The second half. A create carrying a `first_message` now requires an
//! `Idempotency-Key`, and the key→track binding is persisted **in the same
//! transaction that mints the id** (`track_create_idempotency`). The four
//! variants that block make up the middle of this file:
//!
//! * V1 — a replay returns the same track and does not re-deliver;
//! * V2 — a success that landed on a `#N` retry key still replays;
//! * V3 — the arm is decided before the create path validates, so a replay
//!   survives its attached directory being deleted;
//! * V4 — a daemon outage adopts the track it already minted, instead of
//!   minting one per retry.
//!
//! # What is STILL not promised, and is asserted rather than assumed
//!
//! A **message-less** `POST /api/tracks` remains non-idempotent: the header is
//! not read on that path, no binding row is written, and a retry mints a second
//! track exactly as it always has
//! (`a_create_without_a_first_message_is_unchanged`,
//! `a_message_less_create_writes_no_binding_row`).
//!
//! Two further arms are **not covered here, and no test pretends to cover
//! them**: the in-flight duplicate and the cross-process primary-key race.
//! `plan_first_message` takes an in-process claim before either lookup and
//! holds it through the mint, so two same-key creates inside one process
//! serialize and the second takes the resuming arm without ever reaching the
//! primary key. Both need a cross-instance harness.

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
use calm_server::event::EventBus;
use calm_server::model::NewArea;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
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
    tmp: TempDir,
}

/// A real git repository the user owns, the shape `PATCH /api/tracks/{id}`
/// accepts as an attached workspace. Same recipe as
/// `cases/track_workspace_repoint.rs::user_repo`.
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

/// Same fixture, but with the shared codex app-server **not running** —
/// `SharedCodexAppServer::is_running()` is false, which is what
/// `PlannerHarnessStartAdapter::validate` refuses on. This is the production
/// state during a daemon outage / restart window, and it is the only way to
/// reach variant 4: with a fake installed, `is_running()` short-circuits to
/// `true` and the outage is unconstructible.
async fn boot_without_daemon() -> Boot {
    boot_with_daemon(false).await
}

async fn boot_with_daemon(daemon_running: bool) -> Boot {
    let tmp = TempDir::new().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "track-create-first-message".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
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
    let app = routes::router()
        .layer(Extension(Principal {
            user_id: "owner".into(),
            display_name: "owner".into(),
            role: "owner".into(),
            session_id: "test".into(),
        }))
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());
    Boot {
        app,
        state,
        area_id: area.id.to_string(),
        repo,
        tmp,
    }
}

impl Boot {
    /// `POST /api/tracks`. `idempotency_key` and `first_message` are both
    /// optional so one helper covers the legacy shape and the keyed shape — the
    /// point of several tests below is that omitting them changes nothing.
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

    /// `POST /api/tracks` with an **explicit** `cwd`, i.e. the attached branch.
    ///
    /// The attached branch is the one whose create-path validation reads the
    /// disk (`validate_attached_workspace`), so it is the only shape that can
    /// show the ordering bug: the same bytes stop being acceptable the moment
    /// the directory goes away, even though the replay mints nothing.
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
        let mut builder = Request::builder()
            .method("POST")
            .uri("/api/tracks")
            .header("content-type", "application/json");
        if let Some(key) = idempotency_key {
            builder = builder.header("idempotency-key", key);
        }
        let response = self
            .app
            .clone()
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

    /// `PATCH /api/tracks/{id}` — the production route that repoints a managed
    /// workspace at a repository the user owns (#1147 S3).
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

    async fn workspace_row(&self, track_id: &str) -> (String, String) {
        sqlx::query_as("SELECT workspace_kind, workspace_path FROM tracks WHERE id=?1")
            .bind(track_id)
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }

    /// The `cwd` of every `planner-harness-start` payload that carries `needle`
    /// as its `first_message`, oldest first.
    ///
    /// Filtered by `first_message` on purpose: the re-point route submits a
    /// `planner-harness-start` of its own (with the new cwd and
    /// `force_new_thread`), so "the last payload" would answer about the wrong
    /// operation.
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

    /// #1384 — how many `(area, Idempotency-Key)` → track bindings exist.
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

    /// How many copies of `needle` the harness has been handed.
    ///
    /// Counted from the harness, not from the audit event: the audit row is
    /// only *evidence* of a delivery, so counting it to prove a delivery
    /// happened would be circular. Two places have to be summed because a
    /// message may or may not have been drained into a turn yet — turns already
    /// started on the fake app-server, plus observations still queued.
    ///
    /// Substring occurrences, not entries: adjacent `UserMessage`s fold into
    /// one concatenated entry, so counting entries would under-report a double
    /// send. Polls, because the run loop drains on a background task, and
    /// returns what it saw so a failing assertion reports the real number.
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

    /// Everything the fake app-server was ever asked to run a turn on, as one
    /// JSON blob. This is the *rendered* text — `Observation::to_turn_text` —
    /// which is where `TrackGoal` and `UserMessage` visibly differ.
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

    /// Every `planner-harness-start` payload as it was persisted into
    /// `operations.payload_json`.
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

    /// `POST /api/track-recipes` — the production write boundary for a
    /// user-defined recipe (#1292 S1). Returns its id.
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

    /// `GET /api/tracks/{id}`, used here only to read the instantiated report
    /// back out of the same surface the picker reads.
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

    /// Reject the `spawn_succeeded` phase write, so the driver's `set_phase`
    /// errors *after* `spawn_side_effect` has already installed a live
    /// harness. This is the only way to reach the `OperationOutcome::Stuck`
    /// binding of `harness_start_failed` from a route test: the failure has to
    /// land between the side effect and the record of it.
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
///
/// Fails when the `Observation::UserMessage` seed is removed from
/// `PlannerHarnessStartAdapter::prepare_tx`.
#[tokio::test]
async fn the_first_message_reaches_the_agent_exactly_once() {
    let b = boot().await;
    let (status, body) = b
        .create_track(Some("idem-headline"), Some("refactor the parser"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    // "Exactly once", written so it actually says that. `copies_in_harness`
    // returns as soon as it has seen `want`, so asking for 1 and comparing to 1
    // asserts "at least one, and no second copy had arrived at the instant I
    // looked" — the second half is a race, not an assertion. So: wait for the
    // delivery with its own budget, then ask for a SECOND copy, which burns the
    // full deadline before answering 1.
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

/// The message is a **`UserMessage` from the human**, not a `TrackGoal`.
///
/// Two independent assertions because the two halves fail differently:
/// swapping the observation type keeps the audit row (it is written on
/// `first_message.is_some()`, not on the variant) but changes the rendered turn
/// text from `"User says:\n…"` to bare text; attributing the event to a machine
/// actor keeps the render but loses the human.
///
/// Fails when `Observation::UserMessage` is swapped for `Observation::TrackGoal`.
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
    // The persisted `actor` column, verbatim. `ActorId::User` serialises as
    // `{"kind":"User"}`; any AI/session actor is a different shape, so this
    // pins WHO the row is attributed to rather than merely that a row exists.
    assert_eq!(
        b.user_message_actor().await,
        r#"{"kind":"User"}"#,
        "the audit row must attribute the message to the human who typed it"
    );
    b.shutdown_harnesses().await;
}

/// The largest regression surface in this slice: a create with **no**
/// `first_message` is unchanged.
///
/// The header is not merely optional there, it is **not read at all** — so a
/// caller that sends a duplicate key twice still gets two tracks. That is a
/// KNOWN GAP stated in the module header, not an accident, and asserting it
/// here is what keeps someone from "fixing" it without reading why.
///
/// The operation payload must stay byte-identical in shape — no
/// `first_message` key at all, because `skip_serializing_if` is what keeps an
/// in-flight retry across a deploy from becoming a spurious payload-hash 409.
#[tokio::test]
async fn a_create_without_a_first_message_is_unchanged() {
    let b = boot().await;
    let (first, body) = b.create_track(None, None).await;
    assert_eq!(first, StatusCode::CREATED, "body={body}");
    let (second, _) = b.create_track(None, None).await;
    assert_eq!(second, StatusCode::CREATED);
    // Same key twice, no first message: the header is ignored, so these are two
    // more independent tracks.
    let (third, _) = b.create_track(Some("ignored-key"), None).await;
    let (fourth, _) = b.create_track(Some("ignored-key"), None).await;
    assert_eq!(third, StatusCode::CREATED);
    assert_eq!(fourth, StatusCode::CREATED);
    assert_eq!(b.track_count().await, 4);
    assert_eq!(
        b.user_message_event_count().await,
        0,
        "nothing was typed, so nothing may be enqueued"
    );
    let payloads = b.operation_payloads().await;
    assert_eq!(payloads.len(), 4, "one start per create: {payloads:?}");
    for payload in payloads {
        assert!(
            payload.get("first_message").is_none(),
            "the message-less payload must not carry the key at all: {payload}"
        );
        assert!(payload.get("first_message_sha256").is_none());
    }
    b.shutdown_harnesses().await;
}

/// A rejected message leaves nothing behind — the one compensation-shaped
/// promise this slice does make.
///
/// `create_track` mints five kinds of row and materializes a workspace after the
/// commit, so it is not a compensating handler and this slice does not make it
/// one. What it guarantees is narrower and checkable: the `first_message`
/// validation runs before *any* of that.
///
/// Fails when the validation is moved after the mint.
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

    // 32768 characters is the ceiling, counted in CHARACTERS not bytes — a
    // multi-byte string at the limit must be accepted.
    let at_limit = "é".repeat(32_768);
    let (ok, body) = b.create_track(Some("idem-limit"), Some(&at_limit)).await;
    assert_eq!(ok, StatusCode::CREATED, "body={body}");
    b.shutdown_harnesses().await;
}

/// A `template_id` create delivers the message like any other create.
///
/// This case replaces a refusal. Until #1318 S2 there was a second create
/// shape, `{"as_template": true}`, which minted a track and deliberately did
/// **not** start a planner harness; a `first_message` on it had no queue to
/// land in, so this slice refused the combination before the mint. #1318 S2
/// retired that field (it is now an unknown field, 422 at the extractor), and
/// with it the only branch that skipped `start_planner_harness` — the call in
/// `create_track_with_planner_harness` is now unconditional.
///
/// So "template create" today means `template_id`, which names a roster entry
/// (`crates/calm-server/src/templates.rs`) that seeds the report inside the
/// create transaction and then starts the harness exactly like a blank create.
/// There is nothing left to refuse, and the thing worth pinning is the
/// opposite: that this shape delivers. Read from the harness, not from the
/// status code, because a create that dropped the message would also answer
/// 201.
#[tokio::test]
async fn a_template_create_delivers_the_first_message() {
    let b = boot().await;
    // A needle that appears nowhere in the `small-change` template body, so the
    // count cannot be satisfied by the seeded report travelling into the turn.
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

    // Premise: this really is the template shape, not a blank create that
    // ignored `template_id`.
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

// ---------------------------------------------------------------------------
// `first_message` × `recipe_id`
//
// A combination neither suite covered. #1292 S2 added `recipe_id` as a third
// initialization source and pinned it on creates that type nothing; #1299 S1
// added `first_message` and pinned it on creates that name no source. Both are
// optional and independent, so the product is reachable from the synthesiser
// page the moment a user picks a recipe and also types a sentence — and until
// these two cases it was reachable with nothing asserting about it.
//
// The two halves fail differently, which is why there are two cases: the
// recipe is instantiated *inside* the create transaction and the message is
// delivered by the operation submitted *after* it commits, so an interaction
// bug can drop either one while leaving the other looking correct.
// ---------------------------------------------------------------------------

/// A recipe body with one task, so "the recipe was instantiated" is checkable
/// on a structural field and not only on prose.
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

/// `first_message` + an **existing** `recipe_id`: the recipe becomes the
/// report and the sentence is delivered exactly once, on one 201.
///
/// Both halves are asserted on the mechanism rather than on the status code.
/// The recipe half reads the report back through `GET /api/tracks/{id}` and
/// checks the task block, because a create that silently took the blank branch
/// would also answer 201 with a perfectly plausible empty report. The delivery
/// half uses this suite's two-step budget — wait for the first copy, then ask
/// for a second one and let it burn the whole deadline — because comparing a
/// single `want: 1` poll against 1 asserts "no second copy had arrived at the
/// instant I looked", which is a race and not an assertion.
#[tokio::test]
async fn a_first_message_is_delivered_once_on_a_recipe_create() {
    let b = boot().await;
    let recipe_id = b.create_recipe("rollout flow", &recipe_body()).await;

    // A needle that appears nowhere in the recipe, so `copies_in_harness`
    // cannot be satisfied by the instantiated report travelling into the turn.
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
    // A recipe id is not a plugin-bindable template id, and a `first_message`
    // riding along must not change that.
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

/// `first_message` + a **missing** `recipe_id`: the in-transaction 400 rolls
/// the whole create back, and the message leaves no trace either.
///
/// This is the one refusal in this suite that is decided *inside* the create
/// transaction rather than before it (`TrackInit::Recipe` reads the row in the
/// same tx as the mint, on purpose, so a concurrently deleted recipe cannot be
/// seen as present by a pre-tx read and absent by the writer). So "nothing is
/// left behind" here is a rollback claim, not an ordering claim, and it is
/// asserted over every row kind this path can write: the track, its cards, the
/// operation the delivery would have travelled on, and the audit event.
///
/// The `operations` count is the load-bearing one for the interaction: the
/// `planner-harness-start` carrying `first_message` is submitted only *after*
/// the create transaction commits, so a create that submitted it before
/// resolving the recipe would leave an operation row pointing at a track that
/// never existed.
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

// ---------------------------------------------------------------------------
// A harness that fails to start
//
// `start_planner_harness` can fail in four ways — the submit is rejected, the
// wait errors, the operation reaches `Failed`, the operation reaches `Stuck`.
// All four used to be `warn!` + `Ok(())`, i.e. 201, which was the right answer
// while a create carried nothing but structure: "the track exists and its
// planner agent is inert" is recoverable, and the user can start the agent from
// the track.
//
// A create carrying a `first_message` promised something more, and the message
// only ever gets written by that same operation. So the two cases below pin the
// fork: with a message, a failed start is a 5xx; without one, the same failure
// is still a 201. They are a pair on purpose — a fix that made every harness
// failure a 5xx would satisfy the first and break the second, and that
// regression is exactly what the second case exists to catch.
//
// Both drive the real route with `fail_next_thread_start_for_test`, which makes
// the production adapter fail in `AppServerInteract`, i.e. the
// `OperationOutcome::Failed` branch. A third case further down reaches the
// `Stuck` branch by rejecting the `spawn_succeeded` phase write, which is the
// branch that behaves differently in the one way that matters — the message is
// already delivered. The remaining two (submit failed, `wait()` errored) are
// not separately implemented: all four write one `harness_start_failed`
// binding that one `if` reads, so the branch under test is the same code they
// reach.
// ---------------------------------------------------------------------------

/// With a `first_message`, a harness that fails to start is a 5xx — and the
/// track is still there.
///
/// The second half is the part worth writing down. This is not a compensating
/// handler: by the time `start_planner_harness` runs, the create transaction
/// has committed and the workspace is materialized. The 5xx says "the create
/// did not keep its delivery promise", not "nothing happened", and this test
/// pins both halves so nobody later reads the status as a rollback.
///
/// On *this* branch the message really did not arrive (asserted below), but
/// that is a fact about this branch, not about the status code: see
/// `a_stuck_start_after_spawn_has_already_delivered_the_first_message` for the
/// branch where the same 5xx sits on top of a delivered message, which is why
/// the response text asserts an unknown outcome rather than a failed one.
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

    // Premise: the message really was not delivered — otherwise the 5xx would
    // be the lie instead.
    //
    // Checked at the harness, not at the audit event. The audit row IS written
    // here: `prepare_tx` seeds the observation and writes
    // `harness.user_message.enqueued` in one transaction that commits in
    // `TxCommitted`, and the failure happens later, in `AppServerInteract`.
    // `events` is an append-only log and the operation's compensation does not
    // rewrite history — it marks the runtime failed, which is what makes the
    // seeded observation unreachable. So the audit row says "a delivery was
    // attempted", not "a delivery happened", and counting it here would assert
    // the opposite of the thing under test.
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

/// Without a `first_message`, the *same* failure is still a 201.
///
/// The control for the case above, and the reason the new semantics is scoped
/// to `first_message` rather than applied to every harness failure. Nothing the
/// user typed was riding on this operation, so "the track exists, its planner
/// agent is inert" is a complete and recoverable answer — the pre-#1299
/// behaviour, unchanged.
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

// ---------------------------------------------------------------------------
// #1299 — what the 500 is allowed to SAY.
//
// Characterization, not an invariant. The four failure bindings the handler
// collapses into one `harness_start_failed` do not agree about whether the
// message arrived, and the case below is the one that proves it: the phase
// write fails *after* `spawn_side_effect` installed a live harness, so the
// seeded observation is already in it and the turn is already out, while the
// response is still a 500. The 500 text therefore may not assert non-delivery
// — a user who followed such a text and resent from the track would get two
// copies.
//
// "Failing after the spawn means the message is already delivered" is a
// consequence of today's driver semantics (spawn is the side effect, the
// phase write only records it, and there is no compensation on this path), not
// a property anyone promised.
//
// #1384 UPDATE — the unknown survived, and this is why.
//
// The header used to say this test was expected to change once #1384 taught the
// endpoint to answer what actually happened. It cannot, and the reason is not
// effort: `harness.user_message.enqueued` proves only an *attempt*. `prepare_tx`
// seeds the observation and writes that audit row in a transaction that commits
// at `TxCommitted`, the later `AppServerInteract` can still fail, `events` is
// append-only, and compensation only marks the runtime failed. There is no other
// durable record of the turn leaving, so no read the handler can perform answers
// the question — and a *negative* claim would be a lie precisely on this branch.
//
// What #1384 did add is the actionable half, and only for the two things it can
// prove: a retry under the same `Idempotency-Key` creates no second track (the
// binding row) and delivers no second copy (`retryable_operation_key` does not
// step over `Stuck`, so the retry resolves to this same operation and replays
// the recorded failure). The assertions below now pin both, and pin that the
// text still refuses to claim the track is usable — a replay does not repair an
// attached workspace whose directory was deleted.
// ---------------------------------------------------------------------------

/// A start that fails *after* the spawn has already delivered the message —
/// and the 500 must not claim otherwise.
///
/// Two halves, both needed:
///
/// * the observed behaviour (`copies_in_harness == 1` under a 500), which is
///   the reason the wording has to be uncertain rather than negative;
/// * the wording itself, asserted in both directions — the new text is present
///   AND the old "was not delivered" claim is absent. Asserting only one of the
///   two goes silently vacuous the next time somebody rewrites the sentence.
#[tokio::test]
async fn a_stuck_start_after_spawn_has_already_delivered_the_first_message() {
    let b = boot().await;
    // Reject the `spawn_succeeded` phase write, i.e. fail the operation at the
    // one point that is past the side effect. This is the `OperationOutcome::
    // Stuck` binding of `harness_start_failed`.
    b.reject_spawn_succeeded().await;

    let needle = "this sentence is already on its way";
    let (status, body) = b.create_track(Some("idem-stuck"), Some(needle)).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a create whose harness start did not complete still answers 5xx: body={body}"
    );

    // The observed fact that makes a "not delivered" 500 a lie: the harness is
    // live and holding the sentence.
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
    // …and negative: it does not assert the delivery failed. A user acting on
    // that claim resends and ends up with two copies.
    assert!(
        !text.contains("not delivered"),
        "the 500 must not claim the message was not delivered — on this path it was: {text}"
    );
    assert!(
        !text.contains("send the message from the track itself"),
        "…nor instruct an unconditional resend, which duplicates it on this path: {text}"
    );
    // #1384 — the two properties the server CAN prove, named in the text. Both
    // are new: before the binding row existed, a retry under this key minted a
    // second track, so neither sentence would have been true.
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
    // And the claim it must NOT make, now that the create IS retryable: that the
    // track is fine. A replay does not repair an attached workspace whose
    // directory was deleted, so "retryable" is not "healthy".
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

// ===========================================================================
// #1384 — safe retry.
//
// Everything below this line is about ONE `Idempotency-Key` producing at most
// one track. The four variants the issue names are T-V1…T-V4; the rest pin the
// pieces they rest on (the arm table's fail-closed cell, the `Mint`-only
// binding write, `Resume`'s re-materialization and the poisoned-workspace
// trade).
// ===========================================================================

/// `Idempotency-Key` is required exactly when `first_message` is present.
///
/// Fails when the header is made optional on this branch — a `first_message`
/// with no key has no dedup key at all, so a retried create would mint a second
/// track and deliver the instruction twice.
#[tokio::test]
async fn a_first_message_without_an_idempotency_key_is_rejected_before_any_mint() {
    let b = boot().await;
    let (status, body) = b.create_track(None, Some("no key, no track")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert_eq!(b.track_count().await, 0, "no track may survive the refusal");
    assert_eq!(b.card_count().await, 0, "and no cards either");
    assert_eq!(b.binding_count().await, 0);
}

/// T-LEGACY-1 — the binding row is written on the **`Mint` arm only**.
///
/// Sent WITH an `Idempotency-Key` and WITHOUT a `first_message`, which is the
/// only shape that can tell the two conditions apart: "the header was present"
/// and "the plan was `Mint`". `plan_first_message` returns `Legacy` before the
/// header is read, so the key here is inert.
///
/// Writing it on `Legacy` too would be actively wrong rather than merely
/// wasteful: `Legacy` has already returned from the dispatch, so there is no
/// resuming arm for a primary-key collision to map onto, and the second create
/// below — a working 201 today — would become an error.
///
/// Fails when the `Mint`-arm condition on the binding write is removed.
#[tokio::test]
async fn a_message_less_create_writes_no_binding_row() {
    let b = boot().await;
    let (first, body) = b.create_track(Some("idem-legacy"), None).await;
    assert_eq!(first, StatusCode::CREATED, "body={body}");
    assert_eq!(
        b.binding_count().await,
        0,
        "a message-less create must write no binding row even when the caller sends a key — the \
         header is not read on that path at all"
    );
    let (second, body) = b.create_track(Some("idem-legacy"), None).await;
    assert_eq!(
        second,
        StatusCode::CREATED,
        "…and the same key again must still be a plain 201, not a collision: body={body}"
    );
    assert_eq!(
        b.track_count().await,
        2,
        "a message-less create is deliberately still NOT idempotent (KNOWN GAP 1)"
    );
    b.shutdown_harnesses().await;
}

/// T-V1 — replaying a successful create returns the SAME track and does not
/// re-deliver the message.
///
/// Fails when the binding lookup in `plan_first_message` is made to always
/// answer `None`: the replay then mints a second track.
#[tokio::test]
async fn replaying_a_successful_create_returns_the_same_track_and_delivers_once() {
    let b = boot().await;
    let (first, first_body) = b
        .create_track(Some("idem-replay"), Some("ship the thing"))
        .await;
    assert_eq!(first, StatusCode::CREATED, "body={first_body}");
    assert_eq!(b.binding_count().await, 1, "the mint wrote its binding");
    // Give the first delivery its own budget before the replay, so the "no
    // second copy" check below burns its full deadline on the question it is
    // actually asking instead of racing the first enqueue.
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

/// T-V1b — arm (a) with the one piece of server state that can move underneath
/// a replay: the track's workspace.
///
/// `PATCH /api/tracks/{id}` repoints a managed workspace at a repository the
/// user owns, which changes `track.workspace.path`. That path travels in the
/// `planner-harness-start` payload, and `submit` compares `payload_hash` before
/// anything else — so a replay that rebuilt the payload from *current* state
/// would hash differently and be answered 409 `conflict` for a request the
/// client sent byte for byte identically. Permanently, and indistinguishably
/// from a genuine different-body conflict.
///
/// Fails when the `PriorArm::Replay` branch takes `track.workspace.path`
/// instead of the chosen operation's own `cwd`.
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
    // The mechanism, not just the status: the replayed payload carries the
    // predecessor's cwd, which is what makes the hashes match.
    assert_eq!(
        b.first_message_payload_cwds("ship the thing").await,
        vec![managed_path.clone()],
        "the replay must resubmit the chosen operation's payload, cwd included"
    );
    b.shutdown_harnesses().await;
}

/// The counterweight to the case above, resolving the **other** way.
///
/// A genuine retry really executes: it starts a harness. Replaying the failed
/// attempt's `cwd` would start it in a managed directory the re-point has since
/// moved into the trash. Nothing forces it to be byte-identical either — the
/// retry runs under a fresh `#N` key that no earlier payload hash is bound to.
///
/// This test must stay GREEN when the replay fix is mutated away, which is what
/// proves the fix is scoped to the replay arm rather than applied to both.
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

/// T-V2 — the success did not happen on the base key, it happened on `#2`, and
/// replaying it must still be a replay.
///
/// 1. the base attempt terminally fails;
/// 2. the same key succeeds under `#2`, whose payload froze the *managed* cwd;
/// 3. `PATCH /api/tracks/{id}` repoints the track;
/// 4. the create request is replayed byte for byte.
///
/// `retryable_operation_key` walks past the `Failed` base and stops on `#2`
/// because it is not `Failed` — so the chosen key already holds a **succeeded**
/// operation, i.e. this is a replay. A criterion that only asks "does the chosen
/// key carry a `#N` suffix" answers `GenuineRetry` here, rebuilds the payload
/// from the repointed workspace, and `submit` answers 409 for a byte-identical
/// request.
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

/// T-HASH-1 / arm (e) — the same key with a different message is a conflict,
/// not a silent replay of the first sentence.
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

/// Arm (b) — after a terminally failed attempt the same key genuinely RETRIES:
/// it does not replay the recorded failure, and it does not mint a second track.
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

/// The seam between arm (b) and arm (e), measured.
///
/// Arm (e) ("same key, different message ⇒ 409") is not unconditional. The 409
/// comes from the payload hash bound to a *specific* operation key, and a
/// terminal failure moves the retry to a fresh `#N` key that no hash is bound
/// to. So an edited sentence resent after a failure is accepted — and the
/// interesting half is what happens to the *abandoned* draft: it must not be
/// delivered alongside the edit.
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
    // TWO audit rows for ONE delivery, and that is the current truth rather than
    // an oversight: the failed attempt's `prepare_tx` committed — the enqueue and
    // its `harness.user_message.enqueued` row share that transaction — and only
    // the thread start afterwards failed. Compensation aborts the harness task
    // and fails the runtime; it does not roll back a committed event row. So "an
    // audit row exists" does NOT imply "the message was delivered", which is the
    // same fact that keeps the 500's wording uncertain.
    assert_eq!(
        b.user_message_event_count().await,
        2,
        "one delivered message, but two audit rows — the failed attempt's row survives its \
         compensation"
    );
    b.shutdown_harnesses().await;
}

/// T-EXH-1 / arm (d) — 64 terminally failed attempts exhaust the key, and the
/// 65th says so with its own code rather than a generic 500.
///
/// Driven through the real endpoint 64 times rather than by hand-seeding
/// `operations` rows: the `#N` chain, the payload the route writes and the
/// track-reuse branch all have to hold for the count to be reached, and a
/// seeded row would prove none of that.
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
    b.shutdown_harnesses().await;
}

/// T-V3 — the arm is decided BEFORE the create path validates the request.
///
/// 1. a create with an explicit `cwd` succeeds, attaching the track to a
///    directory the user owns;
/// 2. the user deletes that directory;
/// 3. the create request is replayed **byte for byte**.
///
/// The replay mints nothing — the track, its cards and its folder claim all
/// exist — so nothing about it needs the directory. But the handler re-read the
/// disk on the way in (`validate_attached_workspace`) and answered
/// `400 attached workspace ... does not exist` before it ever reached the replay
/// branch. Not a missing frozen payload field: an ordering bug.
///
/// Fails (400 instead of 201) when `validate_attached_workspace` is moved ahead
/// of the `CreatePlan` dispatch.
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

    // The disturbance: the user's directory goes away. The harness is
    // deliberately left running — shutting it down here would drop any copy
    // still sitting in its pending queue, and the final count would then read 0
    // under load.
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
    // KNOWN GAP 6, asserted rather than assumed: the replay 201s and the
    // workspace is still broken. `materialize_workspace` is an unconditional
    // no-op for `Attached`, so `Resume`'s re-materialization does not repair
    // this — and no "safe retry" sentence in this feature claims it does.
    assert!(
        !attached.exists(),
        "the replay must NOT have recreated the user's directory: that is the deliberate \
         carve-out, and a test that stopped observing it would let the carve-out quietly change"
    );
    b.shutdown_harnesses().await;
}

/// The same root cause truncating the genuine-retry arm, which is why the fix is
/// the ordering and not a wider frozen payload.
///
/// Constructed with a `.git` removal rather than a whole-directory delete so the
/// retry has a real directory to run in: `PATCH /api/tracks/{id}` refuses to
/// repoint an *attached* workspace, so "repoint to a valid B" is not reachable
/// for the shape that can 400 here.
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

    // The disturbance: the directory stops satisfying the create-path check
    // (`is not inside a Git work tree`) while remaining a perfectly usable
    // directory for the retry that is about to run in it.
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

/// The counterweight to the two cases above: moving the arm decision in front of
/// the create-path validation must not remove that validation from the path that
/// still mints.
///
/// Written as an assertion rather than as "I read the code and `Legacy` returns
/// before the header is read": every check the reorder skipped on the resuming
/// arms is exercised here on a create with **no** `first_message`, plus a
/// template create, which is the other production entry into this handler.
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

/// **T-V4 — the headline.** A daemon outage adopts the track it already minted
/// instead of turning one `Idempotency-Key` into a track farm.
///
/// The construction, and why it is not covered by "this handler does not
/// compensate": `OperationRuntime::submit` runs `adapter.validate` **before**
/// `insert_operation`, and `PlannerHarnessStartAdapter::validate` refuses while
/// the shared app-server is down. So the refusal writes no operation row at all.
/// Before #1384 the operation row was the only record of which track a key
/// created, so this measured, on exactly this fixture: two requests under one
/// key, both 500, **2** tracks, **4** cards, **0** operations.
///
/// The numbers below are the inverted ones, and the inversion is the point. The
/// rejected daemon-preflight fix asserted `tracks == 0` — it prevented the mint.
/// This design does not prevent it; it *adopts* it. One track, two cards, two
/// 500s, no delivery, and `operations` still 0 because `validate` still refuses.
///
/// Fails (`track_count == 2`) when the `track_create_idempotency` INSERT is
/// deleted from the create closure: the retry then reads no binding and mints.
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
    // The load-bearing number. `2` is the measured pre-#1384 behaviour.
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

/// T-V4b — the control: the message-less path keeps its `warn!` + 201 during the
/// same outage.
///
/// The rejected preflight fix would have turned this 201 into a 500 (and every
/// in-transaction 4xx with it). Nothing in this design puts a daemon check
/// anywhere the `Legacy` arm can reach, and this is what says so.
#[tokio::test]
async fn a_create_without_a_first_message_still_succeeds_during_a_daemon_outage() {
    let b = boot_without_daemon().await;
    let (status, body) = b.create_track(None, None).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    assert_eq!(b.track_count().await, 1);
    assert_eq!(b.binding_count().await, 0);
    b.shutdown_harnesses().await;
}

/// T-ARM-2 — the arm table's last cell: a binding **miss** with an **occupied**
/// chosen key mints nothing.
///
/// The state is unreachable by construction (the binding commits strictly before
/// the operation is submitted), so it is constructed by hand: an operation row
/// under the derived key with no binding row. The earlier draft answered `Mint`
/// here, which fails *open* — the mint commits a track and its cards, and
/// `insert_operation` then raises `idempotency_payload_conflict` on the unique
/// violation, leaving an orphan track behind a 409. That is the exact failure
/// class this feature abolishes, so the honest answer is an error.
///
/// The assertion that matters is `track_count == 0`, not the status: the claim
/// is about what is **written**.
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
    // An operation under the derived key, in a non-`Failed` phase so
    // `retryable_operation_key` stops on it, and with no binding row anywhere.
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

/// T-MAT-1 — `Resume` re-materializes the workspace.
///
/// The failure points the resuming arm exists for include "process died between
/// the COMMIT and `materialize_workspace`" and "`materialize_workspace` returned
/// `Err`". A resume that only re-submitted the operation would answer 201 for a
/// track whose managed directory does not exist — #1147 replayed one layer down.
///
/// Construction: create successfully with key K, then remove the managed
/// directory, then replay K.
///
/// Fails when the `materialize_workspace` call is deleted from
/// `resume_prior_attempt`: the replay then 201s onto a directory with no `HEAD`.
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

/// T-MAT-2 — the idempotence premise §4.4 rests on: re-materializing a HEALTHY
/// managed workspace is a no-op.
///
/// Every other resuming test would stay green if `materialize_workspace` quietly
/// re-ran `git init` and a fresh initial commit on each call — the directory
/// stays valid either way. This one sees it, because it compares the owner
/// marker and the HEAD commit id across the replay.
///
/// Fails when the `if !git_head_resolves(path)` guard is dropped from
/// `materialize_managed_workspace_inner`: HEAD moves.
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

/// T-BRICK-1 — the ownership fence is NOT relaxed, and the answer is
/// 409 `idempotency_key_exhausted` rather than a generic 500 or a 201.
///
/// The state is reachable, not theoretical: `write_owner_marker` creates
/// `<path>/.git` and only then writes the marker, so process death between those
/// two syscalls leaves a directory that has entries and no marker — which
/// `materialize_workspace` refuses forever. Allowlisting "the only entry is
/// `.git/`" would be a marker-absence heuristic, and no positive fingerprint can
/// separate it from a user's own partially-initialised repository, so the
/// refusal stands and the resuming arm inherits it.
///
/// The honest cost, stated in the design and asserted here: this key is poisoned
/// for good. `idempotency_key_exhausted` is the one answer that is actionable —
/// "retry under a new key", which `a_new_idempotency_key_recovers_from_a_poisoned_workspace`
/// shows really works.
///
/// Fails when the materialization failure is mapped back to a generic
/// `CalmError::Internal`.
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

/// T-BRICK-2 — the escape, and it needs no new machinery: the poisoning is
/// **per key**.
///
/// A new `Idempotency-Key` misses the binding, mints a fresh track id, and a
/// managed path is derived from *that* id — so it is a different directory and
/// the poisoned one is never revisited. Nothing pulls the new attempt back onto
/// the old path: a managed workspace takes `FolderClaim::Skip`, so no
/// `area_folders` row contends on it either.
///
/// Fails when the managed path is derived from `(area_id, idempotency_key)`
/// instead of the minted track id: the new key then lands on the poisoned
/// directory and this test 409s.
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

    // A distinct sentence, so the delivery assertion below is about THIS track.
    // The poisoned track's harness was shut down before the disturbance, which
    // drops anything still sitting in its pending queue — counting both copies
    // would be counting a fixture artefact.
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

/// T-HASH-1 — the same key with a different **create** is a conflict.
///
/// Before #1384 the operation payload covered none of the create request's own
/// fields, so this returned 201 and the ORIGINAL track: the caller's new title
/// was silently discarded and nothing said so. `create_request_sha256` binds
/// `title`, `template_id` and `recipe_id` into `payload_hash`, which `submit`
/// compares before anything else runs.
///
/// Fails when `create_request_sha256` is dropped from the payload struct.
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

    // The control: the SAME title still replays. Without it the assertion above
    // would also be satisfied by a key that 409s on every repeat.
    let (replay, replay_body) = b.post_create(Some("idem-title"), base).await;
    assert_eq!(
        replay,
        StatusCode::CREATED,
        "…while a byte-identical replay must still replay: body={replay_body}"
    );
    assert_eq!(first_body["id"], replay_body["id"]);

    // And the other two bound fields, each on its own key so the two cases
    // cannot mask each other.
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
    let mut with_template = with_recipe;
    with_template.as_object_mut().unwrap().remove("recipe_id");
    with_template["template_id"] = json!("small-change");
    let (conflict, body) = b.post_create(Some("idem-source"), with_template).await;
    assert_eq!(
        conflict,
        StatusCode::CONFLICT,
        "…and so is naming a template instead: body={body}"
    );
    b.shutdown_harnesses().await;
}

/// T-HASH-2 — `skip_serializing_if` keeps every existing caller's
/// `payload_hash` stable.
///
/// The four other producers of `PlannerHarnessStartOperationPayload` and every
/// message-less create leave `create_request_sha256` as `None`. If the field
/// serialized as `"create_request_sha256": null` instead of being omitted, their
/// payload bytes would change, their `payload_hash` would move, and an
/// operation submitted by an older binary and retried after a deploy would come
/// back 409 `conflict` — a spurious "you changed your message" for a request
/// nobody changed.
///
/// Companion of `a_create_without_a_first_message_is_unchanged`: that one pins
/// the absence of `first_message`, this one the absence of the digest, and both
/// on the same bytes.
///
/// Fails when `skip_serializing_if` is removed from the field.
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

    // And the positive half, so this is not merely "the field is never written":
    // a keyed create DOES carry it, which is what makes the 409 above reachable.
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
        "a keyed create must carry the digest, or the conflict above could never fire: {:?}",
        keyed[0]
    );
    b.shutdown_harnesses().await;
}
