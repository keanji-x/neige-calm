//! `POST /api/tracks` boots the planner card's codex app-server before returning 201; the boot is
//! non-fatal to track creation.

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
    // Empty seed is fine here: no tracks pre-exist and `track_create_tx` populates the cache write-through.
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
        // Point `codex_bin` at the fake app-server fixture so the boot succeeds without a real codex on PATH.
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

/// The planner-push app-server boot is NON-FATAL to track creation: with a deterministically-broken
/// `codex_bin` the route returns 201 with an inert track (no `codex_thread_id`, no shared source marker).
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

    // Deterministically-broken codex bin: absolute, absent.
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

    // (3) The planner is NOT running: those writes live AFTER the boot, on the success path only.
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

/// Track create must NOT stamp `payload.prompt` on the planner card: the title carries no intent (the agent
/// names the track later). The child-track path's seed is the task goal, not a title, and is untouched.
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
    // The title itself still round-trips onto the track row.
    assert_eq!(track.title, title);
}

/// The route rejects empty titles, but the planner-card seed path must defend against a whitespace title
/// too; the row is created via the repo because the route cannot carry a whitespace title.
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
    // The create may still 500 because the daemon child fails to exec `codex` in CI; both 201 and 500 leave
    // the card row behind, which is what is under test.
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

/// Track create persists `track.cwd` and uses the same path for the optional area folder claim:
/// `tracks.cwd` and `area_folders.path` must observe the same cwd at commit time.
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
    // Real daemon binary: the daemon binds its socket before exec'ing the inner `/bin/sh -c codex`.
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

/// After `POST /api/tracks` and walking the track to Done, the GET detail must surface `terminal_at = Some(_)`.
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

    // March the track through the happy path to Done via the repo (`track_update_tx`) so no PlannerAgent
    // actor is needed at the route boundary.
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
