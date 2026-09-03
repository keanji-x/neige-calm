//! #1110 S4 — `tracks.plugin_scope` create-time copy; PATCH cannot change it.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewArea, NewTrack};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
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
            name: "plugin-scope-test".into(),
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
            std::env::temp_dir().join("calm-plugins-data-1110-s4"),
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

async fn json_request(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("X-Calm-Actor", "user");
    let request = if let Some(body) = body {
        builder.body(Body::from(body.to_string()))
    } else {
        builder.body(Body::empty())
    }
    .unwrap();
    let resp = app.oneshot(request).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn stored_plugin_scope(repo: &Arc<dyn Repo>, track_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT plugin_scope FROM tracks WHERE id = ?1")
        .bind(track_id)
        .fetch_one(&repo.sqlite_pool().expect("sqlite pool"))
        .await
        .expect("select plugin_scope")
}

#[tokio::test]
async fn unbound_create_leaves_plugin_scope_null() {
    let boot = boot().await;
    let (status, body) = json_request(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(json!({
            "area_id": boot.area_id,
            "title": "unbound plugin scope",
            "cwd": attached_repo_fixture("1110-s4-unbound"),
            "attach_folder": true,
            "theme": theme(),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    assert!(
        body["plugin_scope"].is_null(),
        "unbound create must serialize plugin_scope as null, body={body}"
    );
    let track_id = body["id"].as_str().expect("track id");
    assert_eq!(stored_plugin_scope(&boot.repo, track_id).await, None);
}

#[tokio::test]
async fn patch_plugin_scope_is_ignored_not_present() {
    let boot = boot().await;
    let track = boot
        .repo
        .track_create(NewTrack {
            area_id: boot.area_id.clone().into(),
            title: "scoped".into(),
            sort: None,
            cwd: "/tmp/1110-s4-patch".into(),
            template_id: Some("issue-development".into()),
            plugin_scope: Some("dev.neige.git-forge".into()),
            template_input: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("create scoped track");
    let track_id = track.id.to_string();
    assert_eq!(
        stored_plugin_scope(&boot.repo, &track_id).await.as_deref(),
        Some("dev.neige.git-forge")
    );

    let (status, body) = json_request(
        boot.app.clone(),
        "PATCH",
        &format!("/api/tracks/{track_id}"),
        Some(json!({
            "plugin_scope": "dev.neige.other",
            "title": "still scoped",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["title"], "still scoped");
    assert_eq!(body["plugin_scope"], "dev.neige.git-forge");
    assert_eq!(
        stored_plugin_scope(&boot.repo, &track_id).await.as_deref(),
        Some("dev.neige.git-forge"),
        "INV-1110-004: PATCH must not change plugin_scope"
    );
}

#[tokio::test]
async fn create_rejects_client_supplied_plugin_scope() {
    let boot = boot().await;
    let (status, body) = json_request(
        boot.app,
        "POST",
        "/api/tracks",
        Some(json!({
            "area_id": boot.area_id,
            "title": "client plugin_scope",
            "cwd": attached_repo_fixture("1110-s4-create-scope"),
            "attach_folder": true,
            "plugin_scope": "dev.neige.git-forge",
            "theme": theme(),
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "CreateTrackRequest deny_unknown_fields must reject plugin_scope, body={body}"
    );
}
