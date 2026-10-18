use super::gc_writer_tests::{Fixture, Phase, finish, start};
use crate::db::sqlite::begin_immediate_tx;
use crate::track_vcs;

#[tokio::test]
async fn object_sweep_changed_external_bracket_does_not_certify_stale_ancestry() {
    let f = Fixture::new().await;
    let old = f.tree("activated during metadata bracket", 1).await;
    f.age(&old).await;
    let orphan = f.orphan("bracket independent orphan").await;
    let (pause, sweep) = start(f.repo.db().clone(), vec![Phase::AfterMetadata]);
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

#[tokio::test]
async fn object_sweep_required_revision_error_never_falls_back_to_unversioned_proof() {
    let f = Fixture::new().await;
    let orphan = f.orphan("must retain without required state").await;
    // Explicit corruption of this isolated test database, not an ordinary supported DML path.
    sqlx::query("DROP TABLE track_vcs_gc_state")
        .execute(f.repo.pool())
        .await
        .unwrap();
    assert!(
        track_vcs::sweep_unreferenced_objects_once(f.repo.db())
            .await
            .is_err()
    );
    assert!(f.exists(&orphan).await);
}

#[tokio::test]
async fn object_sweep_revision_mismatch_after_admission_preparation_rechecks_cached_tree() {
    let f = Fixture::new().await;
    let orphan = f.orphan("retain when cached root is invalidated").await;
    let (pause, sweep) = start(f.repo.db().clone(), vec![Phase::BeforeDelete]);
    pause.entered().await;
    sqlx::query("UPDATE track_vcs_objects SET kind='blob' WHERE hash=?1")
        .bind(&f.tree.tree_hash)
        .execute(f.repo.pool())
        .await
        .unwrap();
    pause.release();
    let error = finish(sweep).await.unwrap_err();
    assert!(error.to_string().contains(&f.tree.tree_hash));
    assert!(f.exists(&orphan).await);
}

#[tokio::test]
async fn object_sweep_fresh_external_proof_avoids_repeating_full_query_under_writer() {
    let f = Fixture::new().await;
    let activated = f.tree("fresh externally proved tree", 1).await;
    f.age(&activated).await;
    let orphan = f.orphan("fresh proof garbage").await;
    let (pause, sweep) = start(f.repo.db().clone(), vec![Phase::Parse, Phase::BeforeDelete]);
    pause.entered().await;
    f.publish(&activated, Some(&f.commit)).await;
    // Fresh direct publications now have a bounded frontier proof. An actual old-row
    // mutation forces this test's full outside-preparation branch instead.
    invalidate_frontier(&f).await;
    pause.release();
    pause.entered().await;
    // Initial proof's revision is old. A complete Fresh proof was acquired outside BEGIN.
    assert_eq!(pause.metadata_queries(), 2);
    pause.release();
    assert_eq!(finish(sweep).await.unwrap(), 1);
    assert_eq!(pause.metadata_queries(), 2);
    assert!(!f.exists(&orphan).await);
    assert!(f.exists(&activated.tree_hash).await);
    for entry in activated.manifest.entries.values() {
        assert!(f.exists(&entry.blob_hash).await);
    }
}

#[tokio::test]
async fn object_sweep_unstable_fresh_bracket_uses_complete_writer_fallback() {
    let f = Fixture::new().await;
    let first = f.tree("first fresh tree", 1).await;
    let second = f.tree("second tree invalidates fresh bracket", 1).await;
    f.age(&first).await;
    f.age(&second).await;
    let orphan = f.orphan("unstable proof garbage").await;
    let (pause, sweep) = start(
        f.repo.db().clone(),
        vec![Phase::Parse, Phase::AfterMetadata],
    );
    pause.entered().await;
    let first_commit = f.publish(&first, Some(&f.commit)).await;
    invalidate_frontier(&f).await;
    pause.release();
    pause.entered().await;
    f.publish(&second, Some(&first_commit)).await;
    invalidate_frontier(&f).await;
    pause.release();
    assert_eq!(finish(sweep).await.unwrap(), 1);
    assert_eq!(pause.metadata_queries(), 3);
    assert!(!f.exists(&orphan).await);
    for tree in [&first, &second] {
        assert!(f.exists(&tree.tree_hash).await);
        for entry in tree.manifest.entries.values() {
            assert!(f.exists(&entry.blob_hash).await);
        }
    }
}

#[tokio::test]
async fn object_sweep_metadata_bracket_observes_committed_head_aba() {
    let f = Fixture::new().await;
    let orphan = f.orphan("ABA garbage").await;
    let (pause, sweep) = start(f.repo.db().clone(), vec![Phase::AfterMetadata]);
    pause.entered().await;
    f.publish(&f.tree, Some(&f.commit)).await;
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    sqlx::query("UPDATE track_vcs_refs SET head_hash=?1 WHERE track_id=?2")
        .bind(&f.commit)
        .bind(f.track.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    pause.release();
    assert_eq!(finish(sweep).await.unwrap(), 1);
    // Same HEAD cannot hide the two committed mutations from the initial bracket.
    assert_eq!(pause.metadata_queries(), 2);
    assert_eq!(
        track_vcs::head(f.repo.pool(), &f.track).await.unwrap(),
        Some(f.commit.clone())
    );
    assert!(!f.exists(&orphan).await);
}

#[tokio::test]
async fn object_sweep_missing_revision_row_fails_closed_before_deletion() {
    let f = Fixture::new().await;
    let orphan = f.orphan("required row garbage").await;
    f.remove_required_revision().await;
    assert!(
        track_vcs::sweep_unreferenced_objects_once(f.repo.db())
            .await
            .is_err()
    );
    assert!(f.exists(&orphan).await);
}

// The real storage mutation invalidates both required proofs without changing tree contents.
async fn invalidate_frontier(f: &Fixture) {
    sqlx::query("UPDATE track_vcs_commits SET message=message || ' changed' WHERE hash=?1")
        .bind(&f.commit)
        .execute(f.repo.pool())
        .await
        .unwrap();
}
