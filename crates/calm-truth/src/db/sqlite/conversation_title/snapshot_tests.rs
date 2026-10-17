//! SQLite PROFILE fires after a SELECT finishes and before sqlx returns its result.
//! Block only the test reader's SQLite worker while a separate WAL writer commits.
use super::*;
use crate::db::RepoRead;
use crate::session_projection_repo::{AgentProvider, WorkerSessionInit, WorkerSessionState};
use libsqlite3_sys::{SQLITE_TRACE_CLOSE, SQLITE_TRACE_PROFILE, sqlite3_sql, sqlite3_trace_v2};
use serde_json::json;
use std::ffi::{CStr, c_int, c_uint, c_void};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

struct Schedule {
    ready: mpsc::Sender<()>,
    committed: Mutex<mpsc::Receiver<()>>,
    fired: std::sync::atomic::AtomicBool,
    committed_before_return: std::sync::atomic::AtomicBool,
}

// SQLite owns this Box until CLOSE. PROFILE's statement pointer is valid for the callback.
unsafe extern "C" fn trace(
    mask: c_uint,
    context: *mut c_void,
    stmt: *mut c_void,
    _: *mut c_void,
) -> c_int {
    if mask == SQLITE_TRACE_CLOSE as c_uint {
        drop(unsafe { Box::from_raw(context.cast::<Arc<Schedule>>()) });
    } else if mask == SQLITE_TRACE_PROFILE as c_uint {
        let schedule = unsafe { &*context.cast::<Arc<Schedule>>() };
        let sql = unsafe { CStr::from_ptr(sqlite3_sql(stmt.cast())) }.to_string_lossy();
        if sql
            .trim_start()
            .starts_with("SELECT c.id AS card_id, h.id AS item_id")
            && !schedule
                .fired
                .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            // Timeouts only diagnose a broken test handshake; never retry or select an outcome.
            let _ = schedule.ready.send(());
            if schedule
                .committed
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .is_ok()
            {
                schedule
                    .committed_before_return
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }
    0
}

struct Fixture {
    reader: SqlxRepo,
    writer: Arc<SqlxRepo>,
    card: String,
    track: String,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("titles.db").display());
        let writer = Arc::new(SqlxRepo::open(&url).await.unwrap());
        let (card, track) = super::super::session_conversation_tests::card(&writer).await;
        sqlx::query("UPDATE cards SET role='assistant' WHERE id=?1")
            .bind(&card)
            .execute(writer.pool())
            .await
            .unwrap();
        let mut tx = writer.pool().begin().await.unwrap();
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
        let mut reader = SqlxRepo::open(&url).await.unwrap();
        // One reader connection: every production query, including later candidate batches,
        // runs under the same SQLite observer. The writer has its own pool.
        reader.pool.close().await;
        reader.pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
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
            committed: Mutex::new(commit_rx),
            fired: false.into(),
            committed_before_return: false.into(),
        });
        let mut conn = self.reader.pool().acquire().await.unwrap();
        let mut handle = conn.lock_handle().await.unwrap();
        let context = Box::into_raw(Box::new(schedule.clone())).cast();
        // The callback is connection-local, and SQLite CLOSE releases its context.
        assert_eq!(
            unsafe {
                sqlite3_trace_v2(
                    handle.as_raw_handle().as_ptr(),
                    (SQLITE_TRACE_PROFILE | SQLITE_TRACE_CLOSE) as c_uint,
                    Some(trace),
                    context,
                )
            },
            0
        );
        drop(handle);
        drop(conn);
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
        super::tests::insert(&writer, &card, &track, None, "Migrating input").await;
        let mut tx = writer.pool().begin().await.unwrap();
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
    let context = f
        .reader
        .conversation_title_context(std::slice::from_ref(&f.card))
        .await
        .unwrap();
    write.await.unwrap();
    assert!(schedule.fired.load(std::sync::atomic::Ordering::SeqCst));
    assert!(
        schedule
            .committed_before_return
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
    let f = Fixture::new().await;
    let old = f.insert(true, "Original first").await;
    let mut tx = f.writer.pool().begin().await.unwrap();
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
    let write = tokio::spawn(async move {
        tokio::task::spawn_blocking(move || ready.recv_timeout(Duration::from_secs(10)).unwrap())
            .await
            .unwrap();
        let mut tx = writer.pool().begin().await.unwrap();
        // Same transaction boundary as replace_command: cut transcript and persist pending.
        assert_eq!(
            super::super::transcript_delete_thread_suffix_tx(&mut tx, &card, "thread", 0, old,)
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
        committed.send(()).unwrap();
    });
    let context = f
        .reader
        .conversation_title_context(std::slice::from_ref(&f.card))
        .await
        .unwrap();
    write.await.unwrap();
    assert!(schedule.fired.load(std::sync::atomic::Ordering::SeqCst));
    assert!(
        schedule
            .committed_before_return
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
    let write = tokio::spawn(async move {
        tokio::task::spawn_blocking(move || ready.recv_timeout(Duration::from_secs(10)).unwrap())
            .await
            .unwrap();
        let mut tx = writer.pool().begin().await.unwrap();
        assert_eq!(
            super::super::transcript_delete_thread_suffix_tx(
                &mut tx,
                &card,
                "thread",
                visible - 1,
                visible,
            )
            .await
            .unwrap(),
            1
        );
        tx.commit().await.unwrap();
        committed.send(()).unwrap();
    });
    let context = f
        .reader
        .conversation_title_context(std::slice::from_ref(&f.card))
        .await
        .unwrap();
    write.await.unwrap();
    assert!(schedule.fired.load(std::sync::atomic::Ordering::SeqCst));
    assert!(
        schedule
            .committed_before_return
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
