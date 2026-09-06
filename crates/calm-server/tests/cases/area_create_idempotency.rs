//! Issue #1500 A2 — retry Area creation at the production REST boundary.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    _tmp: TempDir,
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
    Boot {
        app,
        repo,
        _tmp: tmp,
    }
}

async fn post(app: axum::Router, key: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/api/areas")
        .header("content-type", "application/json");
    if let Some(key) = key {
        request = request.header("Idempotency-Key", key);
    }
    let response = app
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn area_create_idempotency_repeated_request_returns_one_area_and_event() {
    let boot = boot().await;
    let body = json!({"name":"Retry", "color":"#123456"});
    let (first_status, first) = post(boot.app.clone(), Some("repeat"), body.clone()).await;
    let (second_status, second) = post(boot.app, Some("repeat"), body).await;
    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(second_status, StatusCode::CREATED);
    assert_eq!(first["id"], second["id"]);
    assert_eq!(boot.repo.areas_list_user_visible().await.unwrap().len(), 1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE kind = 'area.updated'")
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn area_create_idempotency_concurrent_requests_commit_once() {
    let boot = boot().await;
    let body = json!({"name":"Concurrent", "color":"#123456"});
    let requests = (0..8).map(|_| post(boot.app.clone(), Some("concurrent"), body.clone()));
    let responses = futures::future::join_all(requests).await;
    let id = &responses[0].1["id"];
    for (status, area) in &responses {
        assert_eq!(*status, StatusCode::CREATED, "{area}");
        assert_eq!(&area["id"], id);
    }
    assert_eq!(boot.repo.areas_list_user_visible().await.unwrap().len(), 1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE kind = 'area.updated'")
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn area_create_idempotency_binds_every_input_but_allows_independent_same_names() {
    let boot = boot().await;
    let original = json!({"name":"Same name", "color":"#123456"});
    let (_, first) = post(boot.app.clone(), Some("shape"), original.clone()).await;
    for (field, value) in [
        ("name", json!("Changed")),
        ("color", json!("#abcdef")),
        ("sort", json!(7)),
        ("default_template_id", json!("small-change")),
        ("default_cwd", json!("/missing/path")),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        let (status, body) = post(boot.app.clone(), Some("shape"), changed).await;
        assert_eq!(status, StatusCode::CONFLICT, "{field}: {body}");
    }
    assert_eq!(boot.repo.areas_list_user_visible().await.unwrap().len(), 1);
    let explicit_nulls = json!({"name":"Same name", "color":"#123456", "sort":null,
        "default_template_id":null, "default_cwd":null, "kind":"system"});
    let (status, replay) = post(boot.app.clone(), Some("shape"), explicit_nulls).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(replay["id"], first["id"]);
    for key in [Some("independent"), None, None] {
        let (status, area) = post(boot.app.clone(), key, original.clone()).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_ne!(area["id"], first["id"]);
    }
    assert_eq!(boot.repo.areas_list_user_visible().await.unwrap().len(), 4);
}

#[tokio::test]
async fn area_create_idempotency_deleted_area_and_binding_tampering_fail_closed() {
    let boot = boot().await;
    let body = json!({"name":"Deleted", "color":"#123456"});
    let (_, first) = post(boot.app.clone(), Some("deleted"), body.clone()).await;
    let response = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/api/areas/{}", first["id"].as_str().unwrap()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let (status, error) = post(boot.app, Some("deleted"), body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert!(
        boot.repo
            .areas_list_user_visible()
            .await
            .unwrap()
            .is_empty()
    );
    for query in [
        "DELETE FROM area_create_idempotency",
        "UPDATE area_create_idempotency SET area_id = 'other'",
    ] {
        assert!(sqlx::query(query).execute(boot.repo.pool()).await.is_err());
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM area_create_idempotency")
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn area_create_idempotency_event_failure_rolls_back_area_and_binding() {
    let boot = boot().await;
    sqlx::query("CREATE TRIGGER test_reject_area_event BEFORE INSERT ON events WHEN NEW.kind = 'area.updated' BEGIN SELECT RAISE(ABORT, 'test event write failed'); END;")
        .execute(boot.repo.pool()).await.unwrap();
    let body = json!({"name":"Atomic", "color":"#123456", "default_template_id":"small-change"});
    let (status, _) = post(boot.app.clone(), Some("atomic"), body.clone()).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        boot.repo
            .areas_list_user_visible()
            .await
            .unwrap()
            .is_empty()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM area_create_idempotency")
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    sqlx::query("DROP TRIGGER test_reject_area_event")
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let (status, created) = post(boot.app.clone(), Some("atomic"), body.clone()).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["default_template_id"], "small-change");
    let (_, replay) = post(boot.app, Some("atomic"), body).await;
    assert_eq!(created["id"], replay["id"]);
}

#[tokio::test]
async fn area_create_idempotency_replays_current_row_after_original_folder_is_removed() {
    let boot = boot().await;
    let dir = boot._tmp.path().join("worktree");
    std::fs::create_dir(&dir).unwrap();
    let output = std::process::Command::new("git")
        .arg("init")
        .arg(&dir)
        .output()
        .unwrap();
    assert!(output.status.success());
    let body = json!({"name":"Folder", "color":"#123456", "default_cwd":dir});
    let (status, first) = post(boot.app.clone(), Some("folder"), body.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let updated = boot
        .repo
        .area_update(
            first["id"].as_str().unwrap(),
            calm_server::model::AreaPatch {
                name: Some("Renamed".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let (status, replay) = post(boot.app, Some("folder"), body).await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(replay["id"], first["id"]);
    assert_eq!(replay["name"], "Renamed");
    assert_eq!(replay["updated_at"], updated.updated_at);
}

#[tokio::test]
async fn area_create_idempotency_malformed_key_and_validation_leave_no_binding() {
    let boot = boot().await;
    let body = json!({"name":"Invalid", "color":"#123456"});
    let (status, _) = post(boot.app.clone(), Some("  "), body.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let mut invalid = body.clone();
    invalid["default_template_id"] = json!("missing-template");
    let (status, _) = post(boot.app.clone(), Some("valid"), invalid).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM area_create_idempotency")
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    let (status, _) = post(boot.app, Some("valid"), body).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn area_create_idempotency_version_advertises_safe_retry_capability() {
    let boot = boot().await;
    let response = boot
        .app
        .oneshot(
            Request::builder()
                .uri("/api/version")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["areaCreateIdempotency"], true);
}
