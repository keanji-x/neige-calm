#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewCove, new_id, now_ms};
use calm_server::operation::forge_action_adapter::{FORGE_ACTION_KIND, ForgeActionPayload};
use calm_server::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Map, Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

mod common;

struct Boot {
    app: axum::Router,
    cove_id: String,
    repo: Arc<dyn Repo>,
    tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "forge-fence-test".into(),
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
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        events,
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-forge-fence-test"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
        )),
        Arc::new(common::fake_codex_client()),
        Some(card_role_cache),
        Some(wave_cove_cache),
    );

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    Boot {
        app,
        cove_id: cove.id.to_string(),
        repo,
        tmp,
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
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn delete(app: axum::Router, uri: &str) -> StatusCode {
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    resp.status()
}

async fn create_wave(boot: &Boot, title: &str) -> String {
    let cwd = boot.tmp.path().join(format!("cwd-{title}"));
    std::fs::create_dir_all(&cwd).expect("wave cwd");
    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": boot.cove_id.clone(),
            "title": title,
            "cwd": cwd.to_string_lossy().to_string(),
            "attach_folder": true,
            "theme": {"fg": [216, 219, 226], "bg": [15, 20, 24]}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "wave create body: {body}");
    body["id"].as_str().expect("wave id").to_string()
}

async fn insert_spawn_started_forge_action(boot: &Boot, wave_id: &str, idem: &str) -> String {
    let card_id = boot
        .repo
        .cards_by_wave(wave_id)
        .await
        .expect("cards by wave")
        .into_iter()
        .next()
        .expect("wave has a card")
        .id
        .to_string();
    let payload = ForgeActionPayload {
        wave_id: wave_id.to_string(),
        card_id,
        subject: None,
        argv: vec!["/bin/true".into()],
        idem_key: idem.into(),
        event_spec: None,
        context: Map::new(),
        probe: None,
        cwd_lease: boot.tmp.path().join(format!("{idem}.cwd")),
        result_path: boot.tmp.path().join(format!("{idem}.result.json")),
        deadline_ms: now_ms() + 60_000,
    };
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    let operation_repo = SqlxOperationRepo::new(pool.clone());
    let op_id = operation_repo
        .insert_operation(
            FORGE_ACTION_KIND,
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(idem.into()),
                payload_hash: format!("forge-fence-test:{idem}"),
            },
            serde_json::to_value(payload).expect("payload json"),
        )
        .await
        .expect("insert forge action");
    sqlx::query("UPDATE operations SET phase = 'spawn_started' WHERE id = ?1")
        .bind(&op_id)
        .execute(&pool)
        .await
        .expect("mark forge action active");
    op_id
}

async fn set_operation_phase(boot: &Boot, op_id: &str, phase: &str) {
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    sqlx::query("UPDATE operations SET phase = ?1 WHERE id = ?2")
        .bind(phase)
        .bind(op_id)
        .execute(&pool)
        .await
        .expect("set operation phase");
}

#[tokio::test]
async fn delete_wave_conflicts_while_forge_action_active_then_allows_terminal_phase() {
    let boot = boot().await;
    let wave_id = create_wave(&boot, "wave-active").await;
    let op_id = insert_spawn_started_forge_action(&boot, &wave_id, "wave-active-op").await;

    let status = delete(boot.app.clone(), &format!("/api/waves/{wave_id}")).await;
    assert_eq!(status, StatusCode::CONFLICT);

    set_operation_phase(&boot, &op_id, "succeeded").await;
    let status = delete(boot.app.clone(), &format!("/api/waves/{wave_id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn delete_cove_conflicts_when_child_wave_has_active_forge_action() {
    let boot = boot().await;
    let wave_id = create_wave(&boot, "cove-active").await;
    insert_spawn_started_forge_action(&boot, &wave_id, "cove-active-op").await;

    let status = delete(boot.app.clone(), &format!("/api/coves/{}", boot.cove_id)).await;
    assert_eq!(status, StatusCode::CONFLICT);
}
