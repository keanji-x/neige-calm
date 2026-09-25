#![cfg(unix)]

use calm_server::ids::TrackId;
use calm_server::mcp_server::ToolCallIdentity;
use calm_server::mcp_server::tools::admin::{TOOL_ADMIN_TRACK_GC, TOOL_ADMIN_VACUUM};
use calm_server::model::CardRole;
use calm_server::plugin_host::mcp::RpcError;
use serde_json::json;
use sqlx::SqlitePool;

use crate::support;

use support::track_file::{boot, call_tool, planner_identity};
use support::track_vcs_seed::seed_linear_commits;

async fn commit_count(pool: &SqlitePool, track_id: &TrackId) -> i64 {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM track_vcs_commits WHERE track_id = ?1")
        .bind(track_id.as_str())
        .fetch_one(pool)
        .await
        .expect("count commits");
    row.0
}

async fn object_count(pool: &SqlitePool) -> i64 {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM track_vcs_objects")
        .fetch_one(pool)
        .await
        .expect("count objects");
    row.0
}

fn worker_identity(boot: &support::track_file::Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.worker_card_id.as_str().to_string(),
        role: CardRole::Worker,
        provider: calm_server::session_projection_repo::AgentProvider::Codex,
        session_id: "worker-session".to_string(),
        track_id: Some(boot.track_id.as_str().to_string()),
        area_id: boot.area_id.as_str().to_string(),
        thread_id: "worker-thread".to_string(),
    }
}

#[tokio::test]
async fn track_gc_dry_run_reports_without_deleting() {
    let boot = boot().await;
    let pool = boot.repo.sqlite_pool().expect("track vcs pool");
    seed_linear_commits(&pool, &boot.track_id, 5).await;

    let result = call_tool(
        &boot,
        TOOL_ADMIN_TRACK_GC,
        planner_identity(&boot),
        json!({ "track_id": boot.track_id.as_str(), "keep": 2, "dry_run": true }),
    )
    .await
    .expect("track-gc dry-run");

    assert_eq!(result["track_id"], json!(boot.track_id.as_str()));
    assert_eq!(result["keep"], json!(2));
    assert_eq!(result["dry_run"], json!(true));
    assert_eq!(result["pruned_commits"], json!(3));
    assert_eq!(result["swept_objects"], json!(0));
    assert_eq!(commit_count(&pool, &boot.track_id).await, 5);
    assert_eq!(object_count(&pool).await, 10);
}

#[tokio::test]
async fn track_gc_real_run_prunes_sweeps_and_is_idempotent() {
    let boot = boot().await;
    let pool = boot.repo.sqlite_pool().expect("track vcs pool");
    seed_linear_commits(&pool, &boot.track_id, 5).await;

    let result = call_tool(
        &boot,
        TOOL_ADMIN_TRACK_GC,
        planner_identity(&boot),
        json!({ "track_id": boot.track_id.as_str(), "keep": 2, "dry_run": false }),
    )
    .await
    .expect("track-gc real run");

    assert_eq!(result["dry_run"], json!(false));
    assert_eq!(result["pruned_commits"], json!(3));
    assert_eq!(result["swept_objects"], json!(6));
    assert_eq!(commit_count(&pool, &boot.track_id).await, 2);
    assert_eq!(object_count(&pool).await, 4);

    let second = call_tool(
        &boot,
        TOOL_ADMIN_TRACK_GC,
        planner_identity(&boot),
        json!({ "track_id": boot.track_id.as_str(), "keep": 2, "dry_run": false }),
    )
    .await
    .expect("track-gc second run");

    assert_eq!(second["pruned_commits"], json!(0));
    assert_eq!(second["swept_objects"], json!(0));
    assert_eq!(commit_count(&pool, &boot.track_id).await, 2);
    assert_eq!(object_count(&pool).await, 4);
}

#[tokio::test]
async fn track_gc_rejects_wrong_track_without_deleting() {
    let boot = boot().await;
    let pool = boot.repo.sqlite_pool().expect("track vcs pool");
    seed_linear_commits(&pool, &boot.track_id, 5).await;

    let err = call_tool(
        &boot,
        TOOL_ADMIN_TRACK_GC,
        planner_identity(&boot),
        json!({ "track_id": "wrong-track", "keep": 2, "dry_run": false }),
    )
    .await
    .expect_err("wrong track rejected");

    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(
        err.message.contains("does not match"),
        "unexpected error: {err:?}"
    );
    assert_eq!(commit_count(&pool, &boot.track_id).await, 5);
    assert_eq!(object_count(&pool).await, 10);
}

#[tokio::test]
async fn track_gc_rejects_worker_identity() {
    let boot = boot().await;
    let pool = boot.repo.sqlite_pool().expect("track vcs pool");
    seed_linear_commits(&pool, &boot.track_id, 5).await;

    let err = call_tool(
        &boot,
        TOOL_ADMIN_TRACK_GC,
        worker_identity(&boot),
        json!({ "track_id": boot.track_id.as_str(), "keep": 2, "dry_run": true }),
    )
    .await
    .expect_err("worker rejected");

    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("Planner"), "unexpected error: {err:?}");
    assert_eq!(commit_count(&pool, &boot.track_id).await, 5);
    assert_eq!(object_count(&pool).await, 10);
}

#[tokio::test]
async fn vacuum_runs_on_populated_db() {
    let boot = boot().await;
    let pool = boot.repo.sqlite_pool().expect("track vcs pool");
    seed_linear_commits(&pool, &boot.track_id, 2).await;

    let result = call_tool(&boot, TOOL_ADMIN_VACUUM, planner_identity(&boot), json!({}))
        .await
        .expect("vacuum");

    assert_eq!(result, json!({ "ok": true }));
}
