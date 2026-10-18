//! Real migrated SQLite coverage for the fresh-publication producer protocol.
use super::gc_writer_tests::Fixture;
use crate::db::sqlite::begin_immediate_tx;
use sqlx::{Sqlite, Transaction};

pub(super) async fn unknown(f: &Fixture) -> i64 {
    sqlx::query_scalar("SELECT revision FROM track_vcs_gc_frontier_state WHERE singleton=1")
        .fetch_one(f.repo.pool())
        .await
        .unwrap()
}
pub(super) async fn legacy(f: &Fixture) -> i64 {
    sqlx::query_scalar("SELECT revision FROM track_vcs_gc_state WHERE singleton=1")
        .fetch_one(f.repo.pool())
        .await
        .unwrap()
}
pub(super) async fn raw_commit(
    tx: &mut Transaction<'_, Sqlite>,
    f: &Fixture,
    hash: &str,
    parent: Option<&str>,
    tree: &str,
) {
    sqlx::query(
        "INSERT INTO track_vcs_commits
         (hash,track_id,parent_hash,tree_hash,manifest_schema_version,created_at)
         VALUES(?1,?2,?3,?4,1,0)",
    )
    .bind(hash)
    .bind(f.track.as_str())
    .bind(parent)
    .bind(tree)
    .execute(&mut **tx)
    .await
    .unwrap();
}
pub(super) async fn raw_head(tx: &mut Transaction<'_, Sqlite>, f: &Fixture, hash: &str) {
    sqlx::query("UPDATE track_vcs_refs SET head_hash=?1 WHERE track_id=?2")
        .bind(hash)
        .bind(f.track.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
}
pub(super) async fn no_pending(f: &Fixture) {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM track_vcs_gc_pending")
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn object_sweep_frontier_real_publication_preserves_unknown_and_legacy_contracts() {
    let f = Fixture::new().await;
    let changed = f.tree("different public content", 1).await;
    let u = unknown(&f).await;
    let r = legacy(&f).await;
    let same = f.publish(&f.tree, Some(&f.commit)).await;
    assert_eq!(unknown(&f).await, u);
    assert_eq!(legacy(&f).await, r);
    no_pending(&f).await;
    let next = f.publish(&changed, Some(&same)).await;
    assert_eq!(unknown(&f).await, u);
    assert!(legacy(&f).await > r); // Old171 consumers must still see actual growth.
    no_pending(&f).await;
    assert_eq!(
        crate::track_vcs::head(f.repo.pool(), &f.track)
            .await
            .unwrap(),
        Some(next)
    );
}

#[tokio::test]
async fn object_sweep_frontier_fresh_slot_rejects_forgery_and_conflict_bypass() {
    let f = Fixture::new().await;
    for conflict in ["", " OR IGNORE", " OR REPLACE"] {
        let sql = format!("INSERT{conflict} INTO track_vcs_gc_pending VALUES(1,?1,?2)");
        assert!(
            sqlx::query(&sql)
                .bind(&f.commit)
                .bind(f.track.as_str())
                .execute(f.repo.pool())
                .await
                .is_err()
        );
    }
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    raw_commit(
        &mut tx,
        &f,
        "unpublished",
        Some(&f.commit),
        &f.tree.tree_hash,
    )
    .await;
    tx.commit().await.unwrap(); // The deferred pointer now names the source row.
    for conflict in ["", " OR IGNORE", " OR REPLACE"] {
        let sql = format!("UPDATE{conflict} track_vcs_gc_pending SET hash='missing'");
        assert!(sqlx::query(&sql).execute(f.repo.pool()).await.is_err());
        let sql = format!("INSERT{conflict} INTO track_vcs_gc_pending VALUES(1,'missing',?1)");
        assert!(
            sqlx::query(&sql)
                .bind(f.track.as_str())
                .execute(f.repo.pool())
                .await
                .is_err()
        );
    }
    let u = unknown(&f).await;
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    raw_commit(
        &mut tx,
        &f,
        "replacement-slot",
        Some(&f.commit),
        &f.tree.tree_hash,
    )
    .await;
    tx.commit().await.unwrap();
    assert!(unknown(&f).await > u); // Losing even an unchanged-tree unpublished witness signals.
    let pending: String = sqlx::query_scalar("SELECT hash FROM track_vcs_gc_pending")
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    assert_eq!(pending, "replacement-slot");
}

#[tokio::test]
async fn object_sweep_frontier_source_delete_reincarnation_and_ignore_invalidate() {
    let f = Fixture::new().await;
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    raw_commit(
        &mut tx,
        &f,
        "reincarnated",
        Some(&f.commit),
        &f.tree.tree_hash,
    )
    .await;
    tx.commit().await.unwrap();
    let u = unknown(&f).await;
    sqlx::query("DELETE FROM track_vcs_commits WHERE hash='reincarnated'")
        .execute(f.repo.pool())
        .await
        .unwrap();
    assert!(unknown(&f).await > u);
    no_pending(&f).await;
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    raw_commit(
        &mut tx,
        &f,
        "reincarnated",
        Some(&f.commit),
        &f.tree.tree_hash,
    )
    .await;
    tx.commit().await.unwrap();
    for conflict in ["IGNORE", "REPLACE"] {
        let u = unknown(&f).await;
        let sql = format!(
            "INSERT OR {conflict} INTO track_vcs_commits SELECT * FROM track_vcs_commits WHERE hash='reincarnated'"
        );
        sqlx::query(&sql).execute(f.repo.pool()).await.unwrap();
        assert!(unknown(&f).await > u);
        no_pending(&f).await;
    }
}

#[tokio::test]
async fn object_sweep_frontier_actual_not_null_admission_preserves_ignored_rows() {
    let f = Fixture::new().await;
    // Every actual NOT NULL commit field; hash separately remains a nullable TEXT PK.
    for field in [
        "track_id",
        "tree_hash",
        "manifest_schema_version",
        "created_at",
        "hash",
    ] {
        let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
        sqlx::query(
            "INSERT OR IGNORE INTO track_vcs_commits
             (hash,track_id,parent_hash,tree_hash,manifest_schema_version,created_at)
             VALUES(?1,?2,?3,?4,?5,?6)",
        )
        .bind((field != "hash").then_some("nullable-probe"))
        .bind((field != "track_id").then_some(f.track.as_str()))
        .bind(&f.commit)
        .bind((field != "tree_hash").then_some(f.tree.tree_hash.as_str()))
        .bind((field != "manifest_schema_version").then_some(1_i64))
        .bind((field != "created_at").then_some(0_i64))
        .execute(&mut **tx)
        .await
        .unwrap();
        tx.commit().await.unwrap(); // No dangling deferred witness is left by IGNORE.
        no_pending(&f).await;
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM track_vcs_commits WHERE hash='nullable-probe'",
        )
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
        assert_eq!(count, 0);
    }
}

#[tokio::test]
async fn object_sweep_frontier_required_state_and_overflow_abort_actual_source() {
    let f = Fixture::new().await;
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    let guard: String =
        sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name='frontier_state_monotonic'")
            .fetch_one(&mut **tx)
            .await
            .unwrap();
    sqlx::query("DROP TRIGGER frontier_state_monotonic")
        .execute(&mut **tx)
        .await
        .unwrap();
    sqlx::query("UPDATE track_vcs_gc_frontier_state SET revision=9223372036854775807")
        .execute(&mut **tx)
        .await
        .unwrap();
    sqlx::query(&guard).execute(&mut **tx).await.unwrap();
    tx.commit().await.unwrap();
    assert!(
        sqlx::query(
            "INSERT OR IGNORE INTO track_vcs_commits SELECT * FROM track_vcs_commits WHERE hash=?1"
        )
        .bind(&f.commit)
        .execute(f.repo.pool())
        .await
        .is_err()
    );
    assert_eq!(unknown(&f).await, i64::MAX);
    assert_eq!(
        crate::track_vcs::head(f.repo.pool(), &f.track)
            .await
            .unwrap(),
        Some(f.commit.clone())
    );
    // A true positive is permitted at MAX; it does not increment with zero or reset state.
    f.publish(&f.tree, Some(&f.commit)).await;
    assert_eq!(unknown(&f).await, i64::MAX);
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    sqlx::query("DROP TRIGGER frontier_state_no_delete")
        .execute(&mut **tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM track_vcs_gc_frontier_state")
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        sqlx::query("UPDATE OR IGNORE track_vcs_refs SET updated_event_id=42")
            .execute(f.repo.pool())
            .await
            .is_err()
    );
    assert!(
        crate::track_vcs::sweep_unreferenced_objects_once(f.repo.db())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn object_sweep_frontier_slot_loss_and_epoch_changes_rollback_together() {
    let f = Fixture::new().await;
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    raw_commit(
        &mut tx,
        &f,
        "rollback-C",
        Some(&f.commit),
        &f.tree.tree_hash,
    )
    .await;
    tx.commit().await.unwrap();
    let u = unknown(&f).await;
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    raw_commit(
        &mut tx,
        &f,
        "rollback-D",
        Some(&f.commit),
        &f.tree.tree_hash,
    )
    .await;
    let during: i64 = sqlx::query_scalar("SELECT revision FROM track_vcs_gc_frontier_state")
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    assert!(during > u);
    tx.rollback().await.unwrap();
    assert_eq!(unknown(&f).await, u);
    let pending: String = sqlx::query_scalar("SELECT hash FROM track_vcs_gc_pending")
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    assert_eq!(pending, "rollback-C");
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM track_vcs_commits WHERE hash='rollback-D')",
    )
    .fetch_one(f.repo.pool())
    .await
    .unwrap();
    assert!(!exists);
    for sql in [
        "DELETE FROM track_vcs_gc_frontier_state",
        "INSERT OR REPLACE INTO track_vcs_gc_frontier_state VALUES(1,0)",
        "UPDATE OR IGNORE track_vcs_gc_frontier_state SET revision=0",
    ] {
        assert!(sqlx::query(sql).execute(f.repo.pool()).await.is_err());
        assert_eq!(unknown(&f).await, u);
    }
}
