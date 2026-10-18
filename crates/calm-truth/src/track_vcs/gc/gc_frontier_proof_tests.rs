//! Real public sweeps pin frontier safety, not a copy of the graph proof algorithm.
use super::gc_frontier_schema_tests::{legacy, no_pending, raw_commit, raw_head, unknown};
use super::gc_writer_tests::{Fixture, Phase, finish, start};
use crate::db::sqlite::begin_immediate_tx;
use crate::track_vcs;

#[tokio::test]
async fn object_sweep_frontier_same_tree_head_aba_cannot_hide_restored_cycle_tree() {
    let f = Fixture::new().await;
    let restored = f.tree("new tree hidden by same-tree ABA", 1).await;
    f.age(&restored).await;
    let orphan = f.orphan("ABA independent garbage").await;
    sqlx::query("UPDATE track_vcs_commits SET parent_hash='cycle-D' WHERE hash=?1")
        .bind(&f.commit)
        .execute(f.repo.pool())
        .await
        .unwrap();
    let (pause, sweep) = start(f.repo.db().clone(), vec![Phase::BeforeDelete]);
    pause.entered().await;
    let u = unknown(&f).await;
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    raw_commit(&mut tx, &f, "cycle-C", Some(&f.commit), &restored.tree_hash).await;
    raw_head(&mut tx, &f, "cycle-C").await;
    raw_commit(&mut tx, &f, "cycle-D", Some("cycle-C"), &f.tree.tree_hash).await;
    raw_head(&mut tx, &f, "cycle-D").await;
    tx.commit().await.unwrap();
    assert_eq!(unknown(&f).await, u); // Both genuine fresh direct publications.
    sqlx::query("UPDATE track_vcs_refs SET head_hash=?1 WHERE track_id=?2")
        .bind(&f.commit)
        .bind(f.track.as_str())
        .execute(f.repo.pool())
        .await
        .unwrap();
    assert!(unknown(&f).await > u); // Existing same-tree target is NOT fresh.
    pause.release();
    assert_eq!(finish(sweep).await.unwrap(), 1);
    assert!(!f.exists(&orphan).await);
    assert!(f.exists(&restored.tree_hash).await);
    for entry in restored.manifest.entries.values() {
        assert!(f.exists(&entry.blob_hash).await);
    }
    assert_eq!(
        track_vcs::head(f.repo.pool(), &f.track).await.unwrap(),
        Some(f.commit)
    );
}

#[tokio::test]
async fn object_sweep_frontier_missing_raw_head_is_not_an_old_covered_cut() {
    let f = Fixture::new().await;
    let restored = f
        .tree("tree restored under a formerly missing raw root", 1)
        .await;
    f.age(&restored).await;
    let orphan = f.orphan("missing root independent garbage").await;
    {
        let mut connection = f.repo.pool().acquire().await.unwrap();
        sqlx::query("PRAGMA foreign_keys=OFF")
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO track_vcs_refs(track_id,head_hash) VALUES('missing-root','restored-H')",
        )
        .execute(&mut *connection)
        .await
        .unwrap();
        sqlx::query("PRAGMA foreign_keys=ON")
            .execute(&mut *connection)
            .await
            .unwrap();
    }
    let (pause, sweep) = start(f.repo.db().clone(), vec![Phase::BeforeDelete]);
    pause.entered().await;
    let u = unknown(&f).await;
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    raw_commit(
        &mut tx,
        &f,
        "restored-H",
        Some(&f.commit),
        &restored.tree_hash,
    )
    .await;
    // Consume the witness atomically: both actual roots now expose H with no pending
    // mismatch to force an outside full proof. Only a stored baseline cut may stop here.
    raw_head(&mut tx, &f, "restored-H").await;
    tx.commit().await.unwrap();
    assert_eq!(unknown(&f).await, u);
    no_pending(&f).await;
    pause.release();
    assert_eq!(finish(sweep).await.unwrap(), 1);
    assert!(!f.exists(&orphan).await);
    assert!(f.exists(&restored.tree_hash).await);
    for entry in restored.manifest.entries.values() {
        assert!(f.exists(&entry.blob_hash).await);
    }
}

#[tokio::test]
async fn object_sweep_frontier_detached_pending_is_not_a_root_or_current_parent_contract() {
    let f = Fixture::new().await;
    let orphan = f.orphan("unpublished missing-tree garbage").await;
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    raw_commit(&mut tx, &f, "other-head", None, &f.tree.tree_hash).await;
    raw_commit(
        &mut tx,
        &f,
        "detached-pending",
        Some(&f.commit),
        "nonexistent-unrooted-tree",
    )
    .await;
    raw_head(&mut tx, &f, "other-head").await; // Unknown retarget; pending's parent is now old.
    tx.commit().await.unwrap();
    assert_eq!(
        track_vcs::sweep_unreferenced_objects_once(f.repo.db())
            .await
            .unwrap(),
        1
    );
    assert!(!f.exists(&orphan).await);
    assert!(f.exists(&f.tree.tree_hash).await);
    let pending: String = sqlx::query_scalar("SELECT hash FROM track_vcs_gc_pending")
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    assert_eq!(pending, "detached-pending");
}

#[tokio::test]
async fn object_sweep_frontier_invalid_fk_disabled_pointer_fails_closed() {
    let f = Fixture::new().await;
    let orphan = f.orphan("retain when pending pointer absent").await;
    {
        let mut connection = f.repo.pool().acquire().await.unwrap();
        sqlx::query("PRAGMA foreign_keys=OFF")
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::query("INSERT INTO track_vcs_gc_pending VALUES(1,'missing-pointer',?1)")
            .bind(f.track.as_str())
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::query("PRAGMA foreign_keys=ON")
            .execute(&mut *connection)
            .await
            .unwrap();
    }
    let error = track_vcs::sweep_unreferenced_objects_once(f.repo.db())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("pending GC publication"));
    assert!(f.exists(&orphan).await);
}

#[tokio::test]
async fn object_sweep_frontier_legacy_certificate_rebases_after_conservative_unknown_bump() {
    let f = Fixture::new().await;
    let orphan = f.orphan("legacy-only valid garbage").await;
    let (pause, sweep) = start(f.repo.db().clone(), vec![Phase::BeforeDelete]);
    pause.entered().await;
    let u = unknown(&f).await;
    let r = legacy(&f).await;
    sqlx::query(
        "INSERT OR IGNORE INTO track_vcs_commits
         SELECT * FROM track_vcs_commits WHERE hash=?1",
    )
    .bind(&f.commit)
    .execute(f.repo.pool())
    .await
    .unwrap();
    assert!(unknown(&f).await > u);
    assert_eq!(legacy(&f).await, r); // Source row was ignored; released proof is sufficient.
    no_pending(&f).await;
    pause.release();
    assert_eq!(finish(sweep).await.unwrap(), 1);
    assert_eq!(pause.metadata_queries(), 1);
    assert!(!f.exists(&orphan).await);
}

#[tokio::test]
async fn object_sweep_frontier_prefix_round_overflow_rebaselines_outside_and_progresses() {
    let f = Fixture::new().await;
    let changed = f
        .tree("bounded prefix with many same-tree commits", 1)
        .await;
    f.age(&changed).await;
    let orphan = f.orphan("bounded prefix garbage").await;
    let (pause, sweep) = start(f.repo.db().clone(), vec![Phase::BeforeDelete]);
    pause.entered().await;
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    let mut parent = f.commit.clone();
    for event in 0..34 {
        parent = track_vcs::commit_tree(
            &mut tx,
            &f.track,
            Some(&parent),
            &changed,
            Some(event),
            "bounded chain",
            1,
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    pause.release();
    assert_eq!(finish(sweep).await.unwrap(), 1);
    assert_eq!(pause.metadata_queries(), 2); // No silent truncation or full writer fallback.
    assert!(!f.exists(&orphan).await);
    assert!(f.exists(&changed.tree_hash).await);
    for entry in changed.manifest.entries.values() {
        assert!(f.exists(&entry.blob_hash).await);
    }
    assert_eq!(
        track_vcs::head(f.repo.pool(), &f.track).await.unwrap(),
        Some(parent)
    );
}

#[tokio::test]
async fn object_sweep_frontier_unknown_bytes_require_reobserving_cached_payload() {
    let f = Fixture::new().await;
    let changed = f.tree("new reference inside old hash bytes", 1).await;
    f.age(&changed).await;
    let orphan = f.orphan("cached bytes independent garbage").await;
    let (pause, sweep) = start(f.repo.db().clone(), vec![Phase::Parse]);
    pause.entered().await; // Old bytes have been fetched but not parsed.
    sqlx::query("UPDATE track_vcs_objects SET bytes=?1 WHERE hash=?2")
        .bind(serde_json::to_vec(&changed.manifest).unwrap())
        .bind(&f.tree.tree_hash)
        .execute(f.repo.pool())
        .await
        .unwrap();
    pause.release();
    assert_eq!(finish(sweep).await.unwrap(), 2); // Detached changed tree plus independent blob.
    assert!(!f.exists(&orphan).await);
    assert!(!f.exists(&changed.tree_hash).await);
    assert!(f.exists(&f.tree.tree_hash).await);
    for entry in changed.manifest.entries.values() {
        assert!(f.exists(&entry.blob_hash).await);
    }
}
