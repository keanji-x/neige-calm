//! The shared-cache in-memory DB must survive total pool-connection churn:
//! sqlite destroys the named cache when its last connection closes.

use std::time::{Duration, Instant};

use sqlx::SqlitePool;
use sqlx::pool::PoolConnection;
use sqlx::sqlite::SqlitePoolOptions;

use super::SqlxRepo;

/// `SqliteConnection` must stay `Send + Sync` for `SqlxRepo` to hold the anchor unlocked.
const _: fn() = || {
    fn requires_send_sync<T: Send + Sync>() {}
    requires_send_sync::<sqlx::sqlite::SqliteConnection>();
};

/// Force-close every open pool connection (a real close, not a return-to-pool).
async fn close_all_pool_connections(pool: &SqlitePool) {
    let mut held: Vec<PoolConnection<sqlx::Sqlite>> = Vec::new();
    // `acquire` may open a brand-new connection while `held` keeps the rest out
    // of the idle queue; the loop settles once held.len() == pool.size().
    while (pool.size() as usize) > held.len() {
        held.push(pool.acquire().await.unwrap());
    }
    for conn in &mut held {
        conn.close_on_drop();
    }
    drop(held);
    let deadline = Instant::now() + Duration::from_secs(20);
    while pool.size() > 0 {
        assert!(
            Instant::now() < deadline,
            "pool connections never finished closing: size={}",
            pool.size()
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

async fn insert_marker(pool: &SqlitePool, key: &str) {
    sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES (?1, 'alive', 0)")
        .bind(key)
        .execute(pool)
        .await
        .unwrap();
}

async fn count_marker(pool: &SqlitePool, key: &str) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT COUNT(*) FROM settings WHERE key = ?1")
        .bind(key)
        .fetch_one(pool)
        .await
}

#[tokio::test]
async fn in_memory_db_survives_closing_every_pool_connection() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let pool = repo.pool();

    assert!(
        repo.has_memory_cache_anchor(),
        "in-memory repo must hold the keepalive anchor"
    );

    insert_marker(pool, "926-marker").await;

    close_all_pool_connections(pool).await;

    let count = count_marker(pool, "926-marker")
        .await
        .expect("schema must survive total pool-connection churn (#926)");
    assert_eq!(
        count, 1,
        "marker row must survive total pool-connection churn"
    );
}

#[tokio::test]
async fn on_disk_db_unaffected_by_closing_every_pool_connection() {
    let tmp = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        tmp.path().join("anchor_disk.db").display()
    );
    let repo = SqlxRepo::open(&url).await.unwrap();
    let pool = repo.pool();

    assert!(
        !repo.has_memory_cache_anchor(),
        "on-disk repo must not hold a keepalive anchor (zero behavior change)"
    );

    insert_marker(pool, "926-disk-marker").await;

    close_all_pool_connections(pool).await;

    let count = count_marker(pool, "926-disk-marker").await.unwrap();
    assert_eq!(count, 1, "on-disk data must survive pool-connection churn");
}

/// Negative control: if a future sqlx keeps in-memory DBs alive by itself this
/// FAILS, signaling `_memory_cache_anchor` has become redundant.
#[tokio::test]
async fn raw_anchorless_pool_loses_in_memory_db_when_every_connection_closes() {
    let pool = SqlitePoolOptions::new()
        .connect("sqlite::memory:")
        .await
        .unwrap();

    sqlx::query("CREATE TABLE anchorless_control (k TEXT PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO anchorless_control (k) VALUES ('doomed')")
        .execute(&pool)
        .await
        .unwrap();

    close_all_pool_connections(&pool).await;

    let err = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM anchorless_control")
        .fetch_one(&pool)
        .await
        .expect_err(
            "anchorless in-memory DB survived total connection churn — sqlx \
             now keeps it alive itself and the #926 anchor is redundant",
        );
    let msg = err.to_string();
    assert!(
        msg.contains("no such table"),
        "expected the cache-death signature 'no such table', got: {msg}"
    );
}
