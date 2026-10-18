use super::gc_writer_tests::{Fixture, Phase, finish, start};
use crate::db::sqlite::begin_immediate_tx;
use crate::model::now_ms;
use crate::track_vcs;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn object_sweep_new_head_preserves_reactivated_old_tree_and_blob() {
    let f = Fixture::new().await;
    let old = f.tree("reactivated document", 1).await;
    f.age(&old).await;
    let orphan = f.orphan("unrelated orphan").await;
    let (pause, sweep) = start(f.repo.pool().clone(), vec![Phase::BeforeDelete]);
    pause.entered().await;
    f.publish(&old, Some(&f.commit)).await;
    pause.release();
    assert_eq!(finish(sweep).await.unwrap(), 1);
    assert!(f.exists(&old.tree_hash).await);
    for entry in old.manifest.entries.values() {
        assert!(f.exists(&entry.blob_hash).await);
    }
    assert!(!f.exists(&orphan).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn object_sweep_restored_ancestor_without_head_change_preserves_objects() {
    use crate::track_vcs::store::{CommitTreeMeta, commit_tree_at_tx};
    let f = Fixture::new().await;
    let ancestor_tree = f.tree("restored ancestor", 1).await;
    f.age(&ancestor_tree).await;
    let ancestor = f.publish(&ancestor_tree, None).await;
    let ancestor_created: i64 =
        sqlx::query_scalar("SELECT created_at FROM track_vcs_commits WHERE hash=?1")
            .bind(&ancestor)
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    let child = f.publish(&f.tree, Some(&ancestor)).await;
    sqlx::query("DELETE FROM track_vcs_commits WHERE hash=?1")
        .bind(&ancestor)
        .execute(f.repo.pool())
        .await
        .unwrap();
    let orphan = f.orphan("independent garbage").await;
    let (pause, sweep) = start(f.repo.pool().clone(), vec![Phase::BeforeDelete]);
    pause.entered().await;
    let mut tx = begin_immediate_tx(f.repo.pool()).await.unwrap();
    let restored = commit_tree_at_tx(
        &mut tx,
        &f.track,
        &ancestor_tree,
        CommitTreeMeta {
            parent_hash: None,
            author: None,
            event_id: None,
            message: "published",
            manifest_schema_version: 1,
            created_at: ancestor_created,
        },
    )
    .await
    .unwrap();
    // The owner restored the identical ancestor; retain the very same HEAD throughout the race.
    sqlx::query("UPDATE track_vcs_refs SET head_hash=?1 WHERE track_id=?2")
        .bind(&child)
        .bind(f.track.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    pause.release();
    let deleted = finish(sweep).await.unwrap();
    assert_eq!(restored, ancestor);
    assert_eq!(
        track_vcs::head(f.repo.pool(), &f.track).await.unwrap(),
        Some(child)
    );
    assert_eq!(deleted, 1);
    assert!(f.exists(&ancestor_tree.tree_hash).await);
    for entry in ancestor_tree.manifest.entries.values() {
        assert!(f.exists(&entry.blob_hash).await);
    }
    assert!(!f.exists(&orphan).await);
}

#[tokio::test]
async fn object_sweep_missing_and_wrong_kind_reachable_trees_fail_closed() {
    for wrong_kind in [false, true] {
        let f = Fixture::new().await;
        let orphan = f.orphan("must retain on corrupt roots").await;
        if wrong_kind {
            sqlx::query("UPDATE track_vcs_objects SET kind='blob' WHERE hash=?1")
                .bind(&f.tree.tree_hash)
                .execute(f.repo.pool())
                .await
                .unwrap();
        } else {
            sqlx::query("DELETE FROM track_vcs_objects WHERE hash=?1")
                .bind(&f.tree.tree_hash)
                .execute(f.repo.pool())
                .await
                .unwrap();
        }
        let error = track_vcs::sweep_unreferenced_objects_once(f.repo.pool())
            .await
            .unwrap_err();
        assert!(error.to_string().contains(&f.tree.tree_hash));
        assert!(f.exists(&orphan).await);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn object_sweep_cached_tree_removed_by_other_sweep_still_requires_presence() {
    let f = Fixture::new().await;
    f.age(&f.tree).await;
    let (pause, sweep) = start(f.repo.pool().clone(), vec![Phase::Parse]);
    pause.entered().await;
    let replacement = f.tree("new root", 1).await;
    f.publish(&replacement, None).await;
    track_vcs::sweep_unreferenced_objects_once(f.repo.pool())
        .await
        .unwrap();
    let old_tree_removed = !f.exists(&f.tree.tree_hash).await;
    sqlx::query("UPDATE track_vcs_refs SET head_hash=?1 WHERE track_id=?2")
        .bind(&f.commit)
        .bind(f.track.as_str())
        .execute(f.repo.pool())
        .await
        .unwrap();
    let orphan = f.orphan("garbage after the other sweep").await;
    pause.release();
    let error = finish(sweep).await.unwrap_err();
    assert!(old_tree_removed);
    assert!(error.to_string().contains(&f.tree.tree_hash));
    assert!(f.exists(&orphan).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn object_sweep_rechecks_candidate_grace_before_delete() {
    let f = Fixture::new().await;
    let orphan = f.orphan("refreshed creation stamp").await;
    let (pause, sweep) = start(f.repo.pool().clone(), vec![Phase::BeforeDelete]);
    pause.entered().await;
    sqlx::query("UPDATE track_vcs_objects SET created_at=?1 WHERE hash=?2")
        .bind(now_ms())
        .bind(&orphan)
        .execute(f.repo.pool())
        .await
        .unwrap();
    pause.release();
    assert_eq!(finish(sweep).await.unwrap(), 0);
    assert!(f.exists(&orphan).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn object_sweep_without_candidates_never_waits_for_writer() {
    let f = Fixture::new().await;
    let tx = begin_immediate_tx(f.repo.pool()).await.unwrap();
    let pool = f.repo.pool().clone();
    let mut sweep =
        tokio::spawn(async move { track_vcs::sweep_unreferenced_objects_once(&pool).await });
    let completed = tokio::time::timeout(Duration::from_secs(1), &mut sweep).await;
    tx.rollback().await.unwrap();
    if completed.is_err() {
        sweep.await.unwrap().unwrap();
    }
    assert!(
        matches!(completed, Ok(Ok(Ok(0)))),
        "no-delete sweep acquired writer: {completed:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn object_sweep_drains_multiple_batches_with_continuous_small_new_trees() {
    let f = Fixture::new().await;
    let mut orphans = Vec::new();
    for n in 0..600 {
        orphans.push(f.orphan(&format!("old garbage {n}")).await);
    }
    let (pause, sweep) = start(
        f.repo.pool().clone(),
        vec![Phase::BeforeDelete, Phase::BeforeDelete],
    );
    let mut parent = f.commit.clone();
    let mut new_trees = Vec::new();
    for n in 0..2 {
        pause.entered().await;
        let tree = f.tree(&format!("concurrent tree {n}"), 1).await;
        parent = f.publish(&tree, Some(&parent)).await;
        new_trees.push(tree);
        pause.release();
    }
    assert_eq!(finish(sweep).await.unwrap(), 600);
    for orphan in orphans {
        assert!(!f.exists(&orphan).await);
    }
    for tree in new_trees {
        assert!(f.exists(&tree.tree_hash).await);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn object_sweep_large_delta_count_and_bytes_are_observed_outside_writer() {
    for many in [false, true] {
        let f = Fixture::new().await;
        let orphan = f.orphan("requires final validation").await;
        let (pause, sweep) = start(
            f.repo.pool().clone(),
            vec![Phase::BeforeDelete, Phase::Parse],
        );
        pause.entered().await;
        let mut parent = f.commit.clone();
        for n in 0..if many { 33 } else { 1 } {
            let tree = f
                .tree(
                    &format!("oversized delta {n}"),
                    if many { 1 } else { 10_000 },
                )
                .await;
            parent = f.publish(&tree, Some(&parent)).await;
        }
        pause.release();
        pause.entered().await;
        let pool = f.repo.pool().clone();
        let mut writer = tokio::spawn(async move {
            let tx = begin_immediate_tx(&pool).await?;
            tx.rollback().await?;
            Ok::<_, crate::error::CalmError>(())
        });
        let released = tokio::time::timeout(Duration::from_secs(1), &mut writer).await;
        pause.release();
        let result = finish(sweep).await.unwrap();
        if released.is_err() {
            writer.await.unwrap().unwrap();
        }
        assert!(
            matches!(released, Ok(Ok(Ok(())))),
            "oversized delta retained writer: {released:?}"
        );
        assert_eq!(result, 1);
        assert!(!f.exists(&orphan).await);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn object_sweep_repeated_oversized_changes_defer_without_hot_loop_or_deletion() {
    let f = Fixture::new().await;
    let orphan = f.orphan("deferred old orphan").await;
    let (pause, sweep) = start(f.repo.pool().clone(), vec![Phase::BeforeDelete; 3]);
    let mut parent = f.commit.clone();
    for n in 0..3 {
        pause.entered().await;
        let tree = f.tree(&format!("large changing tree {n}"), 10_000).await;
        parent = f.publish(&tree, Some(&parent)).await;
        pause.release();
    }
    assert_eq!(finish(sweep).await.unwrap(), 0);
    assert!(f.exists(&orphan).await);
    assert_eq!(
        track_vcs::sweep_unreferenced_objects_once(f.repo.pool())
            .await
            .unwrap(),
        1
    );
    assert!(!f.exists(&orphan).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn object_sweep_later_failure_preserves_earlier_committed_batches() {
    let f = Fixture::new().await;
    for n in 0..600 {
        f.orphan(&format!("partial progress garbage {n}")).await;
    }
    let (pause, sweep) = start(
        f.repo.pool().clone(),
        vec![Phase::BeforeDelete, Phase::BeforeDelete],
    );
    pause.entered().await;
    pause.release();
    pause.entered().await;
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM track_vcs_objects WHERE created_at < ?1")
            .bind(now_ms() - 3_600_000)
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    sqlx::query("UPDATE track_vcs_objects SET kind='blob' WHERE hash=?1")
        .bind(&f.tree.tree_hash)
        .execute(f.repo.pool())
        .await
        .unwrap();
    pause.release();
    let error = finish(sweep).await.unwrap_err();
    assert!(
        remaining > 0 && remaining < 600,
        "first batch must commit independently"
    );
    assert!(error.to_string().contains(&f.tree.tree_hash));
    let after: i64 =
        sqlx::query_scalar("SELECT count(*) FROM track_vcs_objects WHERE created_at < ?1")
            .bind(now_ms() - 3_600_000)
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        after, remaining,
        "failed later validation must not undo prior safe progress"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn object_sweep_restored_cycle_and_duplicate_states_keep_reachable_objects() {
    use crate::track_vcs::store::{CommitTreeMeta, commit_tree_at_tx};
    let f = Fixture::new().await;
    let ancestor_tree = f.tree("restored cyclic ancestor", 1).await;
    let ancestor = f.publish(&ancestor_tree, Some(&f.commit)).await;
    let created: i64 = sqlx::query_scalar("SELECT created_at FROM track_vcs_commits WHERE hash=?1")
        .bind(&ancestor)
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    let middle_tree = f.tree("cycle middle", 1).await;
    let middle = f.publish(&middle_tree, Some(&ancestor)).await;
    let head = f.publish(&f.tree, Some(&middle)).await;
    // Deliberately seed a finite cyclic topology to pin corruption-safe traversal. The
    // original root and HEAD now have identical (parent, tree) state but distinct commit IDs.
    sqlx::query("UPDATE track_vcs_commits SET parent_hash=?1 WHERE hash=?2")
        .bind(&middle)
        .bind(&f.commit)
        .execute(f.repo.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM track_vcs_commits WHERE hash=?1")
        .bind(&ancestor)
        .execute(f.repo.pool())
        .await
        .unwrap();
    for tree in [&f.tree, &ancestor_tree, &middle_tree] {
        f.age(tree).await;
    }
    let orphan = f.orphan("independent cyclic graph garbage").await;
    let (pause, sweep) = start(f.repo.pool().clone(), vec![Phase::BeforeDelete]);
    pause.entered().await;
    let mut tx = begin_immediate_tx(f.repo.pool()).await.unwrap();
    let restored = commit_tree_at_tx(
        &mut tx,
        &f.track,
        &ancestor_tree,
        CommitTreeMeta {
            parent_hash: Some(&f.commit),
            author: None,
            event_id: None,
            message: "published",
            manifest_schema_version: 1,
            created_at: created,
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE track_vcs_refs SET head_hash=?1 WHERE track_id=?2")
        .bind(&head)
        .bind(f.track.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    pause.release();
    assert_eq!(finish(sweep).await.unwrap(), 1);
    assert_eq!(restored, ancestor);
    assert_eq!(
        track_vcs::head(f.repo.pool(), &f.track).await.unwrap(),
        Some(head)
    );
    for tree in [&f.tree, &ancestor_tree, &middle_tree] {
        assert!(f.exists(&tree.tree_hash).await);
        for entry in tree.manifest.entries.values() {
            assert!(f.exists(&entry.blob_hash).await);
        }
    }
    assert!(!f.exists(&orphan).await);
}

#[tokio::test]
async fn object_sweep_sparse_orphans_share_one_candidate_validation_batch() {
    use super::gc_writer_tests::DELETE_VALIDATIONS;
    use crate::track_vcs::{ManifestEntry, commit_tree, put_blob};
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let f = Fixture::new().await;
    let mut tx = begin_immediate_tx(f.repo.pool()).await.unwrap();
    let mut entries = BTreeMap::new();
    for n in 0..1024 {
        let text = format!("retained sparse-GC document {n}");
        let hash = put_blob(&mut tx, "blob", text.as_bytes()).await.unwrap();
        entries.insert(
            format!("documents/{n}.md"),
            ManifestEntry {
                blob_hash: hash,
                byte_len: text.len() as u64,
                content_type: "text/markdown".into(),
            },
        );
    }
    let live = crate::track_vcs::store::store_tree(&mut tx, 1, entries, now_ms())
        .await
        .unwrap();
    commit_tree(
        &mut tx,
        &f.track,
        None,
        &live,
        None,
        "sparse orphan fixture",
        1,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut orphans = BTreeSet::new();
    for n in 0..16 {
        orphans.insert(f.orphan(&format!("scattered old orphan {n}")).await);
    }
    let hashes: Vec<String> =
        sqlx::query_scalar("SELECT hash FROM track_vcs_objects ORDER BY hash")
            .fetch_all(f.repo.pool())
            .await
            .unwrap();
    // Prove this fixture exercises sparse candidates across multiple current 256-row index
    // pages. The performance assertion below counts actual production validation attempts.
    let pages: BTreeSet<_> = hashes
        .iter()
        .enumerate()
        .filter(|(_, hash)| orphans.contains(*hash))
        .map(|(rank, _)| rank / 256)
        .collect();
    assert!(
        pages.len() >= 3,
        "orphans must be scattered across multiple live-object pages"
    );
    let counter = Arc::new(AtomicUsize::new(0));
    let deleted = DELETE_VALIDATIONS
        .scope(
            counter.clone(),
            track_vcs::sweep_unreferenced_objects_once(f.repo.pool()),
        )
        .await
        .unwrap();
    assert_eq!(deleted, 16);
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "a sparse candidate batch must not revalidate ancestry once per index page"
    );
    assert!(f.exists(&live.tree_hash).await);
    for entry in live.manifest.entries.values() {
        assert!(f.exists(&entry.blob_hash).await);
    }
    for orphan in orphans {
        assert!(!f.exists(&orphan).await);
    }
}

#[tokio::test]
async fn object_sweep_stable_graph_reuses_one_complete_metadata_proof_across_batches() {
    use super::gc_writer_tests::{DELETE_VALIDATIONS, METADATA_QUERIES};
    use super::object_sweep::DELETE_BATCH_SIZE;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let f = Fixture::new().await;
    let roots_before: Vec<(String, String)> =
        sqlx::query_as("SELECT track_id, head_hash FROM track_vcs_refs ORDER BY track_id")
            .fetch_all(f.repo.pool())
            .await
            .unwrap();
    let retained: Vec<String> =
        sqlx::query_scalar("SELECT hash FROM track_vcs_objects ORDER BY hash")
            .fetch_all(f.repo.pool())
            .await
            .unwrap();
    let mut orphans = Vec::new();
    for n in 0..DELETE_BATCH_SIZE * 2 {
        orphans.push(f.orphan(&format!("stable proof old garbage {n}")).await);
    }
    let validations = Arc::new(AtomicUsize::new(0));
    let queries = Arc::new(AtomicUsize::new(0));
    let deleted = DELETE_VALIDATIONS
        .scope(
            validations.clone(),
            METADATA_QUERIES.scope(
                queries.clone(),
                track_vcs::sweep_unreferenced_objects_once(f.repo.pool()),
            ),
        )
        .await
        .unwrap();
    assert_eq!(deleted, orphans.len() as u64);
    for orphan in orphans {
        assert!(!f.exists(&orphan).await);
    }
    for hash in retained {
        assert!(f.exists(&hash).await);
    }
    let roots_after: Vec<(String, String)> =
        sqlx::query_as("SELECT track_id, head_hash FROM track_vcs_refs ORDER BY track_id")
            .fetch_all(f.repo.pool())
            .await
            .unwrap();
    assert_eq!(roots_after, roots_before);
    assert_eq!(validations.load(Ordering::SeqCst), 2);
    assert_eq!(
        queries.load(Ordering::SeqCst),
        1,
        "a stable graph needs one complete proof, reused across actual deletion batches"
    );
}

#[tokio::test]
async fn object_sweep_same_tree_head_extensions_reuse_one_complete_proof_across_batches() {
    use super::object_sweep::DELETE_BATCH_SIZE;

    let f = Fixture::new().await;
    let retained: Vec<String> =
        sqlx::query_scalar("SELECT hash FROM track_vcs_objects ORDER BY hash")
            .fetch_all(f.repo.pool())
            .await
            .unwrap();
    let roots_before: Vec<(String, String, Option<i64>)> = sqlx::query_as(
        "SELECT track_id, head_hash, updated_event_id FROM track_vcs_refs ORDER BY track_id",
    )
    .fetch_all(f.repo.pool())
    .await
    .unwrap();
    let commits_before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM track_vcs_commits WHERE track_id=?1")
            .bind(f.track.as_str())
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    let mut orphans = Vec::new();
    for n in 0..DELETE_BATCH_SIZE * 2 {
        orphans.push(f.orphan(&format!("same-tree prefix old garbage {n}")).await);
    }
    let (pause, sweep) = start(
        f.repo.pool().clone(),
        vec![Phase::BeforeDelete, Phase::BeforeDelete],
    );
    let mut history = Vec::new();
    for batch in 0..2 {
        pause.entered().await;
        let mut remaining = 0;
        for orphan in &orphans {
            remaining += usize::from(f.exists(orphan).await);
        }
        assert_eq!(remaining, DELETE_BATCH_SIZE * (2 - batch));
        let parent = track_vcs::head(f.repo.pool(), &f.track)
            .await
            .unwrap()
            .unwrap();
        let commit = f.publish(&f.tree, Some(&parent)).await;
        history.push((commit, parent));
        pause.release();
    }
    assert_eq!(finish(sweep).await.unwrap(), orphans.len() as u64);
    for orphan in orphans {
        assert!(!f.exists(&orphan).await);
    }
    let objects_after: Vec<String> =
        sqlx::query_scalar("SELECT hash FROM track_vcs_objects ORDER BY hash")
            .fetch_all(f.repo.pool())
            .await
            .unwrap();
    assert_eq!(objects_after, retained);
    assert_eq!(history[0].1, f.commit);
    assert_eq!(history[1].1, history[0].0);
    for (commit, parent) in &history {
        assert_ne!(commit, parent);
        let record = track_vcs::commit_record(f.repo.pool(), commit)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.parent_hash.as_ref(), Some(parent));
        assert_eq!(record.tree_hash, f.tree.tree_hash);
        assert_eq!(record.event_id, None);
        assert_eq!(record.message.as_deref(), Some("published"));
    }
    let commits_after: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM track_vcs_commits WHERE track_id=?1")
            .bind(f.track.as_str())
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    assert_eq!(commits_after, commits_before + 2);
    let mut expected_roots = roots_before;
    let target = expected_roots
        .iter_mut()
        .find(|row| row.0 == f.track.as_str())
        .unwrap();
    target.1 = history[1].0.clone();
    target.2 = None;
    let roots_after: Vec<(String, String, Option<i64>)> = sqlx::query_as(
        "SELECT track_id, head_hash, updated_event_id FROM track_vcs_refs ORDER BY track_id",
    )
    .fetch_all(f.repo.pool())
    .await
    .unwrap();
    assert_eq!(roots_after, expected_roots);
    assert_eq!(
        pause.metadata_queries(),
        1,
        "same-tree head extensions preserve one complete live-tree proof"
    );
}
