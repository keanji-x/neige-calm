use calm_server::error::{CalmError, Result};
use calm_server::model::{Task, new_id};
use calm_server::wave_report::tasks_rebuild_tx;
use calm_types::report_blocks::render_fence;
use calm_types::wave_report::{ReportBlock, WaveReportPayload};
use serde_json::{Map, Value, json};
use sqlx::{Sqlite, SqlitePool, Transaction};

pub async fn insert_task_tx(tx: &mut Transaction<'_, Sqlite>, task: &Task) -> Result<()> {
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

pub async fn project_task(pool: &SqlitePool, task: &Task) -> Result<()> {
    let mut declaration = Map::from_iter([
        ("key".into(), json!(task.key)),
        ("kind".into(), json!(task.kind)),
        ("goal".into(), json!(task.goal)),
        ("context".into(), serde_json::from_str(&task.context_json)?),
        (
            "depends_on".into(),
            serde_json::from_str(&task.depends_on_json)?,
        ),
        ("priority".into(), json!(task.priority)),
        ("declared_by".into(), json!(task.declared_by)),
        ("spawn".into(), json!(task.spawn)),
        ("ready".into(), json!(true)),
    ]);
    for (key, value) in [
        ("acceptance", task.acceptance_criteria.as_ref()),
        ("cwd", task.cwd.as_ref()),
    ] {
        if let Some(value) = value {
            declaration.insert(key.into(), json!(value));
        }
    }
    if let Some(gate) = task.gate_json.as_deref() {
        declaration.insert("gate".into(), serde_json::from_str(gate)?);
    } else if !matches!(task.kind, calm_server::model::TaskKind::Terminal) {
        declaration.insert(
            "no_gate_reason".into(),
            json!("integration fixture does not execute a real worker"),
        );
    }
    let block = ReportBlock {
        id: format!("b_{}", new_id()),
        kind: "task".into(),
        rev: 1,
        payload: Value::Object(declaration),
    };
    let report = WaveReportPayload {
        schema_version: WaveReportPayload::SCHEMA_VERSION,
        doc_rev: 1,
        summary: String::new(),
        body: format!(
            "<!-- neige:{} -->\n{}",
            block.id,
            render_fence("task", &block.payload)
        ),
        blocks: Some(vec![block]),
    };
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO cards(id,wave_id,kind,sort,payload,role,deletable,created_at,updated_at) \
         VALUES(?1,?2,'wave-report',-1,?3,'reportcard',0,1,1)",
    )
    .bind(new_id())
    .bind(&task.wave_id)
    .bind(serde_json::to_string(&report)?)
    .execute(&mut *tx)
    .await?;
    let outcome = tasks_rebuild_tx(&mut tx, &task.wave_id).await?;
    let updated = sqlx::query(
        "UPDATE tasks SET status=?1,status_detail=?2,worker_card_id=?3,gate_result_json=?4,\
         gate_attempt=?5,gate_pid=?6,gate_pid_starttime=?7,gate_pid_boot_id=?8,\
         running_deadline_ms=?9,context_stale_at_ms=?10,created_at_ms=?11,updated_at_ms=?12,\
         finished_at_ms=?13 WHERE id=?14",
    )
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
    .bind(task.created_at_ms)
    .bind(task.updated_at_ms)
    .bind(task.finished_at_ms)
    .bind(&task.id)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(CalmError::Internal(format!(
            "test task projection did not materialize {}: {outcome:?}",
            task.key
        )));
    }
    tx.commit().await?;
    Ok(())
}
