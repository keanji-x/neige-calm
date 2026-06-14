use std::collections::{BTreeMap, BTreeSet};

use calm_truth::db::prelude::RepoSyncDomainRaw;
use calm_truth::db::sqlite::{SqlxRepo, begin_immediate_tx};
use calm_truth::ids::WaveId;
use calm_truth::model::{NewCove, NewWave, RequestTheme, now_ms};
use calm_truth::wave_vcs::{
    self, MANIFEST_SCHEMA_VERSION, ManifestEntry, TreeManifest, canonical_json_bytes,
};
use serde_json::json;
use sqlx::{Row, SqlitePool};

const SWEEP_GRACE_MS: i64 = 60 * 60 * 1000;

#[derive(Clone, Debug)]
struct TestCommit {
    hash: String,
    parent_hash: Option<String>,
    tree_hash: String,
    blob_hash: String,
    created_at: i64,
}

struct Fixture {
    repo: SqlxRepo,
    wave_id: WaveId,
    commits: Vec<TestCommit>,
}

impl Fixture {
    fn pool(&self) -> &SqlitePool {
        self.repo.pool()
    }
}

async fn fresh_wave() -> (SqlxRepo, WaveId) {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open sqlite repo");
    let cove = repo
        .cove_create(NewCove {
            name: "cove".into(),
            color: "#336699".into(),
            sort: None,
        })
        .await
        .expect("create cove");
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "wave".into(),
            sort: None,
            cwd: "/tmp".into(),
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("create wave");
    (repo, wave.id)
}

async fn fixture_with_commits(count: usize) -> Fixture {
    let (repo, wave_id) = fresh_wave().await;
    let commits = seed_linear_commits(repo.pool(), &wave_id, count).await;
    Fixture {
        repo,
        wave_id,
        commits,
    }
}

async fn seed_linear_commits(pool: &SqlitePool, wave_id: &WaveId, count: usize) -> Vec<TestCommit> {
    let base = now_ms() - (2 * SWEEP_GRACE_MS);
    let mut tx = pool.begin().await.expect("begin seed commits");
    let mut out = Vec::with_capacity(count);
    let mut parent_hash: Option<String> = None;

    for index in 0..count {
        let created_at = base + index as i64 * 1000;
        let commit_hash = format!("{}-commit-{index}", wave_id.as_str());
        let tree_hash = format!("{}-tree-{index}", wave_id.as_str());
        let blob_hash = format!("{}-blob-{index}", wave_id.as_str());
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
            r#"INSERT INTO wave_vcs_objects (hash, kind, bytes, created_at)
               VALUES (?1, 'blob', ?2, ?3)"#,
        )
        .bind(&blob_hash)
        .bind(&blob_bytes)
        .bind(created_at)
        .execute(&mut *tx)
        .await
        .expect("insert blob");
        sqlx::query(
            r#"INSERT INTO wave_vcs_objects (hash, kind, bytes, created_at)
               VALUES (?1, 'tree', ?2, ?3)"#,
        )
        .bind(&tree_hash)
        .bind(&tree_bytes)
        .bind(created_at)
        .execute(&mut *tx)
        .await
        .expect("insert tree");
        sqlx::query(
            r#"INSERT INTO wave_vcs_commits (
                   hash, wave_id, parent_hash, tree_hash, manifest_schema_version,
                   author, message, lifecycle, event_id, created_at
               )
               VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, 'active', ?7, ?8)"#,
        )
        .bind(&commit_hash)
        .bind(wave_id.as_str())
        .bind(parent_hash.as_deref())
        .bind(&tree_hash)
        .bind(MANIFEST_SCHEMA_VERSION)
        .bind(format!("commit {index}"))
        .bind(index as i64 + 1)
        .bind(created_at)
        .execute(&mut *tx)
        .await
        .expect("insert commit");

        out.push(TestCommit {
            hash: commit_hash.clone(),
            parent_hash: parent_hash.clone(),
            tree_hash,
            blob_hash,
            created_at,
        });
        parent_hash = Some(commit_hash);
    }

    if let Some(head) = parent_hash {
        sqlx::query(
            r#"INSERT INTO wave_vcs_refs (wave_id, head_hash, updated_event_id)
               VALUES (?1, ?2, ?3)"#,
        )
        .bind(wave_id.as_str())
        .bind(head)
        .bind(count as i64)
        .execute(&mut *tx)
        .await
        .expect("insert ref");
    }

    tx.commit().await.expect("commit seed commits");
    out
}

async fn prune_once(pool: &SqlitePool, wave_id: &WaveId, keep: usize) -> u64 {
    let mut tx = begin_immediate_tx(pool).await.expect("begin prune");
    let deleted = wave_vcs::prune_wave_history_tx(&mut tx, wave_id, keep)
        .await
        .expect("prune");
    tx.commit().await.expect("commit prune");
    deleted
}

async fn insert_active_session(
    pool: &SqlitePool,
    wave_id: &WaveId,
    suffix: &str,
    handle_state_json: Option<&str>,
) {
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO worker_sessions (
               id, wave_id, provider, mode, contract, state, handle_state_json,
               created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, 'codex', 'resumable', 'executor', 'running', ?3, ?4, ?5)"#,
    )
    .bind(format!("session-{suffix}"))
    .bind(wave_id.as_str())
    .bind(handle_state_json)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .expect("insert active worker session");
}

fn harness_snapshot(last_seen_head: Option<&str>) -> String {
    json!({
        "schema_version": 1,
        "mode": "harness",
        "last_seen_head": last_seen_head,
    })
    .to_string()
}

async fn commit_exists(pool: &SqlitePool, hash: &str) -> bool {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM wave_vcs_commits WHERE hash = ?1")
        .bind(hash)
        .fetch_one(pool)
        .await
        .expect("count commit");
    row.0 > 0
}

async fn object_exists(pool: &SqlitePool, hash: &str) -> bool {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM wave_vcs_objects WHERE hash = ?1")
        .bind(hash)
        .fetch_one(pool)
        .await
        .expect("count object");
    row.0 > 0
}

async fn commit_count(pool: &SqlitePool, wave_id: &WaveId) -> i64 {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM wave_vcs_commits WHERE wave_id = ?1")
        .bind(wave_id.as_str())
        .fetch_one(pool)
        .await
        .expect("count commits");
    row.0
}

async fn object_count(pool: &SqlitePool) -> i64 {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM wave_vcs_objects")
        .fetch_one(pool)
        .await
        .expect("count objects");
    row.0
}

async fn commit_hashes(pool: &SqlitePool, wave_id: &WaveId) -> BTreeSet<String> {
    sqlx::query("SELECT hash FROM wave_vcs_commits WHERE wave_id = ?1 ORDER BY hash")
        .bind(wave_id.as_str())
        .fetch_all(pool)
        .await
        .expect("load commit hashes")
        .into_iter()
        .map(|row| row.get("hash"))
        .collect()
}

async fn surviving_commits(pool: &SqlitePool, wave_id: &WaveId) -> Vec<TestCommit> {
    sqlx::query(
        r#"SELECT hash, parent_hash, tree_hash, created_at
           FROM wave_vcs_commits
           WHERE wave_id = ?1
           ORDER BY created_at ASC"#,
    )
    .bind(wave_id.as_str())
    .fetch_all(pool)
    .await
    .expect("load surviving commits")
    .into_iter()
    .map(|row| TestCommit {
        hash: row.get("hash"),
        parent_hash: row.get("parent_hash"),
        tree_hash: row.get("tree_hash"),
        blob_hash: String::new(),
        created_at: row.get("created_at"),
    })
    .collect()
}

#[tokio::test]
async fn prune_keep_one_preserves_head_and_ref() {
    let fixture = fixture_with_commits(6).await;
    let head_before = wave_vcs::head(fixture.pool(), &fixture.wave_id)
        .await
        .expect("head")
        .expect("head exists");

    let deleted = prune_once(fixture.pool(), &fixture.wave_id, 1).await;

    assert!(deleted > 0);
    assert_eq!(
        wave_vcs::head(fixture.pool(), &fixture.wave_id)
            .await
            .expect("head"),
        Some(head_before.clone())
    );
    assert!(
        wave_vcs::commit_record(fixture.pool(), &head_before)
            .await
            .expect("head commit")
            .is_some()
    );
    let ref_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM wave_vcs_refs WHERE wave_id = ?1")
        .bind(fixture.wave_id.as_str())
        .fetch_one(fixture.pool())
        .await
        .expect("count ref");
    assert_eq!(ref_count.0, 1);
}

#[tokio::test]
async fn active_session_last_seen_diff_still_works_after_prune() {
    let fixture = fixture_with_commits(6).await;
    let last_seen = &fixture.commits[1];
    let head = fixture.commits.last().expect("head");
    let snapshot = harness_snapshot(Some(&last_seen.hash));
    insert_active_session(
        fixture.pool(),
        &fixture.wave_id,
        "last-seen",
        Some(&snapshot),
    )
    .await;
    let before_diff = wave_vcs::diff(fixture.pool(), &last_seen.hash, &head.hash, None)
        .await
        .expect("diff before prune");

    let deleted = prune_once(fixture.pool(), &fixture.wave_id, 1).await;

    assert!(deleted > 0);
    assert!(commit_exists(fixture.pool(), &last_seen.hash).await);
    assert!(object_exists(fixture.pool(), &last_seen.tree_hash).await);
    assert!(object_exists(fixture.pool(), &last_seen.blob_hash).await);
    assert!(commit_exists(fixture.pool(), &head.hash).await);
    assert!(object_exists(fixture.pool(), &head.tree_hash).await);
    assert!(object_exists(fixture.pool(), &head.blob_hash).await);
    assert_eq!(
        wave_vcs::diff(fixture.pool(), &last_seen.hash, &head.hash, None)
            .await
            .expect("diff after prune"),
        before_diff
    );
}

#[tokio::test]
async fn prune_keeps_every_commit_at_or_after_oldest_protected_floor() {
    let fixture = fixture_with_commits(7).await;
    let floor_commit = &fixture.commits[2];
    let snapshot = harness_snapshot(Some(&floor_commit.hash));
    insert_active_session(fixture.pool(), &fixture.wave_id, "floor", Some(&snapshot)).await;

    let deleted = prune_once(fixture.pool(), &fixture.wave_id, 2).await;

    assert!(deleted > 0);
    for commit in &fixture.commits {
        let exists = commit_exists(fixture.pool(), &commit.hash).await;
        if commit.created_at >= floor_commit.created_at {
            assert!(exists, "{} should survive", commit.hash);
        } else {
            assert!(!exists, "{} should be pruned", commit.hash);
        }
    }

    let survivors = surviving_commits(fixture.pool(), &fixture.wave_id).await;
    let survivor_hashes = survivors
        .iter()
        .map(|commit| commit.hash.as_str())
        .collect::<BTreeSet<_>>();
    for commit in survivors.iter().skip(1) {
        let parent = commit
            .parent_hash
            .as_deref()
            .expect("non-oldest suffix commit has parent");
        assert!(
            survivor_hashes.contains(parent),
            "{} parent {parent} should remain inside kept suffix",
            commit.hash
        );
    }
}

#[tokio::test]
async fn sweep_reclaims_only_objects_reachable_from_pruned_commits() {
    let fixture = fixture_with_commits(6).await;
    let pruned = &fixture.commits[1];
    let kept_last_seen = &fixture.commits[3];
    let head = fixture.commits.last().expect("head");
    let snapshot = harness_snapshot(Some(&kept_last_seen.hash));
    insert_active_session(fixture.pool(), &fixture.wave_id, "kept", Some(&snapshot)).await;
    let before_objects = object_count(fixture.pool()).await;

    let deleted_commits = prune_once(fixture.pool(), &fixture.wave_id, 1).await;
    let deleted_objects = wave_vcs::sweep_unreferenced_objects_once(fixture.pool())
        .await
        .expect("sweep");

    assert!(deleted_commits > 0);
    assert!(deleted_objects > 0);
    assert!(object_count(fixture.pool()).await < before_objects);
    assert!(!object_exists(fixture.pool(), &pruned.tree_hash).await);
    assert!(!object_exists(fixture.pool(), &pruned.blob_hash).await);
    assert!(object_exists(fixture.pool(), &kept_last_seen.tree_hash).await);
    assert!(object_exists(fixture.pool(), &kept_last_seen.blob_hash).await);
    assert!(object_exists(fixture.pool(), &head.tree_hash).await);
    assert!(object_exists(fixture.pool(), &head.blob_hash).await);
}

#[tokio::test]
async fn unparseable_active_snapshot_keeps_all_commits() {
    let fixture = fixture_with_commits(5).await;
    insert_active_session(
        fixture.pool(),
        &fixture.wave_id,
        "garbage",
        Some("{not-json"),
    )
    .await;

    let deleted = prune_once(fixture.pool(), &fixture.wave_id, 1).await;

    assert_eq!(deleted, 0);
    assert_eq!(commit_count(fixture.pool(), &fixture.wave_id).await, 5);
    for commit in &fixture.commits {
        assert!(commit_exists(fixture.pool(), &commit.hash).await);
    }
}

#[tokio::test]
async fn null_or_absent_last_seen_head_does_not_block_prune() {
    let fixture = fixture_with_commits(5).await;
    let null_snapshot = harness_snapshot(None);
    let absent_snapshot = json!({
        "schema_version": 1,
        "mode": "harness",
    })
    .to_string();
    insert_active_session(
        fixture.pool(),
        &fixture.wave_id,
        "null",
        Some(&null_snapshot),
    )
    .await;
    insert_active_session(
        fixture.pool(),
        &fixture.wave_id,
        "absent",
        Some(&absent_snapshot),
    )
    .await;

    let deleted = prune_once(fixture.pool(), &fixture.wave_id, 1).await;

    assert!(deleted > 0);
    assert!(commit_exists(fixture.pool(), &fixture.commits[4].hash).await);
    assert!(!commit_exists(fixture.pool(), &fixture.commits[0].hash).await);
}

#[tokio::test]
async fn active_last_seen_absent_from_commit_table_keeps_all_commits() {
    let fixture = fixture_with_commits(5).await;
    let snapshot = harness_snapshot(Some("missing-commit"));
    insert_active_session(fixture.pool(), &fixture.wave_id, "missing", Some(&snapshot)).await;

    let deleted = prune_once(fixture.pool(), &fixture.wave_id, 1).await;

    assert_eq!(deleted, 0);
    assert_eq!(commit_count(fixture.pool(), &fixture.wave_id).await, 5);
}

#[tokio::test]
async fn prune_and_sweep_are_idempotent() {
    let fixture = fixture_with_commits(6).await;

    let first_prune = prune_once(fixture.pool(), &fixture.wave_id, 2).await;
    let second_prune = prune_once(fixture.pool(), &fixture.wave_id, 2).await;
    let first_sweep = wave_vcs::sweep_unreferenced_objects_once(fixture.pool())
        .await
        .expect("first sweep");
    let after_first_hashes = commit_hashes(fixture.pool(), &fixture.wave_id).await;
    let after_first_objects = object_count(fixture.pool()).await;
    let second_sweep = wave_vcs::sweep_unreferenced_objects_once(fixture.pool())
        .await
        .expect("second sweep");

    assert!(first_prune > 0);
    assert_eq!(second_prune, 0);
    assert!(first_sweep > 0);
    assert_eq!(second_sweep, 0);
    assert_eq!(
        commit_hashes(fixture.pool(), &fixture.wave_id).await,
        after_first_hashes
    );
    assert_eq!(object_count(fixture.pool()).await, after_first_objects);
}

#[tokio::test]
async fn sweep_honors_object_created_at_grace_cutoff() {
    let fixture = fixture_with_commits(4).await;
    let old_orphan = format!("{}-old-orphan", fixture.wave_id.as_str());
    let young_orphan = format!("{}-young-orphan", fixture.wave_id.as_str());
    sqlx::query(
        r#"INSERT INTO wave_vcs_objects (hash, kind, bytes, created_at)
           VALUES (?1, 'blob', ?2, ?3), (?4, 'blob', ?5, ?6)"#,
    )
    .bind(&old_orphan)
    .bind(b"old".as_slice())
    .bind(now_ms() - (2 * SWEEP_GRACE_MS))
    .bind(&young_orphan)
    .bind(b"young".as_slice())
    .bind(now_ms())
    .execute(fixture.pool())
    .await
    .expect("insert orphan objects");

    assert!(prune_once(fixture.pool(), &fixture.wave_id, 1).await > 0);
    let swept = wave_vcs::sweep_unreferenced_objects_once(fixture.pool())
        .await
        .expect("sweep");

    assert!(swept > 0);
    assert!(!object_exists(fixture.pool(), &old_orphan).await);
    assert!(object_exists(fixture.pool(), &young_orphan).await);
}

#[tokio::test]
async fn prune_no_op_cases_and_keep_zero_clamp() {
    let (repo, empty_wave_id) = fresh_wave().await;
    assert_eq!(prune_once(repo.pool(), &empty_wave_id, 1).await, 0);

    let keep_large = fixture_with_commits(3).await;
    assert_eq!(
        prune_once(keep_large.pool(), &keep_large.wave_id, 10).await,
        0
    );
    assert_eq!(
        commit_count(keep_large.pool(), &keep_large.wave_id).await,
        3
    );

    let keep_zero = fixture_with_commits(3).await;
    let head = keep_zero.commits.last().expect("head").hash.clone();
    let deleted = prune_once(keep_zero.pool(), &keep_zero.wave_id, 0).await;
    assert!(deleted > 0);
    assert!(commit_exists(keep_zero.pool(), &head).await);
    assert_eq!(
        wave_vcs::head(keep_zero.pool(), &keep_zero.wave_id)
            .await
            .expect("head"),
        Some(head)
    );
}
