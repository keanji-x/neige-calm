#![cfg(unix)]

use std::collections::BTreeMap;

use calm_server::ids::TrackId;
use calm_server::mcp_server::ToolCallIdentity;
use calm_server::mcp_server::tools::admin::{TOOL_ADMIN_TRACK_GC, TOOL_ADMIN_VACUUM};
use calm_server::model::{CardRole, now_ms};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::track_vcs::{
    MANIFEST_SCHEMA_VERSION, ManifestEntry, TreeManifest, canonical_json_bytes,
};
use serde_json::json;
use sqlx::SqlitePool;

use crate::support;

use support::track_file::{boot, call_tool, spec_identity};

const SWEEP_GRACE_MS: i64 = 60 * 60 * 1000;

async fn seed_linear_commits(pool: &SqlitePool, track_id: &TrackId, count: usize) {
    let base = now_ms() - (2 * SWEEP_GRACE_MS);
    let mut tx = pool.begin().await.expect("begin seed commits");
    let mut parent_hash: Option<String> = None;

    for index in 0..count {
        let created_at = base + index as i64 * 1000;
        let commit_hash = format!("{}-admin-commit-{index}", track_id.as_str());
        let tree_hash = format!("{}-admin-tree-{index}", track_id.as_str());
        let blob_hash = format!("{}-admin-blob-{index}", track_id.as_str());
        let blob_bytes = format!("commit {index}\n").into_bytes();
        let mut entries = BTreeMap::new();
        entries.insert(
            format!("file-{index}.txt"),
            ManifestEntry {
                blob_hash: blob_hash.clone(),
                byte_len: blob_bytes.len() as u64,
                content_type: "text/plain".into(),
            },
        );
        let manifest = TreeManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            entries,
        };
        let tree_bytes = canonical_json_bytes(&manifest).expect("canonical tree json");

        sqlx::query(
            r#"INSERT INTO track_vcs_objects (hash, kind, bytes, created_at)
               VALUES (?1, 'blob', ?2, ?3)"#,
        )
        .bind(&blob_hash)
        .bind(&blob_bytes)
        .bind(created_at)
        .execute(&mut *tx)
        .await
        .expect("insert blob");
        sqlx::query(
            r#"INSERT INTO track_vcs_objects (hash, kind, bytes, created_at)
               VALUES (?1, 'tree', ?2, ?3)"#,
        )
        .bind(&tree_hash)
        .bind(&tree_bytes)
        .bind(created_at)
        .execute(&mut *tx)
        .await
        .expect("insert tree");
        sqlx::query(
            r#"INSERT INTO track_vcs_commits (
                   hash, track_id, parent_hash, tree_hash, manifest_schema_version,
                   author, message, lifecycle, event_id, created_at
               )
               VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, 'active', ?7, ?8)"#,
        )
        .bind(&commit_hash)
        .bind(track_id.as_str())
        .bind(parent_hash.as_deref())
        .bind(&tree_hash)
        .bind(MANIFEST_SCHEMA_VERSION)
        .bind(format!("commit {index}"))
        .bind(index as i64 + 1)
        .bind(created_at)
        .execute(&mut *tx)
        .await
        .expect("insert commit");

        parent_hash = Some(commit_hash);
    }

    if let Some(head) = parent_hash {
        sqlx::query(
            r#"INSERT INTO track_vcs_refs (track_id, head_hash, updated_event_id)
               VALUES (?1, ?2, ?3)"#,
        )
        .bind(track_id.as_str())
        .bind(head)
        .bind(count as i64)
        .execute(&mut *tx)
        .await
        .expect("insert ref");
    }

    tx.commit().await.expect("commit seed commits");
}

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
        spec_identity(&boot),
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
        spec_identity(&boot),
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
        spec_identity(&boot),
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
        spec_identity(&boot),
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
    assert!(err.message.contains("Spec"), "unexpected error: {err:?}");
    assert_eq!(commit_count(&pool, &boot.track_id).await, 5);
    assert_eq!(object_count(&pool).await, 10);
}

#[tokio::test]
async fn vacuum_runs_on_populated_db() {
    let boot = boot().await;
    let pool = boot.repo.sqlite_pool().expect("track vcs pool");
    seed_linear_commits(&pool, &boot.track_id, 2).await;

    let result = call_tool(&boot, TOOL_ADMIN_VACUUM, spec_identity(&boot), json!({}))
        .await
        .expect("vacuum");

    assert_eq!(result, json!({ "ok": true }));
}
