use calm_truth::error::Result;
use calm_truth::model::Task;
use sqlx::{Sqlite, Transaction};

#[cfg(unix)]
pub(crate) use plugin::test_support::assert_pid_dead;

pub(crate) async fn insert_task_tx(tx: &mut Transaction<'_, Sqlite>, task: &Task) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO tasks
           (id,track_id,key,kind,goal,context_json,acceptance_criteria,cwd,
            depends_on_json,priority,gate_json,status,status_detail,worker_card_id,
            gate_result_json,gate_attempt,gate_pid,gate_pid_starttime,gate_pid_boot_id,
            running_deadline_ms,spawn,created_at_ms,updated_at_ms,finished_at_ms,access,start)
           VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,
                  ?18,?19,?20,?21,?22,?23,?24,?25,?26)"#,
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
    .bind(&task.spawn)
    .bind(task.created_at_ms)
    .bind(task.updated_at_ms)
    .bind(task.finished_at_ms)
    .bind(task.access.as_str())
    .bind(task.start.as_str())
    .execute(&mut **tx)
    .await?;
    // `worker_card_id` is written only with the binding (#2493): a seeded worker card runs the
    // attempt in a session of it. A row naming a card the fixture never made stays unbound.
    if let Some(card_id) = task.worker_card_id.as_deref() {
        let card_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cards WHERE id = ?1)")
                .bind(card_id)
                .fetch_one(&mut **tx)
                .await?;
        if card_exists {
            bind_seeded_task_tx(tx, task).await?;
        }
    }
    Ok(())
}

/// Bind a seeded `task` (inserted with `worker_card_id` set, its track row present) to a
/// worker session on that card through the production binder (#2493), whatever status the row was
/// seeded with: the row is bound while `dispatched`, then given its status back. Returns the
/// session id.
pub(crate) async fn bind_seeded_task_tx(
    tx: &mut Transaction<'_, Sqlite>,
    task: &Task,
) -> Result<String> {
    let card_id = task
        .worker_card_id
        .as_deref()
        .expect("a seeded bound task names its worker card");
    sqlx::query("UPDATE tasks SET status = 'dispatched' WHERE id = ?1")
        .bind(&task.id)
        .execute(&mut **tx)
        .await?;
    let session_id = crate::test_seams::bind_fixture_worker_tx(tx, &task.id, card_id)
        .await
        .map_err(|e| calm_truth::TruthError::Internal(e.to_string()))?;
    sqlx::query("UPDATE tasks SET status = ?1 WHERE id = ?2")
        .bind(task.status)
        .bind(&task.id)
        .execute(&mut **tx)
        .await?;
    Ok(session_id)
}

/// Give an attached track its #1830 track worktree, as the create route does (the production
/// `ensure_track_worktree`). Returns it.
pub(crate) async fn attach_track_worktree(
    pool: &sqlx::SqlitePool,
    track_id: &str,
    checkout: &std::path::Path,
) -> std::path::PathBuf {
    crate::test_seams::attach_track_worktree_for_test(pool, track_id, checkout)
        .await
        .expect("make the track worktree")
}

/// A one-commit git repository at `path`: the fixture checkout an attached track's worktree is
/// made from (never the neige-calm checkout).
pub(crate) fn init_fixture_git_repo(path: &std::path::Path) {
    std::fs::create_dir_all(path).expect("create the fixture repo dir");
    std::fs::write(path.join("README.md"), "initial\n").expect("write the fixture file");
    for args in [
        &["init"][..],
        &["config", "user.email", "fixture@example.test"],
        &["config", "user.name", "Fixture"],
        &["add", "README.md"],
        &["commit", "-m", "initial"],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed in {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
