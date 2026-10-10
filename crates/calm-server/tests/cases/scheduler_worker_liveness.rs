//! #2406: the sweep judges a running Codex or Claude worker dead by transcript progress (its
//! card's `worker_flow_cursors.updated_at_ms`), not by a fixed window from the running stamp.

use super::*;

use calm_exec::flow::CapturePosition;
use calm_server::scheduler::LivenessFailTestHook;
use calm_server::worker_flow::cursor::CODEX_ROLLOUT_SOURCE_KIND;

const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;

struct LivenessWorker {
    key: String,
    task_id: String,
    card_id: String,
    session_row_id: String,
    lease_id: String,
    _lease_dir: tempfile::TempDir,
}

/// A codex worker task taken to `running` through the production stamp at `started_ms`, with
/// the deadline the cap gives it then.
async fn seed_worker_started_at(boot: &Boot, label: &str, started_ms: i64) -> LivenessWorker {
    let (card_id, session_row_id, _terminal_id) =
        seed_codex_worker_card_with_terminal(boot, label).await;
    let (lease_id, lease_dir) = seed_held_workspace_lease(boot, &card_id, label).await;
    let mut task = plan_task(&boot.track_id, label, TaskKind::Codex, &[]);
    task.status = TaskStatus::Dispatched;
    task.worker_card_id = Some(card_id.clone());
    let task_id = task.id.clone();
    seed_task(boot, task).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    let deadline = started_ms + WorkerLiveness::DEFAULT.cap.as_millis() as i64;
    let rows =
        calm_server::db::sqlite::task_mark_running_tx(&mut tx, &task_id, started_ms, deadline)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(rows, 1, "the fixture row enters running");
    LivenessWorker {
        key: label.to_string(),
        task_id,
        card_id,
        session_row_id,
        lease_id,
        _lease_dir: lease_dir,
    }
}

/// The transcript capture's checkpoint standing at `at_ms`, a time the wall-clock capture commit
/// cannot choose, so through the fixture-only cursor writer.
async fn record_progress(boot: &Boot, card_id: &str, at_ms: i64) {
    let position = CapturePosition {
        source_path: "/tmp/rollout.jsonl".into(),
        record_index: at_ms,
        byte_offset: at_ms,
        last_source_uuid: None,
        last_line_hash: None,
    };
    calm_server::db::sqlite::worker_flow_cursor_set_for_test(
        &boot.repo.sqlite_pool().unwrap(),
        card_id,
        CODEX_ROLLOUT_SOURCE_KIND,
        &position,
        at_ms,
    )
    .await
    .expect("record transcript progress");
}

/// A booted scheduler with the default windows whose boot lies far in the past, so a stale
/// cursor counts as evidence at once. The caller keeps the runtime alive for the reap.
fn scheduler_booted_long_ago(boot: &Boot) -> (Arc<OperationRuntime>, Arc<Scheduler>) {
    let (runtime, scheduler) = build_scheduler(boot, vec![]);
    scheduler.set_liveness_floor_for_test(0);
    (runtime, scheduler)
}

async fn running_started_at(boot: &Boot, task_id: &str) -> Option<i64> {
    sqlx::query_scalar("SELECT running_started_at_ms FROM tasks WHERE id = ?1")
        .bind(task_id)
        .fetch_one(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap()
}

async fn session_status(boot: &Boot, session_row_id: &str) -> WorkerSessionState {
    boot.repo
        .session_projection_by_id(session_row_id)
        .await
        .expect("runtime lookup")
        .expect("runtime row")
        .status
}

async fn assert_still_running(boot: &Boot, worker: &LivenessWorker) {
    let row = task_row(boot, &worker.key).await;
    assert_eq!(row.status, TaskStatus::Running);
    assert_eq!(row.status_detail, None);
    assert!(event_rows(boot, "task.failed").await.is_empty());
    assert_eq!(workspace_lease_state(boot, &worker.lease_id).await, "held");
    assert_eq!(
        session_status(boot, &worker.session_row_id).await,
        WorkerSessionState::Running
    );
}

async fn assert_timed_out_and_reaped(boot: &Boot, worker: &LivenessWorker, reason: &str) {
    let row = task_row(boot, &worker.key).await;
    assert_eq!(row.status, TaskStatus::Failed);
    assert_eq!(row.status_detail.as_deref(), Some("worker-timeout"));
    let failed = event_rows(boot, "task.failed").await;
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].1["reason"], json!(reason));
    assert_eq!(
        workspace_lease_state(boot, &worker.lease_id).await,
        "released"
    );
    assert_eq!(
        session_status(boot, &worker.session_row_id).await,
        WorkerSessionState::Failed
    );
    assert!(!timeout_cleanup_marker_exists(boot, &worker.card_id).await);
}

#[tokio::test]
async fn worker_making_progress_runs_past_the_idle_window_from_its_start() {
    let boot = boot().await;
    let now = now_ms();
    let worker = seed_worker_started_at(&boot, "progressing", now - 3 * HOUR).await;
    record_progress(&boot, &worker.card_id, now - 10 * MIN).await;

    let (_runtime, scheduler) = scheduler_booted_long_ago(&boot);
    scheduler.sweep_all().await;

    assert_still_running(&boot, &worker).await;
}

#[tokio::test]
async fn worker_without_progress_for_the_idle_window_fails_and_is_reaped() {
    let boot = boot().await;
    let now = now_ms();
    let worker = seed_worker_started_at(&boot, "silent", now - 3 * HOUR).await;
    record_progress(&boot, &worker.card_id, now - 2 * HOUR).await;

    let (_runtime, scheduler) = scheduler_booted_long_ago(&boot);
    scheduler.sweep_all().await;

    assert_timed_out_and_reaped(&boot, &worker, "worker made no transcript progress for 1 h").await;
}

#[tokio::test]
async fn worker_past_its_cap_fails_despite_fresh_progress() {
    let boot = boot().await;
    let now = now_ms();
    let worker = seed_worker_started_at(&boot, "capped", now - 9 * HOUR).await;
    record_progress(&boot, &worker.card_id, now - MIN).await;

    let (_runtime, scheduler) = scheduler_booted_long_ago(&boot);
    scheduler.sweep_all().await;

    assert_timed_out_and_reaped(&boot, &worker, "worker ran past its running cap").await;
}

/// After a kernel restart the capture reattaches and catches up asynchronously: a cursor older
/// than the idle window is no evidence until a full window past boot.
#[tokio::test]
async fn stale_cursor_is_no_evidence_within_a_window_of_boot() {
    let boot = boot().await;
    let now = now_ms();
    let worker = seed_worker_started_at(&boot, "rebooted", now - 3 * HOUR).await;
    record_progress(&boot, &worker.card_id, now - 2 * HOUR).await;

    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    scheduler.sweep_all().await;

    assert_still_running(&boot, &worker).await;
}

/// The sweep judged the worker idle, then its transcript advanced before the fail tx: the tx
/// re-reads the facts and leaves the row running.
#[tokio::test]
async fn progress_landing_after_the_sweep_judged_the_worker_idle_wins() {
    let boot = boot().await;
    let now = now_ms();
    let worker = seed_worker_started_at(&boot, "late-progress", now - 3 * HOUR).await;
    record_progress(&boot, &worker.card_id, now - 2 * HOUR).await;
    let (_runtime, scheduler) = scheduler_booted_long_ago(&boot);
    let hook = LivenessFailTestHook {
        judged: Arc::new(tokio::sync::Notify::new()),
        resume: Arc::new(tokio::sync::Notify::new()),
    };
    scheduler.set_liveness_fail_test_hook(hook.clone());

    let sweep = tokio::spawn({
        let scheduler = Arc::clone(&scheduler);
        async move { scheduler.sweep_all().await }
    });
    tokio::time::timeout(Duration::from_secs(10), hook.judged.notified())
        .await
        .expect("the sweep judges the silent worker idle");
    record_progress(&boot, &worker.card_id, now_ms()).await;
    hook.resume.notify_one();
    sweep.await.expect("sweep task");

    assert_still_running(&boot, &worker).await;
}

/// A row already running before the start column existed carries a deadline but no start: the
/// sweep stamps its start now instead of judging it idle from nothing.
#[tokio::test]
async fn pre_migration_running_row_gets_its_start_stamped_and_keeps_running() {
    let boot = boot().await;
    let (card_id, session_row_id, _terminal_id) =
        seed_codex_worker_card_with_terminal(&boot, "legacy").await;
    let (lease_id, lease_dir) = seed_held_workspace_lease(&boot, &card_id, "legacy").await;
    let deadline = now_ms() + HOUR;
    let mut task = plan_task(&boot.track_id, "legacy", TaskKind::Codex, &[]);
    task.status = TaskStatus::Running;
    task.worker_card_id = Some(card_id.clone());
    task.running_deadline_ms = Some(deadline);
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    assert_eq!(running_started_at(&boot, &task_id).await, None);
    let worker = LivenessWorker {
        key: "legacy".to_string(),
        task_id: task_id.clone(),
        card_id,
        session_row_id,
        lease_id,
        _lease_dir: lease_dir,
    };

    let before = now_ms();
    let (_runtime, scheduler) = scheduler_booted_long_ago(&boot);
    scheduler.sweep_all().await;
    let after = now_ms();

    let started = running_started_at(&boot, &worker.task_id)
        .await
        .expect("the sweep stamps the missing start");
    assert!((before..=after).contains(&started), "start {started}");
    assert_eq!(
        task_row(&boot, "legacy").await.running_deadline_ms,
        Some(deadline)
    );
    assert_still_running(&boot, &worker).await;
}
