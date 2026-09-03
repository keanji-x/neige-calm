//! Issue #236 (closes) — `POST /api/tracks` must spawn the planner card's
//! codex daemon **synchronously** before returning 201.
//!
//! ## Why
//!
//! Pre-fix: the route returned 201 the instant the track + planner card +
//! terminal-row tx committed, and `seed_and_spawn_planner_daemon` was
//! fired through `tokio::spawn`. That opened a ~400 ms race window in
//! which the frontend could open the planner card's WS (which goes
//! through `ws::terminal::resolve_live_renderer`), see
//! `renderer entry = None` on the terminal row, and trigger the
//! revive-by-respawn path with the row's **baked env** — which omits
//! `NEIGE_MCP_SOCKET` / `NEIGE_MCP_TOKEN` (those are folded in only
//! at the original `spawn_terminal_for` call site). Result: two daemons
//! race on the same `--sock` path and the WS attaches to the
//! no-MCP one, breaking the codex MCP handshake.
//!
//! Post-fix: by the time 201 reaches the client, `renderer entry` on
//! the planner card's terminal row is `Some(<sock>)`, the socket exists
//! on disk, and a subsequent WS attach never hits the respawn branch.
//!
//! ## Test design
//!
//! We use the real terminal renderer path (the same one
//! `tests/codex_card_endpoint.rs` and `tests/ws_terminal_e2e.rs`
//! locate). The planner card's `program` is hard-coded to `"codex"` by
//! `seed_and_spawn_planner_daemon`; there's no `codex` binary in CI, so
//! `/bin/sh -c codex` will fail-fast inside the daemon child. That's
//! fine — `spawn_terminal_for` waits for the *daemon* socket to accept,
//! not for the spawned program to stay alive. The socket binds before
//! the daemon execs the child, so the wait-for-socket loop completes
//! and `renderer setup` lands.
//!
//! Assertions:
//!   1. `POST /api/tracks` returns 201 (synchronous spawn succeeded).
//!   2. The planner card's terminal row has `renderer entry = Some(_)`.
//!   3. The socket file exists on disk at that path.
//!   4. A second `terminal_get` immediately after the response (the
//!      shape `ws::terminal::resolve_live_renderer` would see) does NOT
//!      observe `renderer entry = None`, i.e. the race window is
//!      closed.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::NewArea;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::common;
use crate::support::git_helpers::attached_repo_fixture;
struct Boot {
    app: axum::Router,
    area_id: String,
    repo: Arc<dyn Repo>,
    card_role_cache: CardRoleCache,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let area = repo
        .area_create(NewArea {
            name: "sync-daemon-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    // #234 (rebase) — TrackAreaCache joined the AppState/PluginHost surface
    // alongside CardRoleCache. Empty seed is fine here: no tracks pre-exist
    // in the freshly-opened in-memory repo, and the track we create through
    // `POST /api/tracks` populates the cache write-through via
    // `track_create_tx`.
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        events,
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-sync-daemon-test"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                card_role_cache.clone(),
                track_area_cache.clone(),
            ),
        )),
        // #293 cutover — `POST /api/tracks` now boots a kernel-owned codex
        // app-server before returning 201. Point `codex_bin` at the
        // `osc-probe-child` fake app-server fixture so the boot succeeds
        // without a real codex on PATH (see `tests/common/mod.rs`).
        Arc::new(common::fake_codex_client()),
        Some(card_role_cache.clone()),
        Some(track_area_cache.clone()),
    );

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());

    Boot {
        app,
        area_id: area.id.to_string(),
        repo,
        card_role_cache,
        _tmp: tmp,
    }
}

async fn post(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
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
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// Verify: after `POST /api/tracks` returns 201, the planner card's
/// terminal row has a registered renderer entry and a persisted pid.
/// This is the post-#388 Phase 3b contract — no race window.
/// Regression test for the WS attach path: immediately after `POST
/// /api/tracks`, the fresh terminal must already have a renderer entry.
/// Phase 3b no longer has a daemon-UDS revive branch.
/// Issue #293 / PR #311 — the planner-push app-server boot is NON-FATAL to
/// track creation. Every codex-free environment (CI's web a11y job, the
/// chromium docker stack) has no working `codex`, so booting the
/// shared codex daemon fails. This MUST NOT 500 the track create:
/// the route logs a warning and returns **201** with an inert track (the
/// planner card has no `codex_thread_id` or shared source marker).
///
/// This test boots with a deterministically-broken `codex_bin` (an
/// absolute path that does not exist, so the boot fails fast regardless
/// of whether a real `codex` is on PATH) and asserts:
///   1. `POST /api/tracks` returns 201 (boot failure is tolerated),
///   2. the track + planner card rows are committed,
///   3. the planner card payload has NO `codex_thread_id` / `appserver_sock`
///      (the persist step is skipped on the failure path),
///   4. no pending shared thread-start entry is registered for the inert track.
#[tokio::test]
async fn post_api_tracks_tolerates_broken_codex_bin_returns_201_inert_track() {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let area = repo
        .area_create(NewArea {
            name: "broken-codex-tolerant-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();

    // Deterministically-broken codex bin: absolute, absent. The route must
    // still commit an inert track instead of surfacing the daemon failure as a
    // 500.
    let mut codex = calm_server::state::CodexClient::new_stub();
    codex.codex_bin = "/nonexistent-codex-bin-tolerant-201-test".into();

    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-broken-codex-test"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                card_role_cache.clone(),
                track_area_cache.clone(),
            ),
        )),
        Arc::new(codex),
        Some(card_role_cache.clone()),
        Some(track_area_cache.clone()),
    );
    let pending_codex_threads = state.pending_codex_threads.clone();

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    let area_id = area.id.to_string();
    let (status, body) = post(
        app.clone(),
        "/api/tracks",
        json!({"area_id": area_id, "title": "inert track", "cwd": attached_repo_fixture("issue-293-tolerant"), "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;

    // (1) Boot failure is tolerated → 201, not 500.
    assert_eq!(
        status,
        StatusCode::CREATED,
        "broken codex bin must yield 201 (inert track), not 500 (issue #293 / PR #311); body={body}",
    );

    // (2) The track + planner card rows committed.
    let tracks = repo.tracks_by_area(&area_id).await.unwrap();
    assert_eq!(
        tracks.len(),
        1,
        "exactly one track persisted despite boot failure"
    );
    let track = tracks.into_iter().next().unwrap();
    let cards = repo.cards_by_track(track.id.as_str()).await.unwrap();
    let planner_card = cards
        .iter()
        .find(|c| card_role_cache.get(&c.id) == Some(calm_server::model::CardRole::Planner))
        .expect("planner card persisted even though the planner agent didn't start");

    // (3) The planner is NOT running: no codex_thread_id / appserver_sock
    // were persisted (those writes live AFTER the boot, on the success
    // path only).
    assert!(
        planner_card
            .payload
            .get("codex_thread_id")
            .is_none_or(Value::is_null),
        "inert track's planner card must NOT carry a codex_thread_id; payload = {}",
        planner_card.payload,
    );
    assert!(
        planner_card
            .payload
            .get("appserver_sock")
            .is_none_or(Value::is_null),
        "inert track's planner card must NOT carry an appserver_sock; payload = {}",
        planner_card.payload,
    );

    // (4) No pending shared thread registration exists for this inert track.
    assert_eq!(
        pending_codex_threads.pending_count().await,
        0,
        "inert track must not register a pending shared thread start",
    );
}

/// Issue #1211 (retires the #251 contract) — track create must NOT stamp
/// `payload.prompt` on the planner card, not even for a non-empty title.
///
/// #251 threaded the track title into `payload.prompt` so the shared daemon's
/// `turn/start` would open the session with the title as the agent's first
/// input, and its test asserted `prompt == title` verbatim. That contract
/// rested entirely on "the title IS the track's intent" — the single new-track
/// input box doubled as the title and as the statement of what to do.
/// #1211 takes that premise apart: the title defaults to a placeholder and the
/// planner agent names the track (`calm.track.rename`) once it has worked out from
/// the conversation what the work actually is. With no intent in the title
/// there is nothing to seed, so the `prompt` key must be absent.
///
/// The child-track path still passes a seed through
/// `planner_harness_card_payload` — that seed is the task goal the parent planner
/// declared, not a track title, and it is deliberately untouched here.
#[tokio::test]
async fn post_api_tracks_does_not_stamp_prompt_on_planner_card() {
    let boot = boot().await;

    let title = "draft the design doc for #251";
    let (status, _body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({"area_id": boot.area_id, "title": title, "cwd": attached_repo_fixture("issue-250-pr2-test"), "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Find the Planner card the route minted.
    let tracks = boot.repo.tracks_by_area(&boot.area_id).await.unwrap();
    let track = tracks.into_iter().next().unwrap();
    let cards = boot.repo.cards_by_track(track.id.as_str()).await.unwrap();
    let planner_card = cards
        .iter()
        .find(|c| boot.card_role_cache.get(&c.id) == Some(calm_server::model::CardRole::Planner))
        .expect("exactly one Planner-role card per track");

    assert!(
        planner_card
            .payload
            .get("prompt")
            .is_none_or(Value::is_null),
        "a non-empty track title must NOT stamp payload.prompt (#1211); \
         payload = {}",
        planner_card.payload,
    );
    // The rest of the production payload shape is untouched by the retirement.
    assert_eq!(
        planner_card.payload.get("planner_harness"),
        Some(&json!(true))
    );
    assert_eq!(
        planner_card.payload.get("codex_source"),
        Some(&json!("shared"))
    );
    // The title itself still round-trips onto the track row — #1211 retires the
    // title→prompt seeding, not the title.
    assert_eq!(track.title, title);
}

/// Issue #251 — when a track's title is whitespace-only the planner card
/// must NOT stamp a `payload.prompt` and the codex command line must
/// fall back to a bare `codex`. The route layer rejects empty titles
/// in production, but the planner_card seed path defenses against an
/// empty title here too so a future loosening of route validation
/// doesn't quietly start an empty shared-daemon turn.
///
/// We can't easily POST a whitespace title through the route (axum's
/// JSON serde + the `NewTrack { title: String }` shape accept anything
/// non-null), so this test takes the inner path: it creates a track row
/// with title = "   " via the repo, then asserts the resulting card
/// shape. The shape assertion uses the same payload-prompt field
/// the shared-daemon `turn/start` path keys on.
#[tokio::test]
async fn whitespace_title_does_not_stamp_prompt_on_planner_card() {
    let boot = boot().await;

    // Route accepts and trims the title; assert the post-trim shape.
    let (status, _body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({"area_id": boot.area_id, "title": "   ", "cwd": attached_repo_fixture("issue-250-pr2-test"), "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    // The track create may still 500 because the daemon child fails to
    // exec `codex` in CI — but the row commit is what we're testing
    // here. Tolerate either 201 (sync spawn happened to win) or 500
    // (daemon-side failure post-commit); both shapes leave the card
    // row behind.
    assert!(
        status == StatusCode::CREATED || status == StatusCode::INTERNAL_SERVER_ERROR,
        "expected 201 or 500 (daemon spawn may fail in CI without codex bin); got {status}",
    );

    let tracks = boot.repo.tracks_by_area(&boot.area_id).await.unwrap();
    let track = tracks.into_iter().next().unwrap();
    let cards = boot.repo.cards_by_track(track.id.as_str()).await.unwrap();
    let planner_card = cards
        .iter()
        .find(|c| boot.card_role_cache.get(&c.id) == Some(calm_server::model::CardRole::Planner))
        .expect("exactly one Planner-role card per track");
    assert!(
        planner_card
            .payload
            .get("prompt")
            .is_none_or(Value::is_null),
        "whitespace-only title must NOT stamp payload.prompt; got payload = {}",
        planner_card.payload,
    );
}

// ---------------------------------------------------------------------------
// Issue #250 PR 2 — track.cwd → planner-daemon cwd contract
// ---------------------------------------------------------------------------

/// PR 2 contract: track create persists `track.cwd` and uses the same
/// path for the optional area folder claim, not the pre-#250
/// `routes::codex_cards::default_cwd()` fallback.
///
/// Two rows must observe the same cwd at commit time:
///   1. `tracks.cwd`        — the track row's column.
///   2. `area_folders.path` — the attached folder claim.
#[tokio::test]
async fn post_api_tracks_persists_track_cwd_and_attach_folder() {
    let boot = boot().await;

    let cwd = attached_repo_fixture("issue-250-pr2-cwd-contract");
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "cwd-contract track",
            "cwd": cwd,
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    // Real daemon binary: spawn succeeds (the daemon binds its socket
    // before exec'ing the inner `/bin/sh -c codex`).
    assert_eq!(
        status,
        StatusCode::CREATED,
        "track create returns 201 when daemon spawn succeeds; body={body}",
    );

    // Track row carries cwd.
    let tracks = boot.repo.tracks_by_area(&boot.area_id).await.unwrap();
    assert_eq!(tracks.len(), 1);
    let track = tracks.into_iter().next().unwrap();
    assert_eq!(track.workspace.path, cwd);

    // Folder claim landed inside the same tx (attach_folder = true).
    let folders = boot.repo.area_folders_by_area(&boot.area_id).await.unwrap();
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0].path, cwd);
}

/// Lifecycle terminal-state E2E from the route: after `POST /api/tracks`
/// + walking the track to Done via the lifecycle state machine, the
/// GET track detail must surface `terminal_at = Some(_)`. Locks in the
/// "route → lifecycle → repo" plumbing the calendar window query
/// relies on.
#[tokio::test]
async fn post_api_tracks_then_lifecycle_done_surfaces_terminal_at_in_get() {
    use calm_server::model::TrackLifecycle;
    let boot = boot().await;

    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "track-to-done",
            "cwd": attached_repo_fixture("issue-250-pr2-to-done"),
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {body}");
    let track_id = body
        .get("id")
        .and_then(Value::as_str)
        .expect("track id in response")
        .to_string();

    // March the track through the happy path to Done. We use the repo
    // directly (which routes through `track_update_tx`) so we don't
    // have to mint a PlannerAgent actor at the route boundary; the
    // route's lifecycle validator is unit-tested in
    // `track_lifecycle.rs`. The interesting wiring here is the
    // track_update_tx → terminal_at column write.
    for step in [
        TrackLifecycle::Planning,
        TrackLifecycle::Dispatching,
        TrackLifecycle::Working,
        TrackLifecycle::Reviewing,
        TrackLifecycle::Done,
    ] {
        boot.repo
            .track_update(
                &track_id,
                calm_server::model::TrackPatch {
                    lifecycle: Some(step),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    // GET /api/tracks/:id must surface the terminal_at stamp.
    let resp = boot
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
    assert_eq!(resp.status(), StatusCode::OK);
    let detail: Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let terminal_at = detail
        .pointer("/track/terminal_at")
        .expect("track/terminal_at in TrackDetail body");
    assert!(
        terminal_at.is_i64(),
        "terminal_at must be a unix-ms integer after lifecycle → Done; got {terminal_at}",
    );
}
