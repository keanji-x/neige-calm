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
use calm_server::model::{NewArea, NewTerminal, NewTrack, RequestTheme};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    area_id: String,
    repo: Arc<SqlxRepo>,
    workspace_root: PathBuf,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "chat-track-test".into(),
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
        events,
        Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            proc_supervisor_sock: None,
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo_dyn,
            PathBuf::new(),
            std::env::temp_dir().join("calm-chat-track-test"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(roles.clone(), tracks.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(roles),
        Some(tracks),
    )
    // #1147 S2 — D10's no-claim branch now allocates a managed workspace and
    // `git init`s it. Pin the root inside this test's TempDir so the
    // repositories land in the sandbox and vanish with it.
    .with_workspace_root(tmp.path().join("workspaces"));
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
        .with_state(state);
    Boot {
        app,
        area_id: area.id.to_string(),
        repo,
        workspace_root: tmp.path().join("workspaces"),
        _tmp: tmp,
    }
}

async fn post(app: axum::Router, uri: String, body: Value) -> (StatusCode, Value) {
    let response = app
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
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn get(app: axum::Router, uri: String) -> StatusCode {
    app.oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

async fn request_json(
    app: axum::Router,
    method: &str,
    uri: String,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
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

async fn ensure_chat_track(b: &Boot) -> Value {
    b.repo
        .area_folder_create(&b.area_id, "/workspace")
        .await
        .unwrap();
    let (status, body) = post(
        b.app.clone(),
        format!("/api/areas/{}/chat-track/ensure", b.area_id),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    body
}

#[tokio::test]
async fn concurrent_ensure_creates_one_inert_structurally_complete_track() {
    let b = boot().await;
    for path in ["/a/b/c", "/zzzzzzzzzzzz"] {
        let (status, _) = post(
            b.app.clone(),
            format!("/api/areas/{}/folders", b.area_id),
            json!({"path": path}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    calm_server::routes::tracks::install_chat_track_ensure_barrier_for_test(&b.area_id, barrier);
    let uri = format!("/api/areas/{}/chat-track/ensure", b.area_id);
    let (a, b_response) = tokio::join!(
        post(b.app.clone(), uri.clone(), Value::Null),
        post(b.app.clone(), uri, Value::Null)
    );
    assert!(
        matches!(a.0, StatusCode::OK | StatusCode::CREATED),
        "first ensure: {a:?}"
    );
    assert!(
        matches!(b_response.0, StatusCode::OK | StatusCode::CREATED),
        "second ensure: {b_response:?}"
    );
    calm_server::routes::tracks::remove_chat_track_ensure_barrier_for_test(&b.area_id);
    let mut statuses = [a.0, b_response.0];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::CREATED]);
    assert_eq!(a.1["id"], b_response.1["id"]);
    assert_eq!(a.1["cwd"], "/zzzzzzzzzzzz");
    assert_eq!(a.1["lifecycle"], "draft");
    let track_id = a.1["id"].as_str().unwrap();

    let track_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM tracks WHERE area_id=?1 AND purpose='area-chat'")
            .bind(&b.area_id)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(track_count, 1);
    let planner_card_id: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(track_id)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    let harness_items: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM harness_items WHERE card_id=?1")
            .bind(&planner_card_id)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions WHERE card_id=?1")
        .bind(&planner_card_id)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(harness_items, 0);
    assert_eq!(sessions, 0);
    let planner_start_operations: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE kind='planner-harness-start'")
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(planner_start_operations, 0);
    assert_eq!(
        get(b.app, format!("/api/tracks/{track_id}/report")).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn chat_track_unique_index_and_error_matcher_are_pinned() {
    let b = boot().await;
    let insert = |id: &'static str| {
        sqlx::query(
            // #1147 S1 — `cwd` dropped from the column list. Seeding it here
            // without the matching `workspace_path` produced a row production
            // cannot produce (design D1 writes both from one value); this test
            // is about the `purpose='area-chat'` unique index and never reads
            // either column, so the honest fix is to name neither.
            "INSERT INTO tracks (id, area_id, title, sort, lifecycle, purpose, created_at, updated_at) VALUES (?1, ?2, 'chat', 1, 'draft', 'area-chat', 1, 1)",
        )
        .bind(id)
        .bind(&b.area_id)
        .execute(b.repo.pool())
    };
    insert("chat-track-winner").await.unwrap();
    let sqlx_error = insert("chat-track-loser").await.unwrap_err();
    let error: calm_server::error::CalmError = sqlx_error.into();
    let message = error.to_string();
    assert!(
        message.contains("UNIQUE constraint failed: tracks.area_id"),
        "unexpected unique error: {message}"
    );
    assert!(calm_server::routes::tracks::is_unique_constraint_for_test(
        &error,
        "tracks.area_id"
    ));
}

#[tokio::test]
async fn repeated_ensure_preserves_original_cwd_after_shallower_claim() {
    let b = boot().await;
    let (status, _) = post(
        b.app.clone(),
        format!("/api/areas/{}/folders", b.area_id),
        json!({"path": "/original/deep/path"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let uri = format!("/api/areas/{}/chat-track/ensure", b.area_id);
    let (status, first) = post(b.app.clone(), uri.clone(), Value::Null).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = post(
        b.app.clone(),
        format!("/api/areas/{}/folders", b.area_id),
        json!({"path": "/shallow"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, second) = post(b.app, uri, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second["id"], first["id"]);
    assert_eq!(second["cwd"], "/original/deep/path");
}

#[tokio::test]
async fn ensure_unknown_area_returns_not_found() {
    let b = boot().await;
    let (status, body) = post(
        b.app,
        "/api/areas/unknown/chat-track/ensure".into(),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.to_string().contains("area unknown"), "{body}");
}

/// #1147 D10 — an area with no `area_folders` claim used to 409 here.
///
/// Since #1109 made areas pure namespaces, "no claim" is the normal state of
/// every new area, so that 409 meant `POST /api/areas/{id}/conversations`
/// (which calls this unconditionally) failed by definition for every new area.
/// The branch now allocates a managed workspace instead, and materializes it —
/// otherwise the conversation's first codex task dies with `spawn-failed`,
/// which is #1147 itself.
#[tokio::test]
async fn ensure_without_claimed_folder_allocates_a_managed_workspace() {
    let b = boot().await;
    let (status, body) = post(
        b.app,
        format!("/api/areas/{}/chat-track/ensure", b.area_id),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    let (kind, path, frozen): (String, String, Option<i64>) = sqlx::query_as(
        "SELECT workspace_kind, workspace_path, workspace_frozen_at FROM tracks WHERE area_id=?1",
    )
    .bind(&b.area_id)
    .fetch_one(b.repo.pool())
    .await
    .unwrap();
    assert_eq!(kind, "managed");
    assert_eq!(
        path,
        b.workspace_root
            .join(&b.area_id)
            .join(body["id"].as_str().unwrap())
            .to_string_lossy()
    );
    assert!(
        frozen.is_none(),
        "a freshly minted managed workspace must stay re-pointable until work happens (design D4)"
    );
    assert!(
        std::path::Path::new(&path).join(".git").is_dir(),
        "workspace {path} was not materialized"
    );
    assert!(
        std::process::Command::new("git")
            .arg("-C")
            .arg(&path)
            .args(["rev-parse", "--verify", "HEAD"])
            .output()
            .unwrap()
            .status
            .success(),
        "materialized workspace has no init commit; `git worktree add` would fail"
    );
}

/// An area that *does* have a claim keeps attached semantics: the user pointed
/// at that directory, so the server uses it and never `git init`s it.
#[tokio::test]
async fn ensure_with_a_claimed_folder_stays_attached() {
    let b = boot().await;
    let claimed = b._tmp.path().join("claimed");
    std::fs::create_dir_all(&claimed).unwrap();
    let claimed = claimed.to_string_lossy().into_owned();
    let (status, _) = post(
        b.app.clone(),
        format!("/api/areas/{}/folders", b.area_id),
        json!({"path": claimed}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = post(
        b.app,
        format!("/api/areas/{}/chat-track/ensure", b.area_id),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let (kind, path): (String, String) =
        sqlx::query_as("SELECT workspace_kind, workspace_path FROM tracks WHERE area_id=?1")
            .bind(&b.area_id)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(kind, "attached");
    assert_eq!(path, claimed);
    assert!(
        !std::path::Path::new(&claimed).join(".git").exists(),
        "the server `git init`-ed a directory the user pointed at"
    );
}

/// PATCH guard is paired: a chat lifecycle transition is forbidden before
/// the FSM/write, while the same Draft→Planning transition remains valid for
/// an ordinary track.
#[tokio::test]
async fn patch_lifecycle_rejects_chat_but_allows_ordinary_track() {
    let b = boot().await;
    let chat = ensure_chat_track(&b).await;
    let chat_id = chat["id"].as_str().unwrap();
    let (status, body) = request_json(
        b.app.clone(),
        "PATCH",
        format!("/api/tracks/{chat_id}"),
        json!({"lifecycle": "planning"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
    assert_eq!(
        b.repo
            .track_get(chat_id)
            .await
            .unwrap()
            .unwrap()
            .lifecycle
            .as_db_str(),
        "draft"
    );

    let ordinary = b
        .repo
        .track_create(NewTrack {
            area_id: b.area_id.clone().into(),
            title: "ordinary".into(),
            sort: None,
            cwd: "/workspace".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let (status, body) = request_json(
        b.app.clone(),
        "PATCH",
        format!("/api/tracks/{}", ordinary.id),
        json!({"lifecycle": "planning"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["lifecycle"], "planning");
}

/// INV-CHAT-005 backlink counterexample: resolving backlinks for the hidden
/// track itself exercises the repository-wide track title map and must stay 200.
#[tokio::test]
async fn report_backlinks_still_sees_hidden_chat_track() {
    let b = boot().await;
    let chat = ensure_chat_track(&b).await;
    assert_eq!(
        get(
            b.app,
            format!("/api/tracks/{}/backlinks", chat["id"].as_str().unwrap())
        )
        .await,
        StatusCode::OK
    );
}

/// INV-CHAT-005 delete counterexample: the terminal's RESTRICT FK makes this
/// fail if delete_area's repository enumeration ever hides the chat track.
#[tokio::test]
async fn delete_area_still_enumerates_hidden_chat_track_for_terminal_teardown() {
    let b = boot().await;
    let chat = ensure_chat_track(&b).await;
    let card_id: String = sqlx::query_scalar("SELECT id FROM cards WHERE track_id = ?1 LIMIT 1")
        .bind(chat["id"].as_str().unwrap())
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    b.repo
        .terminal_create(NewTerminal {
            card_id: card_id.into(),
            program: "/bin/true".into(),
            cwd: "/workspace".into(),
            env: json!({}),
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let (status, body) = request_json(
        b.app,
        "DELETE",
        format!("/api/areas/{}", b.area_id),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");
    assert!(b.repo.area_get(&b.area_id).await.unwrap().is_none());
}
