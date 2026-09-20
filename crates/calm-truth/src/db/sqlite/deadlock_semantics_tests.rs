//! Deterministic pins of the shared-cache "database is deadlocked" (`SQLITE_LOCKED`, code 6) semantics behind the
//! rule *deferred transactions must be READ-ONLY; every writing transaction uses `begin_immediate_tx`*. sqlx parks a
//! blocked statement in `unlock_notify` until the blocker's tx concludes; the side that closes the cycle gets the
//! error, and only an explicit (lock-holding) tx can be a cycle party — an autocommit statement unwinds its locks before parking.

use std::time::{Duration, Instant};

use sqlx::Connection;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::pool::PoolConnection;
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};

use super::{SqlxRepo, begin_immediate_tx, is_sqlite_busy};

/// Bound for statements that must complete promptly once unblocked; far below the pool reaper's 600 s idle_timeout.
const STALL_BOUND: Duration = Duration::from_secs(30);

/// Grace period after the peer signalled "about to issue the parking statement"; the park itself is sub-millisecond.
const PARK_GRACE: Duration = Duration::from_millis(300);

/// Open the app's real repo (pool + pragmas + after_release hook) and add two scratch tables, one seed row each.
async fn open_semantics_repo() -> SqlxRepo {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let pool = repo.pool();
    // Plain rowid tables — no AUTOINCREMENT so inserts don't drag the sqlite_sequence table's locks into the picture.
    sqlx::query("CREATE TABLE deadlock_x (id INTEGER PRIMARY KEY, v TEXT NOT NULL)")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE deadlock_y (id INTEGER PRIMARY KEY, v TEXT NOT NULL)")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO deadlock_x (v) VALUES ('seed')")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO deadlock_y (v) VALUES ('seed')")
        .execute(pool)
        .await
        .unwrap();
    repo
}

/// Pin the exact error shape the rule rests on, and verify `is_sqlite_busy` matches it.
fn assert_deadlock_shape(e: &sqlx::Error, side: &str) -> String {
    let capture = format!(
        "side={side} | debug={e:?} | display={e} | db.code={:?} | db.message={:?} | is_sqlite_busy={}",
        e.as_database_error().and_then(|d| d.code()),
        e.as_database_error().map(|d| d.message()),
        is_sqlite_busy(e),
    );
    let db = e
        .as_database_error()
        .unwrap_or_else(|| panic!("deadlock error must be sqlx::Error::Database, got: {e:?}"));
    // notify.c sets the PLAIN primary code (6), not the extended SQLITE_LOCKED_SHAREDCACHE (262).
    assert_eq!(
        db.code().as_deref(),
        Some("6"),
        "deadlock must surface plain SQLITE_LOCKED (6): {capture}"
    );
    assert_eq!(
        db.message(),
        "database is deadlocked",
        "exact notify.c message: {capture}"
    );
    assert!(
        is_sqlite_busy(e),
        "infra::is_sqlite_busy must match the deadlock error (6 & 0xFF == 6): {capture}"
    );
    capture
}

async fn count(pool: &SqlitePool, table: &str) -> i64 {
    let sql = format!("SELECT count(*) FROM {table}");
    sqlx::query_scalar(&sql).fetch_one(pool).await.unwrap()
}

/// Wait until every pool connection has been through the async return path and is parked idle again.
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

/// The WRITER parks first, the READER closes the cycle: B gets "database is deadlocked", A stays parked until B's tx concludes.
#[tokio::test]
async fn deadlock_semantics_reader_closes_cycle_reader_gets_error() {
    let repo = open_semantics_repo().await;
    let pool = repo.pool();

    let mut conn_a: PoolConnection<sqlx::Sqlite> = pool.acquire().await.unwrap();
    let mut conn_b: PoolConnection<sqlx::Sqlite> = pool.acquire().await.unwrap();

    let (go_a_tx, go_a_rx) = oneshot::channel::<()>();
    let (a_parking_tx, a_parking_rx) = oneshot::channel::<()>();

    let a = tokio::spawn(async move {
        let mut tx = Connection::begin_with(&mut *conn_a, "BEGIN IMMEDIATE")
            .await
            .unwrap();
        sqlx::query("INSERT INTO deadlock_x (v) VALUES ('a')")
            .execute(&mut *tx)
            .await
            .unwrap();
        go_a_rx.await.unwrap();
        a_parking_tx.send(()).unwrap();
        sqlx::query("INSERT INTO deadlock_y (v) VALUES ('a')")
            .execute(&mut *tx)
            .await
            .expect("first unlock_notify waiter must NOT error; it completes after B rolls back");
        tx.commit().await.unwrap();
    });

    let mut tx_b = Connection::begin(&mut *conn_b).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM deadlock_y")
        .fetch_one(&mut *tx_b)
        .await
        .unwrap();
    assert_eq!(n, 1);
    go_a_tx.send(()).unwrap();
    a_parking_rx.await.unwrap();
    sleep(PARK_GRACE).await; // A is now parked on y

    let err = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM deadlock_x")
        .fetch_one(&mut *tx_b)
        .await
        .expect_err("cycle-closing reader must fail with the deadlock error");
    let capture = assert_deadlock_shape(&err, "reader(B) closes cycle");
    eprintln!("[deadlock-semantics reader-closes] {capture}");

    // B's error does NOT unpark A — A waits for B's tx to CONCLUDE, not for B's statement to fail.
    sleep(Duration::from_millis(200)).await;
    assert!(
        !a.is_finished(),
        "writer must stay parked while the errored reader's tx is still open"
    );

    tx_b.rollback()
        .await
        .expect("errored reader tx must still ROLLBACK cleanly");
    timeout(STALL_BOUND, a)
        .await
        .expect("writer must unpark once the reader tx concluded")
        .unwrap();

    assert_eq!(count(pool, "deadlock_x").await, 2);
    assert_eq!(count(pool, "deadlock_y").await, 2);
}

/// The READER parks first, the WRITER closes the cycle — the production `write_with_event` shape: the `BEGIN IMMEDIATE`
/// side fails. Same-tx statement retry re-deadlocks; the tx is NOT auto-rolled-back; a whole-transaction retry succeeds.
#[tokio::test]
async fn deadlock_semantics_writer_closes_cycle_writer_gets_error_production_shape() {
    let repo = open_semantics_repo().await;
    let pool = repo.pool();

    let mut conn_a: PoolConnection<sqlx::Sqlite> = pool.acquire().await.unwrap();
    let mut conn_b: PoolConnection<sqlx::Sqlite> = pool.acquire().await.unwrap();

    let (b_locked_y_tx, b_locked_y_rx) = oneshot::channel::<()>();
    let (go_b_tx, go_b_rx) = oneshot::channel::<()>();
    let (b_parking_tx, b_parking_rx) = oneshot::channel::<()>();

    let b = tokio::spawn(async move {
        let mut tx = Connection::begin(&mut *conn_b).await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM deadlock_y")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(n, 1);
        b_locked_y_tx.send(()).unwrap();
        go_b_rx.await.unwrap();
        b_parking_tx.send(()).unwrap();
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM deadlock_x")
            .fetch_one(&mut *tx)
            .await
            .expect("first unlock_notify waiter (reader) must NOT get the deadlock error");
        assert_eq!(n, 1, "reader must see A's insert rolled back");
        tx.rollback()
            .await
            .unwrap_or_else(|e| panic!("reader rollback failed: {e}"));
    });

    let mut tx_a = Connection::begin_with(&mut *conn_a, "BEGIN IMMEDIATE")
        .await
        .unwrap();
    sqlx::query("INSERT INTO deadlock_x (v) VALUES ('a')")
        .execute(&mut *tx_a)
        .await
        .unwrap();
    b_locked_y_rx.await.unwrap();
    go_b_tx.send(()).unwrap();
    b_parking_rx.await.unwrap();
    sleep(PARK_GRACE).await; // B is now parked on x

    let err = sqlx::query("INSERT INTO deadlock_y (v) VALUES ('a')")
        .execute(&mut *tx_a)
        .await
        .expect_err("cycle-closing writer must fail with the deadlock error");
    let capture = assert_deadlock_shape(&err, "writer(A) closes cycle [production shape]");
    eprintln!("[deadlock-semantics writer-closes] {capture}");

    // Statement-level retry INSIDE the same tx is futile: B is still parked waiting on OUR transaction.
    let err2 = sqlx::query("INSERT INTO deadlock_y (v) VALUES ('a2')")
        .execute(&mut *tx_a)
        .await
        .expect_err("same-tx statement retry must re-deadlock immediately");
    assert_deadlock_shape(&err2, "writer(A) same-tx statement retry");

    sleep(Duration::from_millis(200)).await;
    assert!(
        !b.is_finished(),
        "reader must stay parked while the errored writer's tx is still open"
    );

    // Post-deadlock tx state: NOT auto-rolled-back — its own uncommitted write is still visible.
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM deadlock_x")
        .fetch_one(&mut *tx_a)
        .await
        .expect("errored writer tx must still serve reads on its own locked table");
    assert_eq!(n, 2, "seed + own uncommitted insert: tx not auto-aborted");
    tx_a.rollback()
        .await
        .expect("errored writer tx must ROLLBACK cleanly");
    timeout(STALL_BOUND, b)
        .await
        .expect("reader must unpark once the writer tx concluded")
        .unwrap();

    // Valid retry shape: a WHOLE-transaction restart.
    let mut tx_retry = Connection::begin_with(&mut *conn_a, "BEGIN IMMEDIATE")
        .await
        .expect("fresh BEGIN IMMEDIATE after rollback must work");
    sqlx::query("INSERT INTO deadlock_x (v) VALUES ('retry')")
        .execute(&mut *tx_retry)
        .await
        .unwrap();
    sqlx::query("INSERT INTO deadlock_y (v) VALUES ('retry')")
        .execute(&mut *tx_retry)
        .await
        .unwrap();
    tx_retry.commit().await.unwrap();

    assert_eq!(count(pool, "deadlock_x").await, 2);
    assert_eq!(count(pool, "deadlock_y").await, 2);
}

/// A PLAIN AUTOCOMMIT join (FROM order `y, x`) does NOT deadlock the writer even though it acquires y's read lock
/// before blocking on x: the implicit transaction unwinds and releases every table lock before sqlx parks.
#[tokio::test]
async fn deadlock_semantics_autocommit_join_y_first_no_cycle_error_unwind_releases_locks() {
    let repo = open_semantics_repo().await;
    let pool = repo.pool();

    // Prove the lock-acquisition order claim via EXPLAIN: the join locks deadlock_y FIRST, deadlock_x second.
    let lock_order: Vec<String> =
        sqlx::query("EXPLAIN SELECT count(*) FROM deadlock_y, deadlock_x")
            .fetch_all(pool)
            .await
            .unwrap()
            .iter()
            .filter(|row| row.get::<String, _>("opcode") == "TableLock")
            .map(|row| row.get::<String, _>("p4"))
            .collect();
    assert_eq!(
        lock_order,
        vec!["deadlock_y".to_string(), "deadlock_x".to_string()],
        "join must attempt y's table lock before x's for this variant to be meaningful"
    );

    let mut conn_a: PoolConnection<sqlx::Sqlite> = pool.acquire().await.unwrap();
    let mut conn_b: PoolConnection<sqlx::Sqlite> = pool.acquire().await.unwrap();

    let (b_parking_tx, b_parking_rx) = oneshot::channel::<()>();

    let mut tx_a = Connection::begin_with(&mut *conn_a, "BEGIN IMMEDIATE")
        .await
        .unwrap();
    sqlx::query("INSERT INTO deadlock_x (v) VALUES ('a')")
        .execute(&mut *tx_a)
        .await
        .unwrap();

    let b = tokio::spawn(async move {
        b_parking_tx.send(()).unwrap();
        // Autocommit: locks y, blocks on x -> implicit tx unwinds (y released) -> parks holding nothing.
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM deadlock_y, deadlock_x")
            .fetch_one(&mut *conn_b)
            .await
            .expect("parked autocommit join must complete after the writer commits");
        n
    });

    b_parking_rx.await.unwrap();
    sleep(PARK_GRACE).await; // B is parked on x — holding NO locks

    // No cycle: the writer's INSERT y proceeds despite the parked join having ACQUIRED y earlier in its prologue.
    timeout(
        Duration::from_secs(10),
        sqlx::query("INSERT INTO deadlock_y (v) VALUES ('a')").execute(&mut *tx_a),
    )
    .await
    .expect("writer must not park: the parked autocommit join's locks were released by the error unwind")
    .expect("no deadlock: autocommit statements cannot hold-and-wait");
    tx_a.commit().await.unwrap();

    let n = timeout(STALL_BOUND, b)
        .await
        .expect("join must unpark once the writer committed")
        .unwrap();
    assert_eq!(n, 4, "(seed+a) x (seed+a) after the writer's commit");
}

/// Same autocommit join but FROM order `x, y`: the FIRST table lock is the blocked one, so the statement parks holding nothing.
#[tokio::test]
async fn deadlock_semantics_autocommit_join_x_first_no_cycle() {
    let repo = open_semantics_repo().await;
    let pool = repo.pool();

    let mut conn_a: PoolConnection<sqlx::Sqlite> = pool.acquire().await.unwrap();
    let mut conn_b: PoolConnection<sqlx::Sqlite> = pool.acquire().await.unwrap();

    let (b_parking_tx, b_parking_rx) = oneshot::channel::<()>();

    let mut tx_a = Connection::begin_with(&mut *conn_a, "BEGIN IMMEDIATE")
        .await
        .unwrap();
    sqlx::query("INSERT INTO deadlock_x (v) VALUES ('a')")
        .execute(&mut *tx_a)
        .await
        .unwrap();

    let b = tokio::spawn(async move {
        b_parking_tx.send(()).unwrap();
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM deadlock_x, deadlock_y")
            .fetch_one(&mut *conn_b)
            .await
            .expect("parked autocommit join must complete after the writer commits");
        n
    });

    b_parking_rx.await.unwrap();
    sleep(PARK_GRACE).await; // B is parked on x, holding NO locks

    timeout(
        Duration::from_secs(10),
        sqlx::query("INSERT INTO deadlock_y (v) VALUES ('a')").execute(&mut *tx_a),
    )
    .await
    .expect("writer must not park: the join blocked on its first lock holds nothing")
    .expect("no deadlock when the reader's blocked lock is its first");
    tx_a.commit().await.unwrap();

    let n = timeout(STALL_BOUND, b)
        .await
        .expect("join must unpark once the writer committed")
        .unwrap();
    assert_eq!(n, 4, "(seed+a) x (seed+a) after the writer's commit");
}

/// `BEGIN IMMEDIATE` on BOTH sides cannot cycle: a shared cache admits one write transaction at a time, so the
/// second parks at BEGIN itself, holding no locks.
#[tokio::test]
async fn deadlock_semantics_both_begin_immediate_serialize_no_deadlock() {
    let repo = open_semantics_repo().await;
    let pool = repo.pool();

    let mut conn_a: PoolConnection<sqlx::Sqlite> = pool.acquire().await.unwrap();
    let mut conn_b: PoolConnection<sqlx::Sqlite> = pool.acquire().await.unwrap();

    let (b_beginning_tx, b_beginning_rx) = oneshot::channel::<()>();

    let mut tx_a = Connection::begin_with(&mut *conn_a, "BEGIN IMMEDIATE")
        .await
        .unwrap();
    sqlx::query("INSERT INTO deadlock_x (v) VALUES ('a')")
        .execute(&mut *tx_a)
        .await
        .unwrap();

    let b = tokio::spawn(async move {
        b_beginning_tx.send(()).unwrap();
        let mut tx = Connection::begin_with(&mut *conn_b, "BEGIN IMMEDIATE")
            .await
            .expect("second BEGIN IMMEDIATE must wait, not deadlock");
        let x: i64 = sqlx::query_scalar("SELECT count(*) FROM deadlock_x")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        let y: i64 = sqlx::query_scalar("SELECT count(*) FROM deadlock_y")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        sqlx::query("INSERT INTO deadlock_y (v) VALUES ('b')")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        (x, y)
    });

    b_beginning_rx.await.unwrap();
    sleep(PARK_GRACE).await; // B is parked at its BEGIN IMMEDIATE
    assert!(
        !b.is_finished(),
        "B must be waiting at BEGIN IMMEDIATE while A's write tx is open"
    );

    timeout(
        Duration::from_secs(10),
        sqlx::query("INSERT INTO deadlock_y (v) VALUES ('a')").execute(&mut *tx_a),
    )
    .await
    .expect("writer must not park: the waiting BEGIN IMMEDIATE holds no locks")
    .expect("no deadlock possible writer-vs-writer when both begin IMMEDIATE");
    tx_a.commit().await.unwrap();

    let (x, y) = timeout(STALL_BOUND, b)
        .await
        .expect("B must unpark after A commits")
        .unwrap();
    assert_eq!((x, y), (2, 2), "B's tx starts after A's commit and sees it");
    assert_eq!(count(pool, "deadlock_y").await, 3);
}

/// Pool-level error-path hygiene: after the writer's deadlock error its `Transaction` guard is DROPPED (the production
/// `?` path); the drop-rollback + after_release hook must fully heal the pool and a whole-transaction retry must succeed.
#[tokio::test]
async fn deadlock_semantics_pool_guard_drop_after_release_repair_and_whole_tx_retry() {
    let repo = open_semantics_repo().await;
    let pool = repo.pool().clone();

    let (b_locked_y_tx, b_locked_y_rx) = oneshot::channel::<()>();
    let (go_b_tx, go_b_rx) = oneshot::channel::<()>();
    let (b_parking_tx, b_parking_rx) = oneshot::channel::<()>();

    let pool_b = pool.clone();
    let b = tokio::spawn(async move {
        let mut tx = pool_b.begin().await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM deadlock_y")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(n, 1);
        b_locked_y_tx.send(()).unwrap();
        go_b_rx.await.unwrap();
        b_parking_tx.send(()).unwrap();
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM deadlock_x")
            .fetch_one(&mut *tx)
            .await
            .expect("parked reader completes after the writer's guard-drop rollback");
        assert_eq!(
            n, 1,
            "writer's insert must have been rolled back by the guard drop"
        );
        tx.rollback().await.unwrap();
    });

    let mut tx_a = begin_immediate_tx(&pool).await.unwrap();
    sqlx::query("INSERT INTO deadlock_x (v) VALUES ('a')")
        .execute(&mut *tx_a)
        .await
        .unwrap();
    b_locked_y_rx.await.unwrap();
    go_b_tx.send(()).unwrap();
    b_parking_rx.await.unwrap();
    sleep(PARK_GRACE).await;

    let err = sqlx::query("INSERT INTO deadlock_y (v) VALUES ('a')")
        .execute(&mut *tx_a)
        .await
        .expect_err("cycle-closing writer must fail with the deadlock error");
    let capture = assert_deadlock_shape(&err, "pool writer(A), guard-drop path");
    eprintln!("[deadlock-semantics pool-level] {capture}");

    // Production error path: drop the guard, no explicit rollback; the ping in after_release flushes the queued rollback.
    drop(tx_a);

    timeout(STALL_BOUND, b)
        .await
        .expect("reader must unpark after the writer's guard drop")
        .unwrap();

    // Sweep EVERY pooled connection — none may still be inside a transaction.
    wait_for_pool_settled(&pool).await;
    let settled = pool.size() as usize;
    let mut held: Vec<PoolConnection<sqlx::Sqlite>> = Vec::with_capacity(settled);
    for _ in 0..settled {
        held.push(pool.acquire().await.unwrap());
    }
    for conn in &held {
        assert!(
            !conn.is_in_transaction(),
            "no pooled connection may be left inside the deadlocked transaction"
        );
    }
    drop(held);
    wait_for_pool_settled(&pool).await;

    let mut tx_retry = timeout(STALL_BOUND, begin_immediate_tx(&pool))
        .await
        .expect("begin_immediate_tx must not stall after the deadlock")
        .expect("begin_immediate_tx must succeed on the healed pool");
    sqlx::query("INSERT INTO deadlock_x (v) VALUES ('retry')")
        .execute(&mut *tx_retry)
        .await
        .unwrap();
    sqlx::query("INSERT INTO deadlock_y (v) VALUES ('retry')")
        .execute(&mut *tx_retry)
        .await
        .unwrap();
    tx_retry.commit().await.unwrap();

    assert_eq!(count(&pool, "deadlock_x").await, 2);
    assert_eq!(count(&pool, "deadlock_y").await, 2);
}
