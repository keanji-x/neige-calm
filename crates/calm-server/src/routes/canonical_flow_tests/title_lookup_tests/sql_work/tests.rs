use super::*;
use futures::TryStreamExt;
use sqlx::{ConnectOptions, Connection};
use std::sync::atomic::AtomicUsize;

async fn connection() -> sqlx::SqliteConnection {
    sqlx::sqlite::SqliteConnectOptions::new()
        .row_buffer_size(1)
        .connect()
        .await
        .unwrap()
}
fn invalid(meter: &Meter) {
    assert!(catch_unwind(AssertUnwindSafe(|| meter.finish())).is_err());
}
async fn value(conn: &mut sqlx::SqliteConnection) -> i64 {
    sqlx::query_scalar("SELECT ?1")
        .bind(1_i64)
        .fetch_one(conn)
        .await
        .unwrap()
}
#[tokio::test]
async fn cached_statement_counts_each_execution_once() {
    let mut conn = connection().await;
    let meter = Arc::new(Meter::default());
    let weak = Arc::downgrade(&meter);
    meter.install(&mut conn).await.unwrap();
    let first = meter.open_window();
    assert_eq!(value(&mut conn).await, 1);
    meter.close_window(first);
    let second = meter.open_window();
    assert_eq!(value(&mut conn).await, 1);
    meter.close_window(second);
    meter.remove(&mut conn).await.unwrap();
    let work = meter.finish();
    assert!(work[first].vm > 0);
    assert_eq!(work[first].vm, work[second].vm);
    conn.close().await.unwrap();
    drop(meter);
    assert!(weak.upgrade().is_none());
}

// Changes epochs after a real PROFILE, without constructing VM or trace records.
struct Boundary {
    context: usize,
    meter: Arc<Meter>,
    from: usize,
    to: AtomicUsize,
    profiles: AtomicUsize,
}
unsafe extern "C" fn boundary(
    mask: c_uint,
    data: *mut c_void,
    stmt: *mut c_void,
    text: *mut c_void,
) -> c_int {
    let observer = unsafe { &*data.cast::<Boundary>() };
    unsafe { trace(mask, observer.context as *mut c_void, stmt, text) };
    if mask == SQLITE_TRACE_PROFILE as c_uint
        && observer.profiles.fetch_add(1, Ordering::SeqCst) == 0
        && catch_unwind(AssertUnwindSafe(|| {
            observer.meter.close_window(observer.from);
            observer
                .to
                .store(observer.meter.open_window(), Ordering::SeqCst);
        }))
        .is_err()
    {
        observer.meter.failed.store(true, Ordering::SeqCst);
    }
    if mask == SQLITE_TRACE_CLOSE as c_uint {
        drop(unsafe { Box::from_raw(data.cast::<Boundary>()) });
    }
    0
}
async fn schema_retry(write: bool) {
    let dir = tempfile::tempdir().unwrap();
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(dir.path().join("retry.db"))
        .create_if_missing(true)
        .row_buffer_size(1)
        .shared_cache(false);
    let mut conn = options.connect().await.unwrap();
    let mut external = options.connect().await.unwrap();
    sqlx::raw_sql(if write {
        concat!(
            "CREATE TABLE t(x); CREATE TABLE audit(x); ",
            "CREATE TRIGGER log AFTER INSERT ON t ",
            "BEGIN INSERT INTO audit VALUES(new.x); END",
        )
    } else {
        "CREATE TABLE t(x); INSERT INTO t VALUES(1)"
    })
    .execute(&mut conn)
    .await
    .unwrap();
    let meter = Arc::new(Meter::default());
    meter.install(&mut conn).await.unwrap();
    retry_query(&mut conn, write).await;
    sqlx::query("CREATE INDEX t_x ON t(x)")
        .execute(&mut external)
        .await
        .unwrap();
    let from = meter.open_window();
    let observer = {
        let mut handle = conn.lock_handle().await.unwrap();
        let raw = handle.as_raw_handle().as_ptr();
        let context = meter.state.lock().unwrap().handles[&(raw as usize)].context;
        let observer = Box::into_raw(Box::new(Boundary {
            context,
            meter: meter.clone(),
            from,
            to: AtomicUsize::new(0),
            profiles: AtomicUsize::new(0),
        }));
        let status = unsafe {
            sqlite3_trace_v2(
                raw,
                (SQLITE_TRACE_STMT | SQLITE_TRACE_PROFILE | SQLITE_TRACE_CLOSE) as u32,
                Some(boundary),
                observer.cast(),
            )
        };
        if status != SQLITE_OK {
            // Failed registration leaves the previous trace/context installed.
            drop(unsafe { Box::from_raw(observer) });
            panic!("cannot install boundary trace: {status}");
        }
        observer
    };
    retry_query(&mut conn, write).await;
    // Scenario precondition: the real retry spans multiple PROFILE callbacks.
    assert!(unsafe { &*observer }.profiles.load(Ordering::SeqCst) >= 2);
    let native_vm = {
        let mut handle = conn.lock_handle().await.unwrap();
        let raw = handle.as_raw_handle().as_ptr();
        // Enumerate the current live cached statement under the worker lock;
        // never dereference a retained trace address or read the meter ledger.
        let mut stmt = unsafe { sqlite3_next_stmt(raw, std::ptr::null_mut()) };
        let mut native_vm = None;
        while !stmt.is_null() {
            if unsafe { sqlite3_stmt_status(stmt, SQLITE_STMTSTATUS_REPREPARE, 0) } > 0 {
                assert!(
                    native_vm.is_none(),
                    "multiple retried statements in scenario"
                );
                native_vm = Some(
                    u64::try_from(unsafe {
                        sqlite3_stmt_status(stmt, SQLITE_STMTSTATUS_VM_STEP, 0)
                    })
                    .unwrap(),
                );
            }
            stmt = unsafe { sqlite3_next_stmt(raw, stmt) };
        }
        native_vm.expect("scenario did not reprepare")
    };
    let to = unsafe { &*observer }.to.load(Ordering::SeqCst);
    assert!(to > from);
    assert_eq!(value(&mut conn).await, 1);
    meter.close_window(to);
    let isolated = meter.open_window();
    assert_eq!(value(&mut conn).await, 1);
    meter.close_window(isolated);
    meter.remove(&mut conn).await.unwrap();
    drop(unsafe { Box::from_raw(observer) });
    let work = meter.finish();
    assert!(native_vm > 0);
    assert_eq!(work[from].vm, native_vm);
    assert_eq!(work[from].starts, 1);
    assert_eq!(work[to].starts, 1);
    assert_eq!(work[to].vm, work[isolated].vm);
    if write {
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit")
                .fetch_one(&mut conn)
                .await
                .unwrap(),
            2
        );
    }
    conn.close().await.unwrap();
    external.close().await.unwrap();
}

async fn retry_query(conn: &mut sqlx::SqliteConnection, write: bool) {
    if write {
        sqlx::query("INSERT INTO t VALUES(?1)")
            .bind(1_i64)
            .execute(conn)
            .await
            .unwrap();
    } else {
        let rows: Vec<i64> = sqlx::query_scalar("SELECT x FROM t")
            .fetch_all(conn)
            .await
            .unwrap();
        assert_eq!(rows, [1]);
    }
}

#[tokio::test]
async fn real_cached_schema_retry_retains_all_vm_in_start_epoch() {
    schema_retry(false).await;
}

#[tokio::test]
async fn real_cached_write_retry_trigger_continuations_retain_start_epoch() {
    schema_retry(true).await;
}

#[tokio::test]
async fn dropped_stream_is_settled_in_start_epoch_at_reset() {
    let mut conn = connection().await;
    let meter = Arc::new(Meter::default());
    meter.install(&mut conn).await.unwrap();
    let from = meter.open_window();
    let mut rows = sqlx::query("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1024) SELECT x FROM n").fetch(&mut conn);
    assert!(rows.try_next().await.unwrap().is_some());
    meter.close_window(from);
    let to = meter.open_window();
    drop(rows);
    {
        let _handle = conn.lock_handle().await.unwrap();
    }
    assert_eq!(value(&mut conn).await, 1);
    meter.close_window(to);
    meter.remove(&mut conn).await.unwrap();
    let work = meter.finish();
    assert!(work[from].vm > work[to].vm);
    assert_eq!(work[from].starts, 1);
    conn.close().await.unwrap();
}
#[tokio::test]
async fn trigger_and_nonretry_error_are_counted_with_comment_sql() {
    let mut conn = connection().await;
    sqlx::raw_sql(concat!(
        "CREATE TABLE t(x UNIQUE); CREATE TABLE audit(x); ",
        "CREATE TRIGGER log AFTER INSERT ON t ",
        "BEGIN INSERT INTO audit VALUES(new.x); END;",
    ))
    .execute(&mut conn)
    .await
    .unwrap();
    let meter = Arc::new(Meter::default());
    meter.install(&mut conn).await.unwrap();
    let first = meter.open_window();
    sqlx::query("-- comment\nINSERT INTO t VALUES(1)")
        .execute(&mut conn)
        .await
        .unwrap();
    meter.close_window(first);
    let error = meter.open_window();
    let err = sqlx::query("-- comment\nINSERT INTO t VALUES(1)")
        .execute(&mut conn)
        .await
        .unwrap_err();
    assert!(err.as_database_error().unwrap().is_unique_violation());
    meter.close_window(error);
    meter.remove(&mut conn).await.unwrap();
    let work = meter.finish();
    assert_eq!(work[first].starts, 1);
    assert_eq!(work[error].starts, 1);
    assert!(work[first].vm > work[error].vm);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit")
            .fetch_one(&mut conn)
            .await
            .unwrap(),
        1
    );
    conn.close().await.unwrap();
}
#[tokio::test]
async fn native_finalize_and_reused_addresses_retain_start_epoch() {
    let mut conn = connection().await;
    let meter = Arc::new(Meter::default());
    meter.install(&mut conn).await.unwrap();
    let mut epochs = Vec::new();
    let mut addresses = HashSet::new();
    let mut reused = false;
    for _ in 0..32 {
        let epoch = meter.open_window();
        let mut handle = conn.lock_handle().await.unwrap();
        let raw = handle.as_raw_handle().as_ptr();
        let mut stmt = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                sqlite3_prepare_v3(
                    raw,
                    c"SELECT 1 UNION ALL SELECT 2".as_ptr(),
                    -1,
                    0,
                    &mut stmt,
                    std::ptr::null_mut(),
                )
            },
            SQLITE_OK
        );
        reused |= !addresses.insert(stmt as usize);
        assert_eq!(unsafe { sqlite3_step(stmt) }, SQLITE_ROW);
        meter.close_window(epoch);
        assert_eq!(unsafe { sqlite3_finalize(stmt) }, SQLITE_OK);
        drop(handle);
        epochs.push(epoch);
    }
    meter.remove(&mut conn).await.unwrap();
    let work = meter.finish();
    let vm = work[epochs[0]].vm;
    assert!(vm > 0);
    assert!(epochs.into_iter().all(|epoch| work[epoch].vm == vm));
    eprintln!("native-finalize pointer-reuse-observed={reused}");
    conn.close().await.unwrap();
}
#[tokio::test]
async fn missing_profile_fails_after_native_reset_and_releases_context() {
    let mut conn = connection().await;
    let meter = Arc::new(Meter::default());
    let weak = Arc::downgrade(&meter);
    meter.install(&mut conn).await.unwrap();
    {
        let mut handle = conn.lock_handle().await.unwrap();
        let raw = handle.as_raw_handle().as_ptr();
        let mut stmt = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                sqlite3_prepare_v3(
                    raw,
                    c"SELECT 1".as_ptr(),
                    -1,
                    0,
                    &mut stmt,
                    std::ptr::null_mut(),
                )
            },
            SQLITE_OK
        );
        assert_eq!(unsafe { sqlite3_step(stmt) }, SQLITE_ROW);
        // Remove PROFILE deliberately, rather than rejecting legal retries.
        unsafe { sqlite3_trace_v2(raw, 0, None, std::ptr::null_mut()) };
        assert_eq!(unsafe { sqlite3_finalize(stmt) }, SQLITE_OK);
    }
    meter.remove(&mut conn).await.unwrap();
    invalid(&meter);
    conn.close().await.unwrap();
    drop(meter);
    assert!(weak.upgrade().is_none());
}
#[tokio::test]
async fn cold_schema_prepare_keeps_nonzero_raw_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(dir.path().join("cold.db"))
        .create_if_missing(true)
        .shared_cache(false);
    let mut setup = options.connect().await.unwrap();
    for index in 0..64 {
        sqlx::raw_sql(&format!("CREATE TABLE t{index}(x)"))
            .execute(&mut setup)
            .await
            .unwrap();
    }
    let mut conn = options.connect().await.unwrap();
    let meter = Arc::new(Meter::default());
    meter.install(&mut conn).await.unwrap();
    let epoch = meter.open_window();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_schema")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(count, 64);
    meter.close_window(epoch);
    let warm = meter.open_window();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_schema")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(count, 64);
    meter.close_window(warm);
    meter.remove(&mut conn).await.unwrap();
    let work = meter.finish();
    assert_eq!(work[epoch].vm, work[warm].vm);
    assert!(work[epoch].raw_ticks > 0);
    assert!(work[epoch].raw_ticks > work[warm].raw_ticks);
    assert!(meter.state.lock().unwrap().untraced_ticks > 0);
    conn.close().await.unwrap();
    setup.close().await.unwrap();
}
#[tokio::test]
async fn zero_window_fails_instead_of_returning_zero() {
    let meter = Meter::default();
    let epoch = meter.open_window();
    meter.close_window(epoch);
    invalid(&meter);
}
#[tokio::test]
async fn poisoned_callback_and_removal_release_context() {
    let mut conn = connection().await;
    let meter = Arc::new(Meter::default());
    let weak = Arc::downgrade(&meter);
    meter.install(&mut conn).await.unwrap();
    meter.update(|_| panic!("callback panic"));
    assert_eq!(value(&mut conn).await, 1);
    meter.remove(&mut conn).await.unwrap();
    invalid(&meter);
    conn.close().await.unwrap();
    drop(meter);
    assert!(weak.upgrade().is_none());
}
#[tokio::test]
async fn unexpected_close_releases_poisoned_context() {
    let mut conn = connection().await;
    let meter = Arc::new(Meter::default());
    let weak = Arc::downgrade(&meter);
    meter.install(&mut conn).await.unwrap();
    meter.update(|_| panic!("callback panic"));
    conn.close().await.unwrap();
    invalid(&meter);
    drop(meter);
    assert!(weak.upgrade().is_none());
}
#[tokio::test]
async fn duplicate_setup_panic_preserves_original_cleanup() {
    use futures::FutureExt;
    let mut conn = connection().await;
    let meter = Arc::new(Meter::default());
    let weak = Arc::downgrade(&meter);
    meter.install(&mut conn).await.unwrap();
    assert!(
        AssertUnwindSafe(meter.install(&mut conn))
            .catch_unwind()
            .await
            .is_err()
    );
    meter.remove(&mut conn).await.unwrap();
    invalid(&meter);
    conn.close().await.unwrap();
    drop(meter);
    assert!(weak.upgrade().is_none());
}
#[tokio::test]
async fn execution_total_overflow_fails() {
    let mut conn = connection().await;
    let meter = Arc::new(Meter::default());
    meter.install(&mut conn).await.unwrap();
    meter.state.lock().unwrap().total = u64::MAX;
    assert_eq!(value(&mut conn).await, 1);
    meter.remove(&mut conn).await.unwrap();
    invalid(&meter);
    conn.close().await.unwrap();
}
#[tokio::test]
async fn counter_history_overflow_fails_before_reading_undefined_vm() {
    let mut conn = connection().await;
    let meter = Arc::new(Meter::default());
    meter.install(&mut conn).await.unwrap();
    {
        let mut handle = conn.lock_handle().await.unwrap();
        let raw = handle.as_raw_handle().as_ptr();
        let mut stmt = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                sqlite3_prepare_v3(
                    raw,
                    c"SELECT 1".as_ptr(),
                    -1,
                    0,
                    &mut stmt,
                    std::ptr::null_mut(),
                )
            },
            SQLITE_OK
        );
        assert_eq!(unsafe { sqlite3_step(stmt) }, SQLITE_ROW);
        meter
            .state
            .lock()
            .unwrap()
            .handles
            .get_mut(&(raw as usize))
            .unwrap()
            .raw = u32::MAX as u64;
        assert_eq!(unsafe { sqlite3_finalize(stmt) }, SQLITE_OK);
    }
    meter.remove(&mut conn).await.unwrap();
    invalid(&meter);
    conn.close().await.unwrap();
}

#[tokio::test]
async fn unfinished_retry_fails_native_liveness_at_removal() {
    let dir = tempfile::tempdir().unwrap();
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(dir.path().join("busy.db"))
        .create_if_missing(true)
        .shared_cache(false);
    let mut conn = options.connect().await.unwrap();
    let mut external = options.connect().await.unwrap();
    sqlx::raw_sql("CREATE TABLE t(x); INSERT INTO t VALUES(1)")
        .execute(&mut conn)
        .await
        .unwrap();
    let meter = Arc::new(Meter::default());
    meter.install(&mut conn).await.unwrap();
    let mut stmt = std::ptr::null_mut();
    {
        let mut handle = conn.lock_handle().await.unwrap();
        let raw = handle.as_raw_handle().as_ptr();
        assert_eq!(
            unsafe {
                sqlite3_prepare_v3(
                    raw,
                    c"SELECT x FROM t".as_ptr(),
                    -1,
                    SQLITE_PREPARE_PERSISTENT,
                    &mut stmt,
                    std::ptr::null_mut(),
                )
            },
            SQLITE_OK
        );
        assert_eq!(unsafe { sqlite3_step(stmt) }, SQLITE_ROW);
        assert_eq!(unsafe { sqlite3_step(stmt) }, SQLITE_DONE);
        assert_eq!(unsafe { sqlite3_reset(stmt) }, SQLITE_OK);
    }
    sqlx::query("CREATE INDEX t_x ON t(x)")
        .execute(&mut external)
        .await
        .unwrap();
    {
        let _handle = conn.lock_handle().await.unwrap();
        assert_eq!(unsafe { sqlite3_step(stmt) }, SQLITE_ROW);
    }
    // The intermediate schema PROFILE is present, but native ROW remains busy.
    assert!(
        meter.state.lock().unwrap().handles.values().all(|h| h
            .execution
            .as_ref()
            .unwrap()
            .profiled)
    );
    meter.remove(&mut conn).await.unwrap();
    invalid(&meter);
    {
        let _handle = conn.lock_handle().await.unwrap();
        assert_eq!(unsafe { sqlite3_finalize(stmt) }, SQLITE_OK);
    }
    conn.close().await.unwrap();
    external.close().await.unwrap();
}
