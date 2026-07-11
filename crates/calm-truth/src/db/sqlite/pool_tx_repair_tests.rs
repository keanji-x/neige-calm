//! #920 — pool must self-heal connections released with a leaked open
//! transaction.
//!
//! Mechanism under test: in sqlx 0.8.6, `SqliteTransactionManager::begin`
//! with a custom statement has two awaits — (1) `conn.worker.begin(...)`
//! which executes the BEGIN and bumps the worker's `transaction_depth`,
//! then (2) `conn.lock_handle().await` to verify `in_transaction()`. If
//! the caller's future is cancelled between the two (e.g. an axum handler
//! dropped because the HTTP client aborted), the BEGIN has already run but
//! no `Transaction` guard exists, so its `Drop` rollback never fires. The
//! `PoolConnection` drop returns the connection to the pool still inside
//! the orphaned transaction:
//!
//! * every later `begin_with` **on that connection** fails with
//!   "attempted to call begin_with at non-zero transaction depth";
//! * plain `begin()` on it silently nests a SAVEPOINT, so "commits" never
//!   actually commit;
//! * and — observed while writing this repro — for `sqlite::memory:`
//!   (shared-cache) pools, `BEGIN IMMEDIATE` on every *other* connection
//!   blocks **indefinitely**: sqlx builds libsqlite3-sys with
//!   `unlock_notify`, so the write-lock conflict parks in
//!   `sqlite3_unlock_notify` waiting for the orphaned transaction to end,
//!   which nothing ever does (until the pool reaper's 600 s idle_timeout
//!   happens to close the poisoned connection). That whole-DB write stall
//!   is the a11y-e2e CI cascade.
//!
//! The tests construct the poisoned state deterministically — begin a
//! transaction on a pooled connection and `mem::forget` the guard, which
//! leaves exactly the depth-bumped/no-guard state a cancelled `begin_with`
//! future leaves behind — instead of trying to race the real cancellation
//! window. Because the leak stalls or errors *some* connection depending
//! on pool routing, the assertions are routing-independent: writes are
//! bounded by a timeout far below the reaper's 600 s release valve, and
//! transaction-state checks sweep every pooled connection.

use std::time::{Duration, Instant};

use sqlx::Connection;
use sqlx::SqlitePool;
use sqlx::pool::PoolConnection;

use super::{SqlxRepo, begin_immediate_tx};

/// Generous bound for operations that are milliseconds post-fix but stall
/// until the pool reaper's 600 s idle_timeout pre-fix.
const STALL_BOUND: Duration = Duration::from_secs(30);

/// Wait until every pool connection has been through the async return path
/// (`PoolConnection::drop` spawns the release — including any release hook
/// — onto the runtime) and is parked idle again.
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

/// Acquire a pooled connection, leave it inside a leaked `BEGIN IMMEDIATE`
/// (transaction depth bumped, no live `Transaction` guard — exactly the
/// state a cancelled `begin_with` future leaves behind), run `extra` on it,
/// then drop it so it returns to the pool poisoned.
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
    // conn drops here -> returns to the pool still inside the transaction
}

/// Simulate a cancelled `begin_with`, then require the pool to still hand
/// out working write transactions. Pre-fix this fails one of two ways
/// depending on which connection the pool routes to: sqlx's "attempted to
/// call begin_with at non-zero transaction depth"
/// (`Error::InvalidSavePointStatement`) on the poisoned connection, or an
/// indefinite unlock_notify stall (caught by the timeout) on any other.
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

    // The pool may or may not have routed the repaired connection to the
    // `begin_immediate_tx` above. Hold ALL pooled connections and
    // `begin_with` on each directly, so the formerly-poisoned connection
    // itself is proven to accept `begin_with` again regardless of routing.
    // Sequential, one write transaction at a time — held idle connections
    // don't hold sqlite locks, so this cannot deadlock.
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

/// Repair semantics: the leaked transaction's uncommitted write must be
/// ROLLED BACK (not silently committed), no pooled connection may still be
/// inside a transaction, and a normal write transaction must work
/// end-to-end afterwards.
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

    // (a) Sweep EVERY pooled connection (routing-independent): none may
    // still be inside the leaked transaction.
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
    // (b) The leaked write is ABSENT — proves ROLLBACK, not commit.
    for conn in &mut held {
        let leaked: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM settings WHERE key = 'leaked'")
            .fetch_one(&mut **conn)
            .await
            .unwrap();
        assert_eq!(leaked, 0, "leaked transaction's write must be rolled back");
    }
    drop(held);
    wait_for_pool_settled(pool).await;

    // (c) A normal write transaction works end-to-end on the healed pool.
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
