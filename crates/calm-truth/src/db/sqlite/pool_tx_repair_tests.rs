//! The pool must self-heal connections released with a leaked open
//! transaction (the state a cancelled `begin_with` future leaves behind).

use std::time::{Duration, Instant};

use sqlx::Connection;
use sqlx::SqlitePool;
use sqlx::pool::PoolConnection;

use super::{SqlxRepo, begin_immediate_tx};

const STALL_BOUND: Duration = Duration::from_secs(30);

/// `PoolConnection::drop` spawns the release (and its hook) onto the runtime.
async fn wait_for_pool_settled(pool: &SqlitePool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (size, idle) = (pool.size() as usize, pool.num_idle());
        if size > 0 && idle == size {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "pool never settled: size={size} idle={idle}"
        );
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// Leave a pooled connection inside a leaked `BEGIN IMMEDIATE` (depth bumped,
/// no live guard) and return it to the pool poisoned.
async fn poison_one_connection(pool: &SqlitePool, extra_sql: Option<&str>) {
    let mut conn = pool.acquire().await.unwrap();
    let mut tx = Connection::begin_with(&mut *conn, "BEGIN IMMEDIATE")
        .await
        .unwrap();
    if let Some(sql) = extra_sql {
        sqlx::query(sql).execute(&mut *tx).await.unwrap();
    }
    std::mem::forget(tx);
    assert!(conn.is_in_transaction());
}

#[tokio::test]
async fn pool_repairs_leaked_open_transaction_on_release() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let pool = repo.pool();

    poison_one_connection(pool, None).await;
    wait_for_pool_settled(pool).await;

    let tx = tokio::time::timeout(STALL_BOUND, begin_immediate_tx(pool))
        .await
        .expect("BEGIN IMMEDIATE must not stall on a leaked transaction's write lock")
        .expect("pool must repair leaked transaction on release");
    tx.commit().await.unwrap();
    wait_for_pool_settled(pool).await;

    // Routing-independent: `begin_with` on every pooled connection directly.
    // Held idle connections don't hold sqlite locks, so this cannot deadlock.
    let settled = pool.size() as usize;
    let mut held: Vec<PoolConnection<sqlx::Sqlite>> = Vec::with_capacity(settled);
    for _ in 0..settled {
        held.push(pool.acquire().await.unwrap());
    }
    for conn in &mut held {
        let tx = Connection::begin_with(&mut **conn, "BEGIN IMMEDIATE")
            .await
            .expect("every pooled connection must accept begin_with after repair");
        tx.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn pool_repair_rolls_back_leaked_uncommitted_writes() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let pool = repo.pool();

    poison_one_connection(
        pool,
        Some("INSERT INTO settings (key, value, updated_at) VALUES ('leaked', 'x', 0)"),
    )
    .await;
    wait_for_pool_settled(pool).await;

    let settled = pool.size() as usize;
    let mut held: Vec<PoolConnection<sqlx::Sqlite>> = Vec::with_capacity(settled);
    for _ in 0..settled {
        held.push(pool.acquire().await.unwrap());
    }
    for conn in &held {
        assert!(
            !conn.is_in_transaction(),
            "released connection must not still be inside the leaked transaction"
        );
    }
    for conn in &mut held {
        let leaked: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM settings WHERE key = 'leaked'")
            .fetch_one(&mut **conn)
            .await
            .unwrap();
        assert_eq!(leaked, 0, "leaked transaction's write must be rolled back");
    }
    drop(held);
    wait_for_pool_settled(pool).await;

    let mut tx = tokio::time::timeout(STALL_BOUND, begin_immediate_tx(pool))
        .await
        .expect("BEGIN IMMEDIATE must not stall on a leaked transaction's write lock")
        .unwrap();
    sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES ('committed', 'y', 0)")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let committed: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM settings WHERE key = 'committed'")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(committed, 1);
}
