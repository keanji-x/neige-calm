//! Actual bundled-engine tests of the owning epoch triggers, including raw source DML.
use super::gc_writer_tests::Fixture;
use crate::db::sqlite::begin_immediate_tx;
use crate::error::Result;
use crate::model::now_ms;
use crate::track_vcs;

pub(super) async fn epoch(f: &Fixture) -> i64 {
    sqlx::query_scalar("SELECT revision FROM track_vcs_gc_state WHERE singleton=1")
        .fetch_one(f.repo.pool())
        .await
        .unwrap()
}

// Raw source fixtures exercise the same table triggers as callers that bypass the store.
// They do not implement a graph proof or imitate the production hash algorithm.
pub(super) async fn replace_commit(
    f: &Fixture,
    hash: &str,
    track: &str,
    parent: Option<&str>,
    tree: &str,
) -> Result<()> {
    let mut tx = begin_immediate_tx(f.repo.db()).await?;
    let result = sqlx::query(concat!(
        "INSERT OR REPLACE INTO track_vcs_commits ",
        "(hash,track_id,parent_hash,tree_hash,manifest_schema_version,message,created_at) ",
        "VALUES(?1,?2,?3,?4,1,'epoch source fixture',?5)",
    ))
    .bind(hash)
    .bind(track)
    .bind(parent)
    .bind(tree)
    .bind(now_ms())
    .execute(&mut **tx)
    .await;
    match result {
        Ok(_) => tx.commit().await.map_err(Into::into),
        Err(error) => {
            tx.rollback().await?;
            Err(error.into())
        }
    }
}

#[tokio::test]
async fn gc_epoch_public_same_tree_checkpoint_keeps_history_and_epoch() {
    let f = Fixture::new().await;
    let before = epoch(&f).await;
    let child = f.publish(&f.tree, Some(&f.commit)).await;
    assert_eq!(epoch(&f).await, before);
    assert_ne!(child, f.commit);
    let record = track_vcs::commit_record(f.repo.pool(), &child)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.parent_hash.as_ref(), Some(&f.commit));
    assert_eq!(record.tree_hash, f.tree.tree_hash);
    assert_eq!(
        track_vcs::head(f.repo.pool(), &f.track).await.unwrap(),
        Some(child)
    );
}

#[tokio::test]
async fn gc_epoch_same_head_metadata_update_changes_metadata_without_invalidation() {
    let f = Fixture::new().await;
    let before = epoch(&f).await;
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    sqlx::query("UPDATE track_vcs_refs SET updated_event_id=42 WHERE track_id=?1")
        .bind(f.track.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(epoch(&f).await, before);
    let row: (String, i64) =
        sqlx::query_as("SELECT head_hash, updated_event_id FROM track_vcs_refs WHERE track_id=?1")
            .bind(f.track.as_str())
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    assert_eq!(row, (f.commit.clone(), 42));
}

#[tokio::test]
async fn gc_epoch_nonqualifying_commit_insertions_invalidate() {
    for case in [
        "tree",
        "parent",
        "child-track",
        "parent-track",
        "missing-parent",
        "root",
        "self",
    ] {
        let f = Fixture::new().await;
        let other = f.tree("different epoch tree", 1).await;
        replace_commit(
            &f,
            "detached-parent",
            f.track.as_str(),
            None,
            &f.tree.tree_hash,
        )
        .await
        .unwrap();
        if case == "parent-track" {
            let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
            sqlx::query("UPDATE track_vcs_commits SET track_id='other-track' WHERE hash=?1")
                .bind(&f.commit)
                .execute(&mut **tx)
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }
        let before = epoch(&f).await;
        let hash = if case == "self" {
            f.commit.as_str()
        } else {
            "new-child"
        };
        let track = if case == "child-track" {
            "other-track"
        } else {
            f.track.as_str()
        };
        let parent = match case {
            "parent" => Some("detached-parent"),
            "missing-parent" => Some("absent-parent"),
            "root" => None,
            _ => Some(f.commit.as_str()),
        };
        let tree = if case == "tree" || case == "self" {
            &other.tree_hash
        } else {
            &f.tree.tree_hash
        };
        replace_commit(&f, hash, track, parent, tree).await.unwrap();
        assert!(epoch(&f).await > before, "{case}");
        let stored = track_vcs::commit_record(f.repo.pool(), hash)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.parent_hash.as_deref(), parent, "{case}");
        assert_eq!(stored.tree_hash, *tree, "{case}");
    }
}

#[tokio::test]
async fn gc_epoch_new_ref_root_and_ref_track_move_invalidate() {
    let f = Fixture::new().await;
    let before = epoch(&f).await;
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    sqlx::query("INSERT INTO track_vcs_refs(track_id,head_hash) VALUES('new-root',?1)")
        .bind(&f.commit)
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(epoch(&f).await > before);
    let before = epoch(&f).await;
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    sqlx::query("UPDATE OR REPLACE track_vcs_refs SET track_id='new-root' WHERE track_id=?1")
        .bind(f.track.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(epoch(&f).await > before);
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM track_vcs_refs WHERE track_id='new-root'")
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        track_vcs::head(f.repo.pool(), &f.track).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn gc_epoch_safe_prefix_rollback_preserves_data_and_epoch() {
    let f = Fixture::new().await;
    let before = epoch(&f).await;
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    let child = track_vcs::commit_tree(
        &mut tx,
        &f.track,
        Some(&f.commit),
        &f.tree,
        None,
        "rollback prefix",
        1,
    )
    .await
    .unwrap();
    let inside: i64 =
        sqlx::query_scalar("SELECT revision FROM track_vcs_gc_state WHERE singleton=1")
            .fetch_one(&mut **tx)
            .await
            .unwrap();
    assert_eq!(inside, before);
    tx.rollback().await.unwrap();
    assert_eq!(epoch(&f).await, before);
    assert_eq!(
        track_vcs::head(f.repo.pool(), &f.track).await.unwrap(),
        Some(f.commit.clone())
    );
    assert!(
        track_vcs::commit_record(f.repo.pool(), &child)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn gc_epoch_safe_prefix_at_max_does_not_write_the_counter() {
    let f = Fixture::new().await;
    f.exhaust_revision().await;
    let child = f.publish(&f.tree, Some(&f.commit)).await;
    assert_eq!(epoch(&f).await, i64::MAX);
    assert_ne!(child, f.commit);
    assert_eq!(
        track_vcs::head(f.repo.pool(), &f.track).await.unwrap(),
        Some(child)
    );
}

#[tokio::test]
async fn gc_epoch_safe_insert_and_ref_update_require_state_even_under_ignore() {
    let f = Fixture::new().await;
    replace_commit(
        &f,
        "ready-prefix",
        f.track.as_str(),
        Some(&f.commit),
        &f.tree.tree_hash,
    )
    .await
    .unwrap();
    f.remove_required_revision().await;
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track_vcs_commits")
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    let error = track_vcs::commit_tree(
        &mut tx,
        &f.track,
        Some(&f.commit),
        &f.tree,
        None,
        "missing state prefix",
        1,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("required GC revision is missing")
    );
    tx.rollback().await.unwrap();
    let error = replace_commit(
        &f,
        "replacement-prefix",
        f.track.as_str(),
        Some(&f.commit),
        &f.tree.tree_hash,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("required GC revision is missing")
    );
    for sql in [
        "UPDATE OR IGNORE track_vcs_refs SET updated_event_id=42 WHERE track_id=?1",
        "UPDATE OR IGNORE track_vcs_refs SET head_hash='ready-prefix' WHERE track_id=?1",
    ] {
        let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
        let error = sqlx::query(sql)
            .bind(f.track.as_str())
            .execute(&mut **tx)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("required GC revision is missing")
        );
        tx.rollback().await.unwrap();
    }
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track_vcs_commits")
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    assert_eq!(after, before);
    let row: (String, Option<i64>) =
        sqlx::query_as("SELECT head_hash,updated_event_id FROM track_vcs_refs WHERE track_id=?1")
            .bind(f.track.as_str())
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    assert_eq!(row, (f.commit.clone(), None));
}

#[tokio::test]
async fn gc_epoch_unsafe_changes_at_max_abort_without_partial_source_mutation() {
    let f = Fixture::new().await;
    let other = f.tree("unsafe max target", 1).await;
    replace_commit(
        &f,
        "unsafe-ready",
        f.track.as_str(),
        Some(&f.commit),
        &other.tree_hash,
    )
    .await
    .unwrap();
    let original = track_vcs::commit_record(f.repo.pool(), &f.commit)
        .await
        .unwrap();
    f.exhaust_revision().await;
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    let error = track_vcs::commit_tree(
        &mut tx,
        &f.track,
        Some(&f.commit),
        &other,
        None,
        "unsafe max prefix",
        1,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("GC revision must advance exactly once")
    );
    tx.rollback().await.unwrap();
    let error = replace_commit(
        &f,
        &f.commit,
        f.track.as_str(),
        Some(&f.commit),
        &other.tree_hash,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("GC revision must advance exactly once")
    );
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    let error = sqlx::query(
        "UPDATE OR IGNORE track_vcs_refs SET head_hash='unsafe-ready' WHERE track_id=?1",
    )
    .bind(f.track.as_str())
    .execute(&mut **tx)
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("GC revision must advance exactly once")
    );
    tx.rollback().await.unwrap();
    assert_eq!(epoch(&f).await, i64::MAX);
    assert_eq!(
        track_vcs::head(f.repo.pool(), &f.track).await.unwrap(),
        Some(f.commit.clone())
    );
    assert_eq!(
        track_vcs::commit_record(f.repo.pool(), &f.commit)
            .await
            .unwrap(),
        original
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track_vcs_commits WHERE track_id=?1")
        .bind(f.track.as_str())
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 2);
}
