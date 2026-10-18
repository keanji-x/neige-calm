use super::gc_writer_tests::Fixture;
use crate::db::sqlite::begin_immediate_tx;

async fn revision(f: &Fixture) -> i64 {
    sqlx::query_scalar("SELECT revision FROM track_vcs_gc_state WHERE singleton=1")
        .fetch_one(f.repo.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn gc_revision_rejects_reset_replace_delete_and_outer_ignore() {
    let f = Fixture::new().await;
    for sql in [
        "DELETE FROM track_vcs_gc_state",
        "INSERT OR REPLACE INTO track_vcs_gc_state VALUES(1,0)",
        "REPLACE INTO track_vcs_gc_state VALUES(1,0)",
        "INSERT OR IGNORE INTO track_vcs_gc_state VALUES(1,0)",
        "UPDATE track_vcs_gc_state SET revision=0",
        "UPDATE OR IGNORE track_vcs_gc_state SET revision=0",
        "UPDATE track_vcs_gc_state SET revision=revision+2",
        "UPDATE track_vcs_gc_state SET revision=1.5",
        "UPDATE track_vcs_gc_state SET revision=-1",
        "UPDATE track_vcs_gc_state SET singleton=2,revision=revision+1",
    ] {
        let before = revision(&f).await;
        let error = sqlx::query(sql).execute(f.repo.pool()).await.unwrap_err();
        assert!(error.to_string().contains("track-vcs:"), "{sql}: {error}");
        assert_eq!(revision(&f).await, before, "{sql}");
    }
}

#[tokio::test]
async fn gc_revision_source_rollback_is_atomic_and_ignore_is_a_noop() {
    let f = Fixture::new().await;
    let before = revision(&f).await;
    let mut tx = begin_immediate_tx(f.repo.pool()).await.unwrap();
    sqlx::query("UPDATE track_vcs_objects SET created_at=created_at WHERE hash=?1")
        .bind(&f.tree.tree_hash)
        .execute(&mut **tx)
        .await
        .unwrap();
    let inside: i64 =
        sqlx::query_scalar("SELECT revision FROM track_vcs_gc_state WHERE singleton=1")
            .fetch_one(&mut **tx)
            .await
            .unwrap();
    assert!(inside > before);
    tx.rollback().await.unwrap();
    assert_eq!(revision(&f).await, before);
    sqlx::query(
        "INSERT OR IGNORE INTO track_vcs_objects SELECT * FROM track_vcs_objects WHERE hash=?1",
    )
    .bind(&f.tree.tree_hash)
    .execute(f.repo.pool())
    .await
    .unwrap();
    assert_eq!(revision(&f).await, before);
}

#[tokio::test]
async fn gc_revision_object_replace_invalidates_but_same_head_upsert_preserves_epoch() {
    let f = Fixture::new().await;
    let before = revision(&f).await;
    // This object is not FK-referenced; replacement may omit DELETE triggers but must INSERT.
    sqlx::query(
        "INSERT OR REPLACE INTO track_vcs_objects SELECT * FROM track_vcs_objects WHERE hash=?1",
    )
    .bind(&f.tree.tree_hash)
    .execute(f.repo.pool())
    .await
    .unwrap();
    assert!(revision(&f).await > before);
    let before = revision(&f).await;
    sqlx::query(concat!(
        "INSERT INTO track_vcs_refs(track_id,head_hash,updated_event_id) ",
        "VALUES(?1,?2,NULL) ON CONFLICT(track_id) DO UPDATE SET ",
        "head_hash=excluded.head_hash",
    ))
    .bind(f.track.as_str())
    .bind(&f.commit)
    .execute(f.repo.pool())
    .await
    .unwrap();
    assert_eq!(revision(&f).await, before);
}

#[tokio::test]
async fn gc_revision_missing_row_aborts_source_write_even_under_ignore() {
    let f = Fixture::new().await;
    f.remove_required_revision().await;
    let before: i64 = sqlx::query_scalar("SELECT created_at FROM track_vcs_objects WHERE hash=?1")
        .bind(&f.tree.tree_hash)
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    for modifier in ["", " OR IGNORE"] {
        let sql =
            format!("UPDATE{modifier} track_vcs_objects SET created_at=created_at+1 WHERE hash=?1");
        assert!(
            sqlx::query(&sql)
                .bind(&f.tree.tree_hash)
                .execute(f.repo.pool())
                .await
                .is_err()
        );
        let after: i64 =
            sqlx::query_scalar("SELECT created_at FROM track_vcs_objects WHERE hash=?1")
                .bind(&f.tree.tree_hash)
                .fetch_one(f.repo.pool())
                .await
                .unwrap();
        assert_eq!(after, before);
    }
    let error = sqlx::query("INSERT OR IGNORE INTO track_vcs_objects(hash,kind,bytes,created_at) VALUES('missing-state-probe','blob',X'00',0)")
        .execute(f.repo.pool()).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("required GC revision is missing")
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM track_vcs_objects WHERE hash='missing-state-probe'",
    )
    .fetch_one(f.repo.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn gc_revision_integer_overflow_aborts_the_source_write() {
    let f = Fixture::new().await;
    f.exhaust_revision().await;
    let before: i64 = sqlx::query_scalar("SELECT created_at FROM track_vcs_objects WHERE hash=?1")
        .bind(&f.tree.tree_hash)
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    for sql in [
        "UPDATE track_vcs_objects SET created_at=created_at+1 WHERE hash=?1",
        "UPDATE OR IGNORE track_vcs_objects SET created_at=created_at+1 WHERE hash=?1",
    ] {
        assert!(
            sqlx::query(sql)
                .bind(&f.tree.tree_hash)
                .execute(f.repo.pool())
                .await
                .is_err()
        );
        assert_eq!(revision(&f).await, i64::MAX);
        let after: i64 =
            sqlx::query_scalar("SELECT created_at FROM track_vcs_objects WHERE hash=?1")
                .bind(&f.tree.tree_hash)
                .fetch_one(f.repo.pool())
                .await
                .unwrap();
        assert_eq!(after, before);
    }
}

#[tokio::test]
async fn gc_revision_all_nine_source_operations_are_visible_across_connections() {
    let f = Fixture::new().await;
    let mut writer = f.repo.pool().acquire().await.unwrap();
    let mut reader = f.repo.pool().acquire().await.unwrap();
    let operations = [
        (
            "track_vcs_objects",
            "hash",
            "raw-object",
            [
                "INSERT INTO track_vcs_objects(hash,kind,bytes,created_at) VALUES('raw-object','blob',X'00',0)",
                "UPDATE track_vcs_objects SET created_at=1 WHERE hash='raw-object'",
                "DELETE FROM track_vcs_objects WHERE hash='raw-object'",
            ],
        ),
        (
            "track_vcs_commits",
            "hash",
            "raw-commit",
            [
                concat!(
                    "INSERT INTO track_vcs_commits(hash,track_id,parent_hash,tree_hash,",
                    "manifest_schema_version,author,message,event_id,created_at) ",
                    "SELECT 'raw-commit',track_id,NULL,tree_hash,manifest_schema_version,",
                    "author,message,event_id,created_at FROM track_vcs_commits LIMIT 1",
                ),
                "UPDATE track_vcs_commits SET message='raw update' WHERE hash='raw-commit'",
                "DELETE FROM track_vcs_commits WHERE hash='raw-commit'",
            ],
        ),
        (
            "track_vcs_refs",
            "track_id",
            "raw-ref-updated",
            [
                concat!(
                    "INSERT INTO track_vcs_refs(track_id,head_hash,updated_event_id) ",
                    "SELECT 'raw-ref',head_hash,NULL FROM track_vcs_refs LIMIT 1",
                ),
                "UPDATE track_vcs_refs SET track_id='raw-ref-updated' WHERE track_id='raw-ref'",
                "DELETE FROM track_vcs_refs WHERE track_id='raw-ref-updated'",
            ],
        ),
    ];
    for (table, key, value, statements) in operations {
        for statement in statements {
            let before: i64 =
                sqlx::query_scalar("SELECT revision FROM track_vcs_gc_state WHERE singleton=1")
                    .fetch_one(&mut *reader)
                    .await
                    .unwrap();
            let affected = sqlx::query(statement)
                .execute(&mut *writer)
                .await
                .unwrap()
                .rows_affected();
            assert_eq!(affected, 1, "{statement}");
            let after: i64 =
                sqlx::query_scalar("SELECT revision FROM track_vcs_gc_state WHERE singleton=1")
                    .fetch_one(&mut *reader)
                    .await
                    .unwrap();
            assert_eq!(after, before + 1, "{statement}");
        }
        let query = format!("SELECT COUNT(*) FROM {table} WHERE {key}=?1");
        let count: i64 = sqlx::query_scalar(&query)
            .bind(value)
            .fetch_one(&mut *reader)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}

#[tokio::test]
async fn gc_revision_bulk_history_prune_advances_for_every_removed_row() {
    let f = Fixture::new().await;
    let mut parent = f.commit.clone();
    for _ in 0..10 {
        parent = f.publish(&f.tree, Some(&parent)).await;
    }
    sqlx::query("UPDATE track_vcs_commits SET created_at=0 WHERE hash<>?1")
        .bind(&parent)
        .execute(f.repo.pool())
        .await
        .unwrap();
    let before = revision(&f).await;
    let mut tx = begin_immediate_tx(f.repo.pool()).await.unwrap();
    let deleted = crate::track_vcs::prune_track_history_tx(&mut tx, &f.track, 1)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(deleted >= 2);
    assert_eq!(revision(&f).await, before + deleted as i64);
    assert_eq!(
        crate::track_vcs::head(f.repo.pool(), &f.track)
            .await
            .unwrap(),
        Some(parent)
    );
}
