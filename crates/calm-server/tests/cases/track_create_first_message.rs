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
//! 4. a create that promised delivery and could not deliver says so — a
//!    harness that fails to start turns the create into a 5xx instead of a
//!    201 that quietly dropped the sentence.
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
//! # What this slice deliberately does NOT promise
//!
//! Retry, replay and idempotency are **not** here, and their absence is a
//! decision rather than a gap. A create is not retryable today (`POST
//! /api/tracks` has never been), and making it retryable needs the key→track
//! binding to be persisted in the same transaction that mints the id — a
//! migration, and its own slice. So a client that repeats a create carrying a
//! `first_message` gets a second track, exactly as a client repeating any
//! other create always has, and nothing in this file asserts otherwise.

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
    #[allow(dead_code)]
    tmp: TempDir,
}

async fn boot() -> Boot {
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
    .with_shared_codex_appserver(SharedCodexAppServer::new_fake_running_with_pending(
        repo_dyn, None,
    ));
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
    /// `POST /api/tracks`. `first_message` is optional so one helper covers the
    /// legacy shape and the #1299 shape — the point of several tests below is
    /// that omitting it changes nothing.
    async fn create_track(&self, first_message: Option<&str>) -> (StatusCode, Value) {
        let mut body = json!({
            "area_id": self.area_id,
            "title": "",
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        });
        if let Some(text) = first_message {
            body["first_message"] = json!(text);
        }
        self.post_create(body).await
    }

    async fn post_create(&self, body: Value) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tracks")
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
    let (status, body) = b.create_track(Some("refactor the parser")).await;
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
    let (status, body) = b.create_track(Some("please rename the track")).await;
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
/// The operation payload must stay byte-identical in shape — no
/// `first_message` key at all, because `skip_serializing_if` is what keeps an
/// in-flight retry across a deploy from becoming a spurious payload-hash 409.
#[tokio::test]
async fn a_create_without_a_first_message_is_unchanged() {
    let b = boot().await;
    let (first, body) = b.create_track(None).await;
    assert_eq!(first, StatusCode::CREATED, "body={body}");
    let (second, _) = b.create_track(None).await;
    assert_eq!(second, StatusCode::CREATED);
    assert_eq!(b.track_count().await, 2);
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
    let (blank, body) = b.create_track(Some("   \n  ")).await;
    assert_eq!(blank, StatusCode::BAD_REQUEST, "body={body}");
    assert_eq!(b.track_count().await, 0);
    assert_eq!(b.card_count().await, 0);

    let too_long = "x".repeat(32_769);
    let (over, body) = b.create_track(Some(&too_long)).await;
    assert_eq!(over, StatusCode::BAD_REQUEST, "body={body}");
    assert_eq!(b.track_count().await, 0);
    assert_eq!(b.card_count().await, 0);

    // 32768 characters is the ceiling, counted in CHARACTERS not bytes — a
    // multi-byte string at the limit must be accepted.
    let at_limit = "é".repeat(32_768);
    let (ok, body) = b.create_track(Some(&at_limit)).await;
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
        .post_create(json!({
            "area_id": b.area_id,
            "title": "",
            "template_id": "small-change",
            "first_message": needle,
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        }))
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
        .post_create(json!({
            "area_id": b.area_id,
            "title": "",
            "recipe_id": recipe_id,
            "first_message": needle,
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        }))
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
        .post_create(json!({
            "area_id": b.area_id,
            "title": "",
            "recipe_id": "recipe-does-not-exist",
            "first_message": "this must not be delivered anywhere",
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        }))
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
// `OperationOutcome::Failed` branch. The other three branches are not reachable
// from a route test with an in-process runtime — they need the runtime itself
// to misbehave — but they are not separately implemented either: all four write
// one `harness_start_failed` binding that one `if` reads, so the branch under
// test is the same code the other three reach.
// ---------------------------------------------------------------------------

/// With a `first_message`, a harness that fails to start is a 5xx — and the
/// track is still there.
///
/// The second half is the part worth writing down. This is not a compensating
/// handler: by the time `start_planner_harness` runs, the create transaction
/// has committed and the workspace is materialized. The 5xx says "the message
/// was not delivered", not "nothing happened", and this test pins both halves
/// so nobody later reads the status as a rollback.
#[tokio::test]
async fn a_failed_harness_start_fails_a_create_that_carried_a_first_message() {
    let b = boot().await;
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();

    let (status, body) = b.create_track(Some("do not lose this sentence")).await;
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

    let (status, body) = b.create_track(None).await;
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
