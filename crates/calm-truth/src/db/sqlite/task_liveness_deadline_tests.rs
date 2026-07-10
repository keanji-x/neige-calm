use super::{
    SqlxRepo, task_claim_pending_tx, task_get_tx, task_insert_tx, task_mark_running_tx,
    task_stamp_missing_running_deadline_tx,
};
use crate::model::{Task, TaskKind, TaskStatus, now_ms};

fn task(key: &str, status: TaskStatus) -> Task {
    let now = now_ms();
    Task {
        id: format!("wave-1:{key}"),
        wave_id: "wave-1".to_string(),
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
        created_at_ms: now,
        updated_at_ms: now,
        finished_at_ms: None,
    }
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
    task_insert_tx(&mut tx, &row).await.expect("insert task");
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
    task_insert_tx(&mut tx, &row).await.expect("insert task");
    let rows = task_claim_pending_tx(&mut tx, &id, 1000)
        .await
        .expect("claim pending");
    assert_eq!(rows, 1);
    let claimed = task_get_tx(&mut tx, &id)
        .await
        .expect("read claimed")
        .expect("claimed row");
    assert_eq!(claimed.status, TaskStatus::Dispatched);
    assert_eq!(claimed.running_deadline_ms, None);

    let rows = task_mark_running_tx(&mut tx, &id, Some("worker-card"), 2000, 9200)
        .await
        .expect("mark running");
    assert_eq!(rows, 1);
    let running = task_get_tx(&mut tx, &id)
        .await
        .expect("read running")
        .expect("running row");
    tx.commit().await.expect("commit");
    assert_eq!(running.status, TaskStatus::Running);
    assert_eq!(running.worker_card_id.as_deref(), Some("worker-card"));
    assert_eq!(running.running_deadline_ms, Some(9200));
}

#[tokio::test]
async fn stamp_missing_running_liveness_deadline_includes_claude_and_excludes_terminal() {
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
    task_insert_tx(&mut tx, &claude)
        .await
        .expect("insert claude task");
    task_insert_tx(&mut tx, &terminal)
        .await
        .expect("insert terminal task");

    let rows = task_stamp_missing_running_deadline_tx(&mut tx, &claude_id, 3000, 9700)
        .await
        .expect("stamp claude");
    assert_eq!(rows, 1);
    let rows = task_stamp_missing_running_deadline_tx(&mut tx, &terminal_id, 3000, 9700)
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
    tx.commit().await.expect("commit");
    assert_eq!(stamped.running_deadline_ms, Some(9700));
    assert_eq!(terminal.running_deadline_ms, None);
}
