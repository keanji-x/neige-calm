//! #1050 — acceptance for the one-statement cove task summary (B1–B9).

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use super::read::{COVE_TASK_SUMMARY_SQL, cove_task_summary_on};
use super::wave_tree::{MAX_WAVE_TREE_DEPTH, WAVE_TREE_MEMBERS_WITH_FIXED_SPEC_SQL};
use super::{SqlxRepo, cove_create_tx, wave_create_tx};
use crate::db::{COVE_TASK_SUMMARY_MAX_WAVES, RepoRead};
use crate::model::{CoveTaskSummary, NewCove, NewWave, RequestTheme};

async fn fresh_repo() -> SqlxRepo {
    SqlxRepo::open("sqlite::memory:").await.expect("open repo")
}

async fn seed_cove(repo: &SqlxRepo, name: &str) -> String {
    let mut tx = repo.pool().begin().await.unwrap();
    let cove = cove_create_tx(
        &mut tx,
        NewCove {
            name: name.into(),
            color: "#000".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    cove.id.to_string()
}

async fn seed_wave(repo: &SqlxRepo, cove_id: &str, title: &str) -> String {
    let mut tx = repo.pool().begin().await.unwrap();
    let wave = wave_create_tx(
        &mut tx,
        NewWave {
            cove_id: cove_id.into(),
            title: title.into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            workflow_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        repo.wave_cove_cache(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    wave.id.to_string()
}

async fn seed_wave_with_id(repo: &SqlxRepo, cove_id: &str, wave_id: &str, updated_at: i64) {
    sqlx::query(
        "INSERT INTO waves(id,cove_id,title,sort,created_at,updated_at) \
         VALUES(?1,?2,?1,0,?3,?3)",
    )
    .bind(wave_id)
    .bind(cove_id)
    .bind(updated_at)
    .execute(repo.pool())
    .await
    .unwrap();
}

async fn seed_task(
    repo: &SqlxRepo,
    wave_id: &str,
    key: &str,
    status: &str,
    declared_by: &str,
    origin: &str,
) {
    sqlx::query(
        "INSERT INTO tasks( \
           id,wave_id,key,kind,goal,context_json,depends_on_json,status, \
           declared_by,origin,created_at_ms,updated_at_ms \
         ) VALUES(?1,?2,?3,'codex',?3,'{}','[]',?4,?5,?6,1,1)",
    )
    .bind(format!("{wave_id}:{key}"))
    .bind(wave_id)
    .bind(key)
    .bind(status)
    .bind(declared_by)
    .bind(origin)
    .execute(repo.pool())
    .await
    .unwrap();
}

async fn summary(repo: &SqlxRepo, cove_id: &str) -> CoveTaskSummary {
    repo.cove_task_summary(cove_id)
        .await
        .unwrap()
        .expect("cove exists")
}

#[tokio::test]
async fn b1_shared_legacy_predicate_has_positive_and_user_legacy_negative_fixture() {
    let repo = fresh_repo().await;
    let cove = seed_cove(&repo, "B1").await;
    let wave = seed_wave(&repo, &cove, "predicate").await;
    seed_task(&repo, &wave, "spec-legacy", "pending", "spec", "legacy").await;
    seed_task(&repo, &wave, "user-legacy", "pending", "user", "legacy").await;
    seed_task(&repo, &wave, "user-legacy-2", "running", "user", "legacy").await;

    let got = summary(&repo, &cove).await;
    assert_eq!(got.waves[0].counts.legacy_live, 1);
    assert_eq!(got.waves[0].counts.user_live, 2);
    assert_eq!(got.waves[0].counts.spec_live, 1);

    let fixed: Vec<(String, i64, i64)> = sqlx::query_as(WAVE_TREE_MEMBERS_WITH_FIXED_SPEC_SQL)
        .bind(&wave)
        .bind(MAX_WAVE_TREE_DEPTH + 1)
        .fetch_all(repo.pool())
        .await
        .unwrap();
    assert_eq!(fixed[0].2, got.waves[0].counts.legacy_live);
    // A user-declared row may have legacy origin, but intentionally belongs
    // to userLive rather than the spec-only legacyLive/fixed-live leg.
    assert_ne!(
        got.waves[0].counts.user_live,
        got.waves[0].counts.legacy_live
    );
}

#[tokio::test]
async fn b2_materializing_k_rows_preserves_status_and_live_totals_and_moves_origin_buckets() {
    let repo = fresh_repo().await;
    let cove = seed_cove(&repo, "B2").await;
    let wave = seed_wave(&repo, &cove, "materialize").await;
    for (key, status) in [("a", "pending"), ("b", "running"), ("c", "pending")] {
        seed_task(&repo, &wave, key, status, "spec", "legacy").await;
    }
    seed_task(&repo, &wave, "u", "pending", "user", "legacy").await;
    let before = summary(&repo, &cove).await;

    sqlx::query("UPDATE tasks SET origin='block' WHERE wave_id=?1 AND key IN ('a','b')")
        .bind(&wave)
        .execute(repo.pool())
        .await
        .unwrap();
    let after = summary(&repo, &cove).await;

    assert_eq!(after.totals.pending, before.totals.pending);
    assert_eq!(after.totals.in_flight, before.totals.in_flight);
    assert_eq!(after.totals.spec_live, before.totals.spec_live);
    assert_eq!(after.totals.user_live, before.totals.user_live);
    assert_eq!(before.totals.legacy_live + before.totals.block_live, 3);
    assert_eq!(after.totals.legacy_live + after.totals.block_live, 3);
    assert_eq!(before.totals.legacy_live - after.totals.legacy_live, 2);
    assert_eq!(after.totals.block_live - before.totals.block_live, 2);
}

unsafe extern "C" fn trace_statement(
    kind: u32,
    context: *mut c_void,
    _statement: *mut c_void,
    _sql: *mut c_void,
) -> i32 {
    if kind & libsqlite3_sys::SQLITE_TRACE_STMT as u32 != 0 {
        // SAFETY: `context` points to the live AtomicUsize boxed by the test;
        // the callback is unregistered before that allocation is reclaimed.
        unsafe { &*(context.cast::<AtomicUsize>()) }.fetch_add(1, Ordering::SeqCst);
    }
    0
}

#[tokio::test]
async fn b3_static_shape_and_real_sqlite_trace_prove_one_statement() {
    let total_pos = COVE_TASK_SUMMARY_SQL.find("SUM(pending) OVER ()").unwrap();
    let limit_pos = COVE_TASK_SUMMARY_SQL
        .find("LEFT JOIN ranked r ON r.ordinal <= ?2")
        .unwrap();
    assert!(
        COVE_TASK_SUMMARY_SQL
            .trim_start()
            .starts_with("WITH wave_counts AS")
    );
    assert!(!COVE_TASK_SUMMARY_SQL.contains(';'));
    assert!(
        total_pos < limit_pos,
        "window totals must precede truncation"
    );
    assert!(
        COVE_TASK_SUMMARY_SQL.contains("ORDER BY legacy_live DESC, updated_at DESC, wave_id ASC")
    );
    assert!(!COVE_TASK_SUMMARY_SQL.contains("task_diagnostics"));
    assert!(!COVE_TASK_SUMMARY_SQL.contains("evaluate_schedulability"));

    let repo = fresh_repo().await;
    let cove = seed_cove(&repo, "B3").await;
    let mut conn = repo.pool().acquire().await.unwrap();
    let counter = Box::new(AtomicUsize::new(0));
    let counter_ptr = Box::into_raw(counter);
    {
        let mut handle = conn.lock_handle().await.unwrap();
        let rc = unsafe {
            libsqlite3_sys::sqlite3_trace_v2(
                handle.as_raw_handle().as_ptr(),
                libsqlite3_sys::SQLITE_TRACE_STMT as u32,
                Some(trace_statement),
                counter_ptr.cast(),
            )
        };
        assert_eq!(rc, libsqlite3_sys::SQLITE_OK);
    }
    let got = cove_task_summary_on(&mut conn, &cove).await.unwrap();
    assert!(got.is_some());
    {
        let mut handle = conn.lock_handle().await.unwrap();
        unsafe {
            libsqlite3_sys::sqlite3_trace_v2(
                handle.as_raw_handle().as_ptr(),
                0,
                None,
                std::ptr::null_mut(),
            );
        }
    }
    let traced = unsafe { Box::from_raw(counter_ptr) }.load(Ordering::SeqCst);
    assert_eq!(traced, 1, "real sqlite statement trace: {traced}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn b4_barrier_result_is_one_complete_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("summary.sqlite");
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let repo = Arc::new(SqlxRepo::open(&url).await.unwrap());
    let cove = seed_cove(&repo, "B4").await;
    for index in 0..40 {
        let wave = format!("wave-{index:03}");
        seed_wave_with_id(&repo, &cove, &wave, index).await;
        seed_task(&repo, &wave, "p", "pending", "spec", "legacy").await;
    }
    let before = summary(&repo, &cove).await;

    let mut reader = repo.pool().acquire().await.unwrap();
    let (reached_tx, reached_rx) = mpsc::sync_channel(0);
    let (release_tx, release_rx) = mpsc::sync_channel(0);
    let calls = Arc::new(AtomicUsize::new(0));
    {
        let calls = calls.clone();
        let mut handle = reader.lock_handle().await.unwrap();
        handle.set_progress_handler(10, move || {
            if calls.fetch_add(1, Ordering::SeqCst) == 20 {
                reached_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            true
        });
    }

    let writer_repo = repo.clone();
    let cove_for_writer = cove.clone();
    let writer = tokio::task::spawn_blocking(move || {
        reached_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let runtime = tokio::runtime::Handle::current();
        runtime.block_on(async move {
            sqlx::query("DELETE FROM waves WHERE id='wave-000'")
                .execute(writer_repo.pool())
                .await
                .unwrap();
            seed_wave_with_id(&writer_repo, &cove_for_writer, "wave-new", 999).await;
            seed_task(&writer_repo, "wave-new", "d", "done", "spec", "legacy").await;
        });
        release_tx.send(()).unwrap();
    });

    let during = cove_task_summary_on(&mut reader, &cove)
        .await
        .unwrap()
        .unwrap();
    writer.await.unwrap();
    {
        let mut handle = reader.lock_handle().await.unwrap();
        handle.remove_progress_handler();
    }
    drop(reader);
    let after = summary(&repo, &cove).await;

    assert!(
        during == before || during == after,
        "torn snapshot: {during:?}"
    );
    if !during.truncated {
        let pending: i64 = during.waves.iter().map(|wave| wave.counts.pending).sum();
        let done: i64 = during.waves.iter().map(|wave| wave.counts.done).sum();
        assert_eq!((during.totals.pending, during.totals.done), (pending, done));
    }
}

#[tokio::test]
async fn b5_truncates_rows_but_keeps_full_totals() {
    let repo = fresh_repo().await;
    let cove = seed_cove(&repo, "B5").await;
    for index in 0..COVE_TASK_SUMMARY_MAX_WAVES + 3 {
        let wave = format!("wave-{index:03}");
        seed_wave_with_id(&repo, &cove, &wave, index).await;
        seed_task(&repo, &wave, "p", "pending", "spec", "legacy").await;
    }
    let got = summary(&repo, &cove).await;
    assert_eq!(got.waves.len(), COVE_TASK_SUMMARY_MAX_WAVES as usize);
    assert!(got.truncated);
    assert_eq!(got.totals.pending, COVE_TASK_SUMMARY_MAX_WAVES + 3);
    assert_eq!(got.totals.legacy_live, COVE_TASK_SUMMARY_MAX_WAVES + 3);
    assert_eq!(got.waves.iter().map(|w| w.counts.pending).sum::<i64>(), 200);
}

#[tokio::test]
async fn b6_missing_and_existing_empty_coves_are_distinct() {
    let repo = fresh_repo().await;
    assert!(repo.cove_task_summary("missing").await.unwrap().is_none());
    let cove = seed_cove(&repo, "empty").await;
    let got = summary(&repo, &cove).await;
    assert_eq!(got, CoveTaskSummary::default());
}

#[tokio::test]
async fn b7_never_leaks_tasks_or_waves_across_coves() {
    let repo = fresh_repo().await;
    let home = seed_cove(&repo, "home").await;
    let other = seed_cove(&repo, "other").await;
    let home_wave = seed_wave(&repo, &home, "home wave").await;
    let other_wave = seed_wave(&repo, &other, "other wave").await;
    seed_task(&repo, &home_wave, "home", "pending", "spec", "legacy").await;
    seed_task(&repo, &other_wave, "other", "failed", "spec", "block").await;

    let got = summary(&repo, &home).await;
    assert_eq!(got.waves.len(), 1);
    assert_eq!(got.waves[0].wave_id, home_wave);
    assert_eq!(got.totals.pending, 1);
    assert_eq!(got.totals.failed, 0);
}

#[tokio::test]
async fn b8_terminal_rows_never_enter_live_origin_buckets() {
    let repo = fresh_repo().await;
    let cove = seed_cove(&repo, "B8").await;
    let wave = seed_wave(&repo, &cove, "terminal mix").await;
    for (index, status) in ["done", "failed", "canceled"].into_iter().enumerate() {
        seed_task(
            &repo,
            &wave,
            &format!("legacy-{index}"),
            status,
            "spec",
            "legacy",
        )
        .await;
        seed_task(
            &repo,
            &wave,
            &format!("block-{index}"),
            status,
            "spec",
            "block",
        )
        .await;
    }
    let got = summary(&repo, &cove).await;
    assert_eq!(
        (got.totals.done, got.totals.failed, got.totals.canceled),
        (2, 2, 2)
    );
    assert_eq!((got.totals.legacy_live, got.totals.block_live), (0, 0));
    assert_eq!(got.totals.spec_live, 0);
}

#[tokio::test]
async fn b9_sort_has_unique_wave_id_tie_break() {
    let repo = fresh_repo().await;
    let cove = seed_cove(&repo, "B9").await;
    for wave in ["wave-z", "wave-a", "wave-m"] {
        seed_wave_with_id(&repo, &cove, wave, 7).await;
        seed_task(&repo, wave, "p", "pending", "spec", "legacy").await;
    }
    let got = summary(&repo, &cove).await;
    assert_eq!(
        got.waves
            .iter()
            .map(|row| row.wave_id.as_str())
            .collect::<Vec<_>>(),
        ["wave-a", "wave-m", "wave-z"]
    );
    assert!(
        COVE_TASK_SUMMARY_SQL.contains("ORDER BY legacy_live DESC, updated_at DESC, wave_id ASC")
    );
}
