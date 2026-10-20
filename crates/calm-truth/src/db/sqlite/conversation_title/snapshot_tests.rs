//! SQLite PROFILE fires after a SELECT finishes and before sqlx returns its result.
//! Start a separate WAL writer at the first SELECT; reader commit releases it.
use super::*;
use crate::db::RepoRead;
use crate::session_projection_repo::{AgentProvider, WorkerSessionInit, WorkerSessionState};
use libsqlite3_sys::{
    SQLITE_TRACE_CLOSE, SQLITE_TRACE_PROFILE, SQLITE_TRACE_STMT, sqlite3_sql, sqlite3_trace_v2,
};
use serde_json::json;
use std::ffi::{CStr, c_int, c_uint, c_void};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

struct Schedule {
    ready: mpsc::Sender<()>,
    proceed: Mutex<mpsc::Receiver<()>>,
    fired: std::sync::atomic::AtomicBool,
    writer_started_before_return: std::sync::atomic::AtomicBool,
    begin_boundary: std::sync::atomic::AtomicBool,
    begin_seen: std::sync::atomic::AtomicBool,
    queries: AtomicUsize,
    query_ns: AtomicU64,
    acquired: Mutex<Option<Instant>>,
    held: Mutex<Option<Duration>>,
}

// SQLite owns this Box until CLOSE. PROFILE's statement pointer is valid for the callback.
unsafe extern "C" fn trace(
    mask: c_uint,
    context: *mut c_void,
    stmt: *mut c_void,
    elapsed: *mut c_void,
) -> c_int {
    if mask == SQLITE_TRACE_CLOSE as c_uint {
        drop(unsafe { Box::from_raw(context.cast::<Arc<Schedule>>()) });
    } else if mask == SQLITE_TRACE_PROFILE as c_uint || mask == SQLITE_TRACE_STMT as c_uint {
        let schedule = unsafe { &*context.cast::<Arc<Schedule>>() };
        let sql = unsafe { CStr::from_ptr(sqlite3_sql(stmt.cast())) }.to_string_lossy();
        let sql = sql.trim_start();
        if mask == SQLITE_TRACE_PROFILE as c_uint {
            if sql == "BEGIN IMMEDIATE" {
                *schedule.acquired.lock().unwrap() = Some(Instant::now());
            } else if sql == "COMMIT" {
                if let Some(start) = schedule.acquired.lock().unwrap().take() {
                    *schedule.held.lock().unwrap() = Some(start.elapsed());
                }
            } else if sql.starts_with("SELECT") {
                schedule.queries.fetch_add(1, Ordering::SeqCst);
                schedule
                    .query_ns
                    .fetch_add(unsafe { *elapsed.cast::<u64>() }, Ordering::SeqCst);
            }
        }
        let begin = mask == SQLITE_TRACE_STMT as c_uint
            && sql == "BEGIN IMMEDIATE"
            && schedule.begin_boundary.load(Ordering::SeqCst);
        if begin {
            schedule.begin_seen.store(true, Ordering::SeqCst);
        }
        if (begin
            || (mask == SQLITE_TRACE_PROFILE as c_uint
                && sql.starts_with("SELECT c.id AS card_id, h.id AS item_id")))
            && !schedule
                .fired
                .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            // Timeouts only diagnose a broken test handshake; never retry or select an outcome.
            let _ = schedule.ready.send(());
            // STMT runs inside SQLite's execution prologue. Never park here:
            // begin itself must wait on writer eligibility, with no test-held mutex.
            if begin {
                return 0;
            }
            if schedule
                .proceed
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .is_ok()
            {
                schedule
                    .writer_started_before_return
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }
    0
}

struct Fixture {
    reader: Arc<SqlxRepo>,
    writer: Arc<SqlxRepo>,
    card: String,
    track: String,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("titles.db").display());
        Self::open(&url, dir, false).await
    }

    async fn memory() -> Self {
        let url = format!(
            "sqlite:file:title-{}?mode=memory&cache=shared",
            uuid::Uuid::new_v4()
        );
        Self::open(&url, tempfile::tempdir().unwrap(), false).await
    }

    async fn open(url: &str, dir: tempfile::TempDir, shared: bool) -> Self {
        let writer = Arc::new(SqlxRepo::open(url).await.unwrap());
        let (card, track) = super::super::session_conversation_tests::card(&writer).await;
        sqlx::query("UPDATE cards SET role='assistant' WHERE id=?1")
            .bind(&card)
            .execute(writer.db().writer_pool())
            .await
            .unwrap();
        let mut tx = super::super::begin_immediate_tx(writer.db()).await.unwrap();
        super::super::session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit::managed_assistant(
                "title-runtime".into(),
                card.clone(),
                AgentProvider::Codex,
                WorkerSessionState::Idle,
                None,
                json!({"pending":"original"}),
                0,
            ),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let reader = if shared {
            writer.clone()
        } else {
            Arc::new(SqlxRepo::open(url).await.unwrap())
        };
        Self {
            reader,
            writer,
            card,
            track,
            _dir: dir,
        }
    }

    async fn insert(&self, visible: bool, text: &str) -> i64 {
        super::tests::insert(
            &self.writer,
            &self.card,
            &self.track,
            (!visible)
                .then(|| json!([{"presentation":"system","text":"context","attachments":[]}])),
            text,
        )
        .await
    }

    async fn schedule(&self) -> (Arc<Schedule>, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (commit_tx, commit_rx) = mpsc::channel();
        let schedule = Arc::new(Schedule {
            ready: ready_tx,
            proceed: Mutex::new(commit_rx),
            fired: false.into(),
            writer_started_before_return: false.into(),
            begin_boundary: false.into(),
            begin_seen: false.into(),
            queries: 0.into(),
            query_ns: 0.into(),
            acquired: Mutex::new(None),
            held: Mutex::new(None),
        });
        // Instrument every connection while retaining the real repo pool/hooks.
        let mut held = Vec::new();
        for _ in 0..self
            .reader
            .db()
            .writer_pool()
            .options()
            .get_max_connections()
        {
            let mut conn = self.reader.db().writer_pool().acquire().await.unwrap();
            let mut handle = conn.lock_handle().await.unwrap();
            let context = Box::into_raw(Box::new(schedule.clone())).cast();
            assert_eq!(
                unsafe {
                    sqlite3_trace_v2(
                        handle.as_raw_handle().as_ptr(),
                        (SQLITE_TRACE_PROFILE | SQLITE_TRACE_CLOSE | SQLITE_TRACE_STMT) as c_uint,
                        Some(trace),
                        context,
                    )
                },
                0
            );
            drop(handle);
            held.push(conn);
        }
        drop(held);
        (schedule, ready_rx, commit_tx)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_title_snapshot_drain_between_reads_retains_source() {
    let f = Fixture::new().await;
    let (schedule, ready, committed) = f.schedule().await;
    let writer = f.writer.clone();
    let card = f.card.clone();
    let track = f.track.clone();
    let write = tokio::spawn(async move {
        tokio::task::spawn_blocking(move || ready.recv_timeout(Duration::from_secs(10)).unwrap())
            .await
            .unwrap();
        committed.send(()).unwrap();
        super::tests::insert(&writer, &card, &track, None, "Migrating input").await;
        let mut tx = super::super::begin_immediate_tx(writer.db()).await.unwrap();
        super::super::session_set_handle_state_tx(
            &mut tx,
            &"title-runtime".into(),
            Some(json!({"pending":null})),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    });
    let context = f
        .reader
        .conversation_title_context(std::slice::from_ref(&f.card))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), write)
        .await
        .expect("writer must complete after reader commit")
        .unwrap();
    assert!(schedule.fired.load(std::sync::atomic::Ordering::SeqCst));
    assert!(
        schedule
            .writer_started_before_return
            .load(std::sync::atomic::Ordering::SeqCst)
    );
    assert!(context.sources[0].input.is_none());
    assert_eq!(
        context.runtimes[&f.card].handle_state_json,
        Some(json!({"pending":"original"}))
    );
    let after = f
        .writer
        .conversation_title_context(std::slice::from_ref(&f.card))
        .await
        .unwrap();
    assert_eq!(
        after.sources[0].input.as_ref().unwrap().title().as_deref(),
        Some("Migrating input")
    );
    assert_eq!(
        after.runtimes[&f.card].handle_state_json,
        Some(json!({"pending":null}))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_title_snapshot_atomic_first_turn_replace_is_coherent() {
    atomic_replace(Fixture::new().await).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_title_shared_cache_reader_first_replace() {
    atomic_replace(Fixture::memory().await).await;
}

async fn atomic_replace(f: Fixture) {
    let old = f.insert(true, "Original first").await;
    let mut tx = f.writer.db().writer_pool().begin().await.unwrap();
    super::super::session_set_handle_state_tx(
        &mut tx,
        &"title-runtime".into(),
        Some(json!({"pending":null})),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (schedule, ready, committed) = f.schedule().await;
    let writer = f.writer.clone();
    let card = f.card.clone();
    let track = f.track.clone();
    let write = tokio::spawn(async move {
        tokio::task::spawn_blocking(move || ready.recv_timeout(Duration::from_secs(10)).unwrap())
            .await
            .unwrap();
        committed.send(()).unwrap();
        let mut tx = super::super::begin_immediate_tx(writer.db()).await.unwrap();
        // Same transaction boundary as replace_command: cut transcript and persist pending.
        assert_eq!(
            super::super::transcript_delete_thread_suffix_tx(
                &mut tx,
                "title-runtime",
                &card,
                &track,
                "thread",
                0,
                old,
            )
            .await
            .unwrap(),
            1
        );
        super::super::session_set_handle_state_tx(
            &mut tx,
            &"title-runtime".into(),
            Some(json!({"pending":"replacement"})),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    });
    let context = f
        .reader
        .conversation_title_context(std::slice::from_ref(&f.card))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), write)
        .await
        .expect("writer must complete after reader commit")
        .unwrap();
    assert!(schedule.fired.load(std::sync::atomic::Ordering::SeqCst));
    assert!(
        schedule
            .writer_started_before_return
            .load(std::sync::atomic::Ordering::SeqCst)
    );
    assert_eq!(
        context.sources[0]
            .input
            .as_ref()
            .unwrap()
            .title()
            .as_deref(),
        Some("Original first")
    );
    assert_eq!(
        context.runtimes[&f.card].handle_state_json,
        Some(json!({"pending":null}))
    );
    let after = f
        .writer
        .conversation_title_context(std::slice::from_ref(&f.card))
        .await
        .unwrap();
    assert!(after.sources[0].input.is_none());
    assert_eq!(
        after.runtimes[&f.card].handle_state_json,
        Some(json!({"pending":"replacement"}))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_title_snapshot_candidate_batches_do_not_drift() {
    let f = Fixture::new().await;
    for _ in 0..CANDIDATE_BATCH {
        f.insert(false, "context").await;
    }
    let visible = f.insert(true, "Beyond first batch").await;
    let (schedule, ready, committed) = f.schedule().await;
    let writer = f.writer.clone();
    let card = f.card.clone();
    let track = f.track.clone();
    let write = tokio::spawn(async move {
        tokio::task::spawn_blocking(move || ready.recv_timeout(Duration::from_secs(10)).unwrap())
            .await
            .unwrap();
        committed.send(()).unwrap();
        let mut tx = super::super::begin_immediate_tx(writer.db()).await.unwrap();
        assert_eq!(
            super::super::transcript_delete_thread_suffix_tx(
                &mut tx,
                "title-runtime",
                &card,
                &track,
                "thread",
                visible - 1,
                visible,
            )
            .await
            .unwrap(),
            1
        );
        tx.commit().await.unwrap();
    });
    let context = f
        .reader
        .conversation_title_context(std::slice::from_ref(&f.card))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), write)
        .await
        .expect("writer must complete after reader commit")
        .unwrap();
    assert!(schedule.fired.load(std::sync::atomic::Ordering::SeqCst));
    assert!(
        schedule
            .writer_started_before_return
            .load(std::sync::atomic::Ordering::SeqCst)
    );
    assert_eq!(
        context.sources[0]
            .input
            .as_ref()
            .and_then(|input| input.title())
            .as_deref(),
        Some("Beyond first batch")
    );
    assert!(
        f.writer
            .conversation_title_context(std::slice::from_ref(&f.card))
            .await
            .unwrap()
            .sources[0]
            .input
            .is_none()
    );
}

// Negative control: execute the same production candidate/runtime helpers with
// independent autocommit statements. A committed drain really loses both sources.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_title_autocommit_control_loses_drain_source() {
    let f = Fixture::new().await;
    let (schedule, ready, committed) = f.schedule().await;
    let writer = f.writer.clone();
    let card = f.card.clone();
    let track = f.track.clone();
    let write = tokio::spawn(async move {
        tokio::task::spawn_blocking(move || ready.recv_timeout(Duration::from_secs(10)).unwrap())
            .await
            .unwrap();
        super::tests::insert(&writer, &card, &track, None, "Migrating input").await;
        let mut tx = super::super::begin_immediate_tx(writer.db()).await.unwrap();
        super::super::session_set_handle_state_tx(
            &mut tx,
            &"title-runtime".into(),
            Some(json!({"pending":null})),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        committed.send(()).unwrap();
    });
    let mut conn = f.reader.db().writer_pool().acquire().await.unwrap();
    let ids = std::slice::from_ref(&f.card);
    let sources = sources_on_connection(&mut conn, ids).await.unwrap();
    let runtimes =
        super::super::session_projection::runtime_get_projectable_for_cards_on_connection(
            &mut conn, ids,
        )
        .await
        .unwrap();
    write.await.unwrap();
    assert!(schedule.writer_started_before_return.load(Ordering::SeqCst));
    assert!(sources[0].input.is_none());
    assert_eq!(
        runtimes[&f.card].handle_state_json,
        Some(json!({"pending":null}))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_title_shared_cache_reverse_writer_first() {
    let f = Fixture::memory().await;
    let old = f.insert(true, "Original first").await;
    let (schedule, ready, release) = f.schedule().await;
    let mut tx = super::super::begin_immediate_tx(f.writer.db())
        .await
        .unwrap();
    // Real reset/replace order: runtime first, transcript second, reverse of metadata.
    super::super::session_set_handle_state_tx(
        &mut tx,
        &"title-runtime".into(),
        Some(json!({"pending":"replacement"})),
    )
    .await
    .unwrap();
    schedule.begin_boundary.store(true, Ordering::SeqCst);
    let reader = f.reader.clone();
    let card = f.card.clone();
    let read = tokio::spawn(async move { reader.conversation_title_context(&[card]).await });
    tokio::task::spawn_blocking(move || ready.recv_timeout(Duration::from_secs(10)).unwrap())
        .await
        .unwrap();
    let began_before_read = schedule.begin_seen.load(Ordering::SeqCst);
    if began_before_read {
        tokio::time::timeout(
            Duration::from_secs(10),
            super::super::transcript_delete_thread_suffix_tx(
                &mut tx,
                "title-runtime",
                &f.card,
                &f.track,
                "thread",
                0,
                old,
            ),
        )
        .await
        .expect("metadata must hold no table locks while waiting at BEGIN")
        .unwrap();
        tx.commit().await.unwrap();
    } else {
        // Safely conclude a mutated deferred reader's cycle before reporting red.
        tx.rollback().await.unwrap();
    }
    release.send(()).unwrap();
    let context = tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .expect("metadata must finish after writer release")
        .unwrap()
        .unwrap();
    assert!(
        began_before_read,
        "metadata must claim writer eligibility before candidate SELECT"
    );
    assert!(context.sources[0].input.is_none());
    assert_eq!(
        context.runtimes[&f.card].handle_state_json,
        Some(json!({"pending":"replacement"}))
    );
}

async fn assert_pool_reusable(repo: &SqlxRepo) {
    let tx = tokio::time::timeout(
        Duration::from_secs(10),
        super::super::begin_immediate_tx(repo.db()),
    )
    .await
    .expect("released metadata transaction must not retain writer eligibility")
    .unwrap();
    tx.commit().await.unwrap();
    for pool in [repo.db().read_pool(), repo.db().writer_pool()] {
        let mut held = Vec::new();
        for _ in 0..pool.options().get_max_connections() {
            let conn = pool.acquire().await.unwrap();
            assert!(!sqlx::Connection::is_in_transaction(&*conn));
            held.push(conn);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_title_shared_cache_cancel_releases_transaction() {
    let f = Fixture::memory().await;
    let (_, ready, release) = f.schedule().await;
    let reader = f.reader.clone();
    let card = f.card.clone();
    let read = tokio::spawn(async move { reader.conversation_title_context(&[card]).await });
    tokio::task::spawn_blocking(move || ready.recv_timeout(Duration::from_secs(10)).unwrap())
        .await
        .unwrap();
    read.abort();
    release.send(()).unwrap();
    assert!(read.await.unwrap_err().is_cancelled());
    assert_pool_reusable(&f.reader).await;
    assert_eq!(
        f.reader
            .conversation_title_context(&[f.card])
            .await
            .unwrap()
            .sources
            .len(),
        1
    );
}

#[tokio::test]
async fn conversation_title_shared_cache_decode_error_releases_transaction() {
    let f = Fixture::memory().await;
    // SQL-valid JSON with missing typed fields exercises the production decoder.
    super::tests::insert(
        &f.writer,
        &f.card,
        &f.track,
        Some(json!([{}])),
        "invalid typed input",
    )
    .await;
    let error = f
        .reader
        .conversation_title_context(std::slice::from_ref(&f.card))
        .await
        .unwrap_err();
    assert!(
        matches!(error, crate::error::CalmError::Serde(_)),
        "{error:?}"
    );
    assert_pool_reusable(&f.reader).await;
}

#[tokio::test]
async fn conversation_title_query_and_lock_cost() {
    for invisible in [0, 1024] {
        let f = Fixture::new().await;
        for _ in 0..invisible {
            f.insert(false, "context").await;
        }
        f.insert(true, "Beyond prefix").await;
        let (schedule, _ready, release) = f.schedule().await;
        release.send(()).unwrap();
        f.reader
            .conversation_title_context(&[f.card])
            .await
            .unwrap();
        let queries = schedule.queries.load(Ordering::SeqCst);
        assert_eq!(queries, invisible / CANDIDATE_BATCH as usize + 2);
        eprintln!(
            "title-cost invisible={invisible} selects={queries} sqlite_query_ns={} writer_held={:?}",
            schedule.query_ns.load(Ordering::SeqCst),
            schedule.held.lock().unwrap().expect(
                "metadata must have held writer eligibility from BEGIN IMMEDIATE through COMMIT"
            )
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_title_shared_cache_cancel_begin_releases_connection() {
    let f = Fixture::memory().await;
    let (schedule, ready, release) = f.schedule().await;
    let tx = super::super::begin_immediate_tx(f.writer.db())
        .await
        .unwrap();
    schedule.begin_boundary.store(true, Ordering::SeqCst);
    let reader = f.reader.clone();
    let card = f.card.clone();
    let read = tokio::spawn(async move { reader.conversation_title_context(&[card]).await });
    tokio::task::spawn_blocking(move || ready.recv_timeout(Duration::from_secs(10)).unwrap())
        .await
        .unwrap();
    read.abort();
    release.send(()).unwrap();
    assert!(read.await.unwrap_err().is_cancelled());
    tx.commit().await.unwrap();
    assert_pool_reusable(&f.reader).await;
}

#[tokio::test]
async fn conversation_title_empty_ids_do_not_acquire_connection() {
    let f = Fixture::memory().await;
    f.reader.db().close().await;
    let context = f.reader.conversation_title_context(&[]).await.unwrap();
    assert!(context.sources.is_empty());
    assert!(context.runtimes.is_empty());
}
