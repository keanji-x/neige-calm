use std::collections::{BTreeMap, BTreeSet};

use calm_truth::db::prelude::RepoSyncDomainRaw;
use calm_truth::db::sqlite::{SqlxRepo, begin_immediate_tx};
use calm_truth::ids::TrackId;
use calm_truth::model::{NewArea, NewTrack, RequestTheme, now_ms};
use calm_truth::track_vcs::{
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
    track_id: TrackId,
    commits: Vec<TestCommit>,
}

impl Fixture {
    fn pool(&self) -> &SqlitePool {
        self.repo.pool()
    }
}

async fn fresh_track() -> (SqlxRepo, TrackId) {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open sqlite repo");
    let area = repo
        .area_create(NewArea {
            name: "area".into(),
            color: "#336699".into(),
            sort: None,
        })
        .await
        .expect("create area");
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "track".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("create track");
    (repo, track.id)
}

async fn fixture_with_commits(count: usize) -> Fixture {
    let (repo, track_id) = fresh_track().await;
    let commits = seed_linear_commits(repo.pool(), &track_id, count).await;
    Fixture {
        repo,
        track_id,
        commits,
    }
}

async fn seed_linear_commits(
    pool: &SqlitePool,
    track_id: &TrackId,
    count: usize,
) -> Vec<TestCommit> {
    let base = now_ms() - (2 * SWEEP_GRACE_MS);
    let mut tx = pool.begin().await.expect("begin seed commits");
    let mut out = Vec::with_capacity(count);
    let mut parent_hash: Option<String> = None;

    for index in 0..count {
        let created_at = base + index as i64 * 1000;
        let commit_hash = format!("{}-commit-{index}", track_id.as_str());
        let tree_hash = format!("{}-tree-{index}", track_id.as_str());
        let blob_hash = format!("{}-blob-{index}", track_id.as_str());
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
    out
}

async fn prune_once(pool: &SqlitePool, track_id: &TrackId, keep: usize) -> u64 {
    let mut tx = begin_immediate_tx(pool).await.expect("begin prune");
    let deleted = track_vcs::prune_track_history_tx(&mut tx, track_id, keep)
        .await
        .expect("prune");
    tx.commit().await.expect("commit prune");
    deleted
}

async fn insert_active_session(
    pool: &SqlitePool,
    track_id: &TrackId,
    suffix: &str,
    handle_state_json: Option<&str>,
) {
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO worker_sessions (
               id, track_id, provider, mode, contract, state, handle_state_json,
               created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, 'codex', 'resumable', 'executor', 'running', ?3, ?4, ?5)"#,
    )
    .bind(format!("session-{suffix}"))
    .bind(track_id.as_str())
    .bind(handle_state_json)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .expect("insert active worker session");
}

fn harness_snapshot(last_seen_head: Option<&str>) -> String {
    harness_snapshot_with_heads(last_seen_head, None)
}

fn harness_snapshot_with_heads(
    last_seen_head: Option<&str>,
    issued_turn_head: Option<&str>,
) -> String {
    json!({
        "schema_version": 1,
        "mode": "harness",
        "last_seen_head": last_seen_head,
        "issued_turn_head": issued_turn_head,
    })
    .to_string()
}

async fn commit_exists(pool: &SqlitePool, hash: &str) -> bool {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM track_vcs_commits WHERE hash = ?1")
        .bind(hash)
        .fetch_one(pool)
        .await
        .expect("count commit");
    row.0 > 0
}

async fn object_exists(pool: &SqlitePool, hash: &str) -> bool {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM track_vcs_objects WHERE hash = ?1")
        .bind(hash)
        .fetch_one(pool)
        .await
        .expect("count object");
    row.0 > 0
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

async fn commit_hashes(pool: &SqlitePool, track_id: &TrackId) -> BTreeSet<String> {
    sqlx::query("SELECT hash FROM track_vcs_commits WHERE track_id = ?1 ORDER BY hash")
        .bind(track_id.as_str())
        .fetch_all(pool)
        .await
        .expect("load commit hashes")
        .into_iter()
        .map(|row| row.get("hash"))
        .collect()
}

async fn surviving_commits(pool: &SqlitePool, track_id: &TrackId) -> Vec<TestCommit> {
    sqlx::query(
        r#"SELECT hash, parent_hash, tree_hash, created_at
           FROM track_vcs_commits
           WHERE track_id = ?1
           ORDER BY created_at ASC"#,
    )
    .bind(track_id.as_str())
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
async fn prune_all_tracks_once_trims_history_to_keep_and_preserves_head() {
    let fixture = fixture_with_commits(7).await;
    let keep = 3;
    let head_before = track_vcs::head(fixture.pool(), &fixture.track_id)
        .await
        .expect("head")
        .expect("head exists");

    let pruned = track_vcs::prune_all_tracks_once(fixture.pool(), keep)
        .await
        .expect("prune all tracks");

    assert_eq!(pruned, 4);
    assert_eq!(
        commit_count(fixture.pool(), &fixture.track_id).await,
        keep as i64
    );
    assert_eq!(
        track_vcs::head(fixture.pool(), &fixture.track_id)
            .await
            .expect("head after prune"),
        Some(head_before)
    );

    let expected_recent = fixture
        .commits
        .iter()
        .rev()
        .take(keep)
        .map(|commit| commit.hash.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        commit_hashes(fixture.pool(), &fixture.track_id).await,
        expected_recent
    );

    for commit in &fixture.commits[..fixture.commits.len() - keep] {
        assert!(
            !commit_exists(fixture.pool(), &commit.hash).await,
            "{} should be pruned",
            commit.hash
        );
    }
}

#[tokio::test]
async fn prune_all_tracks_once_leaves_track_at_or_below_keep_untouched() {
    let fixture = fixture_with_commits(4).await;
    let keep = 4;
    let head_before = track_vcs::head(fixture.pool(), &fixture.track_id)
        .await
        .expect("head")
        .expect("head exists");
    let hashes_before = commit_hashes(fixture.pool(), &fixture.track_id).await;

    let pruned = track_vcs::prune_all_tracks_once(fixture.pool(), keep)
        .await
        .expect("prune all tracks");

    assert_eq!(pruned, 0);
    assert_eq!(
        commit_count(fixture.pool(), &fixture.track_id).await,
        keep as i64
    );
    assert_eq!(
        track_vcs::head(fixture.pool(), &fixture.track_id)
            .await
            .expect("head after prune"),
        Some(head_before)
    );
    assert_eq!(
        commit_hashes(fixture.pool(), &fixture.track_id).await,
        hashes_before
    );
}

#[tokio::test]
async fn prune_all_tracks_once_preserves_active_session_last_seen_endpoint() {
    let fixture = fixture_with_commits(6).await;
    let last_seen = &fixture.commits[1];
    let head = fixture.commits.last().expect("head");
    let snapshot = harness_snapshot(Some(&last_seen.hash));
    insert_active_session(
        fixture.pool(),
        &fixture.track_id,
        "wrapper-last-seen",
        Some(&snapshot),
    )
    .await;
    let before_diff = track_vcs::diff(fixture.pool(), &last_seen.hash, &head.hash, None)
        .await
        .expect("diff before prune");

    let pruned = track_vcs::prune_all_tracks_once(fixture.pool(), 1)
        .await
        .expect("prune all tracks");

    assert!(pruned > 0);
    assert!(commit_exists(fixture.pool(), &last_seen.hash).await);
    assert!(commit_exists(fixture.pool(), &head.hash).await);
    assert_eq!(
        track_vcs::diff(fixture.pool(), &last_seen.hash, &head.hash, None)
            .await
            .expect("diff after prune"),
        before_diff
    );
}

#[tokio::test]
async fn prune_keep_one_preserves_head_and_ref() {
    let fixture = fixture_with_commits(6).await;
    let head_before = track_vcs::head(fixture.pool(), &fixture.track_id)
        .await
        .expect("head")
        .expect("head exists");

    let deleted = prune_once(fixture.pool(), &fixture.track_id, 1).await;

    assert!(deleted > 0);
    assert_eq!(
        track_vcs::head(fixture.pool(), &fixture.track_id)
            .await
            .expect("head"),
        Some(head_before.clone())
    );
    assert!(
        track_vcs::commit_record(fixture.pool(), &head_before)
            .await
            .expect("head commit")
            .is_some()
    );
    let ref_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM track_vcs_refs WHERE track_id = ?1")
            .bind(fixture.track_id.as_str())
            .fetch_one(fixture.pool())
            .await
            .expect("count ref");
    assert_eq!(ref_count.0, 1);
}

#[tokio::test]
async fn default_log_treats_oldest_retained_commit_as_visible_history_root() {
    let fixture = fixture_with_commits(4).await;
    let head = fixture.commits.last().expect("head");

    let deleted = prune_once(fixture.pool(), &fixture.track_id, 1).await;
    assert_eq!(deleted, 3);

    let log = track_vcs::log(fixture.pool(), &fixture.track_id, None, 1, false)
        .await
        .expect("default log after prune");
    assert_eq!(log.commits.len(), 1, "log = {log:?}");
    assert_eq!(log.commits[0].hash, head.hash);
    assert_eq!(log.commits[0].changed_paths, vec!["file-3.txt"]);
    assert!(!log.truncated);
}

#[tokio::test]
async fn log_reports_a_missing_parent_tree_as_corruption() {
    let fixture = fixture_with_commits(2).await;
    let parent = &fixture.commits[0];

    sqlx::query("DELETE FROM track_vcs_objects WHERE hash = ?1")
        .bind(&parent.tree_hash)
        .execute(fixture.pool())
        .await
        .expect("delete parent tree object");

    let err = track_vcs::log(fixture.pool(), &fixture.track_id, None, 1, false)
        .await
        .expect_err("missing tree for a retained parent must fail loudly");
    let message = err.to_string();
    assert!(message.contains(&parent.hash), "err = {message}");
    assert!(message.contains(&parent.tree_hash), "err = {message}");
    assert!(message.contains("missing"), "err = {message}");
}

#[tokio::test]
async fn default_log_finds_changes_beyond_one_thousand_empty_commits() {
    const EMPTY_COMMITS: usize = 1_001;

    let fixture = fixture_with_commits(2).await;
    let changed = fixture.commits.last().expect("latest changed commit");
    let mut parent_hash = changed.hash.clone();
    let mut tx = fixture.pool().begin().await.expect("begin empty commits");
    for index in 0..EMPTY_COMMITS {
        let hash = format!("{}-empty-{index:04}", fixture.track_id.as_str());
        sqlx::query(
            r#"INSERT INTO track_vcs_commits (
                   hash, track_id, parent_hash, tree_hash, manifest_schema_version,
                   author, message, lifecycle, event_id, created_at
               )
               VALUES (?1, ?2, ?3, ?4, ?5, NULL, 'harness.item.added',
                       'active', ?6, ?7)"#,
        )
        .bind(&hash)
        .bind(fixture.track_id.as_str())
        .bind(&parent_hash)
        .bind(&changed.tree_hash)
        .bind(MANIFEST_SCHEMA_VERSION)
        .bind(3 + index as i64)
        .bind(changed.created_at + 1 + index as i64)
        .execute(&mut *tx)
        .await
        .expect("insert empty commit");
        parent_hash = hash;
    }
    sqlx::query(
        "UPDATE track_vcs_refs SET head_hash = ?1, updated_event_id = ?2 WHERE track_id = ?3",
    )
    .bind(&parent_hash)
    .bind(2 + EMPTY_COMMITS as i64)
    .bind(fixture.track_id.as_str())
    .execute(&mut *tx)
    .await
    .expect("advance head across empty commits");
    tx.commit().await.expect("commit empty history");

    let log = track_vcs::log(fixture.pool(), &fixture.track_id, None, 1, false)
        .await
        .expect("default log beyond empty commits");
    assert_eq!(log.commits.len(), 1, "log = {log:?}");
    assert_eq!(log.commits[0].hash, changed.hash);
    assert!(!log.commits[0].changed_paths.is_empty());
    assert!(log.truncated);
}

#[tokio::test]
async fn active_session_last_seen_diff_still_works_after_prune() {
    let fixture = fixture_with_commits(6).await;
    let last_seen = &fixture.commits[1];
    let head = fixture.commits.last().expect("head");
    let snapshot = harness_snapshot(Some(&last_seen.hash));
    insert_active_session(
        fixture.pool(),
        &fixture.track_id,
        "last-seen",
        Some(&snapshot),
    )
    .await;
    let before_diff = track_vcs::diff(fixture.pool(), &last_seen.hash, &head.hash, None)
        .await
        .expect("diff before prune");

    let deleted = prune_once(fixture.pool(), &fixture.track_id, 1).await;

    assert!(deleted > 0);
    assert!(commit_exists(fixture.pool(), &last_seen.hash).await);
    assert!(object_exists(fixture.pool(), &last_seen.tree_hash).await);
    assert!(object_exists(fixture.pool(), &last_seen.blob_hash).await);
    assert!(commit_exists(fixture.pool(), &head.hash).await);
    assert!(object_exists(fixture.pool(), &head.tree_hash).await);
    assert!(object_exists(fixture.pool(), &head.blob_hash).await);
    assert_eq!(
        track_vcs::diff(fixture.pool(), &last_seen.hash, &head.hash, None)
            .await
            .expect("diff after prune"),
        before_diff
    );
}

#[tokio::test]
async fn active_session_issued_turn_head_diff_still_works_after_prune_and_sweep() {
    let fixture = fixture_with_commits(6).await;
    let issued_turn_head = &fixture.commits[1];
    let head = fixture.commits.last().expect("head");
    let snapshot = harness_snapshot_with_heads(None, Some(&issued_turn_head.hash));
    insert_active_session(
        fixture.pool(),
        &fixture.track_id,
        "issued-turn-head",
        Some(&snapshot),
    )
    .await;
    let before_diff = track_vcs::diff(fixture.pool(), &issued_turn_head.hash, &head.hash, None)
        .await
        .expect("diff before prune");

    let deleted = prune_once(fixture.pool(), &fixture.track_id, 1).await;
    let swept = track_vcs::sweep_unreferenced_objects_once(fixture.pool())
        .await
        .expect("sweep");

    assert!(deleted > 0);
    assert!(swept > 0);
    assert!(commit_exists(fixture.pool(), &issued_turn_head.hash).await);
    assert!(object_exists(fixture.pool(), &issued_turn_head.tree_hash).await);
    assert!(object_exists(fixture.pool(), &issued_turn_head.blob_hash).await);
    assert_eq!(
        track_vcs::diff(fixture.pool(), &issued_turn_head.hash, &head.hash, None)
            .await
            .expect("diff after prune"),
        before_diff
    );
}

#[tokio::test]
async fn active_session_protects_distinct_last_seen_and_issued_turn_heads() {
    let fixture = fixture_with_commits(7).await;
    let issued_turn_head = &fixture.commits[1];
    let last_seen_head = &fixture.commits[3];
    let head = fixture.commits.last().expect("head");
    let snapshot =
        harness_snapshot_with_heads(Some(&last_seen_head.hash), Some(&issued_turn_head.hash));
    insert_active_session(
        fixture.pool(),
        &fixture.track_id,
        "both-endpoints",
        Some(&snapshot),
    )
    .await;
    let before_diff = track_vcs::diff(fixture.pool(), &issued_turn_head.hash, &head.hash, None)
        .await
        .expect("diff before prune");

    let deleted = prune_once(fixture.pool(), &fixture.track_id, 1).await;
    let swept = track_vcs::sweep_unreferenced_objects_once(fixture.pool())
        .await
        .expect("sweep");

    assert!(deleted > 0);
    assert!(swept > 0);
    assert!(!commit_exists(fixture.pool(), &fixture.commits[0].hash).await);
    assert!(commit_exists(fixture.pool(), &issued_turn_head.hash).await);
    assert!(object_exists(fixture.pool(), &issued_turn_head.tree_hash).await);
    assert!(object_exists(fixture.pool(), &issued_turn_head.blob_hash).await);
    assert!(commit_exists(fixture.pool(), &last_seen_head.hash).await);
    assert!(object_exists(fixture.pool(), &last_seen_head.tree_hash).await);
    assert!(object_exists(fixture.pool(), &last_seen_head.blob_hash).await);
    assert_eq!(
        track_vcs::diff(fixture.pool(), &issued_turn_head.hash, &head.hash, None)
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
    insert_active_session(fixture.pool(), &fixture.track_id, "floor", Some(&snapshot)).await;

    let deleted = prune_once(fixture.pool(), &fixture.track_id, 2).await;

    assert!(deleted > 0);
    for commit in &fixture.commits {
        let exists = commit_exists(fixture.pool(), &commit.hash).await;
        if commit.created_at >= floor_commit.created_at {
            assert!(exists, "{} should survive", commit.hash);
        } else {
            assert!(!exists, "{} should be pruned", commit.hash);
        }
    }

    let survivors = surviving_commits(fixture.pool(), &fixture.track_id).await;
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
async fn sweep_preserves_live_objects_from_other_tracks() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open sqlite repo");
    let area = repo
        .area_create(NewArea {
            name: "area".into(),
            color: "#336699".into(),
            sort: None,
        })
        .await
        .expect("create area");
    let track_a = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "track a".into(),
            sort: None,
            cwd: "/tmp/a".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("create track a");
    let track_b = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "track b".into(),
            sort: None,
            cwd: "/tmp/b".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("create track b");
    let track_a_commits = seed_linear_commits(repo.pool(), &track_a.id, 5).await;
    let track_b_commits = seed_linear_commits(repo.pool(), &track_b.id, 4).await;

    let deleted = prune_once(repo.pool(), &track_a.id, 1).await;
    let swept = track_vcs::sweep_unreferenced_objects_once(repo.pool())
        .await
        .expect("sweep");

    assert!(deleted > 0);
    assert!(swept > 0);
    assert!(!object_exists(repo.pool(), &track_a_commits[0].tree_hash).await);
    assert!(!object_exists(repo.pool(), &track_a_commits[0].blob_hash).await);
    for commit in &track_b_commits {
        assert!(commit_exists(repo.pool(), &commit.hash).await);
        assert!(object_exists(repo.pool(), &commit.tree_hash).await);
        assert!(object_exists(repo.pool(), &commit.blob_hash).await);
    }
    assert_eq!(
        track_vcs::head(repo.pool(), &track_b.id)
            .await
            .expect("head"),
        Some(track_b_commits.last().expect("track b head").hash.clone())
    );
}

#[tokio::test]
async fn sweep_preserves_shared_blob_referenced_by_kept_tree() {
    let (repo, track_id) = fresh_track().await;
    let base = now_ms() - (2 * SWEEP_GRACE_MS);
    let old_commit_hash = format!("{}-old-shared-commit", track_id.as_str());
    let kept_commit_hash = format!("{}-kept-shared-commit", track_id.as_str());
    let old_tree_hash = format!("{}-old-shared-tree", track_id.as_str());
    let kept_tree_hash = format!("{}-kept-shared-tree", track_id.as_str());
    let shared_blob_hash = format!("{}-shared-blob", track_id.as_str());
    let shared_blob_bytes = b"shared content\n".to_vec();
    let mut old_entries = BTreeMap::new();
    old_entries.insert(
        "old.txt".to_string(),
        ManifestEntry {
            blob_hash: shared_blob_hash.clone(),
            byte_len: shared_blob_bytes.len() as u64,
            content_type: "text/plain".into(),
        },
    );
    let mut kept_entries = BTreeMap::new();
    kept_entries.insert(
        "kept.txt".to_string(),
        ManifestEntry {
            blob_hash: shared_blob_hash.clone(),
            byte_len: shared_blob_bytes.len() as u64,
            content_type: "text/plain".into(),
        },
    );
    let old_tree_bytes = canonical_json_bytes(&TreeManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        entries: old_entries,
    })
    .expect("canonical old tree json");
    let kept_tree_bytes = canonical_json_bytes(&TreeManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        entries: kept_entries,
    })
    .expect("canonical kept tree json");

    let mut tx = repo.pool().begin().await.expect("begin seed shared blob");
    sqlx::query(
        r#"INSERT INTO track_vcs_objects (hash, kind, bytes, created_at)
           VALUES (?1, 'blob', ?2, ?3)"#,
    )
    .bind(&shared_blob_hash)
    .bind(&shared_blob_bytes)
    .bind(base)
    .execute(&mut *tx)
    .await
    .expect("insert shared blob");
    sqlx::query(
        r#"INSERT INTO track_vcs_objects (hash, kind, bytes, created_at)
           VALUES (?1, 'tree', ?2, ?3), (?4, 'tree', ?5, ?6)"#,
    )
    .bind(&old_tree_hash)
    .bind(&old_tree_bytes)
    .bind(base)
    .bind(&kept_tree_hash)
    .bind(&kept_tree_bytes)
    .bind(base + 1000)
    .execute(&mut *tx)
    .await
    .expect("insert trees");
    sqlx::query(
        r#"INSERT INTO track_vcs_commits (
               hash, track_id, parent_hash, tree_hash, manifest_schema_version,
               author, message, lifecycle, event_id, created_at
           )
           VALUES (?1, ?2, NULL, ?3, ?4, NULL, 'old shared blob', 'active', 1, ?5),
                  (?6, ?2, ?1, ?7, ?4, NULL, 'kept shared blob', 'active', 2, ?8)"#,
    )
    .bind(&old_commit_hash)
    .bind(track_id.as_str())
    .bind(&old_tree_hash)
    .bind(MANIFEST_SCHEMA_VERSION)
    .bind(base)
    .bind(&kept_commit_hash)
    .bind(&kept_tree_hash)
    .bind(base + 1000)
    .execute(&mut *tx)
    .await
    .expect("insert commits");
    sqlx::query(
        r#"INSERT INTO track_vcs_refs (track_id, head_hash, updated_event_id)
           VALUES (?1, ?2, 2)"#,
    )
    .bind(track_id.as_str())
    .bind(&kept_commit_hash)
    .execute(&mut *tx)
    .await
    .expect("insert ref");
    tx.commit().await.expect("commit seed shared blob");

    let deleted = prune_once(repo.pool(), &track_id, 1).await;
    let swept = track_vcs::sweep_unreferenced_objects_once(repo.pool())
        .await
        .expect("sweep");

    assert_eq!(deleted, 1);
    assert!(swept > 0);
    assert!(!commit_exists(repo.pool(), &old_commit_hash).await);
    assert!(!object_exists(repo.pool(), &old_tree_hash).await);
    assert!(commit_exists(repo.pool(), &kept_commit_hash).await);
    assert!(object_exists(repo.pool(), &kept_tree_hash).await);
    assert!(object_exists(repo.pool(), &shared_blob_hash).await);
}

#[tokio::test]
async fn sweep_reclaims_only_objects_reachable_from_pruned_commits() {
    let fixture = fixture_with_commits(6).await;
    let pruned = &fixture.commits[1];
    let kept_last_seen = &fixture.commits[3];
    let head = fixture.commits.last().expect("head");
    let snapshot = harness_snapshot(Some(&kept_last_seen.hash));
    insert_active_session(fixture.pool(), &fixture.track_id, "kept", Some(&snapshot)).await;
    let before_objects = object_count(fixture.pool()).await;

    let deleted_commits = prune_once(fixture.pool(), &fixture.track_id, 1).await;
    let deleted_objects = track_vcs::sweep_unreferenced_objects_once(fixture.pool())
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
        &fixture.track_id,
        "garbage",
        Some("{not-json"),
    )
    .await;

    let deleted = prune_once(fixture.pool(), &fixture.track_id, 1).await;

    assert_eq!(deleted, 0);
    assert_eq!(commit_count(fixture.pool(), &fixture.track_id).await, 5);
    for commit in &fixture.commits {
        assert!(commit_exists(fixture.pool(), &commit.hash).await);
    }
}

#[tokio::test]
async fn parseable_rejected_active_snapshots_keep_all_commits() {
    let cases = [
        (
            "schema-version",
            json!({
                "schema_version": 2,
                "mode": "harness",
                "last_seen_head": null,
                "issued_turn_head": null,
            }),
        ),
        (
            "mode",
            json!({
                "schema_version": 1,
                "mode": "worker",
                "last_seen_head": null,
                "issued_turn_head": null,
            }),
        ),
        (
            "last-seen-type",
            json!({
                "schema_version": 1,
                "mode": "harness",
                "last_seen_head": 123,
                "issued_turn_head": null,
            }),
        ),
        (
            "issued-turn-type",
            json!({
                "schema_version": 1,
                "mode": "harness",
                "last_seen_head": null,
                "issued_turn_head": ["not", "a", "hash"],
            }),
        ),
    ];

    for (suffix, snapshot) in cases {
        let fixture = fixture_with_commits(5).await;
        let snapshot = snapshot.to_string();
        let before_objects = object_count(fixture.pool()).await;
        insert_active_session(fixture.pool(), &fixture.track_id, suffix, Some(&snapshot)).await;

        let deleted = prune_once(fixture.pool(), &fixture.track_id, 1).await;

        assert_eq!(deleted, 0, "{suffix}");
        assert_eq!(
            commit_count(fixture.pool(), &fixture.track_id).await,
            5,
            "{suffix}"
        );
        assert_eq!(
            object_count(fixture.pool()).await,
            before_objects,
            "{suffix}"
        );
        for commit in &fixture.commits {
            assert!(
                commit_exists(fixture.pool(), &commit.hash).await,
                "{suffix}"
            );
        }
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
        &fixture.track_id,
        "null",
        Some(&null_snapshot),
    )
    .await;
    insert_active_session(
        fixture.pool(),
        &fixture.track_id,
        "absent",
        Some(&absent_snapshot),
    )
    .await;

    let deleted = prune_once(fixture.pool(), &fixture.track_id, 1).await;

    assert!(deleted > 0);
    assert!(commit_exists(fixture.pool(), &fixture.commits[4].hash).await);
    assert!(!commit_exists(fixture.pool(), &fixture.commits[0].hash).await);
}

#[tokio::test]
async fn active_last_seen_absent_from_commit_table_keeps_all_commits() {
    let fixture = fixture_with_commits(5).await;
    let snapshot = harness_snapshot(Some("missing-commit"));
    insert_active_session(
        fixture.pool(),
        &fixture.track_id,
        "missing",
        Some(&snapshot),
    )
    .await;

    let deleted = prune_once(fixture.pool(), &fixture.track_id, 1).await;

    assert_eq!(deleted, 0);
    assert_eq!(commit_count(fixture.pool(), &fixture.track_id).await, 5);
}

#[tokio::test]
async fn prune_and_sweep_are_idempotent() {
    let fixture = fixture_with_commits(6).await;

    let first_prune = prune_once(fixture.pool(), &fixture.track_id, 2).await;
    let second_prune = prune_once(fixture.pool(), &fixture.track_id, 2).await;
    let first_sweep = track_vcs::sweep_unreferenced_objects_once(fixture.pool())
        .await
        .expect("first sweep");
    let after_first_hashes = commit_hashes(fixture.pool(), &fixture.track_id).await;
    let after_first_objects = object_count(fixture.pool()).await;
    let second_sweep = track_vcs::sweep_unreferenced_objects_once(fixture.pool())
        .await
        .expect("second sweep");

    assert!(first_prune > 0);
    assert_eq!(second_prune, 0);
    assert!(first_sweep > 0);
    assert_eq!(second_sweep, 0);
    assert_eq!(
        commit_hashes(fixture.pool(), &fixture.track_id).await,
        after_first_hashes
    );
    assert_eq!(object_count(fixture.pool()).await, after_first_objects);
}

#[tokio::test]
async fn sweep_honors_object_created_at_grace_cutoff() {
    let fixture = fixture_with_commits(4).await;
    let old_orphan = format!("{}-old-orphan", fixture.track_id.as_str());
    let young_orphan = format!("{}-young-orphan", fixture.track_id.as_str());
    sqlx::query(
        r#"INSERT INTO track_vcs_objects (hash, kind, bytes, created_at)
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

    assert!(prune_once(fixture.pool(), &fixture.track_id, 1).await > 0);
    let swept = track_vcs::sweep_unreferenced_objects_once(fixture.pool())
        .await
        .expect("sweep");

    assert!(swept > 0);
    assert!(!object_exists(fixture.pool(), &old_orphan).await);
    assert!(object_exists(fixture.pool(), &young_orphan).await);
}

#[tokio::test]
async fn prune_no_op_cases_and_keep_zero_clamp() {
    let (repo, empty_track_id) = fresh_track().await;
    assert_eq!(prune_once(repo.pool(), &empty_track_id, 1).await, 0);

    let keep_large = fixture_with_commits(3).await;
    assert_eq!(
        prune_once(keep_large.pool(), &keep_large.track_id, 10).await,
        0
    );
    assert_eq!(
        commit_count(keep_large.pool(), &keep_large.track_id).await,
        3
    );

    let keep_zero = fixture_with_commits(3).await;
    let head = keep_zero.commits.last().expect("head").hash.clone();
    let deleted = prune_once(keep_zero.pool(), &keep_zero.track_id, 0).await;
    assert!(deleted > 0);
    assert!(commit_exists(keep_zero.pool(), &head).await);
    assert_eq!(
        track_vcs::head(keep_zero.pool(), &keep_zero.track_id)
            .await
            .expect("head"),
        Some(head)
    );
}
