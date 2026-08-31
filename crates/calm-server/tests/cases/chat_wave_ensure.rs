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
use calm_server::model::{NewCove, NewTerminal, NewWave, RequestTheme};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_cove_cache::WaveCoveCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    cove_id: String,
    repo: Arc<SqlxRepo>,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "chat-wave-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let events = EventBus::new();
    let roles = CardRoleCache::new();
    let waves = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&waves).await.unwrap();
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
            std::env::temp_dir().join("calm-chat-wave-test"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(roles.clone(), waves.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(roles),
        Some(waves),
    );
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
        cove_id: cove.id.to_string(),
        repo,
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

async fn ensure_chat_wave(b: &Boot) -> Value {
    b.repo
        .cove_folder_create(&b.cove_id, "/workspace")
        .await
        .unwrap();
    let (status, body) = post(
        b.app.clone(),
        format!("/api/coves/{}/chat-wave/ensure", b.cove_id),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    body
}

#[tokio::test]
async fn concurrent_ensure_creates_one_inert_structurally_complete_wave() {
    let b = boot().await;
    for path in ["/a/b/c", "/zzzzzzzzzzzz"] {
        let (status, _) = post(
            b.app.clone(),
            format!("/api/coves/{}/folders", b.cove_id),
            json!({"path": path}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    calm_server::routes::waves::install_chat_wave_ensure_barrier_for_test(&b.cove_id, barrier);
    let uri = format!("/api/coves/{}/chat-wave/ensure", b.cove_id);
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
    calm_server::routes::waves::remove_chat_wave_ensure_barrier_for_test(&b.cove_id);
    let mut statuses = [a.0, b_response.0];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::CREATED]);
    assert_eq!(a.1["id"], b_response.1["id"]);
    assert_eq!(a.1["cwd"], "/zzzzzzzzzzzz");
    assert_eq!(a.1["lifecycle"], "draft");
    let wave_id = a.1["id"].as_str().unwrap();

    let wave_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM waves WHERE cove_id=?1 AND purpose='cove-chat'")
            .bind(&b.cove_id)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(wave_count, 1);
    let spec_card_id: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE wave_id=?1 AND role='spec'")
            .bind(wave_id)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    let harness_items: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM harness_items WHERE card_id=?1")
            .bind(&spec_card_id)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions WHERE card_id=?1")
        .bind(&spec_card_id)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(harness_items, 0);
    assert_eq!(sessions, 0);
    let spec_start_operations: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE kind='spec-harness-start'")
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(spec_start_operations, 0);
    assert_eq!(
        get(b.app, format!("/api/waves/{wave_id}/report")).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn chat_wave_unique_index_and_error_matcher_are_pinned() {
    let b = boot().await;
    let insert = |id: &'static str| {
        sqlx::query(
            // #1147 S1 — `cwd` dropped from the column list. Seeding it here
            // without the matching `workspace_path` produced a row production
            // cannot produce (design D1 writes both from one value); this test
            // is about the `purpose='cove-chat'` unique index and never reads
            // either column, so the honest fix is to name neither.
            "INSERT INTO waves (id, cove_id, title, sort, lifecycle, purpose, created_at, updated_at) VALUES (?1, ?2, 'chat', 1, 'draft', 'cove-chat', 1, 1)",
        )
        .bind(id)
        .bind(&b.cove_id)
        .execute(b.repo.pool())
    };
    insert("chat-wave-winner").await.unwrap();
    let sqlx_error = insert("chat-wave-loser").await.unwrap_err();
    let error: calm_server::error::CalmError = sqlx_error.into();
    let message = error.to_string();
    assert!(
        message.contains("UNIQUE constraint failed: waves.cove_id"),
        "unexpected unique error: {message}"
    );
    assert!(calm_server::routes::waves::is_unique_constraint_for_test(
        &error,
        "waves.cove_id"
    ));
}

#[tokio::test]
async fn repeated_ensure_preserves_original_cwd_after_shallower_claim() {
    let b = boot().await;
    let (status, _) = post(
        b.app.clone(),
        format!("/api/coves/{}/folders", b.cove_id),
        json!({"path": "/original/deep/path"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let uri = format!("/api/coves/{}/chat-wave/ensure", b.cove_id);
    let (status, first) = post(b.app.clone(), uri.clone(), Value::Null).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = post(
        b.app.clone(),
        format!("/api/coves/{}/folders", b.cove_id),
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
async fn ensure_unknown_cove_returns_not_found() {
    let b = boot().await;
    let (status, body) = post(
        b.app,
        "/api/coves/unknown/chat-wave/ensure".into(),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.to_string().contains("cove unknown"), "{body}");
}

#[tokio::test]
async fn ensure_without_claimed_folder_is_actionable_and_creates_nothing() {
    let b = boot().await;
    let (status, body) = post(
        b.app,
        format!("/api/coves/{}/chat-wave/ensure", b.cove_id),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.to_string().contains("claim a folder"), "{body}");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM waves WHERE cove_id=?1")
        .bind(&b.cove_id)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

/// PATCH guard is paired: a chat lifecycle transition is forbidden before
/// the FSM/write, while the same Draft→Planning transition remains valid for
/// an ordinary wave.
#[tokio::test]
async fn patch_lifecycle_rejects_chat_but_allows_ordinary_wave() {
    let b = boot().await;
    let chat = ensure_chat_wave(&b).await;
    let chat_id = chat["id"].as_str().unwrap();
    let (status, body) = request_json(
        b.app.clone(),
        "PATCH",
        format!("/api/waves/{chat_id}"),
        json!({"lifecycle": "planning"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
    assert_eq!(
        b.repo
            .wave_get(chat_id)
            .await
            .unwrap()
            .unwrap()
            .lifecycle
            .as_db_str(),
        "draft"
    );

    let ordinary = b
        .repo
        .wave_create(NewWave {
            cove_id: b.cove_id.clone().into(),
            title: "ordinary".into(),
            sort: None,
            cwd: "/workspace".into(),
            workflow_id: None,
            plugin_scope: None,
            workflow_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let (status, body) = request_json(
        b.app.clone(),
        "PATCH",
        format!("/api/waves/{}", ordinary.id),
        json!({"lifecycle": "planning"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["lifecycle"], "planning");
}

/// INV-CHAT-005 backlink counterexample: resolving backlinks for the hidden
/// wave itself exercises the repository-wide wave title map and must stay 200.
#[tokio::test]
async fn report_backlinks_still_sees_hidden_chat_wave() {
    let b = boot().await;
    let chat = ensure_chat_wave(&b).await;
    assert_eq!(
        get(
            b.app,
            format!("/api/waves/{}/backlinks", chat["id"].as_str().unwrap())
        )
        .await,
        StatusCode::OK
    );
}

/// INV-CHAT-005 delete counterexample: the terminal's RESTRICT FK makes this
/// fail if delete_cove's repository enumeration ever hides the chat wave.
#[tokio::test]
async fn delete_cove_still_enumerates_hidden_chat_wave_for_terminal_teardown() {
    let b = boot().await;
    let chat = ensure_chat_wave(&b).await;
    let card_id: String = sqlx::query_scalar("SELECT id FROM cards WHERE wave_id = ?1 LIMIT 1")
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
        format!("/api/coves/{}", b.cove_id),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");
    assert!(b.repo.cove_get(&b.cove_id).await.unwrap().is_none());
}
