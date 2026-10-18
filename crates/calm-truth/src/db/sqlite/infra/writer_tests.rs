use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use super::begin_immediate_tx;
use crate::db::RepoOutOfDomain;
use crate::db::sqlite::SqlxRepo;
use crate::model::NewPlugin;
use tokio::time::timeout;

const LIMIT: Duration = Duration::from_secs(10);

// Register acquisition synchronously, so order means first poll rather than task spawn order.
async fn pending<F: Future>(mut future: Pin<&mut F>) {
    poll_fn(|cx| {
        assert!(
            future.as_mut().poll(cx).is_pending(),
            "writer must remain queued"
        );
        Poll::Ready(())
    })
    .await;
}

#[tokio::test]
async fn writer_queue_is_fifo_across_clones_and_readers_progress() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let db = repo.db().clone();
    let clone = db.clone();
    let holder = begin_immediate_tx(&db).await.unwrap();
    let second = begin_immediate_tx(&clone);
    let third = begin_immediate_tx(&db);
    tokio::pin!(second, third);
    pending(second.as_mut()).await;
    pending(third.as_mut()).await;
    let count: i64 = timeout(
        LIMIT,
        sqlx::query_scalar("SELECT count(*) FROM settings").fetch_one(db.read_pool()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(count, 0);
    holder.rollback().await.unwrap();
    let second = timeout(LIMIT, second).await.unwrap().unwrap();
    pending(third.as_mut()).await;
    let late = begin_immediate_tx(&clone);
    tokio::pin!(late);
    pending(late.as_mut()).await;
    second.rollback().await.unwrap();
    let third = timeout(LIMIT, third).await.unwrap().unwrap();
    pending(late.as_mut()).await;
    third.rollback().await.unwrap();
    timeout(LIMIT, late)
        .await
        .unwrap()
        .unwrap()
        .rollback()
        .await
        .unwrap();
    db.close().await;
}

#[tokio::test]
async fn cancelled_queued_writer_does_not_block_its_successor() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let db = repo.db();
    let holder = begin_immediate_tx(db).await.unwrap();
    let mut cancelled = Box::pin(begin_immediate_tx(db));
    let mut successor = Box::pin(begin_immediate_tx(db));
    pending(cancelled.as_mut()).await;
    pending(successor.as_mut()).await;
    drop(cancelled);
    holder.rollback().await.unwrap();
    timeout(LIMIT, successor)
        .await
        .unwrap()
        .unwrap()
        .rollback()
        .await
        .unwrap();
    db.close().await;
}

#[tokio::test]
async fn autocommit_execute_and_returning_share_writer_admission() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    // Occupy capacity without an SQLite transaction: any readiness would prove a bypass.
    let held = repo.db().writer_pool().acquire().await.unwrap();
    // Leave exactly one established reader idle. A bypass consumes it on first poll,
    // even when statement execution has not yet returned from the SQLite worker.
    let readers = repo.db().read_pool();
    let mut held_readers = Vec::new();
    for _ in 0..readers.options().get_max_connections() {
        held_readers.push(readers.acquire().await.unwrap());
    }
    drop(held_readers.pop());
    timeout(LIMIT, async {
        while readers.num_idle() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let update = repo.settings_upsert("admission", "queued");
    let install = repo.plugin_install(NewPlugin {
        id: "writer-probe".into(),
        version: "1".into(),
        install_path: "/fixture".into(),
        manifest: serde_json::json!({}),
        enabled: false,
        user_config: serde_json::json!({}),
    });
    tokio::pin!(update, install);
    pending(update.as_mut()).await;
    pending(install.as_mut()).await;
    let reader_preserved = readers.num_idle() == 1;
    drop(held);
    timeout(LIMIT, update).await.unwrap().unwrap();
    let installed = timeout(LIMIT, install).await.unwrap().unwrap();
    assert_eq!(installed.id, "writer-probe");
    let value: String = sqlx::query_scalar("SELECT value FROM settings WHERE key='admission'")
        .fetch_one(repo.db().read_pool())
        .await
        .unwrap();
    assert_eq!(value, "queued");
    drop(held_readers);
    repo.db().close().await;
    assert!(
        reader_preserved,
        "queued autocommit mutations must not consume read capacity"
    );
}

#[tokio::test]
async fn memory_roles_share_namespace_and_clones_keep_anchor_alive() {
    for url in [
        "sqlite::memory:".to_string(),
        format!(
            "sqlite:file:{}?mode=memory&cache=shared",
            uuid::Uuid::new_v4()
        ),
    ] {
        let repo = SqlxRepo::open(&url).await.unwrap();
        assert!(repo.has_memory_cache_anchor());
        let db = repo.db().clone();
        assert_eq!(db.read_pool().options().get_max_connections(), 10);
        assert_eq!(db.writer_pool().options().get_max_connections(), 1);
        drop(repo);
        let mut tx = begin_immediate_tx(&db).await.unwrap();
        sqlx::query("INSERT INTO settings(key,value,updated_at) VALUES('shared','yes',0)")
            .execute(&mut **tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let value: String = sqlx::query_scalar("SELECT value FROM settings WHERE key='shared'")
            .fetch_one(db.read_pool())
            .await
            .unwrap();
        assert_eq!(value, "yes");
        db.close().await;
        assert!(db.read_pool().is_closed());
        assert!(db.writer_pool().is_closed());
        assert!(!db.has_memory_cache_anchor());
    }
}

#[tokio::test]
async fn independent_memory_handles_have_independent_capacity_and_data() {
    let first = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let other = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let mut holder = begin_immediate_tx(first.db()).await.unwrap();
    sqlx::query("INSERT INTO settings(key,value,updated_at) VALUES('private','yes',0)")
        .execute(&mut **holder)
        .await
        .unwrap();
    timeout(LIMIT, begin_immediate_tx(other.db()))
        .await
        .unwrap()
        .unwrap()
        .rollback()
        .await
        .unwrap();
    holder.commit().await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM settings WHERE key='private'")
        .fetch_one(other.db().read_pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    first.db().close().await;
    other.db().close().await;
}

#[tokio::test]
async fn close_waits_for_both_roles_and_closes_every_clone() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let db = repo.db().clone();
    let reader = db.read_pool().acquire().await.unwrap();
    let writer = db.writer_pool().acquire().await.unwrap();
    let closing = db.close();
    tokio::pin!(closing);
    pending(closing.as_mut()).await;
    drop(reader);
    pending(closing.as_mut()).await;
    drop(writer);
    timeout(LIMIT, closing).await.unwrap();
    assert!(repo.db().read_pool().is_closed());
    assert!(repo.db().writer_pool().is_closed());
    assert!(!repo.has_memory_cache_anchor());
}

#[tokio::test]
async fn private_memory_cache_is_rejected_before_opening_role_pools() {
    match SqlxRepo::open("sqlite::memory:?cache=private").await {
        Err(error) => assert!(error.to_string().contains("require cache=shared")),
        Ok(_) => panic!("private memory connections cannot share the role schema"),
    }
}
