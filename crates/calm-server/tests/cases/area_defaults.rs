//! Issue #1370 — Area-scoped New Track defaults through the real REST routes.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::NewTrack;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::routes::theme::RequestTheme;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.expect("open repo"));
    let erased: Arc<dyn Repo> = repo.clone();
    let events = EventBus::new();
    let roles = calm_server::card_role_cache::CardRoleCache::new();
    let tracks = calm_server::track_area_cache::TrackAreaCache::new();
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        erased.clone(),
        PathBuf::new(),
        tmp.path().join("plugins-data"),
        Vec::new(),
        events.clone(),
        WriteContext::new(roles.clone(), tracks.clone()),
    ));
    let state = AppState::from_parts(
        erased,
        events,
        Arc::new(DaemonClient::new_stub()),
        plugin,
        Arc::new(CodexClient::new_stub()),
        Some(roles),
        Some(tracks),
    );
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);
    Boot { app, repo, tmp }
}

fn init_git_worktree(path: &Path) {
    std::fs::create_dir_all(path).expect("create worktree dir");
    let output = Command::new("git")
        .args(["-c", "init.defaultBranch=main", "init"])
        .arg(path)
        .output()
        .expect("run git init");
    assert!(
        output.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn request(app: axum::Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("route response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn create_area(app: axum::Router, extra: Value) -> (StatusCode, Value) {
    let mut body = json!({"name": "Work", "color": "#123456"});
    body.as_object_mut()
        .expect("object")
        .extend(extra.as_object().expect("extra object").clone());
    request(app, Method::POST, "/api/areas", body).await
}

#[tokio::test]
async fn post_area_persists_and_returns_both_defaults() {
    let boot = boot().await;
    let worktree = boot.tmp.path().join("repo");
    init_git_worktree(&worktree);
    let with_slash = format!("{}/", worktree.display());

    let (status, body) = create_area(
        boot.app,
        json!({
            "default_template_id": "small-change",
            "default_cwd": with_slash,
        }),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["default_template_id"], "small-change");
    assert_eq!(body["default_cwd"], worktree.to_string_lossy().as_ref());
    let stored = boot
        .repo
        .area_get(body["id"].as_str().expect("Area id"))
        .await
        .expect("read Area")
        .expect("Area exists");
    assert_eq!(stored.default_template_id.as_deref(), Some("small-change"));
    assert_eq!(stored.default_cwd.as_deref(), worktree.to_str());
}

#[tokio::test]
async fn patch_defaults_distinguishes_omitted_null_and_value() {
    let boot = boot().await;
    let first = boot.tmp.path().join("first");
    let second = boot.tmp.path().join("second");
    init_git_worktree(&first);
    init_git_worktree(&second);
    let (status, created) = create_area(
        boot.app.clone(),
        json!({
            "default_template_id": "small-change",
            "default_cwd": first,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let uri = format!("/api/areas/{}", created["id"].as_str().unwrap());

    let (status, name_only) = request(
        boot.app.clone(),
        Method::PATCH,
        &uri,
        json!({"name": "Renamed"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{name_only}");
    assert_eq!(name_only["default_template_id"], "small-change");
    assert_eq!(name_only["default_cwd"], first.to_string_lossy().as_ref());

    let (status, changed) = request(
        boot.app.clone(),
        Method::PATCH,
        &uri,
        json!({"default_template_id": "investigation", "default_cwd": second}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(changed["default_template_id"], "investigation");
    assert_eq!(changed["default_cwd"], second.to_string_lossy().as_ref());

    let (status, cleared) = request(
        boot.app,
        Method::PATCH,
        &uri,
        json!({"default_template_id": null, "default_cwd": null}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert!(cleared["default_template_id"].is_null(), "{cleared}");
    assert!(cleared["default_cwd"].is_null(), "{cleared}");
}

#[tokio::test]
async fn invalid_defaults_are_rejected_before_an_area_or_event_is_written() {
    for extra in [
        json!({"default_template_id": "not-a-template"}),
        json!({"default_cwd": "/definitely/missing/neige-calm-area-default"}),
    ] {
        let boot = boot().await;
        let (status, body) = create_area(boot.app, extra).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let areas: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM areas")
            .fetch_one(boot.repo.pool())
            .await
            .expect("count Areas");
        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
            .fetch_one(boot.repo.pool())
            .await
            .expect("count events");
        assert_eq!(
            (areas, events),
            (0, 0),
            "a refused default must be side-effect free"
        );
    }
}

#[tokio::test]
async fn an_invalid_patch_preserves_the_existing_area_defaults() {
    let boot = boot().await;
    let worktree = boot.tmp.path().join("repo");
    init_git_worktree(&worktree);
    let (status, created) = create_area(
        boot.app.clone(),
        json!({"default_template_id": "small-change", "default_cwd": worktree}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let area_id = created["id"].as_str().expect("Area id");
    let uri = format!("/api/areas/{area_id}");

    let (status, body) = request(
        boot.app,
        Method::PATCH,
        &uri,
        json!({"name": "Must not land", "default_template_id": "unknown"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let stored = boot
        .repo
        .area_get(area_id)
        .await
        .expect("read Area")
        .expect("Area exists");
    assert_eq!(stored.name, "Work");
    assert_eq!(stored.default_template_id.as_deref(), Some("small-change"));
    assert_eq!(stored.default_cwd.as_deref(), worktree.to_str());
}

#[tokio::test]
async fn patch_checks_area_existence_before_validating_defaults() {
    let boot = boot().await;
    let (status, body) = request(
        boot.app,
        Method::PATCH,
        "/api/areas/missing",
        json!({
            "default_template_id": "unknown",
            "default_cwd": "/definitely/missing/neige-calm-area-default",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn changing_area_defaults_never_rewrites_existing_tracks() {
    let boot = boot().await;
    let worktree = boot.tmp.path().join("repo");
    init_git_worktree(&worktree);
    let (status, created) = create_area(boot.app.clone(), json!({})).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let area_id = created["id"].as_str().expect("Area id");
    let existing = boot
        .repo
        .track_create(NewTrack {
            area_id: area_id.to_owned().into(),
            title: "Existing".into(),
            sort: Some(1.0),
            cwd: "/existing/worktree".into(),
            template_id: Some("investigation".into()),
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("create existing Track through the production writer");

    let uri = format!("/api/areas/{area_id}");
    let (status, body) = request(
        boot.app,
        Method::PATCH,
        &uri,
        json!({"default_template_id": "small-change", "default_cwd": worktree}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let row: (Option<String>, String) =
        sqlx::query_as("SELECT template_id, workspace_path FROM tracks WHERE id = ?1")
            .bind(existing.id.as_str())
            .fetch_one(boot.repo.pool())
            .await
            .expect("read existing Track");
    assert_eq!(
        row,
        (Some("investigation".into()), "/existing/worktree".into())
    );
}
