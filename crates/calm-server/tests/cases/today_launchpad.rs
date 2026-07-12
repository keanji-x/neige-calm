#![cfg(unix)]

use std::{path::PathBuf, sync::Arc};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use calm_server::{
    card_role_cache::CardRoleCache,
    db::{Repo, RepoOutOfDomain, sqlite::SqlxRepo},
    event::EventBus,
    plugin_host::{PluginHost, PluginRegistry},
    routes,
    shared_codex_appserver::SharedCodexAppServer,
    state::{AppState, CodexClient, DaemonClient, WriteContext},
    wave_cove_cache::WaveCoveCache,
};
use http_body_util::BodyExt;
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let roles = CardRoleCache::new();
    let waves = WaveCoveCache::new();
    let events = EventBus::new();
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().join("data"),
        proc_supervisor_sock: None,
    });
    std::fs::create_dir_all(&daemon.data_dir).unwrap();
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        repo_dyn.clone(),
        PathBuf::new(),
        tmp.path().join("plugins-data"),
        Vec::new(),
        events.clone(),
        WriteContext::new(roles.clone(), waves.clone()),
    ));
    let state = AppState::from_parts(
        repo_dyn,
        events,
        daemon,
        plugin,
        Arc::new(CodexClient::new_stub()),
        Some(roles),
        Some(waves),
    )
    .with_shared_codex_appserver(SharedCodexAppServer::new_fake_running_with_pending(
        repo.clone(),
        None,
    ));
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

async fn ensure(app: axum::Router) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::post("/api/today/launchpad/ensure")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn first_ensure_mints_launchpad_with_all_cards_and_idle_spec() {
    let b = boot().await;
    let (status, body) = ensure(b.app.clone()).await;
    let operation_error: Option<String> =
        sqlx::query_scalar("SELECT last_error FROM operations ORDER BY rowid DESC LIMIT 1")
            .fetch_optional(b.repo.pool())
            .await
            .unwrap()
            .flatten();
    assert_eq!(
        status,
        StatusCode::CREATED,
        "body={body}, operation_error={operation_error:?}"
    );
    for key in ["wave_id", "spec_card_id", "terminal_card_id", "terminal_id"] {
        assert!(
            body[key].as_str().is_some_and(|id| !id.is_empty()),
            "missing {key}: {body}"
        );
    }
    let pool = b.repo.pool();
    let purpose: String = sqlx::query_scalar("SELECT purpose FROM waves WHERE id=?1")
        .bind(body["wave_id"].as_str().unwrap())
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(purpose, "launchpad");
    let kinds: Vec<String> =
        sqlx::query_scalar("SELECT kind FROM cards WHERE wave_id=?1 ORDER BY kind")
            .bind(body["wave_id"].as_str().unwrap())
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(kinds, ["codex", "terminal", "wave-report"]);
    let payload: String = sqlx::query_scalar("SELECT payload FROM cards WHERE id=?1")
        .bind(body["spec_card_id"].as_str().unwrap())
        .fetch_one(pool)
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["harness"]["pendingQueue"], serde_json::json!([]));
    assert!(payload["harness"].get("goal").is_none());
}

#[tokio::test]
async fn repeated_ensure_preserves_spec_transcript_and_ids_and_singleton() {
    let b = boot().await;
    let (first_status, first) = ensure(b.app.clone()).await;
    assert_eq!(first_status, StatusCode::CREATED);
    b.repo
        .harness_item_insert(
            "runtime",
            first["spec_card_id"].as_str().unwrap(),
            first["wave_id"].as_str().unwrap(),
            "thread",
            Some("turn"),
            Some("item"),
            Some("agent_message"),
            "item/completed",
            "{}",
        )
        .await
        .unwrap();
    let (second_status, second) = ensure(b.app.clone()).await;
    assert_eq!(second_status, StatusCode::OK, "body={second}");
    assert_eq!(second, first);
    let items: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM harness_items WHERE card_id=?1")
        .bind(first["spec_card_id"].as_str().unwrap())
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(items, 1);
    let launchpads: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM waves WHERE purpose='launchpad'")
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(launchpads, 1);
}

#[tokio::test]
async fn legacy_today_adoption_resets_spec_transcript_and_preserves_terminal() {
    let b = boot().await;
    let (_, original) = ensure(b.app.clone()).await;
    sqlx::query("UPDATE waves SET purpose=NULL WHERE id=?1")
        .bind(original["wave_id"].as_str().unwrap())
        .execute(b.repo.pool())
        .await
        .unwrap();
    b.repo
        .harness_item_insert(
            "legacy-runtime",
            original["spec_card_id"].as_str().unwrap(),
            original["wave_id"].as_str().unwrap(),
            "legacy-thread",
            None,
            Some("legacy-item"),
            Some("agent_message"),
            "item/completed",
            "{}",
        )
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET payload=?2 WHERE id=?1")
        .bind(original["spec_card_id"].as_str().unwrap())
        .bind(r#"{"schemaVersion":1,"harness":{"snapshotVersion":9,"pendingQueue":["legacy"]}}"#)
        .execute(b.repo.pool())
        .await
        .unwrap();

    let (status, adopted) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={adopted}");
    assert_eq!(adopted, original);
    let items: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM harness_items WHERE card_id=?1")
        .bind(original["spec_card_id"].as_str().unwrap())
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(items, 0);
    let purpose: String = sqlx::query_scalar("SELECT purpose FROM waves WHERE id=?1")
        .bind(original["wave_id"].as_str().unwrap())
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(purpose, "launchpad");
    assert_eq!(adopted["terminal_card_id"], original["terminal_card_id"]);
    assert_eq!(adopted["terminal_id"], original["terminal_id"]);
}
