//! Public GC safety against source transitions which resemble same-tree head extensions.
use super::gc_epoch_tests::{epoch, replace_commit};
use super::gc_writer_tests::{Fixture, Phase, finish, start};
use crate::db::sqlite::begin_immediate_tx;
use crate::track_vcs;

#[tokio::test]
async fn gc_epoch_self_alias_head_replacement_protects_newly_rooted_tree() {
    let f = Fixture::new().await;
    let new = f.tree("self-alias target tree", 1).await;
    f.age(&new).await;
    let orphan = f.orphan("self-alias unrelated garbage").await;
    let (pause, sweep) = start(f.repo.db().clone(), vec![Phase::BeforeDelete]);
    pause.entered().await;
    let before = epoch(&f).await;
    replace_commit(
        &f,
        &f.commit,
        f.track.as_str(),
        Some(&f.commit),
        &new.tree_hash,
    )
    .await
    .unwrap();
    assert!(epoch(&f).await > before);
    pause.release();
    assert_eq!(finish(sweep).await.unwrap(), 1);
    assert!(f.exists(&new.tree_hash).await);
    for entry in new.manifest.entries.values() {
        assert!(f.exists(&entry.blob_hash).await);
    }
    assert!(!f.exists(&orphan).await);
    let record = track_vcs::commit_record(f.repo.pool(), &f.commit)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.parent_hash.as_ref(), Some(&f.commit));
    assert_eq!(record.tree_hash, new.tree_hash);
}

#[tokio::test]
async fn gc_epoch_nondirect_same_tree_ref_target_protects_unseen_ancestor() {
    let f = Fixture::new().await;
    let ancestor = f.tree("unseen nondirect ancestor", 1).await;
    f.age(&ancestor).await;
    replace_commit(
        &f,
        "detached-ancestor",
        f.track.as_str(),
        None,
        &ancestor.tree_hash,
    )
    .await
    .unwrap();
    replace_commit(
        &f,
        "nondirect-child",
        f.track.as_str(),
        Some("detached-ancestor"),
        &f.tree.tree_hash,
    )
    .await
    .unwrap();
    let orphan = f.orphan("nondirect unrelated garbage").await;
    let (pause, sweep) = start(f.repo.db().clone(), vec![Phase::BeforeDelete]);
    pause.entered().await;
    let before = epoch(&f).await;
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    sqlx::query("UPDATE track_vcs_refs SET head_hash='nondirect-child' WHERE track_id=?1")
        .bind(f.track.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(epoch(&f).await > before);
    pause.release();
    assert_eq!(finish(sweep).await.unwrap(), 1);
    assert!(f.exists(&ancestor.tree_hash).await);
    for entry in ancestor.manifest.entries.values() {
        assert!(f.exists(&entry.blob_hash).await);
    }
    assert!(!f.exists(&orphan).await);
    assert_eq!(
        track_vcs::head(f.repo.pool(), &f.track)
            .await
            .unwrap()
            .as_deref(),
        Some("nondirect-child")
    );
}

#[tokio::test]
async fn gc_epoch_same_tree_ancestor_replacement_cycle_preserves_cached_tree_coverage() {
    let f = Fixture::new().await;
    let old = f.tree("ancestor replaced by current-root tree", 1).await;
    f.age(&old).await;
    let ancestor = f.publish(&old, None).await;
    let head = f.publish(&f.tree, Some(&ancestor)).await;
    let orphan = f.orphan("cycle unrelated garbage").await;
    let (pause, sweep) = start(f.repo.db().clone(), vec![Phase::BeforeDelete]);
    pause.entered().await;
    let before = epoch(&f).await;
    replace_commit(
        &f,
        &ancestor,
        f.track.as_str(),
        Some(&head),
        &f.tree.tree_hash,
    )
    .await
    .unwrap();
    assert_eq!(epoch(&f).await, before);
    pause.release();
    assert_eq!(finish(sweep).await.unwrap(), 1);
    assert_eq!(pause.metadata_queries(), 1);
    assert_eq!(
        track_vcs::head(f.repo.pool(), &f.track).await.unwrap(),
        Some(head.clone())
    );
    let record = track_vcs::commit_record(f.repo.pool(), &ancestor)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.parent_hash, Some(head));
    assert_eq!(record.tree_hash, f.tree.tree_hash);
    // Old conservative marks may retain detached trees for this invocation.
    for tree in [&f.tree, &old] {
        assert!(f.exists(&tree.tree_hash).await);
        for entry in tree.manifest.entries.values() {
            assert!(f.exists(&entry.blob_hash).await);
        }
    }
    assert!(!f.exists(&orphan).await);
}

#[tokio::test]
async fn gc_epoch_replacing_another_root_uses_global_tree_coverage() {
    let f = Fixture::new().await;
    let old = f.tree("second global root tree", 1).await;
    f.age(&old).await;
    replace_commit(&f, "second-root-head", "second-root", None, &old.tree_hash)
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    sqlx::query(
        "INSERT INTO track_vcs_refs(track_id,head_hash) VALUES('second-root','second-root-head')",
    )
    .execute(&mut **tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let orphan = f.orphan("global roots unrelated garbage").await;
    let (pause, sweep) = start(f.repo.db().clone(), vec![Phase::BeforeDelete]);
    pause.entered().await;
    let before = epoch(&f).await;
    replace_commit(
        &f,
        "second-root-head",
        f.track.as_str(),
        Some(&f.commit),
        &f.tree.tree_hash,
    )
    .await
    .unwrap();
    assert_eq!(epoch(&f).await, before);
    pause.release();
    assert_eq!(finish(sweep).await.unwrap(), 1);
    assert_eq!(pause.metadata_queries(), 1);
    assert_eq!(
        track_vcs::head(f.repo.pool(), &f.track).await.unwrap(),
        Some(f.commit.clone())
    );
    let second: String =
        sqlx::query_scalar("SELECT head_hash FROM track_vcs_refs WHERE track_id='second-root'")
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    assert_eq!(second, "second-root-head");
    for tree in [&f.tree, &old] {
        assert!(f.exists(&tree.tree_hash).await);
        for entry in tree.manifest.entries.values() {
            assert!(f.exists(&entry.blob_hash).await);
        }
    }
    assert!(!f.exists(&orphan).await);
}
