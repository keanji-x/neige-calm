//! #926 — the shared-cache in-memory DB must survive total pool-connection
//! churn.
//!
//! Mechanism under test: sqlx 0.8.6 maps `sqlite::memory:` (and
//! `mode=memory`) to a NAMED shared-cache database
//! (`file:sqlx-in-memory-{seqno}?cache=shared`, seqno fixed per parsed
//! `SqliteConnectOptions` — see sqlx-sqlite `options/parse.rs`). The cache
//! — i.e. the entire database — lives only while at least one connection
//! holds it. `SqlxRepo::open` uses `SqlitePoolOptions` defaults
//! (`idle_timeout` 600 s, `max_lifetime` 1800 s, `min_connections` 0), so
//! every pool connection churns: the reaper closes idle connections, the
//! same-age connections hit the lifetime boundary together, and error
//! paths `close_hard` (including #920's fail-closed `after_release`
//! branch). When the LAST connection closes, sqlite destroys the cache;
//! the next acquire attaches a fresh EMPTY database of the same name,
//! migrations do NOT re-run, and every query fails "no such table" until
//! process restart. Long-lived replay/preview servers are exposed; on-disk
//! DBs are immune (reopen from disk is lossless).
//!
//! The tests do not wait 600 s for the reaper: they deterministically
//! close every open pool connection via `PoolConnection::close_on_drop`
//! (a real close, not a return-to-pool) and then prove a fresh acquire
//! still sees the schema and data.
//!
//! Parallel-test isolation: each `SqlxRepo::open("sqlite::memory:")` parse
//! draws a fresh `sqlx-in-memory-{seqno}`, so concurrent tests never share
//! a cache.

use std::time::{Duration, Instant};

use sqlx::SqlitePool;
use sqlx::pool::PoolConnection;
use sqlx::sqlite::SqlitePoolOptions;

use super::SqlxRepo;

/// Compile-time record of the trait fact the anchor field relies on:
/// `SqliteConnection` proxies all work to its worker thread over channels
/// (`flume::Sender` + `Arc<WorkerSharedState>`) and is `Send + Sync`, so
/// `SqlxRepo` — shared as `Arc<SqlxRepo>` across threads — can hold one
/// directly, no `Mutex` wrapper needed.
const _: fn() = || {
    fn requires_send_sync<T: Send + Sync>() {}
    requires_send_sync::<sqlx::sqlite::SqliteConnection>();
};

/// Force-close EVERY currently-open pool connection for real (a graceful
/// close, not a return-to-pool): hold all open connections, mark each
/// [`PoolConnection::close_on_drop`], drop them, and wait until the pool
/// reports zero open connections (the close runs on a spawned task).
///
/// This is the deterministic stand-in for the ways production loses pool
/// connections: idle reaping, `max_lifetime` churn, and error-path
/// `close_hard`.
async fn close_all_pool_connections(pool: &SqlitePool) {
    let mut held: Vec<PoolConnection<sqlx::Sqlite>> = Vec::new();
    // Acquire until we hold every open connection. `acquire` may open a
    // brand-new connection while `held` keeps the rest out of the idle
    // queue; the loop settles once held.len() == pool.size() (bounded by
    // the pool's max_connections).
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

/// #926 hard gate: after every pool connection has closed, a fresh acquire
/// must still see the migrated schema and previously-written rows.
///
/// Pre-fix the shared cache dies with the last connection and the SELECT
/// fails with `no such table: settings`.
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

    // Fresh acquire — attaches a new connection to the named cache.
    let count = count_marker(pool, "926-marker")
        .await
        .expect("schema must survive total pool-connection churn (#926)");
    assert_eq!(
        count, 1,
        "marker row must survive total pool-connection churn"
    );
}

/// On-disk regression guard: the identical close-everything-then-reacquire
/// sequence keeps seeing the data (trivially — it is on disk). Pins that
/// in-memory anchoring does not disturb on-disk opens.
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

/// Negative control pinning the UPSTREAM sqlx mechanism the anchor exists
/// for: a raw anchorless pool — built the way any sqlx user would
/// (`SqlitePoolOptions::connect`, no `SqlxRepo`) — loses the entire
/// in-memory database once its last connection closes. If a future sqlx
/// version keeps in-memory DBs alive by itself, this test FAILS —
/// signaling that the `_memory_cache_anchor` has become redundant and can
/// be removed.
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

    // Fresh acquire attaches a new EMPTY cache of the same name: the
    // table created above must be gone.
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
