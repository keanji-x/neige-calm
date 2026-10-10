use super::{
    RunningLivenessFacts, SqlxRepo, task_claim_pending_tx, task_get_tx, task_mark_running_tx,
    task_running_liveness_tx, task_stamp_missing_running_liveness_tx,
};
use crate::model::{Task, TaskKind, TaskStatus, now_ms};

fn task(key: &str, status: TaskStatus) -> Task {
    let now = now_ms();
    Task {
        id: format!("track-1:{key}"),
        track_id: "track-1".to_string(),
        key: key.to_string(),
        kind: TaskKind::Codex,
        goal: format!("do {key}"),
        context_json: "null".to_string(),
        acceptance_criteria: None,
        cwd: None,
        depends_on_json: "[]".to_string(),
        priority: 0,
        gate_json: None,
        status,
        status_detail: None,
        worker_card_id: None,
        gate_result_json: None,
        gate_attempt: 0,
        gate_pid: None,
        gate_pid_starttime: None,
        gate_pid_boot_id: None,
        running_deadline_ms: None,
        context_stale_at_ms: None,
        declared_by: "spec".into(),
        spawn: "in-wave".into(),
        access: crate::model::TaskAccess::ReadWrite,
        start: crate::model::TaskStart::Checkout,
        created_at_ms: now,
        updated_at_ms: now,
        finished_at_ms: None,
    }
}

async fn insert_task(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, task: &Task) {
    sqlx::query(
        r#"INSERT INTO tasks (
               id,track_id,key,kind,goal,context_json,acceptance_criteria,cwd,
               depends_on_json,priority,gate_json,status,status_detail,worker_card_id,
               gate_result_json,gate_attempt,gate_pid,gate_pid_starttime,gate_pid_boot_id,
               running_deadline_ms,context_stale_at_ms,declared_by,claim_context_json,
               context_closure_truncated,decl_ready,decl_released_by_user,
               context_verify_failures,spawn,child_track_id,created_at_ms,updated_at_ms,
               finished_at_ms
           ) VALUES (
               ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,
               ?17,?18,?19,?20,?21,?22,NULL,0,0,0,0,?23,NULL,?24,?25,?26
           )"#,
    )
    .bind(&task.id)
    .bind(&task.track_id)
    .bind(&task.key)
    .bind(task.kind)
    .bind(&task.goal)
    .bind(&task.context_json)
    .bind(&task.acceptance_criteria)
    .bind(&task.cwd)
    .bind(&task.depends_on_json)
    .bind(task.priority)
    .bind(&task.gate_json)
    .bind(task.status)
    .bind(&task.status_detail)
    .bind(&task.worker_card_id)
    .bind(&task.gate_result_json)
    .bind(task.gate_attempt)
    .bind(task.gate_pid)
    .bind(task.gate_pid_starttime)
    .bind(&task.gate_pid_boot_id)
    .bind(task.running_deadline_ms)
    .bind(task.context_stale_at_ms)
    .bind(&task.declared_by)
    .bind(&task.spawn)
    .bind(task.created_at_ms)
    .bind(task.updated_at_ms)
    .bind(task.finished_at_ms)
    .execute(&mut **tx)
    .await
    .expect("insert task fixture");
}

#[tokio::test]
async fn migration_adds_task_running_liveness_column_and_task_round_trips_it() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open in-memory sqlite repo");
    let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('tasks')")
        .fetch_all(repo.pool())
        .await
        .expect("table info");
    assert!(columns.iter().any(|c| c == "running_deadline_ms"));
    let index_sql: Option<String> =
        sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name = ?1")
            .bind("idx_tasks_liveness_deadlines")
            .fetch_optional(repo.pool())
            .await
            .expect("index lookup");
    assert!(
        index_sql
            .as_deref()
            .is_some_and(|sql| sql.contains("WHERE status = 'running'")),
        "partial liveness index missing or drifted: {index_sql:?}"
    );

    let mut row = task("roundtrip", TaskStatus::Running);
    row.running_deadline_ms = Some(5678);
    let id = row.id.clone();
    let mut tx = repo.pool().begin().await.expect("begin insert tx");
    insert_task(&mut tx, &row).await;
    let read = task_get_tx(&mut tx, &id)
        .await
        .expect("read task")
        .expect("task row");
    tx.commit().await.expect("commit");
    assert_eq!(read.running_deadline_ms, Some(5678));
}

#[tokio::test]
async fn mark_running_stamps_running_liveness_deadline() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open in-memory sqlite repo");
    let row = task("stamp", TaskStatus::Pending);
    let id = row.id.clone();
    let mut tx = repo.pool().begin().await.expect("begin insert tx");
    insert_task(&mut tx, &row).await;
    let rows = task_claim_pending_tx(&mut tx, &id, 1000, &[], false)
        .await
        .expect("claim pending");
    assert_eq!(rows, 1);
    let claimed = task_get_tx(&mut tx, &id)
        .await
        .expect("read claimed")
        .expect("claimed row");
    assert_eq!(claimed.status, TaskStatus::Dispatched);
    assert_eq!(claimed.running_deadline_ms, None);

    let rows = task_mark_running_tx(&mut tx, &id, 2000, 9200)
        .await
        .expect("mark running");
    assert_eq!(rows, 1);
    let running = task_get_tx(&mut tx, &id)
        .await
        .expect("read running")
        .expect("running row");
    let facts = task_running_liveness_tx(&mut tx, &id, Some("worker-card"))
        .await
        .expect("read liveness facts");
    tx.commit().await.expect("commit");
    assert_eq!(running.status, TaskStatus::Running);
    // #2493: the worker card is bound when the spawn prepares, never stamped here.
    assert_eq!(running.worker_card_id, None);
    assert_eq!(running.running_deadline_ms, Some(9200));
    assert_eq!(
        facts,
        Some(RunningLivenessFacts {
            started_at_ms: 2000,
            deadline_ms: 9200,
            last_progress_ms: None,
        })
    );
}

#[tokio::test]
async fn liveness_facts_take_the_latest_cursor_of_the_worker_card_only() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open in-memory sqlite repo");
    let row = task("progress", TaskStatus::Pending);
    let id = row.id.clone();
    let mut tx = repo.pool().begin().await.expect("begin insert tx");
    insert_task(&mut tx, &row).await;
    task_claim_pending_tx(&mut tx, &id, 1000, &[], false)
        .await
        .expect("claim pending");
    task_mark_running_tx(&mut tx, &id, 2000, 9200)
        .await
        .expect("mark running");
    tx.commit().await.expect("commit");
    super::worker_flow_cursor_tests::seed_worker_cards(&repo, &["worker-card", "other-card"]).await;
    for (card, kind, at) in [
        ("worker-card", "codex_rollout", 4000),
        ("worker-card", "claude_transcript", 5000),
        ("other-card", "codex_rollout", 8000),
    ] {
        let position = calm_exec::flow::CapturePosition {
            source_path: "/rollout.jsonl".into(),
            record_index: 1,
            byte_offset: 10,
            last_source_uuid: None,
            last_line_hash: None,
        };
        super::worker_flow_cursor_set_for_test(repo.pool(), card, kind, &position, at)
            .await
            .expect("set cursor");
    }
    let mut tx = repo.pool().begin().await.expect("begin read tx");
    let facts = task_running_liveness_tx(&mut tx, &id, Some("worker-card"))
        .await
        .expect("read facts")
        .expect("running facts");
    let unknown_card = task_running_liveness_tx(&mut tx, &id, None)
        .await
        .expect("read facts without a card")
        .expect("running facts");
    tx.commit().await.expect("commit");
    assert_eq!(facts.last_progress_ms, Some(5000));
    assert_eq!(unknown_card.last_progress_ms, None);
}

#[tokio::test]
async fn stamp_missing_running_liveness_includes_claude_and_excludes_terminal() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open in-memory sqlite repo");
    let mut claude = task("claude", TaskStatus::Running);
    claude.kind = TaskKind::Claude;
    let mut terminal = task("terminal", TaskStatus::Running);
    terminal.kind = TaskKind::Terminal;
    let claude_id = claude.id.clone();
    let terminal_id = terminal.id.clone();
    let mut tx = repo.pool().begin().await.expect("begin insert tx");
    insert_task(&mut tx, &claude).await;
    insert_task(&mut tx, &terminal).await;

    let rows = task_stamp_missing_running_liveness_tx(&mut tx, &claude_id, 3000, 9700)
        .await
        .expect("stamp claude");
    assert_eq!(rows, 1);
    let rows = task_stamp_missing_running_liveness_tx(&mut tx, &terminal_id, 3000, 9700)
        .await
        .expect("stamp terminal");
    assert_eq!(rows, 0);
    let stamped = task_get_tx(&mut tx, &claude_id)
        .await
        .expect("read claude")
        .expect("claude row");
    let terminal = task_get_tx(&mut tx, &terminal_id)
        .await
        .expect("read terminal")
        .expect("terminal row");
    let stamped_facts = task_running_liveness_tx(&mut tx, &claude_id, None)
        .await
        .expect("read claude facts");
    tx.commit().await.expect("commit");
    assert_eq!(stamped.running_deadline_ms, Some(9700));
    assert_eq!(terminal.running_deadline_ms, None);
    assert_eq!(
        stamped_facts,
        Some(RunningLivenessFacts {
            started_at_ms: 3000,
            deadline_ms: 9700,
            last_progress_ms: None,
        })
    );
}

/// A row running before the start column existed keeps the deadline it carries; only its start
/// is stamped, once.
#[tokio::test]
async fn stamp_missing_running_liveness_keeps_an_existing_deadline_once() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open in-memory sqlite repo");
    let mut legacy = task("legacy", TaskStatus::Running);
    legacy.running_deadline_ms = Some(1234);
    let id = legacy.id.clone();
    let mut tx = repo.pool().begin().await.expect("begin insert tx");
    insert_task(&mut tx, &legacy).await;
    assert_eq!(
        task_running_liveness_tx(&mut tx, &id, None)
            .await
            .expect("read unstamped facts"),
        None
    );
    let rows = task_stamp_missing_running_liveness_tx(&mut tx, &id, 3000, 9700)
        .await
        .expect("stamp legacy");
    assert_eq!(rows, 1);
    let rows = task_stamp_missing_running_liveness_tx(&mut tx, &id, 4000, 9999)
        .await
        .expect("restamp legacy");
    assert_eq!(rows, 0);
    let facts = task_running_liveness_tx(&mut tx, &id, None)
        .await
        .expect("read stamped facts");
    tx.commit().await.expect("commit");
    assert_eq!(
        facts,
        Some(RunningLivenessFacts {
            started_at_ms: 3000,
            deadline_ms: 1234,
            last_progress_ms: None,
        })
    );
}
