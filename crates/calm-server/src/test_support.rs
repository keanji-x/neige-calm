use calm_truth::error::Result;
use calm_truth::model::Task;
use sqlx::{Sqlite, Transaction};

pub(crate) async fn insert_task_tx(tx: &mut Transaction<'_, Sqlite>, task: &Task) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO tasks
           (id,wave_id,key,kind,goal,context_json,acceptance_criteria,cwd,
            depends_on_json,priority,gate_json,status,status_detail,worker_card_id,
            gate_result_json,gate_attempt,gate_pid,gate_pid_starttime,gate_pid_boot_id,
            running_deadline_ms,spawn,created_at_ms,updated_at_ms,finished_at_ms)
           VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,
                  ?18,?19,?20,?21,?22,?23,?24)"#,
    )
    .bind(&task.id)
    .bind(&task.wave_id)
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
    .execute(&mut **tx)
    .await?;
    Ok(())
}
