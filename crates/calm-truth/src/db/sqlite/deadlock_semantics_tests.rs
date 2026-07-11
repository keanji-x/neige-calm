//! #930 — deterministic pins of the shared-cache **"database is
//! deadlocked"** (`SQLITE_LOCKED`, code 6) semantics on the exact stack
//! the app uses: sqlx 0.8.6 pool (via `SqlxRepo::open`) on
//! `sqlite::memory:`.
//!
//! These tests are the mechanism backbone behind the #930 rule —
//! *production deferred transactions must be READ-ONLY; every writing
//! transaction uses `begin_immediate_tx`* — and must keep passing so the
//! rule's premises stay verified against the vendored sqlite/sqlx pair:
//!
//! * only a lock-HOLDING waiter (an explicit tx) can be a cycle party;
//! * both-sides-IMMEDIATE is structurally immune (the second IMMEDIATE
//!   parks at BEGIN holding nothing);
//! * autocommit statements can never deadlock a writer (the implicit-tx
//!   unwind releases locks before parking);
//! * mid-transaction statement retry re-deadlocks instantly; only a
//!   whole-transaction restart is a valid retry;
//! * the #920 after_release hook heals the pool after a deadlock error's
//!   guard drop.
//!
//! Mechanism (verified against vendored sqlx-sqlite 0.8.6 +
//! libsqlite3-sys 0.30.1 / bundled SQLite 3.46.0):
//!
//! * `sqlite::memory:` is a shared-cache database. Shared-cache uses
//!   table-granularity read/write locks; locks are held until the end of
//!   the owning transaction (explicit tx) or statement (autocommit).
//! * sqlx builds libsqlite3-sys with `unlock_notify`. When `sqlite3_step`
//!   returns `SQLITE_LOCKED_SHAREDCACHE` (extended 262), sqlx's
//!   `StatementHandle::step` calls `sqlite3_unlock_notify` and **parks the
//!   connection's worker thread** on a condvar until the blocking
//!   connection *concludes its transaction* (`statement/unlock_notify.rs`).
//! * `sqlite3_unlock_notify` walks the waits-for graph at registration
//!   time. If registering would close a cycle it fails the registration
//!   with **plain** `SQLITE_LOCKED` (6, not 262) and sets the connection
//!   error message to `"database is deadlocked"` (notify.c). sqlx then
//!   surfaces `SqliteError { code: 6, message: "database is deadlocked" }`.
//! * Therefore: the side that **closes the cycle** (registers second) gets
//!   the error; the first waiter **stays parked** until the erroring
//!   side's transaction concludes — retrying the failed statement inside
//!   the same transaction re-deadlocks instantly.
//! * Only EXPLICIT transactions can be the lock-HOLDING waiter: when a
//!   statement fails with LOCKED in autocommit mode, `sqlite3VdbeHalt`
//!   rolls back the implicit transaction (releasing its table locks)
//!   before sqlx parks, whereas inside an explicit tx it only rolls back
//!   the statement journal and the tx keeps its locks.
//!
//! `PRAGMA busy_timeout` is irrelevant here (it only governs
//! `SQLITE_BUSY` file-level locks; shared-cache conflicts are
//! `SQLITE_LOCKED_*`), and `PRAGMA journal_mode = WAL` is a no-op for
//! in-memory databases — there is no WAL to give readers snapshot
//! isolation, which is exactly why reader/writer table-lock cycles exist.

use std::time::{Duration, Instant};

use sqlx::Connection;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::pool::PoolConnection;
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};

use super::{SqlxRepo, begin_immediate_tx, is_sqlite_busy};

/// Bound for joins/statements that must complete promptly once unblocked;
/// far below the pool reaper's 600 s idle_timeout release valve.
const STALL_BOUND: Duration = Duration::from_secs(30);

/// Grace period after the peer signalled "about to issue the parking
/// statement": dispatch to the connection's dedicated worker thread +
/// step + unlock_notify registration is sub-millisecond, so 300 ms pins
/// the park order ~10^3 x over. The asserts on WHICH side errors would
/// fail loudly if this were ever violated (measured 20/20 in the loop).
const PARK_GRACE: Duration = Duration::from_millis(300);

/// Open the app's real repo (pool + pragmas + #920 after_release hook +
/// #926 memory anchor) and add two scratch tables, one seed row each.
async fn open_semantics_repo() -> SqlxRepo {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let pool = repo.pool();
    // Plain rowid tables — deliberately no AUTOINCREMENT so inserts don't
    // drag the sqlite_sequence table's locks into the picture.
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

/// Pin the exact error shape the #930 rule rests on, and verify the
/// app's `is_sqlite_busy` predicate matches it. Returns a capture string
/// for the test log.
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
    // notify.c sets the PLAIN primary code (6), not the extended
    // SQLITE_LOCKED_SHAREDCACHE (262) that triggered the parked wait.
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

/// Copied from `pool_tx_repair_tests` — wait until every pool connection
/// has been through the async return path (incl. the #920 after_release
/// hook) and is parked idle again.
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

/// Interleaving 1 — the WRITER parks first, the READER closes the cycle:
///
/// * A (writer): `BEGIN IMMEDIATE`; `INSERT x` (write-lock x) — then
///   `INSERT y`, which parks in unlock_notify behind B's read-lock on y.
/// * B (reader): `BEGIN` (deferred); `SELECT y` (read-lock y, held for the
///   tx) — then `SELECT x`, which would wait on A: registration closes the
///   cycle, so **B gets "database is deadlocked"**; A stays parked.
///
/// Also proves: the error does NOT unpark A; only B's ROLLBACK does.
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
        // Parks: B holds the shared-cache read lock on deadlock_y.
        sqlx::query("INSERT INTO deadlock_y (v) VALUES ('a')")
            .execute(&mut *tx)
            .await
            .expect("first unlock_notify waiter must NOT error; it completes after B rolls back");
        tx.commit().await.unwrap();
    });

    // Reader: deferred tx, acquire + hold read-lock on y.
    let mut tx_b = Connection::begin(&mut *conn_b).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM deadlock_y")
        .fetch_one(&mut *tx_b)
        .await
        .unwrap();
    assert_eq!(n, 1);
    go_a_tx.send(()).unwrap();
    a_parking_rx.await.unwrap();
    sleep(PARK_GRACE).await; // A is now parked on y

    // Close the cycle from the reader side.
    let err = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM deadlock_x")
        .fetch_one(&mut *tx_b)
        .await
        .expect_err("cycle-closing reader must fail with the deadlock error");
    let capture = assert_deadlock_shape(&err, "reader(B) closes cycle");
    eprintln!("[deadlock-semantics reader-closes] {capture}");

    // KEY semantic: B's error does NOT unpark A — A waits for B's tx to
    // CONCLUDE, not for B's statement to fail.
    sleep(Duration::from_millis(200)).await;
    assert!(
        !a.is_finished(),
        "writer must stay parked while the errored reader's tx is still open"
    );

    // Errored reader tx is still open and rolls back cleanly...
    tx_b.rollback()
        .await
        .expect("errored reader tx must still ROLLBACK cleanly");
    // ...which concludes B's tx and unparks A.
    timeout(STALL_BOUND, a)
        .await
        .expect("writer must unpark once the reader tx concluded")
        .unwrap();

    assert_eq!(count(pool, "deadlock_x").await, 2);
    assert_eq!(count(pool, "deadlock_y").await, 2);
}

/// Interleaving 2 — the READER parks first, the WRITER closes the cycle.
/// This is the production `write_with_event` symptom: the `BEGIN
/// IMMEDIATE` write transaction is the side that fails with
/// "database is deadlocked".
///
/// Also proves, on the erroring writer:
/// * same-tx statement retry re-deadlocks immediately (cycle still there);
/// * the tx is NOT auto-rolled-back by the error (own uncommitted write
///   still visible) and ROLLBACKs cleanly;
/// * a whole-transaction retry succeeds once the reader's tx has ended.
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
        // Parks behind A's write-lock on deadlock_x until A's tx concludes.
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM deadlock_x")
            .fetch_one(&mut *tx)
            .await
            .expect("first unlock_notify waiter (reader) must NOT get the deadlock error");
        // A rolled back, so only the seed row is visible.
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

    // Close the cycle from the writer side — production shape.
    let err = sqlx::query("INSERT INTO deadlock_y (v) VALUES ('a')")
        .execute(&mut *tx_a)
        .await
        .expect_err("cycle-closing writer must fail with the deadlock error");
    let capture = assert_deadlock_shape(&err, "writer(A) closes cycle [production shape]");
    eprintln!("[deadlock-semantics writer-closes] {capture}");

    // Statement-level retry INSIDE the same tx is futile: B is still parked
    // waiting on OUR transaction, so re-registration closes the same cycle.
    let err2 = sqlx::query("INSERT INTO deadlock_y (v) VALUES ('a2')")
        .execute(&mut *tx_a)
        .await
        .expect_err("same-tx statement retry must re-deadlock immediately");
    assert_deadlock_shape(&err2, "writer(A) same-tx statement retry");

    // B is still parked — the writer's error concluded nothing.
    sleep(Duration::from_millis(200)).await;
    assert!(
        !b.is_finished(),
        "reader must stay parked while the errored writer's tx is still open"
    );

    // Post-deadlock tx state: NOT auto-rolled-back — the tx still serves
    // reads and its own uncommitted write is still visible...
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM deadlock_x")
        .fetch_one(&mut *tx_a)
        .await
        .expect("errored writer tx must still serve reads on its own locked table");
    assert_eq!(n, 2, "seed + own uncommitted insert: tx not auto-aborted");
    // ...and ROLLBACKs cleanly, which unparks B.
    tx_a.rollback()
        .await
        .expect("errored writer tx must ROLLBACK cleanly");
    timeout(STALL_BOUND, b)
        .await
        .expect("reader must unpark once the writer tx concluded")
        .unwrap();

    // Valid retry shape — a WHOLE-transaction restart succeeds on attempt 2 now
    // that the reader's tx has ended.
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

/// Variant (a), part 1 — the reader is a PLAIN AUTOCOMMIT single-statement
/// join (no explicit transaction), FROM order `y, x`.
///
/// Empirical answer (verified against the bundled 3.46.0 amalgamation,
/// `sqlite3VdbeHalt`): the autocommit join does **NOT** deadlock the
/// writer, even though its OP_TableLock prologue provably acquires y's
/// read lock BEFORE blocking on x (asserted below via EXPLAIN). When the
/// step fails with `SQLITE_LOCKED_SHAREDCACHE`, VdbeHalt's autocommit
/// branch runs `sqlite3RollbackAll(db, SQLITE_OK)` — the implicit
/// transaction unwinds and releases every table lock the statement had
/// acquired — and only then does sqlx park in unlock_notify. A parked
/// autocommit statement therefore holds NO locks and can never be the
/// lock-holding side of a cycle. (Contrast: a statement failing inside an
/// EXPLICIT tx only takes `eStatementOp = SAVEPOINT_ROLLBACK` — the tx and
/// its table locks survive, which is what makes tests 1/2 deadlock.)
///
/// Production implication: the reader that deadlocked `write_with_event`
/// CANNOT have been a plain autocommit query; it must have been an
/// explicit (deferred/read) transaction on another pool connection.
#[tokio::test]
async fn deadlock_semantics_autocommit_join_y_first_no_cycle_error_unwind_releases_locks() {
    let repo = open_semantics_repo().await;
    let pool = repo.pool();

    // Prove the lock-acquisition order claim: the join's OP_TableLock
    // prologue (shared-cache only) locks deadlock_y FIRST, deadlock_x
    // second — so when it parks on x it HAD acquired y.
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
        // Autocommit: locks y, blocks on x -> implicit tx unwinds (y
        // released) -> parks holding nothing.
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM deadlock_y, deadlock_x")
            .fetch_one(&mut *conn_b)
            .await
            .expect("parked autocommit join must complete after the writer commits");
        n
    });

    b_parking_rx.await.unwrap();
    sleep(PARK_GRACE).await; // B is parked on x — holding NO locks

    // No cycle: the writer's INSERT y proceeds despite the parked join
    // having ACQUIRED y's lock earlier in its statement prologue.
    timeout(
        Duration::from_secs(10),
        sqlx::query("INSERT INTO deadlock_y (v) VALUES ('a')").execute(&mut *tx_a),
    )
    .await
    .expect("writer must not park: the parked autocommit join's locks were released by the error unwind")
    .expect("no deadlock: autocommit statements cannot hold-and-wait");
    tx_a.commit().await.unwrap();

    // The join restarts after A's commit and sees the committed state.
    let n = timeout(STALL_BOUND, b)
        .await
        .expect("join must unpark once the writer committed")
        .unwrap();
    assert_eq!(n, 4, "(seed+a) x (seed+a) after the writer's commit");
}

/// Variant (a), part 2 — same autocommit join but FROM order `x, y`: the
/// FIRST OP_TableLock (x) is the blocked one, so the statement parks
/// holding NO table locks. No cycle is possible: the writer's `INSERT y`
/// succeeds, it commits, the join unparks, restarts, and sees the
/// committed data (2 x 2 rows). Lock-acquisition ORDER inside the reader's
/// statement — not autocommit-ness — decides whether variant (a) deadlocks.
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
        // Parks on its FIRST table lock (x) — holds nothing while parked.
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM deadlock_x, deadlock_y")
            .fetch_one(&mut *conn_b)
            .await
            .expect("parked autocommit join must complete after the writer commits");
        n
    });

    b_parking_rx.await.unwrap();
    sleep(PARK_GRACE).await; // B is parked on x, holding NO locks

    // No cycle: the parked reader holds nothing, so the writer proceeds.
    timeout(
        Duration::from_secs(10),
        sqlx::query("INSERT INTO deadlock_y (v) VALUES ('a')").execute(&mut *tx_a),
    )
    .await
    .expect("writer must not park: the join blocked on its first lock holds nothing")
    .expect("no deadlock when the reader's blocked lock is its first");
    tx_a.commit().await.unwrap();

    // The join restarts after A's commit and sees the committed state.
    let n = timeout(STALL_BOUND, b)
        .await
        .expect("join must unpark once the writer committed")
        .unwrap();
    assert_eq!(n, 4, "(seed+a) x (seed+a) after the writer's commit");
}

/// Variant (b) — `BEGIN IMMEDIATE` on BOTH sides cannot cycle: a shared
/// cache admits at most ONE write transaction at a time, so the second
/// `BEGIN IMMEDIATE` parks at BEGIN itself, holding no locks while it
/// waits. The first writer runs to commit unhindered; the second then
/// proceeds. The deadlock cycle requires a lock-HOLDING waiter — i.e. a
/// reader (or a deferred tx that acquired read locks before writing).
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
        // Parks at BEGIN IMMEDIATE (one write tx per shared cache).
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

    // No cycle can form: the waiting BEGIN IMMEDIATE holds nothing, so A
    // writes BOTH tables and commits unhindered.
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

/// Error-path pool hygiene + retry shape at the POOL level, on the app's
/// own primitives:
/// `begin_immediate_tx` writer vs a `pool.begin()` reader; after the
/// writer's deadlock error its `Transaction` guard is DROPPED (the
/// production `?`-propagation path, no explicit rollback). The queued
/// drop-rollback + #920 after_release hook must fully heal the pool: the
/// parked reader unparks, no pooled connection stays inside a transaction,
/// and a whole-transaction retry via `begin_immediate_tx` succeeds.
#[tokio::test]
async fn deadlock_semantics_pool_guard_drop_after_release_repair_and_whole_tx_retry() {
    let repo = open_semantics_repo().await;
    let pool = repo.pool().clone();

    let (b_locked_y_tx, b_locked_y_rx) = oneshot::channel::<()>();
    let (go_b_tx, go_b_rx) = oneshot::channel::<()>();
    let (b_parking_tx, b_parking_rx) = oneshot::channel::<()>();

    let pool_b = pool.clone();
    let b = tokio::spawn(async move {
        // Deferred read tx on its own pool connection.
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

    // Writer — the app's real helper, exactly like `write_with_event`.
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

    // Production error path: drop the guard, no explicit rollback. The
    // Transaction drop queues ROLLBACK on the worker; releasing the
    // PoolConnection runs the #920 after_release hook (ping flushes the
    // queued rollback, so the hook sees a clean connection).
    drop(tx_a);

    // The drop-rollback CONCLUDES A's tx -> the parked reader unparks.
    timeout(STALL_BOUND, b)
        .await
        .expect("reader must unpark after the writer's guard drop")
        .unwrap();

    // Pool must be fully healed: settle, then sweep EVERY pooled
    // connection — none may still be inside a transaction.
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

    // Valid retry shape — op-level whole-transaction retry on the healed pool:
    // fresh BEGIN IMMEDIATE, re-run both statements, commit. Succeeds on
    // attempt 2 because the reader's tx has ended.
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
