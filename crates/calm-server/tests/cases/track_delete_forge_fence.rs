#![cfg(unix)]

use calm_types::organization::LEGACY_OWNER_USER_ID;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewArea, new_id, now_ms};
use calm_server::operation::forge_action_adapter::{FORGE_ACTION_KIND, ForgeActionPayload};
use calm_server::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Map, Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::common;
use crate::support::git_helpers::attached_repo_fixture;

struct Boot {
    app: axum::Router,
    area_id: String,
    repo: Arc<dyn Repo>,
    tmp: TempDir,
    state: AppState,
    execution: crate::support::execution::Fixture,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let area = repo
        .area_create(
            LEGACY_OWNER_USER_ID,
            NewArea {
                name: "forge-fence-test".into(),
                color: "#000".into(),
                sort: None,
            },
        )
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
            calm_server::state::WriteContext::new(
                card_role_cache.clone(),
                track_area_cache.clone(),
            ),
        )),
        Arc::new(common::fake_codex_client()),
        Some(card_role_cache),
        Some(track_area_cache),
    );

    let execution =
        crate::support::execution::Fixture::new(repo.sqlite_db().unwrap().clone()).await;
    let app = routes::router()
        .layer(axum::middleware::from_fn_with_state(
            calm_server::auth::AuthState::new(calm_server::auth::AuthConfig {
                username: None,
                password: None,
                dev_autologin: true,
                display_name: "Owner".into(),
            }),
            calm_server::auth::require_session,
        ))
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());

    Boot {
        app,
        area_id: area.id.to_string(),
        repo,
        tmp,
        state,
        execution,
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

async fn create_track(boot: &Boot, title: &str) -> String {
    // An attached `cwd` must be a real Git work tree; these tests never look at the directory, so any real repository will do.
    let cwd = attached_repo_fixture(&format!("track-delete-forge-fence-{title}"));
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "planner_provider": "codex",
            "area_id": boot.area_id.clone(),
            "title": title,
            "cwd": cwd,
            "attach_folder": true,
            "theme": {"fg": [216, 219, 226], "bg": [15, 20, 24]}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "track create body: {body}");
    body["id"].as_str().expect("track id").to_string()
}

async fn insert_forge_action(
    boot: &Boot,
    track_id: &str,
    idem: &str,
    probe: Option<calm_server::operation::forge_action_adapter::ProbeSpec>,
) -> String {
    let card_id = boot
        .repo
        .cards_by_track(track_id)
        .await
        .expect("cards by track")
        .into_iter()
        .next()
        .expect("track has a card")
        .id
        .to_string();
    let payload = ForgeActionPayload {
        track_id: track_id.to_string(),
        card_id,
        subject: probe
            .as_ref()
            .map(|_| serde_json::from_value(json!({"pr_number":42})).unwrap()),
        argv: vec!["/bin/true".into()],
        idem_key: idem.into(),
        event_spec: probe.as_ref().map(|_| {
            serde_json::from_value(json!({
                "event_kind":"forge.pr.merged", "fields":{
                    "head_sha":{"json_field":{"path":"/headRefOid"}},
                    "merge_sha":{"json_field":{"path":"/oid"}}
                }
            }))
            .unwrap()
        }),
        context: Map::new(),
        probe,
        cwd_lease: PathBuf::from(
            boot.repo
                .track_get(track_id)
                .await
                .unwrap()
                .unwrap()
                .workspace
                .agent_cwd(),
        ),
        result_path: boot.tmp.path().join(format!("{idem}.result.json")),
        deadline_ms: now_ms() + 60_000,
    };
    let pool = boot.repo.sqlite_db().expect("sqlite pool");
    let operation_repo = SqlxOperationRepo::new(pool.clone());
    operation_repo
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
        .expect("insert forge action")
}

async fn insert_native_forge_action(boot: &Boot, track_id: &str, idem: &str) -> String {
    let id = insert_forge_action(boot, track_id, idem, None).await;
    set_operation_phase(boot, &id, "spawn_started").await;
    id
}

async fn set_operation_phase(boot: &Boot, op_id: &str, phase: &str) {
    let pool = boot.repo.sqlite_db().expect("sqlite pool");
    sqlx::query("UPDATE operations SET phase = ?1 WHERE id = ?2")
        .bind(phase)
        .bind(op_id)
        .execute(pool.writer_pool())
        .await
        .expect("set operation phase");
}

#[tokio::test]
async fn c5_delete_track_forge_unsettled_blocks_settled_parked_allows() {
    let boot = boot().await;
    let track_id = create_track(&boot, "track-active").await;
    let op_id = real_forge_action(&boot, &track_id, "track-active-op").await;

    let status = delete(boot.app.clone(), &format!("/api/tracks/{track_id}")).await;
    assert_eq!(status, StatusCode::CONFLICT);

    settle_action(&boot, &op_id).await;
    let status = delete(boot.app.clone(), &format!("/api/tracks/{track_id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn c5_delete_area_forge_unsettled_blocks_settled_parked_allows() {
    let boot = boot().await;
    let track_id = create_track(&boot, "area-active").await;
    let op = real_forge_action(&boot, &track_id, "area-active-op").await;

    let status = delete(boot.app.clone(), &format!("/api/areas/{}", boot.area_id)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    settle_action(&boot, &op).await;
    assert_eq!(
        delete(boot.app.clone(), &format!("/api/areas/{}", boot.area_id)).await,
        StatusCode::NO_CONTENT
    );
}

async fn assert_closed_worktree_clean_refuses_forge(kind: &str) {
    use crate::support::git_helpers::run_git;
    use calm_server::test_seams::clean_closed_worktrees_with_history_for_test as clean;
    use std::collections::HashSet;

    let boot = boot().await;
    let track = create_track(&boot, "clean-active").await;
    let db = boot.repo.sqlite_db().unwrap();
    let pool = db.writer_pool();
    let sessions: Vec<String> = sqlx::query_scalar("SELECT id FROM worker_sessions")
        .fetch_all(pool)
        .await
        .unwrap();
    for session in sessions {
        if let Some(handle) = boot.state.harness.remove(&session) {
            let _ = handle.shutdown().await;
        }
    }
    let path: String = sqlx::query_scalar("SELECT workspace_worktree_path FROM tracks WHERE id=?")
        .bind(&track)
        .fetch_one(pool)
        .await
        .unwrap();
    let path = PathBuf::from(path);
    run_git(&path, ["config", "core.excludesFile", "/dev/null"]);
    std::fs::write(path.join(".gitignore"), "target/\n").unwrap();
    std::fs::create_dir_all(path.join("target")).unwrap();
    let ignored = path.join("target/ignored");
    std::fs::write(&ignored, "build output").unwrap();
    sqlx::query("UPDATE tracks SET closed_at=123 WHERE id=?")
        .bind(&track)
        .execute(pool)
        .await
        .unwrap();
    let op = if kind != "migrated" {
        insert_native_forge_action(&boot, &track, "clean-active-op").await
    } else {
        real_forge_action(&boot, &track, "clean-active-op").await
    };
    if kind == "native-stuck" {
        // Seed the released native checkpoint, then exercise real compensation refusal.
        set_operation_phase(&boot, &op, "pending").await;
        let operations = SqlxOperationRepo::new(db.clone());
        let claimed = operations
            .claim_operation_for_recovery(&op)
            .await
            .unwrap()
            .unwrap();
        operations
            .prepare_tx_and_advance(
                &claimed,
                &calm_server::operation::forge_action_adapter::ForgeActionAdapter::new(),
                None,
            )
            .await
            .unwrap()
            .committed()
            .unwrap();
        sqlx::query("UPDATE operations SET phase='spawn_started',spawn_artifacts_json=?2,lease_owner=NULL,lease_until_ms=NULL WHERE id=?1")
            .bind(&op).bind(json!({"pid":1,"pgid":1,"start_time":1,"boot_id":"old-boot","log_path":null,"extra":{}}).to_string()).execute(pool).await.unwrap();
        let _ = boot.state.operation_runtime.drive().await;
        let phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id=?")
            .bind(&op)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(
            phase, "stuck",
            "native compensation must refuse process control"
        );
        assert_eq!(
            delete(boot.app.clone(), &format!("/api/tracks/{track}")).await,
            StatusCode::CONFLICT
        );
        assert_eq!(
            delete(boot.app.clone(), &format!("/api/areas/{}", boot.area_id)).await,
            StatusCode::CONFLICT
        );
    }

    if kind == "no-record" {
        assert_eq!(
            delete(boot.app.clone(), &format!("/api/tracks/{track}")).await,
            StatusCode::CONFLICT
        );
        assert_eq!(
            delete(boot.app.clone(), &format!("/api/areas/{}", boot.area_id)).await,
            StatusCode::CONFLICT
        );
    }
    let mut history = HashSet::new();
    assert_eq!(
        clean(&db, &boot.state.operation_runtime, &mut history)
            .await
            .unwrap(),
        0
    );
    assert!(history.is_empty(), "refused clean must remain retryable");
    assert!(ignored.exists(), "active forge must protect ignored files");
    if kind != "migrated" {
        set_operation_phase(&boot, &op, "succeeded").await;
    } else {
        settle_action(&boot, &op).await;
    }
    assert_eq!(
        clean(&db, &boot.state.operation_runtime, &mut history)
            .await
            .unwrap(),
        1
    );
    assert!(!ignored.exists());
    assert!(history.contains(&(track, 123)));
}

#[tokio::test]
async fn c5_clean_forge_unsettled_blocks_settled_parked_allows() {
    assert_closed_worktree_clean_refuses_forge("migrated").await;
}
#[tokio::test]
async fn c3_closed_worktree_clean_refuses_unresolved_native_stuck_forge_row() {
    assert_closed_worktree_clean_refuses_forge("native-stuck").await;
}

#[path = "track_delete_forge_fence/c5_probe.rs"]
mod c5_probe;
use c5_probe::{real_forge_action, settle_action};

#[tokio::test]
async fn c5_no_record_forge_recoverable_phase_blocks_track_area_and_clean() {
    assert_closed_worktree_clean_refuses_forge("no-record").await;
}
