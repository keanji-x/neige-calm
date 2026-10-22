#[cfg(target_os = "linux")]
pub(crate) mod proc;

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
            running_deadline_ms,spawn,created_at_ms,updated_at_ms,finished_at_ms,access,start,
            result_contract,child_track_json)
           VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,
                  ?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28)"#,
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
    .bind(&task.result_contract)
    .bind(&task.child_track_json)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Finish the track worktree declared by the fixture's initial creation plan.
pub(crate) async fn ensure_track_worktree(
    pool: &crate::db::sqlite::SqliteDb,
    track_id: &str,
) -> std::path::PathBuf {
    crate::test_seams::ensure_track_worktree_for_test(pool, track_id)
        .await
        .expect("make the declared track worktree")
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
