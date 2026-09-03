//! #1110 S1 — kernel view/template overlay on a track.
//!
//! A template track is a regular track plus
//! `(plugin_id=kernel, entity_kind=view, kind=template)`. Create with
//! `as_template: true` upserts that overlay in the same tx as layout and
//! skips `planner-harness-start`. Lists hide templates; detail and overlay
//! list still expose them. Planner start / `/planner/reset` refuse with a 4xx
//! that names the overlay.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewArea, NewOverlay};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use calm_server::validation::{
    OVERLAY_TEMPLATE_ENTITY_KIND, OVERLAY_TEMPLATE_KIND, OVERLAY_TEMPLATE_PLUGIN_ID,
    template_overlay_payload,
};
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
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let area = repo
        .area_create(NewArea {
            name: "template-overlay-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            proc_supervisor_sock: None,
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-1110-s1"),
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
    let shared = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let state = state.with_shared_codex_appserver(shared);
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);
    Boot {
        app,
        area_id: area.id.to_string(),
        repo,
        _tmp: tmp,
    }
}

fn theme() -> Value {
    json!({"fg": [216, 219, 226], "bg": [15, 20, 24]})
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

async fn get(app: axum::Router, uri: &str) -> (StatusCode, Value) {
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
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn operation_count(repo: &Arc<dyn Repo>, kind: &str) -> i64 {
    let pool = repo.sqlite_pool().expect("sqlite pool");
    sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE kind = ?1")
        .bind(kind)
        .fetch_one(&pool)
        .await
        .expect("operations count")
}

async fn planner_harness_ops_for_track(repo: &Arc<dyn Repo>, track_id: &str) -> i64 {
    let pool = repo.sqlite_pool().expect("sqlite pool");
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM operations \
         WHERE kind = 'planner-harness-start' \
           AND json_extract(payload_json, '$.wave_id') = ?1",
    )
    .bind(track_id)
    .fetch_one(&pool)
    .await
    .expect("track planner-harness-start count")
}

fn create_body(area_id: &str, title: &str, cwd: &str, as_template: Option<bool>) -> Value {
    let mut body = json!({
        "area_id": area_id,
        "title": title,
        "cwd": cwd,
        "attach_folder": true,
        "theme": theme(),
    });
    if let Some(as_template) = as_template {
        body["as_template"] = json!(as_template);
    }
    body
}

#[tokio::test]
async fn post_api_tracks_as_template_writes_overlay_and_skips_planner_harness_start() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        create_body(
            &boot.area_id,
            "template track",
            &attached_repo_fixture("1110-s1-template"),
            Some(true),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track_id = body["id"].as_str().expect("track id");

    let overlays = boot
        .repo
        .overlays_for("view", track_id)
        .await
        .expect("overlays");
    let template = overlays
        .iter()
        .find(|overlay| overlay.plugin_id == "kernel" && overlay.kind == "template")
        .expect("template overlay row");
    assert_eq!(template.entity_kind, "view");
    assert_eq!(template.payload, json!({ "schemaVersion": 1 }));
    assert!(
        overlays.iter().any(|overlay| overlay.kind == "layout"),
        "layout overlay still minted"
    );

    let cards = boot.repo.cards_by_track(track_id).await.unwrap();
    assert!(
        cards.iter().any(|card| card.kind == "codex"),
        "planner card still minted"
    );
    assert!(
        cards.iter().any(|card| card.kind == "track-report"),
        "report card still minted"
    );

    assert_eq!(
        planner_harness_ops_for_track(&boot.repo, track_id).await,
        0,
        "as_template must not enqueue planner-harness-start"
    );
    assert_eq!(
        operation_count(&boot.repo, "planner-harness-start").await,
        0
    );
}

#[tokio::test]
async fn post_api_tracks_omitted_as_template_still_starts_planner_harness() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        create_body(
            &boot.area_id,
            "ordinary track",
            &attached_repo_fixture("1110-s1-ordinary"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track_id = body["id"].as_str().expect("track id");
    let overlays = boot
        .repo
        .overlays_for("view", track_id)
        .await
        .expect("overlays");
    assert!(
        overlays.iter().all(|overlay| overlay.kind != "template"),
        "omitted as_template must not write the template overlay"
    );
    assert!(
        planner_harness_ops_for_track(&boot.repo, track_id).await >= 1,
        "today's create still enqueues planner-harness-start"
    );
}

#[tokio::test]
async fn template_tracks_are_hidden_from_lists_and_visible_by_id() {
    let boot = boot().await;
    let (status, template) = post(
        boot.app.clone(),
        "/api/tracks",
        create_body(
            &boot.area_id,
            "hidden template",
            &attached_repo_fixture("1110-s1-hidden"),
            Some(true),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={template}");
    let template_id = template["id"].as_str().unwrap().to_string();

    let (status, ordinary) = post(
        boot.app.clone(),
        "/api/tracks",
        create_body(
            &boot.area_id,
            "visible ordinary",
            &attached_repo_fixture("1110-s1-visible"),
            Some(false),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={ordinary}");
    let ordinary_id = ordinary["id"].as_str().unwrap().to_string();

    for uri in [
        format!("/api/areas/{}/tracks", boot.area_id),
        "/api/tracks".to_string(),
        format!("/api/tracks?area_id={}", boot.area_id),
    ] {
        let (status, body) = get(boot.app.clone(), &uri).await;
        assert_eq!(status, StatusCode::OK, "{uri} body={body}");
        let ids: Vec<&str> = body
            .as_array()
            .unwrap()
            .iter()
            .map(|track| track["id"].as_str().unwrap())
            .collect();
        assert!(
            ids.contains(&ordinary_id.as_str()),
            "{uri}: ordinary track missing; ids={ids:?}"
        );
        assert!(
            !ids.contains(&template_id.as_str()),
            "{uri}: template leaked; ids={ids:?}"
        );
    }

    let (status, detail) = get(boot.app.clone(), &format!("/api/tracks/{template_id}")).await;
    assert_eq!(status, StatusCode::OK, "detail body={detail}");
    assert_eq!(detail["track"]["id"], template_id);

    let (status, overlays) = get(boot.app.clone(), "/api/overlays?entity_kind=view").await;
    assert_eq!(status, StatusCode::OK, "overlays body={overlays}");
    let found = overlays.as_array().unwrap().iter().any(|overlay| {
        overlay["plugin_id"] == "kernel"
            && overlay["kind"] == "template"
            && overlay["entity_id"] == template_id
    });
    assert!(
        found,
        "template overlay must be discoverable via GET /api/overlays?entity_kind=view; body={overlays}"
    );
}

#[tokio::test]
async fn planner_reset_on_template_track_is_refused() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        create_body(
            &boot.area_id,
            "template reset",
            &attached_repo_fixture("1110-s1-reset"),
            Some(true),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track_id = body["id"].as_str().unwrap();
    let (status, detail) = get(boot.app.clone(), &format!("/api/tracks/{track_id}")).await;
    assert_eq!(status, StatusCode::OK);
    let planner_id = detail["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["kind"] == "codex")
        .and_then(|card| card["id"].as_str())
        .expect("planner card")
        .to_string();

    let (status, body) = post(
        boot.app.clone(),
        &format!("/api/cards/{planner_id}/planner/reset"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    let error = body["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("template overlay"),
        "4xx must name the template overlay; error={error}"
    );
    assert_eq!(planner_harness_ops_for_track(&boot.repo, track_id).await, 0);
}

/// #1297: marking an *existing* track as a template is a kernel-internal
/// operation. It used to be reachable by POSTing the overlay through
/// `/api/overlays`, which is the same door an attacker would use to strand
/// a running track (template tracks are held out of dispatch and out of
/// `GET /api/tracks`), so the public route now refuses it.
///
/// The downstream behaviour that POST was standing in for — a template track
/// refuses `/planner/reset` — is still proven, from the kernel-internal write
/// that production actually uses.
#[tokio::test]
async fn overlay_post_cannot_mark_an_existing_track_as_template() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        create_body(
            &boot.area_id,
            "live track",
            &attached_repo_fixture("1297-forge"),
            Some(false),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track_id = body["id"].as_str().unwrap().to_string();

    let (status, err) = post(
        boot.app.clone(),
        "/api/overlays",
        json!({
            "plugin_id": "kernel",
            "entity_kind": "view",
            "entity_id": track_id,
            "kind": "template",
            "payload": { "schemaVersion": 1 }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={err}");

    // The track is untouched: no template row landed, and it stays visible.
    // This is the half that matters — a 403 that still wrote the row would
    // be worse than no gate at all.
    //
    // Scoped deliberately: this asserts the row is absent and the track is
    // still listed, NOT that the scheduler still dispatches it. Dispatch
    // admission is exercised against a real scheduler in
    // `scheduler::inv_1110_001_template_track_does_not_dispatch`; claiming it
    // here off a list-visibility check would be asserting more than the test
    // runs.
    // Scoped to `kind == template`: the kernel writes its own `layout` row
    // under the same `view` namespace when it builds the track, so an
    // "empty overlay list" assertion here would be asserting something
    // false about a different row.
    let overlays = boot
        .repo
        .overlays_for(OVERLAY_TEMPLATE_ENTITY_KIND, &track_id)
        .await
        .expect("overlays");
    assert!(
        !overlays.iter().any(|o| o.kind == OVERLAY_TEMPLATE_KIND),
        "refused write must not land: {overlays:?}"
    );
    let (status, list) = get(boot.app.clone(), "/api/tracks").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|w| w["id"] == track_id.as_str()),
        "track must remain listed; list={list}"
    );
}

#[tokio::test]
async fn template_overlay_written_internally_blocks_planner_reset() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        create_body(
            &boot.area_id,
            "later template",
            &attached_repo_fixture("1110-s1-later"),
            Some(false),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track_id = body["id"].as_str().unwrap().to_string();
    assert!(planner_harness_ops_for_track(&boot.repo, &track_id).await >= 1);

    boot.repo
        .overlay_upsert(NewOverlay {
            plugin_id: OVERLAY_TEMPLATE_PLUGIN_ID.into(),
            entity_kind: OVERLAY_TEMPLATE_ENTITY_KIND.into(),
            entity_id: track_id.clone(),
            kind: OVERLAY_TEMPLATE_KIND.into(),
            payload: template_overlay_payload(),
        })
        .await
        .expect("mark as template");

    let (status, detail) = get(boot.app.clone(), &format!("/api/tracks/{track_id}")).await;
    assert_eq!(status, StatusCode::OK);
    let planner_id = detail["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["kind"] == "codex")
        .and_then(|card| card["id"].as_str())
        .expect("planner card")
        .to_string();
    let (status, body) = post(
        boot.app,
        &format!("/api/cards/{planner_id}/planner/reset"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("template overlay"),
        "body={body}"
    );
}
