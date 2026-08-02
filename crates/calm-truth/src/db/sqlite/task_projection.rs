use std::collections::{BTreeMap, BTreeSet};

use calm_types::report_blocks::tasks::{
    Diagnostic, TaskDeclaration, gate_rule_violations, unknown_deps,
};
use calm_types::report_links::parse_destination;
use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, Transaction};
use utoipa::ToSchema;

use crate::error::{CalmError, Result};
use crate::model::now_ms;

const DEFAULT_SPEC_TASK_CEILING: i64 = 32;
type FrozenDeclarationRow = (
    String,
    String,
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlockVerdict {
    pub block_id: String,
    pub key: String,
    pub diagnostics: Vec<Diagnostic>,
    pub schedulable: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskProjectionOutcome {
    pub changed_keys: Vec<String>,
    pub diagnostics: Vec<BlockVerdict>,
}

async fn wave_projection_policy_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &str,
) -> Result<(Option<String>, i64, bool)> {
    let row: Option<(Option<String>, Option<i64>, i64)> = sqlx::query_as(
        "SELECT automation_policy, spec_task_ceiling, require_task_gates FROM waves WHERE id=?1",
    )
    .bind(wave_id)
    .fetch_optional(&mut **tx)
    .await?;
    let (policy, ceiling, require_gates) =
        row.ok_or_else(|| CalmError::NotFound(format!("wave {wave_id}")))?;
    Ok((
        policy,
        ceiling.unwrap_or(DEFAULT_SPEC_TASK_CEILING).max(0),
        require_gates != 0,
    ))
}

/// The single DB-aware schedulability predicate used by writes, rebuilds and reads.
pub async fn evaluate_schedulability_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &str,
    declarations: &[TaskDeclaration],
    block_local_diags: &[Vec<Diagnostic>],
) -> Result<Vec<BlockVerdict>> {
    let (configured_policy, ceiling, require_gates) =
        wave_projection_policy_tx(tx, wave_id).await?;
    let effective_wait = configured_policy.as_deref() == Some("declare-and-wait")
        || (configured_policy.is_none() && declarations.iter().any(|d| d.tombstoned_by_user));
    let inflight: Vec<(String,)> = sqlx::query_as(
        "SELECT key FROM tasks WHERE wave_id=?1 AND status IN ('dispatched','running','verifying')",
    )
    .bind(wave_id)
    .fetch_all(&mut **tx)
    .await?;
    let inflight_keys: Vec<String> = inflight.into_iter().map(|(key,)| key).collect();
    let inflight_key_set: BTreeSet<&str> = inflight_keys.iter().map(String::as_str).collect();
    let occupied: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tasks WHERE wave_id=?1 AND declared_by='spec' AND origin='block' AND status IN ('dispatched','running','verifying')",
    )
    .bind(wave_id)
    .fetch_one(&mut **tx)
    .await?;
    let capacity = ceiling.saturating_sub(occupied).max(0) as usize;

    let unknown: BTreeSet<_> = unknown_deps(declarations, &inflight_keys)
        .into_iter()
        .collect();
    let gate_bad: BTreeSet<_> = gate_rule_violations(declarations, require_gates)
        .into_iter()
        .collect();
    let source_cove: String = sqlx::query_scalar("SELECT cove_id FROM waves WHERE id=?1")
        .bind(wave_id)
        .fetch_one(&mut **tx)
        .await?;
    let mut verdicts = Vec::with_capacity(declarations.len());
    for declaration in declarations {
        let mut diagnostics = block_local_diags
            .get(declaration.block_index)
            .cloned()
            .unwrap_or_default();
        for (_, dependency) in unknown.iter().filter(|(key, _)| key == &declaration.key) {
            diagnostics.push(Diagnostic::new(
                "depends_on",
                format!("unknown dependency `{dependency}`"),
            ));
        }
        if gate_bad.contains(&declaration.key) {
            diagnostics.push(Diagnostic::new(
                "gate",
                "task requires a gate or no_gate_reason",
            ));
        }
        for reference in &declaration.refs {
            let Some((dst_wave, _)) = parse_destination(reference) else {
                continue;
            };
            let allowed: Option<(i64,)> = sqlx::query_as(
                "SELECT 1 FROM waves w JOIN coves c ON c.id=w.cove_id WHERE w.id=?1 AND (w.cove_id=?2 OR c.kind='system')",
            )
            .bind(dst_wave)
            .bind(&source_cove)
            .fetch_optional(&mut **tx)
            .await?;
            if allowed.is_none() {
                diagnostics.push(Diagnostic::new(
                    "refs",
                    format!("cross-cove reference `{reference}` is not schedulable"),
                ));
            }
        }
        if effective_wait
            && declaration.declared_by == "spec"
            && !declaration.released_by_user
            && !declaration.tombstone
        {
            diagnostics.push(Diagnostic::new(
                "released_by_user",
                "this wave requires user release before spec tasks are queued",
            ));
        }
        verdicts.push(BlockVerdict {
            block_id: declaration.block_id.clone(),
            key: declaration.key.clone(),
            schedulable: declaration.ready && !declaration.tombstone && diagnostics.is_empty(),
            diagnostics,
        });
    }

    // Pending rows are outputs, never inputs. Only clean spec declarations compete for
    // the capacity left after immutable in-flight input, in block order then key order.
    let mut candidates: Vec<usize> = verdicts
        .iter()
        .enumerate()
        .filter(|(i, verdict)| {
            verdict.schedulable
                && declarations[*i].declared_by == "spec"
                && !inflight_key_set.contains(declarations[*i].key.as_str())
        })
        .map(|(i, _)| i)
        .collect();
    candidates.sort_by_key(|i| (declarations[*i].block_index, declarations[*i].key.clone()));
    for index in candidates.into_iter().skip(capacity) {
        verdicts[index].diagnostics.push(Diagnostic::new(
            "key",
            format!("spec task ceiling of {ceiling} is reached"),
        ));
        verdicts[index].schedulable = false;
    }
    for (declaration, verdict) in declarations.iter().zip(&mut verdicts) {
        let row: Option<FrozenDeclarationRow> =
            sqlx::query_as(
                "SELECT status,kind,goal,depends_on_json,priority,gate_json,declared_by FROM tasks WHERE wave_id=?1 AND key=?2 AND status!='pending'",
            )
            .bind(wave_id)
            .bind(&declaration.key)
            .fetch_optional(&mut **tx)
            .await?;
        if let Some((_status, kind, goal, depends, priority, gate, declared_by)) = row {
            let expected_depends = serde_json::to_string(&declaration.depends_on)
                .map_err(|e| CalmError::Internal(format!("serialize task dependencies: {e}")))?;
            let expected_gate = declaration
                .gate
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| CalmError::Internal(format!("serialize task gate: {e}")))?;
            if kind != declaration.kind
                || goal != declaration.goal
                || depends != expected_depends
                || priority != declaration.priority
                || gate != expected_gate
                || declared_by.as_deref() != Some(declaration.declared_by.as_str())
            {
                verdict.diagnostics.push(Diagnostic::new(
                    "key",
                    "task is already executing; declaration changes were not applied",
                ));
                verdict.schedulable = false;
            }
        }
    }
    Ok(verdicts)
}

pub async fn task_delete_pending_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<u64> {
    Ok(
        sqlx::query("DELETE FROM tasks WHERE id=?1 AND status='pending'")
            .bind(id)
            .execute(&mut **tx)
            .await?
            .rows_affected(),
    )
}

pub async fn project_tasks_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &str,
    declarations: &[TaskDeclaration],
    block_local_diags: &[Vec<Diagnostic>],
) -> Result<TaskProjectionOutcome> {
    let verdicts = evaluate_schedulability_tx(tx, wave_id, declarations, block_local_diags).await?;
    let verdict_by_key: BTreeMap<_, _> = verdicts
        .iter()
        .filter(|v| !v.key.is_empty())
        .map(|v| (v.key.as_str(), v))
        .collect();
    let existing: Vec<(String, String, String)> =
        sqlx::query_as("SELECT id,key,status FROM tasks WHERE wave_id=?1 AND origin='block'")
            .bind(wave_id)
            .fetch_all(&mut **tx)
            .await?;
    let mut changed = BTreeSet::new();
    for (id, key, status) in &existing {
        if !verdict_by_key
            .get(key.as_str())
            .is_some_and(|v| v.schedulable)
            && status == "pending"
            && task_delete_pending_tx(tx, id).await? != 0
        {
            changed.insert(key.clone());
        }
    }
    let now = now_ms();
    for (declaration, verdict) in declarations.iter().zip(&verdicts) {
        if !declaration.tombstone {
            let adopted = sqlx::query(
                "UPDATE tasks SET origin='block',updated_at_ms=?1 WHERE wave_id=?2 AND key=?3 AND origin='legacy' AND status!='pending'",
            )
            .bind(now)
            .bind(wave_id)
            .bind(&declaration.key)
            .execute(&mut **tx)
            .await?;
            if adopted.rows_affected() != 0 {
                changed.insert(declaration.key.clone());
            }
        }
        if !verdict.schedulable {
            continue;
        }
        let id = format!("{wave_id}:{}", declaration.key);
        let context = serde_json::to_string(&declaration.context)
            .map_err(|e| CalmError::Internal(format!("serialize task context: {e}")))?;
        let depends = serde_json::to_string(&declaration.depends_on)
            .map_err(|e| CalmError::Internal(format!("serialize task dependencies: {e}")))?;
        let gate = declaration
            .gate
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| CalmError::Internal(format!("serialize task gate: {e}")))?;
        let result = sqlx::query(
            "INSERT INTO tasks(id,wave_id,key,kind,goal,context_json,acceptance_criteria,cwd,depends_on_json,priority,gate_json,status,declared_by,origin,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'pending',?12,'block',?13,?13) ON CONFLICT(wave_id,key) DO UPDATE SET kind=excluded.kind,goal=excluded.goal,context_json=excluded.context_json,acceptance_criteria=excluded.acceptance_criteria,cwd=excluded.cwd,depends_on_json=excluded.depends_on_json,priority=excluded.priority,gate_json=excluded.gate_json,declared_by=excluded.declared_by,origin='block',updated_at_ms=excluded.updated_at_ms WHERE tasks.status='pending' AND (tasks.origin='block' OR tasks.origin='legacy') AND (tasks.kind IS NOT excluded.kind OR tasks.goal IS NOT excluded.goal OR tasks.context_json IS NOT excluded.context_json OR tasks.acceptance_criteria IS NOT excluded.acceptance_criteria OR tasks.cwd IS NOT excluded.cwd OR tasks.depends_on_json IS NOT excluded.depends_on_json OR tasks.priority IS NOT excluded.priority OR tasks.gate_json IS NOT excluded.gate_json OR tasks.declared_by IS NOT excluded.declared_by OR tasks.origin IS NOT 'block')",
        )
        .bind(&id).bind(wave_id).bind(&declaration.key).bind(&declaration.kind)
        .bind(&declaration.goal).bind(context).bind(&declaration.acceptance)
        .bind(&declaration.cwd).bind(depends).bind(declaration.priority).bind(gate)
        .bind(&declaration.declared_by).bind(now).execute(&mut **tx).await?;
        if result.rows_affected() != 0 {
            changed.insert(declaration.key.clone());
        }
    }
    Ok(TaskProjectionOutcome {
        changed_keys: changed.into_iter().collect(),
        diagnostics: verdicts,
    })
}

pub async fn tasks_rebuild_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &str,
) -> Result<TaskProjectionOutcome> {
    let payload: String = sqlx::query_scalar(
        "SELECT json(payload) FROM cards WHERE wave_id=?1 AND kind='wave-report'",
    )
    .bind(wave_id)
    .fetch_one(&mut **tx)
    .await?;
    let payload: calm_types::wave_report::WaveReportPayload = serde_json::from_str(&payload)
        .map_err(|e| CalmError::Internal(format!("decode report payload for rebuild: {e}")))?;
    let blocks = payload.blocks.unwrap_or_else(|| {
        calm_types::report_blocks::reassign_ids(
            &[],
            &calm_types::report_blocks::split_body(&payload.body),
        )
    });
    let (declarations, local) =
        calm_types::report_blocks::tasks::project_task_declarations(&blocks);
    project_tasks_tx(tx, wave_id, &declarations, &local).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_types::report_blocks::tasks::TaskDeclaration;
    use serde_json::json;

    fn declaration(index: usize, key: &str) -> TaskDeclaration {
        TaskDeclaration {
            block_index: index,
            block_id: format!("b_{index:04x}"),
            key: key.into(),
            kind: "codex".into(),
            goal: format!("goal {key}"),
            acceptance: None,
            gate: None,
            no_gate_reason: Some("not needed".into()),
            depends_on: Vec::new(),
            context: json!({}),
            cwd: None,
            priority: 0,
            refs: Vec::new(),
            declared_by: "spec".into(),
            released_by_user: false,
            tombstoned_by_user: false,
            ready: true,
            tombstone: false,
        }
    }

    async fn setup() -> (super::super::SqlxRepo, String) {
        let repo = super::super::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let wave = "wave-projection".to_string();
        sqlx::query("INSERT INTO coves(id,name,color,sort,kind,created_at,updated_at) VALUES('cove-projection','c','#000',0,'user',0,0)")
            .execute(&repo.pool).await.unwrap();
        sqlx::query("INSERT INTO waves(id,cove_id,title,sort,lifecycle,cwd,created_at,updated_at,spec_task_ceiling,require_task_gates) VALUES(?1,'cove-projection','w',0,'draft','/',0,0,1,0)")
            .bind(&wave).execute(&repo.pool).await.unwrap();
        (repo, wave)
    }

    async fn insert_block_task(repo: &super::super::SqlxRepo, wave: &str, key: &str, status: &str) {
        sqlx::query("INSERT INTO tasks(id,wave_id,key,kind,goal,context_json,depends_on_json,priority,status,declared_by,origin,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,'codex',?4,'{}','[]',0,?5,'spec','block',0,0)")
            .bind(format!("{wave}:{key}")).bind(wave).bind(key).bind(format!("goal {key}")).bind(status)
            .execute(&repo.pool).await.unwrap();
    }

    #[tokio::test]
    async fn ceiling_i_a_inflight_is_input_and_holds_the_upper_bound() {
        let (repo, wave) = setup().await;
        insert_block_task(&repo, &wave, "k1", "dispatched").await;
        let declarations = vec![declaration(0, "k2"), declaration(1, "k1")];
        let mut tx = repo.pool.begin().await.unwrap();
        let outcome = project_tasks_tx(&mut tx, &wave, &declarations, &[vec![], vec![]])
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert!(!outcome.diagnostics[0].schedulable);
        assert!(
            outcome.diagnostics[0]
                .diagnostics
                .iter()
                .any(|d| d.message.contains("ceiling"))
        );
        assert!(outcome.diagnostics[1].schedulable);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE wave_id=?1 AND status IN ('pending','dispatched','running','verifying')")
            .bind(&wave).fetch_one(&repo.pool).await.unwrap();
        assert_eq!(
            count, 1,
            "invariant 7b: no pending row may exceed occupied capacity"
        );
    }

    #[tokio::test]
    async fn ceiling_i_b_diagnosed_pending_is_output_and_does_not_cause_jitter() {
        let (repo, wave) = setup().await;
        insert_block_task(&repo, &wave, "k1", "pending").await;
        let declarations = vec![declaration(0, "k1"), declaration(1, "k2")];
        let local = vec![vec![Diagnostic::new("key", "broken")], vec![]];
        for _ in 0..2 {
            let mut tx = repo.pool.begin().await.unwrap();
            let outcome = project_tasks_tx(&mut tx, &wave, &declarations, &local)
                .await
                .unwrap();
            tx.commit().await.unwrap();
            assert!(!outcome.diagnostics[0].schedulable);
            assert!(outcome.diagnostics[1].schedulable);
            let keys: Vec<(String,)> =
                sqlx::query_as("SELECT key FROM tasks WHERE wave_id=?1 ORDER BY key")
                    .bind(&wave)
                    .fetch_all(&repo.pool)
                    .await
                    .unwrap();
            assert_eq!(keys, vec![("k2".into(),)]);
        }
    }
}
