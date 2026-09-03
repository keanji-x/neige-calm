//! #1299 S1 — `POST /api/tracks` delivers the synthesiser page's first message
//! atomically.
//!
//! The sentence the user types on `/area/{id}/new` used to go nowhere. These
//! tests pin the four things that had to become true for it to arrive:
//!
//! 1. it reaches the agent at all, exactly once;
//! 2. it arrives as a **`UserMessage` attributed to the human**, not as a
//!    `TrackGoal` (different render, no hard-fire, no human attribution);
//! 3. `Idempotency-Key` makes a retry land on the same track and re-deliver
//!    nothing — and is *required* precisely when `first_message` is present;
//! 4. a rejected message leaves nothing behind.
//!
//! Plus the largest regression surface: a create WITHOUT `first_message` must
//! behave exactly as it did before this slice.
//!
//! And one product with a neighbouring slice: `first_message` × `recipe_id`
//! (#1292 S2). Both fields are optional and independent, so picking a recipe
//! *and* typing a sentence became reachable the moment both shipped, with
//! nothing asserting about the pair. The two cases at the bottom of this file
//! cover it in both directions — an existing recipe and a missing one.
//!
//! # The replay-vs-retry table, row by row
//!
//! `routes::tracks::create::PriorSelection` is that decision written as a table
//! over *what already sits on the operation key `retryable_operation_key`
//! chose*. Each row and the case that drives it end to end:
//!
//! | operation on the chosen key | driven by |
//! |---|---|
//! | none, chosen key is the base | `the_first_message_reaches_the_agent_exactly_once` (201, mints the track) |
//! | present and `Succeeded` | `replaying_a_successful_create_returns_the_same_track_and_delivers_once`, and `a_replay_survives_the_track_being_repointed_in_between` for the cwd half |
//! | present and `Succeeded` **on a `#N` key** | `a_replay_of_a_success_that_happened_on_a_retry_key_survives_a_repoint` — the regression this table replaced a point patch to fix |
//! | none, chosen key is `#N` | `the_same_key_after_a_failed_start_retries_against_the_same_track`, and `a_retry_after_a_failure_uses_the_repointed_workspace` for the cwd half |
//! | present and **in flight** | NOT covered here, and not coverable here: `plan_first_message` holds
//!   `conversation_first_message_locks` across the whole submit-and-wait, so a
//!   second in-process request under one key never observes the first's
//!   operation mid-flight. The row is reachable only across instances (the lock
//!   is in-process, as `FirstMessagePlan::_same_key_claim` says). |
//! | present and **`Stuck`** | NOT covered here: `planner-harness-start` never parks and nothing
//!   injects a compensation-step error, so `Stuck` has no in-process injection
//!   point — the same conclusion `cases/today_summary.rs` reaches about it. |
//!
//! # The arm decision precedes the create path's request validation
//!
//! Neither prior-attempt arm mints, so neither may be gated on a re-read of
//! mutable state the request already passed once. Three constructions of that
//! one root cause, one test each:
//!
//! | disturbance between the attempts | test |
//! |---|---|
//! | the track is repointed | `a_replay_survives_the_track_being_repointed_in_between` |
//! | the success happened on a `#N` key, then a repoint | `a_replay_of_a_success_that_happened_on_a_retry_key_survives_a_repoint` |
//! | the attached directory is **deleted** | `a_replay_survives_the_attached_directory_being_deleted` (round-3 BLOCKER) |
//! | same, truncating arm (b) instead | `a_retry_after_a_failure_survives_the_attached_directory_ceasing_to_validate` |
//!
//! The counterweight — that the validation is *skipped*, not *deleted* — is
//! `a_create_without_a_first_message_still_runs_every_create_check`.
//!
//! For the two uncovered rows the decision itself is still pinned, and pinned
//! *structurally* rather than by enumeration: `select_prior` takes a `bool`
//! ("is the chosen key occupied"), so no `Phase` can reach it and no phase can
//! select a different arm. `the_arm_is_decided_by_what_sits_on_the_chosen_key_not_by_its_name`
//! (unit, in `create.rs`) walks that table. What is genuinely unproven for those
//! two rows is the end-to-end response, not the arm.

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

async fn boot() -> Boot {
    boot_with_daemon(true).await
}

/// Same fixture, but with the shared codex app-server **not running** —
/// `SharedCodexAppServer::is_running()` is false, which is what
/// `PlannerHarnessStartAdapter::validate` refuses on. This is the production
/// state during a daemon outage / restart window.
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

impl Boot {
    /// `POST /api/tracks`. `idempotency_key` and `first_message` are both
    /// optional so one helper covers the legacy shape and the #1299 shape —
    /// the point of several tests below is that omitting them changes nothing.
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

    /// The `cwd` of every `planner-harness-start` payload that carries `needle` as
    /// its `first_message`, oldest first.
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

    /// The `first_message` key as it was persisted into `operations.payload_json`.
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

    async fn operation_count(&self) -> i64 {
        self.count("SELECT COUNT(*) FROM operations").await
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
        .create_track(Some("idem-1"), Some("refactor the parser"))
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
        .create_track(Some("idem-attr"), Some("please rename the track"))
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

/// `Idempotency-Key` is required exactly when `first_message` is present.
///
/// Fails when the header is made optional on this branch.
#[tokio::test]
async fn a_first_message_without_an_idempotency_key_is_rejected_before_any_mint() {
    let b = boot().await;
    let (status, body) = b.create_track(None, Some("no key, no track")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert_eq!(b.track_count().await, 0, "no track may survive the refusal");
    assert_eq!(b.card_count().await, 0, "and no cards either");
}

/// The other half of the requiredness decision, and the largest regression
/// surface in this slice: a create with **no** `first_message` is unchanged.
///
/// The header is not merely optional there, it is not read at all — so a caller
/// that sends a duplicate key twice still gets two tracks, exactly as before.
/// The operation payload must also stay byte-identical in shape: no
/// `first_message` key at all, because `skip_serializing_if` is what keeps an
/// in-flight retry across a deploy from becoming a spurious payload-hash 409.
#[tokio::test]
async fn a_create_without_a_first_message_is_unchanged() {
    let b = boot().await;
    let (first, body) = b.create_track(None, None).await;
    assert_eq!(first, StatusCode::CREATED, "body={body}");
    // Same key twice, no first message: the header is ignored, so these are two
    // independent tracks.
    let (second, _) = b.create_track(Some("ignored-key"), None).await;
    let (third, _) = b.create_track(Some("ignored-key"), None).await;
    assert_eq!(second, StatusCode::CREATED);
    assert_eq!(third, StatusCode::CREATED);
    assert_eq!(b.track_count().await, 3);
    assert_eq!(
        b.user_message_event_count().await,
        0,
        "nothing was typed, so nothing may be enqueued"
    );
    for payload in b.operation_payloads().await {
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

/// Arm (a): replaying a successful create returns the SAME track and does not
/// re-deliver the message.
#[tokio::test]
async fn replaying_a_successful_create_returns_the_same_track_and_delivers_once() {
    let b = boot().await;
    let (first, first_body) = b
        .create_track(Some("idem-replay"), Some("ship the thing"))
        .await;
    assert_eq!(first, StatusCode::CREATED, "body={first_body}");
    // Give the first delivery its own budget before the replay, so the "no
    // second copy" check below burns its full deadline on the question it is
    // actually asking instead of racing the first enqueue. (Without this the
    // assertion reads 0 under load, and 0 is indistinguishable here from a
    // regression that delivered nothing at all.)
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

/// Arm (a) again, with the one piece of server state that can move underneath a
/// replay: the track's workspace.
///
/// `PATCH /api/tracks/{id}` repoints a managed workspace at a repository the user
/// owns, which changes `track.workspace.path`. That path travels in the
/// `planner-harness-start` payload, and `OperationRuntime::submit` compares
/// `payload_hash` before anything else — so a replay that rebuilt the payload
/// from *current* state would hash differently and be answered 409 `conflict`
/// ("already used with different payload") for a request the client sent byte
/// for byte identically. Permanently, and indistinguishably from the genuine
/// arm-(e) conflict.
///
/// Fails when `create_track_with_first_message` goes back to passing
/// `track.workspace.path` on the `PriorArm::Replay` branch.
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
    // Its own budget, for the reason spelled out in
    // `replaying_a_successful_create_returns_the_same_track_and_delivers_once`.
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
        "the replay must resubmit the predecessor's payload, cwd included"
    );
    b.shutdown_harnesses().await;
}

/// Arm (b) under the same disturbance, and it must resolve the **other** way.
///
/// A genuine retry really executes: it starts a harness. Replaying the failed
/// attempt's `cwd` would start it in a managed directory the re-point has since
/// moved into the trash. Nothing forces it to be byte-identical either — the
/// retry runs under a fresh `#N` key that no earlier payload hash is bound to.
///
/// So this test is the counterweight to the one above: it must stay GREEN when
/// the replay fix is mutated away, which is what proves the fix is scoped to the
/// replay arm rather than applied to both.
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

/// REPRO (#1299 review construction): the success did not happen on the base
/// key, it happened on `#2` — and replaying it must still be a replay.
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
    // Wait for that delivery here rather than folding it into the final
    // assertion: `copies_in_harness` returns as soon as it has seen `want`, so
    // asking for 1 now costs nothing and gives the enqueue its own budget. The
    // final check then always burns its full deadline looking for a SECOND
    // copy, and cannot mistake "the first delivery was still in flight" for
    // "the replay delivered nothing".
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

/// Arm (e): the same key with a different message is a conflict, not a silent
/// replay of the first sentence.
///
/// Fails when `first_message` stops travelling in the operation payload (it is
/// bound into `payload_hash`, which is what `submit` compares first).
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

/// Arm (b): after a terminally failed attempt the same key genuinely RETRIES —
/// it does not replay the recorded failure, and it does not mint a second track.
///
/// The failed attempt's track survives (this handler creates it outside the
/// operation and compensation never touches it), so "retry" has to mean "run
/// the harness start again against that same track".
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
    let tracks_after_failure = b.track_count().await;
    assert_eq!(tracks_after_failure, 1, "the failed attempt left its track");

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
    // Same "exactly once" shape as `the_first_message_reaches_the_agent_exactly_once`:
    // the interesting failure here is the retry delivering a SECOND copy
    // alongside the failed attempt's, and `want = 1` cannot see it.
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

/// The seam between arm (b) and arm (e), on this route, measured.
///
/// Arm (e) ("same key, different message ⇒ 409") is not unconditional, and the
/// OpenAPI text used to read as if it were — which contradicted arm (b) two
/// clauses earlier. The truth is one rule seen from two sides: the 409 comes
/// from the payload hash bound to a *specific* operation key, and a terminal
/// failure moves the retry to a fresh `#N` key that no hash is bound to. So an
/// edited sentence resent after a failure is accepted.
///
/// This is the kernel's established behaviour (`retryable_operation_key` is
/// shared with `create_area_conversation`), so nothing here changes it. The
/// test exists because the documentation fix is only trustworthy if the
/// sentence it adds is the one the code actually implements — and because the
/// interesting half is what happens to the *abandoned* draft: it must not be
/// delivered alongside the edit.
///
/// Fails if the retry were made to replay the failed attempt's payload, or if
/// the failed attempt's message had reached the harness after all.
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
    // TWO audit rows for ONE delivery, and that is the current truth rather
    // than an oversight: the failed attempt's `prepare_tx` committed — the
    // enqueue and its `harness.user_message.enqueued` row share that
    // transaction — and only the thread start afterwards failed. Compensation
    // aborts the harness task and fails the runtime; it does not roll back a
    // committed event row. So "an audit row exists" does NOT imply "the
    // message was delivered", and the two assertions above are what actually
    // prove the delivery count.
    //
    // Harmless today: the only reader of these rows,
    // `user_message_already_enqueued`, is called with derived *conversation*
    // card ids and can never see a track's planner card. #1314 moves the two
    // conversation write paths onto this same operation, at which point the
    // implication stops holding for them too — that is filed on #1314 and must
    // land with its own counter-example test, not be papered over here.
    assert_eq!(
        b.user_message_event_count().await,
        2,
        "one delivered message, but two audit rows — the failed attempt's row survives its \
         compensation (see the comment above and #1314)"
    );
    b.shutdown_harnesses().await;
}

/// Arm (d): 64 terminally failed attempts exhaust the key, and the 65th says so
/// with its own code rather than a generic 500 that reads as "the server broke".
///
/// Driven through the real endpoint 64 times rather than by hand-seeding
/// `operations` rows: the `#N` chain, the payload the route writes and the
/// track-reuse branch all have to hold for the count to be reached, and a seeded
/// row would prove none of that.
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

/// REPRO (#1299 review round 3, BLOCKER): the third variant of the same root
/// cause — `create_track` used to run the create path's request validation
/// before it knew whether this request was a replay.
///
/// 1. a create with an explicit `cwd` succeeds, attaching the track to a
///    directory the user owns;
/// 2. the user deletes (or moves) that directory;
/// 3. the create request is replayed **byte for byte**.
///
/// The replay mints nothing — the track, its cards and its folder claim all
/// exist — so nothing about it needs the directory. But the handler re-read the
/// disk on the way in (`validate_attached_workspace`, `routes/tracks.rs:794`)
/// and answered `400 attached workspace ... does not exist` before it ever
/// reached the replay branch. Not a missing frozen payload field: an ordering
/// bug. The arm decision now runs first and returns through
/// `resume_prior_attempt` without touching any of it.
///
/// Fails (400 instead of 201) when the arm decision is moved back after the
/// validation.
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
    // Its own budget, so the final "no second copy" check can burn the full
    // deadline rather than racing the first delivery.
    assert_eq!(
        b.copies_in_harness("ship the thing", 1).await,
        1,
        "premise: the successful create delivered the sentence once"
    );

    // The disturbance: the user's directory goes away. The harness is
    // deliberately left running — shutting it down here would drop any copy
    // still sitting in its pending queue (the premise above is satisfied by a
    // queued copy just as well as by a started turn), and the final count would
    // then read 0 under load. Deleting a directory out from under a running
    // process is exactly the situation being reproduced anyway.
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
    b.shutdown_harnesses().await;
}

/// The same root cause truncating arm (b), which is why the fix is the ordering
/// and not a wider frozen payload.
///
/// A genuine retry is *going* to execute against the workspace the track has
/// **now**. It never got the chance: the create path re-validated the `cwd` the
/// request carried — the one the failed attempt named — and answered 400 before
/// the arm was allowed to use anything.
///
/// Constructed with a `.git` removal rather than a whole-directory delete so the
/// retry has a real directory to run in: `PATCH /api/tracks/{id}` refuses to
/// repoint an *attached* workspace (`already has an attached workspace`), so
/// "repoint to a valid B" is not reachable for the shape that can 400 here. What
/// is asserted is the load-bearing half either way — the 400 no longer fires,
/// the retry really executes, and it starts in the workspace the track has now.
///
/// Fails (400 instead of 201) when the arm decision is moved back after the
/// validation.
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

/// The counterweight to the two tests above: moving the arm decision in front of
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

    // And the happy legacy paths still work, plain and templated, with the
    // template really instantiated into the create transaction.
    let (plain_create, plain_body) = b.post_create(None, base.clone()).await;
    assert_eq!(plain_create, StatusCode::CREATED, "body={plain_body}");
    let mut templated = base.clone();
    templated["template_id"] = json!(calm_server::templates::SMALL_CHANGE);
    let (template_create, template_body) = b.post_create(None, templated).await;
    assert_eq!(template_create, StatusCode::CREATED, "body={template_body}");
    let stored: Option<String> = sqlx::query_scalar("SELECT template_id FROM tracks WHERE id = ?1")
        .bind(template_body["id"].as_str().unwrap())
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        stored.as_deref(),
        Some(calm_server::templates::SMALL_CHANGE)
    );
    assert_eq!(b.track_count().await, 2);
    assert_eq!(
        b.user_message_event_count().await,
        0,
        "nothing was typed on either, so nothing may be enqueued"
    );
    b.shutdown_harnesses().await;
}

/// `first_message` on a template create is refused rather than silently
/// dropped: `as_template` skips the planner harness entirely, so there would be
/// nothing to deliver it to.
#[tokio::test]
async fn a_template_create_refuses_a_first_message() {
    let b = boot().await;
    let body = json!({
        "area_id": b.area_id,
        "title": "",
        "as_template": true,
        "first_message": "this cannot be delivered",
        "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
    });
    let response = b
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tracks")
                .header("content-type", "application/json")
                .header("idempotency-key", "idem-template")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(b.track_count().await, 0);
}

/// A daemon outage must not turn one `Idempotency-Key` into a track farm.
///
/// The construction, and why it is not covered by "this handler does not
/// compensate": `OperationRuntime::submit` runs `adapter.validate` **before**
/// `insert_operation`, and `PlannerHarnessStartAdapter::validate` refuses while
/// the shared app-server is down. So the refusal writes no operation row — and
/// the operation row is the only record of which track a key created. Before
/// the fix this measured, on exactly this fixture: two requests under one key,
/// both 500, **2** tracks, **4** cards, **0** operations. The declared
/// exemption allows a failed create to leave *a* track; it does not allow the
/// next retry under the same key to mint another one, which is the opposite of
/// what the `Idempotency-Key` header documents.
///
/// The fix runs the adapter's own `ensure_running` before
/// `create_track_structure`, so nothing is minted at all.
#[tokio::test]
async fn a_daemon_outage_does_not_mint_a_track_per_retry_under_one_key() {
    let b = boot_without_daemon().await;
    let (first, first_body) = b.create_track(Some("idem-out"), Some("do the thing")).await;
    let (second, second_body) = b.create_track(Some("idem-out"), Some("do the thing")).await;
    assert_eq!(
        first,
        StatusCode::INTERNAL_SERVER_ERROR,
        "body={first_body}"
    );
    assert_eq!(
        second,
        StatusCode::INTERNAL_SERVER_ERROR,
        "body={second_body}"
    );
    // The load-bearing number. `1` would still be a regression: it would mean
    // the first attempt minted and the second adopted nothing.
    assert_eq!(b.track_count().await, 0, "tracks minted during the outage");
    assert_eq!(b.card_count().await, 0, "cards minted during the outage");
    assert_eq!(b.user_message_event_count().await, 0);
    b.shutdown_harnesses().await;
}

/// The counterweight to the case above: the preflight is on the
/// `first_message` **mint** arm only, so a create that carries no
/// `first_message` still succeeds during the same outage — `start_planner_harness`
/// is deliberately best-effort there (the track is the whole deliverable and an
/// inert planner agent is recoverable). Moving the preflight anywhere that the
/// legacy path can reach turns this 201 into a 500.
#[tokio::test]
async fn a_create_without_a_first_message_still_succeeds_during_a_daemon_outage() {
    let b = boot_without_daemon().await;
    let (status, body) = b.create_track(None, None).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    assert_eq!(b.track_count().await, 1);
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
            Some("idem-recipe-first-message"),
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
            Some("idem-recipe-ghost"),
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
