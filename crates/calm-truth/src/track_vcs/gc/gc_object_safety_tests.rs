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
        .execute(&mut *tx)
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
