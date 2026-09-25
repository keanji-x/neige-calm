//! Linear track-vcs history seeded straight into the audit tables, old enough that the object sweeper may reclaim it.

use calm_server::ids::TrackId;
use calm_server::model::now_ms;
use calm_server::track_vcs::{
    MANIFEST_SCHEMA_VERSION, ManifestEntry, TreeManifest, canonical_json_bytes,
};
use sqlx::SqlitePool;
use std::collections::BTreeMap;

const SWEEP_GRACE_MS: i64 = 60 * 60 * 1000;

/// `count` commits `<track>-admin-commit-<i>`, each holding only `file-<i>.txt`, with HEAD on the last.
pub async fn seed_linear_commits(pool: &SqlitePool, track_id: &TrackId, count: usize) {
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
