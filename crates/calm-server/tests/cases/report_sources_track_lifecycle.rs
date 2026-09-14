//! #1669 §2.4 / I3 — captured sources follow their track: a fork copies
//! every `report_sources` row into the child inside the create transaction
//! (ids and anchors verbatim); a capture after the fork stays with the
//! parent; deleting the parent leaves the child's copy intact; deleting a
//! track cascades its rows away and empties its slot in the transient ring.
//!
//! Same route boot as `cards_deletable.rs`: the fake codex app-server
//! fixture answers the track-create handshake, so `POST /api/tracks` (the
//! fork) and `DELETE /api/tracks/{id}` run the production routes.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{self, AuthConfig, AuthState, SESSION_COOKIE};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::mcp_server::registry::AppContext;
use calm_server::model::NewArea;
use calm_server::plugin_host::mcp::{CallToolResult, ContentBlock};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::report_sources::{NewSource, Origin, Provenance, Quote, store};
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
    ctx: Arc<AppContext>,
    cookie: String,
    area_id: String,
    repo: Arc<dyn Repo>,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.expect("sqlite"));
    let area = repo
        .area_create(NewArea {
            name: "sources-lifecycle".into(),
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
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();
    let write =
        calm_server::state::WriteContext::new(card_role_cache.clone(), track_area_cache.clone());
    let plugin_host = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        repo.clone(),
        PathBuf::new(),
        tmp.path().join("plugins-data"),
        Vec::new(),
        EventBus::new(),
        write.clone(),
    ));
    let plugin_host_cell = Arc::new(tokio::sync::OnceCell::new());
    assert!(plugin_host_cell.set(plugin_host.clone()).is_ok());
    // The route state's own context, so the ring the delete route empties
    // is the one this test fills.
    let ctx = AppContext::new(
        repo.clone(),
        events.clone(),
        write,
        None,
        plugin_host_cell,
        Arc::new(tokio::sync::OnceCell::new()),
        tmp.path().join("gate-logs"),
        calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
    );
    let state = AppState::from_parts(
        repo.clone(),
        events,
        daemon,
        plugin_host,
        Arc::new(common::fake_codex_client()),
        Some(card_role_cache),
        Some(track_area_cache),
    )
    .with_mcp_context(ctx.clone());
    let auth_state = AuthState::new(AuthConfig {
        username: Some("alice".into()),
        password: Some("hunter2".into()),
        dev_autologin: false,
        display_name: "alice".into(),
    });
    let protected = routes::protected_router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            auth_state.clone(),
            auth::require_session,
        ));
    let app = axum::Router::new()
        .merge(protected)
        .merge(routes::public_router())
        .with_state(state)
        .merge(auth::router().with_state(auth_state));
    let cookie = login(&app).await;
    Boot {
        app,
        ctx,
        cookie,
        area_id: area.id.to_string(),
        repo,
        _tmp: tmp,
    }
}

async fn login(app: &axum::Router) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"username":"alice", "password":"hunter2"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let raw = response.headers()[header::SET_COOKIE].to_str().unwrap();
    let cookie = raw.split(';').next().unwrap().to_string();
    assert!(cookie.starts_with(&format!("{SESSION_COOKIE}=")));
    cookie
}

async fn request(boot: &Boot, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, &boot.cookie);
    let body = match body {
        Some(body) => {
            request = request.header("content-type", "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let resp = boot
        .app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn create_track(boot: &Boot, title: &str, fork_from: Option<&str>) -> String {
    let mut body = json!({
        "area_id": boot.area_id,
        "title": title,
        "cwd": attached_repo_fixture(&format!("issue-1669-{title}")),
        "attach_folder": true,
        "theme": {"fg": [216,219,226], "bg": [15,20,24]},
    });
    if let Some(source) = fork_from {
        body["fork_report_from"] = json!(source);
    }
    let (status, created) = request(boot, "POST", "/api/tracks", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "create {title}: {created}");
    created["id"].as_str().unwrap().to_string()
}

/// Insert one source through the store, the way the tool does (one
/// `write_in_tx`), with a quote anchor.
async fn insert_source(boot: &Boot, track_id: &str, source_id: &str, body: &str) {
    let track = track_id.to_string();
    let row = NewSource {
        source_id: source_id.into(),
        provenance: Provenance::FullText,
        origin: Origin::Plugin {
            plugin_id: "dev.wisburg".into(),
            tool: "get-article-detail".into(),
            args_sha256: "ab".repeat(32),
            args_canon: "v1".into(),
            content_id: Some("752972".into()),
        },
        title: format!("title of {source_id}"),
        published_at: Some("2026-09-14".into()),
        body: body.into(),
        body_sha256: calm_server::report_sources::sha256_hex(body.as_bytes()),
        captured_at: 1_700_000_000_000,
        quotes: vec![Quote {
            id: "q1".into(),
            text: body[..4].into(),
            start: 0,
            end: 4,
        }],
    };
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move { store::insert_tx(tx, &track, &row).await })
    })
    .await
    .expect("insert source");
}

async fn sources_of(boot: &Boot, track_id: &str) -> Vec<Value> {
    let (status, list) = request(
        boot,
        "GET",
        &format!("/api/tracks/{track_id}/sources"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    list["sources"].as_array().expect("sources").clone()
}

async fn detail_of(boot: &Boot, track_id: &str, source_id: &str) -> (StatusCode, Value) {
    request(
        boot,
        "GET",
        &format!("/api/tracks/{track_id}/sources/{source_id}"),
        None,
    )
    .await
}

#[tokio::test]
async fn fork_copies_sources_verbatim_and_the_copies_outlive_the_parent() {
    let boot = boot().await;
    let parent = create_track(&boot, "parent", None).await;
    insert_source(&boot, &parent, "src_00000001", "alpha body").await;
    insert_source(&boot, &parent, "src_00000002", "beta body").await;
    let before = sources_of(&boot, &parent).await;
    assert_eq!(before.len(), 2);

    let child = create_track(&boot, "child", Some(&parent)).await;
    let copied = sources_of(&boot, &child).await;
    assert_eq!(
        copied, before,
        "every row travels, ids and anchors included"
    );
    let (status, detail) = detail_of(&boot, &child, "src_00000002").await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["body"], "beta body");
    assert_eq!(detail["quotes"][0]["id"], "q1");
    assert_eq!(detail["quotes"][0]["text"], "beta");

    // A capture after the fork stays with the parent.
    insert_source(&boot, &parent, "src_00000003", "gamma body").await;
    assert_eq!(sources_of(&boot, &parent).await.len(), 3);
    assert_eq!(sources_of(&boot, &child).await.len(), 2);
    let (status, _) = detail_of(&boot, &child, "src_00000003").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Deleting the parent cascades its rows and leaves the child whole.
    let (status, body) = request(&boot, "DELETE", &format!("/api/tracks/{parent}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(boot.repo.track_get(&parent).await.unwrap().is_none());
    let pool = boot.repo.sqlite_pool().unwrap();
    let parent_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM report_sources WHERE track_id = ?1")
            .bind(&parent)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(parent_rows, 0, "FK cascade removed the parent's rows");
    assert_eq!(sources_of(&boot, &child).await, before);
}

#[tokio::test]
async fn deleting_a_track_empties_its_slot_in_the_transient_ring() {
    let boot = boot().await;
    let track = create_track(&boot, "ring", None).await;
    let other = create_track(&boot, "ring-other", None).await;
    let result = CallToolResult {
        content: vec![ContentBlock {
            kind: "text".into(),
            text: Some("body".into()),
            extra: Default::default(),
        }],
        is_error: None,
        meta: None,
        structured_content: None,
    };
    boot.ctx
        .plugin_results
        .record(&track, "p", "tool", &json!({}), &result);
    boot.ctx
        .plugin_results
        .record(&other, "p", "tool", &json!({}), &result);
    assert_eq!(boot.ctx.plugin_results.len(&track), 1);

    let (status, body) = request(&boot, "DELETE", &format!("/api/tracks/{track}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(
        boot.ctx.plugin_results.is_empty(&track),
        "the deleted track's ring entries are gone"
    );
    assert_eq!(
        boot.ctx.plugin_results.len(&other),
        1,
        "another track's entries are untouched"
    );
}
