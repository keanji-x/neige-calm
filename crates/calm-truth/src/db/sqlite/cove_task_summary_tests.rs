//! #1050 — acceptance for the one-statement cove task summary (B1–B9).

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use super::read::{COVE_TASK_SUMMARY_SQL, cove_task_summary_on};
use super::wave_tree::{MAX_WAVE_TREE_DEPTH, WAVE_TREE_MEMBERS_WITH_FIXED_SPEC_SQL};
use super::{SqlxRepo, cove_create_tx, wave_create_tx};
use crate::db::{COVE_TASK_SUMMARY_MAX_WAVES, RepoRead};
use crate::model::{CoveTaskSummary, NewCove, NewWave, RequestTheme, TaskSummaryCounts};

fn assert_summary_identities(counts: &TaskSummaryCounts) {
    assert_eq!(
        counts.spec_live,
        counts.block_live + counts.legacy_live,
        "real SQL result must satisfy specLive = blockLive + legacyLive"
    );
    assert_eq!(
        counts.pending + counts.in_flight,
        counts.spec_live + counts.user_live,
        "real SQL result must satisfy pending + inFlight = specLive + userLive"
    );
}

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
    seed_task(
        &repo,
        &wave,
        "spec-legacy-verifying",
        "verifying",
        "spec",
        "legacy",
    )
    .await;
    seed_task(
        &repo,
        &wave,
        "spec-legacy-running",
        "running",
        "spec",
        "legacy",
    )
    .await;
    seed_task(
        &repo,
        &wave,
        "spec-block-dispatched",
        "dispatched",
        "spec",
        "block",
    )
    .await;
    seed_task(&repo, &wave, "user-legacy", "pending", "user", "legacy").await;
    seed_task(&repo, &wave, "user-legacy-2", "running", "user", "legacy").await;
    seed_task(&repo, &wave, "spec-done", "done", "spec", "legacy").await;
    seed_task(&repo, &wave, "user-failed", "failed", "user", "legacy").await;
    seed_task(&repo, &wave, "user-canceled", "canceled", "user", "legacy").await;

    let got = summary(&repo, &cove).await;
    let expected = TaskSummaryCounts {
        pending: 2,
        in_flight: 4,
        done: 1,
        failed: 1,
        canceled: 1,
        legacy_live: 3,
        block_live: 1,
        spec_live: 4,
        user_live: 2,
    };
    assert_eq!(
        got.waves[0].counts, expected,
        "all nine real SQL wave buckets"
    );
    assert_eq!(got.totals, expected, "all nine real SQL total buckets");
    assert_summary_identities(&got.waves[0].counts);
    assert_summary_identities(&got.totals);

    let fixed: Vec<(String, i64, i64)> = sqlx::query_as(WAVE_TREE_MEMBERS_WITH_FIXED_SPEC_SQL)
        .bind(&wave)
        .bind(MAX_WAVE_TREE_DEPTH + 1)
        .fetch_all(repo.pool())
        .await
        .unwrap();
    assert_eq!(fixed[0].2, got.waves[0].counts.spec_live);
    // A user-declared row may have legacy origin, but intentionally belongs
    // to userLive rather than the spec-only legacyLive/fixed-live leg.
    assert_eq!(fixed[0].2, 4);
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
    let read_source = include_str!("read.rs");
    let public_wrapper = read_source
        .split_once("async fn cove_task_summary")
        .expect("public cove_task_summary wrapper exists")
        .1
        .split_once("async fn cove_get_system")
        .expect("cove_get_system follows the summary wrapper")
        .0;
    assert!(
        !public_wrapper.contains("connect_options"),
        "public cove_task_summary must not bypass its pool with connect_options().connect()"
    );
    assert!(
        !public_wrapper.contains(".connect(") && !public_wrapper.contains("connect_with("),
        "public cove_task_summary must not open any connection outside its pool acquire"
    );
    assert!(
        !public_wrapper.contains("SqliteConnection::connect"),
        "public cove_task_summary must not open a direct SQLite connection"
    );
    assert_eq!(
        public_wrapper.matches("self.pool.acquire()").count(),
        1,
        "public cove_task_summary must acquire exactly one pool connection"
    );

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
    // Occupy every pool slot, then release exactly one traced connection.
    // The public RepoRead entrypoint must acquire that sole connection, so
    // the trace covers both its wrapper and the summary helper it calls.
    let max_connections = repo.pool().options().get_max_connections();
    let mut held_connections = Vec::with_capacity(max_connections as usize);
    for _ in 0..max_connections {
        held_connections.push(repo.pool().acquire().await.unwrap());
    }
    let mut traced_conn = held_connections.pop().unwrap();
    let counter = Box::new(AtomicUsize::new(0));
    let counter_ptr = Box::into_raw(counter);
    {
        let mut handle = traced_conn.lock_handle().await.unwrap();
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
    drop(traced_conn);

    let got = repo.cove_task_summary(&cove).await.unwrap();
    assert!(got.is_some());

    // The other slots are still occupied, so this reacquires the same
    // traced connection and lets us unregister before reclaiming context.
    let mut traced_conn = repo.pool().acquire().await.unwrap();
    {
        let mut handle = traced_conn.lock_handle().await.unwrap();
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
    assert_eq!(
        traced, 1,
        "pool-bound public cove_task_summary must execute exactly one sqlite statement; traced {traced}"
    );
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
    for index in 0..COVE_TASK_SUMMARY_MAX_WAVES {
        let wave = format!("a-zero-{index:03}");
        seed_wave_with_id(&repo, &cove, &wave, index).await;
    }
    let high_legacy_waves = ["z-legacy-a", "z-legacy-b", "z-legacy-c"];
    for wave in high_legacy_waves {
        seed_wave_with_id(&repo, &cove, wave, 999).await;
        for task in 0..5 {
            seed_task(
                &repo,
                wave,
                &format!("legacy-{task}"),
                "pending",
                "spec",
                "legacy",
            )
            .await;
        }
    }
    let got = summary(&repo, &cove).await;
    assert_eq!(got.waves.len(), COVE_TASK_SUMMARY_MAX_WAVES as usize);
    assert!(got.truncated);
    assert_eq!(got.totals.pending, 15);
    assert_eq!(got.totals.legacy_live, 15);
    assert_eq!(
        got.waves
            .iter()
            .take(high_legacy_waves.len())
            .map(|wave| wave.wave_id.as_str())
            .collect::<Vec<_>>(),
        high_legacy_waves,
        "legacy-heavy waves must sort first even when their ids sort last"
    );
    assert!(high_legacy_waves.into_iter().all(|wave_id| {
        got.waves
            .iter()
            .any(|wave| wave.wave_id == wave_id && wave.counts.legacy_live == 5)
    }));
    assert_eq!(
        got.waves
            .iter()
            .filter(|wave| wave.counts.legacy_live == 0)
            .count(),
        COVE_TASK_SUMMARY_MAX_WAVES as usize - high_legacy_waves.len(),
        "truncation must drop legacyLive=0 waves before legacy-heavy waves"
    );
    assert!(
        (0..3).all(|index| !got
            .waves
            .iter()
            .any(|wave| wave.wave_id == format!("a-zero-{index:03}"))),
        "the three truncated rows must all have legacyLive=0"
    );
}

#[tokio::test]
async fn b10_truncation_boundary_is_strictly_above_limit() {
    assert_eq!(
        COVE_TASK_SUMMARY_MAX_WAVES, 200,
        "the API/UI cove summary limit is fixed at 200 waves"
    );
    let repo = fresh_repo().await;
    for wave_count in [199, 200, 201] {
        let cove = seed_cove(&repo, &format!("boundary-{wave_count}")).await;
        for index in 0..wave_count {
            let wave = format!("boundary-{wave_count}-{index:03}");
            seed_wave_with_id(&repo, &cove, &wave, index).await;
        }

        let got = summary(&repo, &cove).await;
        assert!(!got.waves.is_empty(), "boundary fixture must be non-empty");
        assert_eq!(got.waves.len(), wave_count.min(200) as usize);
        assert_eq!(
            got.truncated,
            wave_count > 200,
            "{wave_count} waves: truncated must be true only strictly above the 200-wave limit"
        );
    }
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
        seed_task(
            &repo,
            &wave,
            &format!("user-{index}"),
            status,
            "user",
            "legacy",
        )
        .await;
    }
    let got = summary(&repo, &cove).await;
    assert_eq!(
        (got.totals.done, got.totals.failed, got.totals.canceled),
        (3, 3, 3)
    );
    assert_eq!((got.totals.legacy_live, got.totals.block_live), (0, 0));
    assert_eq!(got.totals.spec_live, 0);
    assert_eq!(got.totals.user_live, 0);
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
