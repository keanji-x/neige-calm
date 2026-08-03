use std::collections::{BTreeMap, BTreeSet};

use calm_types::report_blocks::tasks::{
    Diagnostic, GateInput, TaskDeclaration, gate_rule_violations, json_eq, opt_json_eq,
    unknown_deps,
};
use calm_types::report_links::parse_destination;
use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, SqliteConnection, Transaction};
use utoipa::ToSchema;

use crate::error::{CalmError, Result};
use crate::model::now_ms;

const DEFAULT_SPEC_TASK_CEILING: i64 = 32;
type FrozenDeclarationRow = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    i64,
    Option<String>,
    String,
    String,
);

fn declaration_context_json(declaration: &TaskDeclaration, legacy: bool) -> Result<String> {
    let mut context = declaration.context.clone();
    if legacy && let Some(reason) = &declaration.no_gate_reason {
        match &mut context {
            serde_json::Value::Null => {
                context = serde_json::json!({"no_gate_reason": reason});
            }
            serde_json::Value::Object(map) => {
                map.insert(
                    "no_gate_reason".into(),
                    serde_json::Value::String(reason.clone()),
                );
            }
            _ => {}
        }
    }
    serde_json::to_string(&context)
        .map_err(|e| CalmError::Internal(format!("serialize task context: {e}")))
}

fn context_eq(actual: &str, expected: &str, declaration: &TaskDeclaration, legacy: bool) -> bool {
    if legacy
        && declaration.no_gate_reason.is_none()
        && declaration
            .context
            .as_object()
            .is_some_and(|value| value.is_empty())
        && serde_json::from_str::<serde_json::Value>(actual)
            .is_ok_and(|value| value.is_null() || value.as_object().is_some_and(|v| v.is_empty()))
    {
        return true;
    }
    json_eq(actual, expected)
}

fn gate_eq(actual: &Option<String>, expected: &Option<GateInput>) -> Result<bool> {
    if let Some(actual) = actual
        && let Ok(actual) = serde_json::from_str::<GateInput>(actual)
    {
        return Ok(Some(actual) == *expected);
    }
    let expected = expected
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| CalmError::Internal(format!("serialize task gate: {e}")))?;
    Ok(opt_json_eq(actual, &expected))
}

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

fn withdrawal_diagnostic(key: &str, status: &str) -> Diagnostic {
    Diagnostic::new(
        "key",
        format!(
            "task `{key}` is in flight ({status}) and cannot be withdrawn immediately; its declaration context is now stale, so any gate operation that has not started will be rejected"
        ),
    )
}

/// One in-flight `tasks` row, as carried by [`wave_projection_state`]'s
/// `json_group_array`. Shape mirrors the four columns the predicate needs;
/// `declared_by` / `origin` are `NOT NULL` since migration 0068.
#[derive(Deserialize)]
struct InflightTaskRow {
    key: String,
    status: String,
    declared_by: String,
    origin: String,
}

/// Everything the schedulability verdict needs from `waves` + the in-flight
/// `tasks` rows, read in ONE statement (#1016).
struct WaveProjectionState {
    policy: Option<String>,
    ceiling: i64,
    require_gates: bool,
    /// Cove of the wave being projected — the source side of the
    /// cross-cove reference check.
    source_cove: String,
    inflight: Vec<InflightTaskRow>,
}

#[derive(sqlx::FromRow)]
struct WaveProjectionStateRow {
    automation_policy: Option<String>,
    spec_task_ceiling: Option<i64>,
    require_task_gates: i64,
    cove_id: String,
    inflight_json: String,
}

/// Reads the wave's projection policy, its cove, and every in-flight task
/// row in a SINGLE statement.
///
/// This used to be four statements — policy/ceiling/gates, the in-flight key
/// list, the ceiling-occupancy `count(*)`, and `SELECT cove_id` — which is
/// fine inside the write path's IMMEDIATE transaction but mixes database
/// versions on the read path, where the caller runs in autocommit. A ceiling
/// read at t0 against an occupancy counted at t1 can report a capacity that
/// never existed, and the orphaned-in-flight scan could contradict the
/// in-flight key list. One statement is one implicit transaction, so all of
/// it now comes from one version. It also removes the `SELECT cove_id ...
/// fetch_one` that turned a concurrently deleted wave into a 500 (it is the
/// `NotFound` below, i.e. a 404, on the same row that carries the policy).
///
/// The three subsets the predicate needs are all derivable from the in-flight
/// rows, so the extra statements bought nothing even before consistency:
/// every in-flight key, the `declared_by='spec' AND origin='block'` count,
/// and the `origin='block'` orphan candidates.
async fn wave_projection_state(
    conn: &mut SqliteConnection,
    wave_id: &str,
) -> Result<WaveProjectionState> {
    let row: Option<WaveProjectionStateRow> = sqlx::query_as(
        r#"SELECT w.automation_policy, w.spec_task_ceiling, w.require_task_gates, w.cove_id,
                  (SELECT json_group_array(json_object(
                       'key', t.key, 'status', t.status,
                       'declared_by', t.declared_by, 'origin', t.origin))
                   FROM tasks t
                   WHERE t.wave_id = w.id
                     AND t.status IN ('dispatched','running','verifying')) AS inflight_json
           FROM waves w WHERE w.id = ?1"#,
    )
    .bind(wave_id)
    .fetch_optional(&mut *conn)
    .await?;
    let row = row.ok_or_else(|| CalmError::NotFound(format!("wave {wave_id}")))?;
    Ok(WaveProjectionState {
        policy: row.automation_policy,
        ceiling: row
            .spec_task_ceiling
            .unwrap_or(DEFAULT_SPEC_TASK_CEILING)
            .max(0),
        require_gates: row.require_task_gates != 0,
        source_cove: row.cove_id,
        inflight: serde_json::from_str(&row.inflight_json)?,
    })
}

/// The single DB-aware schedulability predicate used by writes, rebuilds and reads.
///
/// Takes a bare connection rather than a transaction (#1016): the WRITE path
/// hands it `&mut **tx` so its verdict stays atomic with the projection it
/// drives, while the read-only path (`read.rs::task_diagnostics`) hands it a
/// pooled connection in AUTOCOMMIT mode.
///
/// Consistency on the READ path, stated precisely — including what is still
/// broken. The verdict core — policy, ceiling, in-flight occupancy, in-flight
/// keys, the wave's cove — is ONE statement ([`wave_projection_state`]), so a
/// displayed capacity or schedulability decision can no longer be assembled
/// from two different versions of the database. Two reads remain outside that
/// statement, each its own autocommit version:
///
///   * the frozen-declaration scan (`status!='pending'`), used to flag "this
///     key is already executing"; and
///   * the per-reference existence/cove lookups, which are questions about
///     OTHER waves and cards and are inherently point-in-time.
///
/// The per-reference lookups really are only "possibly stale": they ask about
/// unrelated entities and a stale answer is a stale answer.
///
/// The frozen scan is NOT merely stale — it can make the returned verdict set
/// SELF-CONTRADICTORY, and this doc says so rather than rounding it down to
/// "pointwise consistent". The core snapshot at t0 and the frozen scan at t1
/// disagree about any task whose row changed in between. Concretely: a task
/// that is in-flight at t0 and deleted (or driven terminal) before t1 still
/// occupies a ceiling slot in `capacity`, yet no longer appears in
/// `frozen_by_key`. The result can be a verdict carrying `schedulable = true`
/// for a key whose slot the same call already counted as taken — a decision
/// that no single database version would have produced. The reverse skew
/// (row becomes frozen after t0) is the same shape.
///
/// This is not a #1016 regression: the pre-#1016 four-statement version had
/// the identical split — #1016 only shrank the number of versions that can be
/// mixed, from four to two. Collapsing the remainder is a separate job:
/// `evaluate_schedulability` fans out one data-dependent query per declared
/// ref, so it cannot become a single statement without restructuring the
/// predicate that the write path shares.
///
/// What keeps the skew off the correctness path: on the WRITE path all of
/// this runs inside the caller's IMMEDIATE transaction, so the verdict that
/// actually ADMITS tasks is fully atomic. The read path
/// (`read.rs::task_diagnostics`) is a diagnostics surface — a contradictory
/// verdict there is displayed, never acted on.
///
/// Why not an explicit deferred tx on the read path: it would hold R locks
/// across statements and could cycle against an IMMEDIATE writer touching the
/// same tables in the opposite order — the `SQLITE_LOCKED` (6) abort
/// `deferred_read_tx_deadlock_repro` pins for the sibling reader. That cycle
/// only exists on a shared-cache database (the in-memory sqlite used by CI
/// and `make dev-fresh`); the production file database (PRIVATECACHE + WAL)
/// gives readers a snapshot and never cycles. An autocommit statement that
/// blocks unwinds its implicit transaction — releasing every table lock it
/// took — before parking in `unlock_notify`, so it can never be the
/// lock-HOLDING waiter (pinned by `deadlock_semantics_autocommit_join_*`).
/// `begin_immediate_tx` would also be cycle-free and fully consistent, but it
/// takes the writer slot and would serialize this read against every writer
/// on production too — a real production cost for a non-production gap.
pub async fn evaluate_schedulability(
    conn: &mut SqliteConnection,
    wave_id: &str,
    declarations: &[TaskDeclaration],
    block_local_diags: &[Vec<Diagnostic>],
) -> Result<Vec<BlockVerdict>> {
    let state = wave_projection_state(&mut *conn, wave_id).await?;
    let configured_policy = state.policy;
    let ceiling = state.ceiling;
    let require_gates = state.require_gates;
    let source_cove = state.source_cove;
    let effective_wait = configured_policy.as_deref() == Some("declare-and-wait")
        || (configured_policy.is_none() && declarations.iter().any(|d| d.tombstoned_by_user));
    // unknown_deps knows every in-flight key in the wave, including rows
    // backfilled as legacy.  Ceiling occupancy is intentionally narrower.
    let inflight_keys: Vec<String> = state.inflight.iter().map(|r| r.key.clone()).collect();
    let inflight_key_set: BTreeSet<&str> = inflight_keys.iter().map(String::as_str).collect();
    let occupied: i64 = state
        .inflight
        .iter()
        .filter(|r| r.declared_by == "spec" && r.origin == "block")
        .count() as i64;
    let capacity = ceiling.saturating_sub(occupied).max(0) as usize;

    let unknown: BTreeSet<_> = unknown_deps(declarations, &inflight_keys)
        .into_iter()
        .collect();
    let gate_bad: BTreeSet<_> = gate_rule_violations(declarations, require_gates)
        .into_iter()
        .collect();
    let mut verdicts = Vec::with_capacity(declarations.len());
    for declaration in declarations {
        let mut diagnostics = block_local_diags
            .get(declaration.block_index.unwrap_or(usize::MAX))
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
            let target: Option<(String, String)> = if let Some((dst_wave, _)) =
                parse_destination(reference)
            {
                sqlx::query_as("SELECT w.cove_id,c.kind FROM waves w JOIN coves c ON c.id=w.cove_id WHERE w.id=?1")
                    .bind(dst_wave).fetch_optional(&mut *conn).await?
            } else if let Some(card_id) = reference
                .strip_prefix("neige://card/")
                .filter(|id| !id.is_empty() && !id.contains('/'))
            {
                sqlx::query_as("SELECT w.cove_id,c.kind FROM cards card JOIN waves w ON w.id=card.wave_id JOIN coves c ON c.id=w.cove_id WHERE card.id=?1")
                    .bind(card_id).fetch_optional(&mut *conn).await?
            } else {
                continue;
            };
            match target {
                None => diagnostics.push(Diagnostic::new(
                    "refs",
                    format!("reference target `{reference}` does not exist"),
                )),
                Some((target_cove, target_kind))
                    if target_cove != source_cove && target_kind != "system" =>
                {
                    diagnostics.push(Diagnostic::new(
                        "refs",
                        format!("cross-cove reference `{reference}` is not schedulable"),
                    ));
                }
                Some(_) => {}
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

    // Freeze all persisted declaration columns before ceiling admission. A stale
    // in-flight declaration cannot consume capacity, and a terminal key can never
    // produce a new live row.
    let frozen: Vec<FrozenDeclarationRow> = sqlx::query_as(
        "SELECT status,key,kind,goal,context_json,acceptance_criteria,cwd,depends_on_json,priority,gate_json,declared_by,origin FROM tasks WHERE wave_id=?1 AND status!='pending'",
    )
    .bind(wave_id)
    .fetch_all(&mut *conn)
    .await?;
    let frozen_by_key: BTreeMap<_, _> =
        frozen.into_iter().map(|row| (row.1.clone(), row)).collect();
    for (declaration, verdict) in declarations.iter().zip(&mut verdicts) {
        let Some((
            status,
            _key,
            kind,
            goal,
            context,
            acceptance,
            cwd,
            depends,
            priority,
            gate,
            declared_by,
            origin,
        )) = frozen_by_key.get(&declaration.key)
        else {
            continue;
        };
        let legacy = origin == "legacy";
        let expected_context = declaration_context_json(declaration, legacy)?;
        let mut actual_depends: Vec<String> = serde_json::from_str(depends).unwrap_or_default();
        actual_depends.sort();
        actual_depends.dedup();
        let mut expected_depends = declaration.depends_on.clone();
        expected_depends.sort();
        expected_depends.dedup();
        let changed = kind != &declaration.kind
            || goal != &declaration.goal
            || !context_eq(context, &expected_context, declaration, legacy)
            || acceptance != &declaration.acceptance
            || cwd != &declaration.cwd
            || actual_depends != expected_depends
            || priority != &declaration.priority
            || !gate_eq(gate, &declaration.gate)?
            || declared_by != &declaration.declared_by;
        if changed || !matches!(status.as_str(), "dispatched" | "running" | "verifying") {
            verdict.diagnostics.push(Diagnostic::new(
                "key",
                if changed {
                    "task is already executing; declaration changes were not applied"
                } else {
                    "task key has already completed; declare a new key instead"
                },
            ));
            verdict.schedulable = false;
        }
        if !verdict.schedulable && matches!(status.as_str(), "dispatched" | "running" | "verifying")
        {
            verdict
                .diagnostics
                .push(withdrawal_diagnostic(&declaration.key, status));
        }
    }

    // A deleted block has no declaration to drive the loop above. Surface its
    // still-live projection row as a synthetic verdict so both read APIs retain
    // the §6.5 withdrawal diagnostic without changing their response shape.
    let declared_keys: BTreeSet<&str> = declarations.iter().map(|d| d.key.as_str()).collect();
    // Same rows the in-flight key list came from (one statement, one
    // version), narrowed to block-origin projection rows.
    for row in state.inflight.iter().filter(|r| r.origin == "block") {
        if !declared_keys.contains(row.key.as_str()) {
            verdicts.push(BlockVerdict {
                block_id: String::new(),
                diagnostics: vec![withdrawal_diagnostic(&row.key, &row.status)],
                key: row.key.clone(),
                schedulable: false,
            });
        }
    }

    // Pending rows are outputs, never inputs. Only clean declarations that can
    // produce a live row compete for remaining capacity.
    let mut candidates: Vec<usize> = verdicts
        .iter()
        .enumerate()
        .filter(|(i, verdict)| {
            verdict.schedulable
                && declarations[*i].declared_by == "spec"
                && !inflight_key_set.contains(declarations[*i].key.as_str())
                && !frozen_by_key.contains_key(&declarations[*i].key)
        })
        .map(|(i, _)| i)
        .collect();
    candidates.sort_by_key(|i| {
        (
            declarations[*i].block_index.unwrap_or(usize::MAX),
            declarations[*i].key.clone(),
        )
    });
    for index in candidates.into_iter().skip(capacity) {
        verdicts[index].diagnostics.push(Diagnostic::new(
            "key",
            format!("spec task ceiling of {ceiling} is reached"),
        ));
        verdicts[index].schedulable = false;
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
    let verdicts = evaluate_schedulability(tx, wave_id, declarations, block_local_diags).await?;
    let schedulable_by_key: BTreeMap<_, _> = verdicts
        .iter()
        .filter(|v| !v.key.is_empty())
        .map(|v| (v.key.clone(), v.schedulable))
        .collect();
    let existing: Vec<(String, String, String)> =
        sqlx::query_as("SELECT id,key,status FROM tasks WHERE wave_id=?1 AND origin='block'")
            .bind(wave_id)
            .fetch_all(&mut **tx)
            .await?;
    let mut verdicts = verdicts;
    let verdict_index_by_key: BTreeMap<_, _> = verdicts
        .iter()
        .enumerate()
        .filter(|(_, verdict)| !verdict.key.is_empty())
        .map(|(index, verdict)| (verdict.key.clone(), index))
        .collect();
    let mut changed = BTreeSet::new();
    for (id, key, status) in &existing {
        if !schedulable_by_key.get(key).is_some_and(|value| *value) {
            if matches!(status.as_str(), "dispatched" | "running" | "verifying") {
                let diagnostic = withdrawal_diagnostic(key, status);
                // Declaration-backed rows already received this diagnostic from
                // evaluate_schedulability; this branch only covers deleted blocks.
                if !verdict_index_by_key.contains_key(key) {
                    verdicts.push(BlockVerdict {
                        block_id: String::new(),
                        key: key.clone(),
                        diagnostics: vec![diagnostic],
                        schedulable: false,
                    });
                }
            } else if task_delete_pending_tx(tx, id).await? != 0 {
                changed.insert(key.clone());
            }
            // SQLite serializes writers, and this function already owns the write
            // transaction, so a pending row cannot be claimed between SELECT and
            // the guarded DELETE; a zero-row DELETE needs no race diagnostic here.
        }
    }
    let now = now_ms();
    for (declaration, verdict) in declarations.iter().zip(&verdicts) {
        if verdict.schedulable && !declaration.tombstone {
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

#[cfg(test)]
mod tests {
    use super::*;
    use calm_types::report_blocks::tasks::TaskDeclaration;
    use serde_json::json;

    fn declaration(index: usize, key: &str) -> TaskDeclaration {
        TaskDeclaration {
            block_index: Some(index),
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
