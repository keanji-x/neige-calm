use std::collections::{BTreeMap, BTreeSet};

use calm_types::event::{Event, EventScope, TaskContextChangedRef, TaskContextRef};
use calm_types::ids::{ActorId, WaveId};
use calm_types::report_blocks::tasks::{
    Diagnostic, GateInput, TASK_BLOCKING_DIAGNOSTIC_PATHS, TaskDeclaration, diagnostic_args,
    gate_rule_violations, json_eq, opt_json_eq, task_diagnostic_action, unknown_deps,
};
use calm_types::report_links::{parse_destination, scan_links};
use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, SqliteConnection, Transaction};
use utoipa::ToSchema;

use super::wave_tree::{DEFAULT_SPEC_TASK_CEILING, WaveTreeTerm, effective_limit};
use crate::error::{CalmError, Result};
use crate::model::now_ms;

/// Persisted task columns compared for in-flight declaration drift. `refs` is
/// resolved through the frozen context/index rather than stored on `tasks`;
/// `no_gate_reason` is folded into legacy context and otherwise only affects
/// gate validation, so neither belongs to this direct column comparison.
pub const PROJECTION_DRIFT_TASK_FIELDS: &[&str] = &[
    "kind",
    "goal",
    "context",
    "acceptance",
    "cwd",
    "depends_on",
    "gate",
];

fn declaration_field_changed(
    field: &str,
    row: &FrozenDeclarationRow,
    declaration: &TaskDeclaration,
    expected_context: &str,
    legacy: bool,
) -> Result<bool> {
    let (_, _, kind, goal, context, acceptance, cwd, depends, _, gate, _, _, _, _) = row;
    Ok(match field {
        "kind" => kind != &declaration.kind,
        "goal" => goal != &declaration.goal,
        "context" => !context_eq(context, expected_context, declaration, legacy),
        "acceptance" => acceptance != &declaration.acceptance,
        "cwd" => cwd != &declaration.cwd,
        "depends_on" => {
            let mut actual: Vec<String> = serde_json::from_str(depends).unwrap_or_default();
            actual.sort();
            actual.dedup();
            let mut expected = declaration.depends_on.clone();
            expected.sort();
            expected.dedup();
            actual != expected
        }
        "gate" => !gate_eq(gate, &declaration.gate)?,
        unknown => {
            return Err(CalmError::Internal(format!(
                "unknown projection drift field: {unknown}"
            )));
        }
    })
}
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
    i64,
    i64,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum WithdrawalEdge {
    Ready,
    ReleasedByUser,
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_card_id: Option<String>,
    /// Written after claim, so exposing it preserves the #1030 read-state
    /// exception. `spawn` must never be added beside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_wave_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_wave_deleted: Option<bool>,
    #[serde(skip)]
    #[schema(ignore)]
    pub withdrawal: Option<WithdrawalEdge>,
}

#[derive(Deserialize)]
struct TaskReadState {
    key: String,
    status: String,
    gate_result_json: Option<String>,
    worker_card_id: Option<String>,
    child_wave_id: Option<String>,
    child_wave_deleted: i64,
    context_stale_at_ms: Option<i64>,
    context_closure_truncated: i64,
    claim_context_json: Option<String>,
}

/// Attach the task-table state carried by `wave_projection_state`'s single
/// statement. This deliberately stays beside the projection query instead of
/// introducing a live block data-source abstraction.
fn attach_task_read_state(rows: &[TaskReadState], wave_id: &str, verdicts: &mut [BlockVerdict]) {
    let by_key: BTreeMap<_, _> = rows.iter().map(|row| (row.key.as_str(), row)).collect();
    for verdict in verdicts {
        let Some(row) = by_key.get(verdict.key.as_str()) else {
            continue;
        };
        verdict.status = Some(row.status.clone());
        verdict.gate_result = row
            .gate_result_json
            .as_deref()
            .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
            .and_then(|gate| {
                let passed = gate.get("passed")?.as_bool()?;
                let mut public = serde_json::Map::from_iter([(
                    "passed".into(),
                    serde_json::Value::Bool(passed),
                )]);
                if let Some(step) = gate.get("failing_step").and_then(serde_json::Value::as_str) {
                    public.insert(
                        "failing_step".into(),
                        serde_json::Value::String(step.into()),
                    );
                }
                Some(serde_json::Value::Object(public))
            });
        verdict.worker_card_id = row.worker_card_id.clone();
        verdict.child_wave_id = row.child_wave_id.clone();
        verdict.child_wave_deleted = row
            .child_wave_id
            .as_ref()
            .map(|_| row.child_wave_deleted != 0);
        let related_context_blocks = || {
            let grouped = row
                .claim_context_json
                .as_deref()
                .and_then(|json| serde_json::from_str::<Vec<TaskContextRef>>(json).ok())
                .unwrap_or_default()
                .into_iter()
                .filter(|item| !item.is_root)
                .fold(
                    BTreeMap::<String, Vec<String>>::new(),
                    |mut grouped, item| {
                        grouped
                            .entry(item.wave_id.to_string())
                            .or_default()
                            .push(item.block_id);
                        grouped
                    },
                );
            if grouped.is_empty() {
                return vec![(None, vec![])];
            }
            grouped
                .into_iter()
                .map(|(related_wave_id, block_ids)| {
                    let related_wave_id = (related_wave_id != wave_id).then_some(related_wave_id);
                    (related_wave_id, block_ids)
                })
                .collect::<Vec<_>>()
        };
        if row.context_closure_truncated != 0 {
            verdict
                .diagnostics
                .extend(related_context_blocks().into_iter().map(
                    |(related_wave_id, related_block_ids)| {
                        Diagnostic::coded(
                            "reference_chain_too_large",
                            "refs",
                            BTreeMap::new(),
                            related_block_ids,
                            related_wave_id,
                            Some("narrow_task_context".into()),
                        )
                    },
                ));
        }
        let already_declaration_stale = verdict.diagnostics.iter().any(|diagnostic| {
            matches!(
                diagnostic.code.as_str(),
                "context_stale_declaration" | "declaration_changed_in_flight"
            )
        });
        if row.context_stale_at_ms.is_some() && !already_declaration_stale {
            verdict
                .diagnostics
                .extend(related_context_blocks().into_iter().map(
                    |(related_wave_id, related_block_ids)| {
                        Diagnostic::coded(
                            "context_stale_reference",
                            "refs",
                            BTreeMap::new(),
                            related_block_ids,
                            related_wave_id,
                            Some("relink_reference".into()),
                        )
                    },
                ));
        }
        if !verdict.diagnostics.is_empty() {
            verdict.schedulable = false;
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TaskProjectionOutcome {
    pub changed_keys: Vec<String>,
    pub diagnostics: Vec<BlockVerdict>,
    pub kernel_events: Vec<(ActorId, EventScope, Event)>,
    /// Recursive tree statements used to obtain this projection's tree term.
    /// Whole-tree callers use this countable seam to prevent an accidental
    /// return to one full member walk per projected wave.
    pub tree_cte_queries: u32,
}

/// Single-winner material verdict. Callers that commit this transaction must
/// merge the returned kernel events into that same eventized write.
pub async fn mark_context_material_tx(
    tx: &mut Transaction<'_, Sqlite>,
    task_id: &str,
    wave_id: &str,
    changed_refs: Vec<TaskContextChangedRef>,
    rationale: &str,
) -> Result<Vec<(ActorId, EventScope, Event)>> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT t.key,w.cove_id FROM tasks t JOIN waves w ON w.id=t.wave_id WHERE t.id=?1 AND t.wave_id=?2",
    )
    .bind(task_id)
    .bind(wave_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((task_key, cove_id)) = row else {
        tracing::warn!(
            task_id,
            wave_id,
            "context material verdict lost because its task row disappeared"
        );
        return Ok(Vec::new());
    };
    let changed = sqlx::query(
        "UPDATE tasks SET context_stale_at_ms=?1,context_verify_failures=0 WHERE id=?2 AND status IN ('dispatched','running','verifying') AND context_stale_at_ms IS NULL",
    )
    .bind(now_ms())
    .bind(task_id)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        == 1;
    if !changed {
        return Ok(Vec::new());
    }
    Ok(vec![(
        ActorId::Kernel,
        EventScope::Wave {
            wave: WaveId::from(wave_id),
            cove: cove_id.into(),
        },
        Event::TaskContextAdvanced {
            wave_id: WaveId::from(wave_id),
            task_key,
            task_id: task_id.into(),
            changed_refs,
            verdict: "material".into(),
            rationale: rationale.into(),
        },
    )])
}

fn withdrawal_diagnostic(key: &str, status: &str) -> Diagnostic {
    Diagnostic::coded(
        "context_stale_declaration",
        "key",
        diagnostic_args([
            ("key", serde_json::Value::String(key.into())),
            ("status", serde_json::Value::String(status.into())),
        ]),
        vec![],
        None,
        Some("open_worker_output".into()),
    )
}

fn reference_diagnostic(code: &str, reference: &str, wave_id: Option<String>) -> Diagnostic {
    let related_block_ids = parse_destination(reference)
        .and_then(|(_, block)| block)
        .into_iter()
        .collect();
    Diagnostic::coded(
        code,
        "refs",
        diagnostic_args([("reference", serde_json::Value::String(reference.into()))]),
        related_block_ids,
        wave_id,
        Some("relink_reference".into()),
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
    /// Non-terminal spec rows which projection cannot treat as its own
    /// replaceable `origin='block' AND status='pending'` output. Migration
    /// 0068 legacy rows live here, including legacy pending rows: they must
    /// consume tree share but must never be culled to make room.
    non_block_live_spec: i64,
    task_read_state: Vec<TaskReadState>,
}

#[derive(sqlx::FromRow)]
struct WaveProjectionStateRow {
    automation_policy: Option<String>,
    spec_task_ceiling: Option<i64>,
    require_task_gates: i64,
    cove_id: String,
    inflight_json: String,
    non_block_live_spec: i64,
    task_read_state_json: String,
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
/// The in-flight key set, block-origin ceiling occupancy, orphan candidates,
/// and non-block live spec occupancy are captured by the same statement. The
/// last count is deliberately separate: legacy rows consume TREE share while
/// the per-wave ceiling retains its approved block-only predicate.
async fn wave_projection_state(
    conn: &mut SqliteConnection,
    wave_id: &str,
    include_read_state: bool,
) -> Result<WaveProjectionState> {
    let sql = if include_read_state {
        r#"SELECT w.automation_policy, w.spec_task_ceiling, w.require_task_gates, w.cove_id,
                  (SELECT json_group_array(json_object(
                       'key', t.key, 'status', t.status,
                       'declared_by', t.declared_by, 'origin', t.origin))
                   FROM tasks t
                   WHERE t.wave_id = w.id
                      AND t.status IN ('dispatched','running','verifying')) AS inflight_json,
                   (SELECT count(*) FROM tasks t
                    WHERE t.wave_id = w.id AND t.declared_by = 'spec'
                      AND t.origin != 'block'
                      AND t.status NOT IN ('done','failed','canceled')) AS non_block_live_spec,
                   (SELECT json_group_array(json_object(
                       'key', t.key, 'status', t.status,
                       'gate_result_json', t.gate_result_json,
                       'worker_card_id', t.worker_card_id,
                       'child_wave_id', t.child_wave_id,
                       'child_wave_deleted', CASE WHEN t.child_wave_id IS NOT NULL
                         AND NOT EXISTS(SELECT 1 FROM waves child WHERE child.id=t.child_wave_id)
                         THEN 1 ELSE 0 END,
                       'context_stale_at_ms', t.context_stale_at_ms,
                       'context_closure_truncated', t.context_closure_truncated,
                       'claim_context_json', t.claim_context_json))
                   FROM tasks t WHERE t.wave_id = w.id) AS task_read_state_json
           FROM waves w WHERE w.id = ?1"#
    } else {
        r#"SELECT w.automation_policy, w.spec_task_ceiling, w.require_task_gates, w.cove_id,
                  (SELECT json_group_array(json_object(
                       'key', t.key, 'status', t.status,
                       'declared_by', t.declared_by, 'origin', t.origin))
                   FROM tasks t
                   WHERE t.wave_id = w.id
                      AND t.status IN ('dispatched','running','verifying')) AS inflight_json,
                   (SELECT count(*) FROM tasks t
                    WHERE t.wave_id = w.id AND t.declared_by = 'spec'
                      AND t.origin != 'block'
                      AND t.status NOT IN ('done','failed','canceled')) AS non_block_live_spec,
                   '[]' AS task_read_state_json
           FROM waves w WHERE w.id = ?1"#
    };
    let row: Option<WaveProjectionStateRow> = sqlx::query_as(sql)
        .bind(wave_id)
        .fetch_optional(&mut *conn)
        .await?;
    let row = row.ok_or_else(|| CalmError::NotFound(format!("wave {wave_id}")))?;
    Ok(WaveProjectionState {
        policy: row.automation_policy,
        ceiling: effective_limit(row.spec_task_ceiling, DEFAULT_SPEC_TASK_CEILING),
        require_gates: row.require_task_gates != 0,
        source_cove: row.cove_id,
        inflight: serde_json::from_str(&row.inflight_json)?,
        non_block_live_spec: row.non_block_live_spec,
        task_read_state: serde_json::from_str(&row.task_read_state_json)?,
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
/// Every declaration of a wave whose tree root cannot be resolved is
/// unschedulable, with a diagnostic that says so. Deliberately NOT "skip the
/// tree term when there is no tree": one broken link would then exempt an
/// entire subtree from the bound.
fn tree_root_unresolved_diagnostic() -> Diagnostic {
    Diagnostic::coded(
        "tree_root_unresolved",
        "key",
        BTreeMap::new(),
        vec![],
        None,
        Some("repair_wave_tree".into()),
    )
}

pub async fn evaluate_schedulability(
    conn: &mut SqliteConnection,
    wave_id: &str,
    declarations: &[TaskDeclaration],
    block_local_diags: &[Vec<Diagnostic>],
    include_read_state: bool,
) -> Result<Vec<BlockVerdict>> {
    let tree = super::wave_tree::wave_tree_term(&mut *conn, wave_id).await?;
    evaluate_schedulability_with_tree_term(
        conn,
        wave_id,
        declarations,
        block_local_diags,
        include_read_state,
        tree.term,
    )
    .await
}

async fn evaluate_schedulability_with_tree_term(
    conn: &mut SqliteConnection,
    wave_id: &str,
    declarations: &[TaskDeclaration],
    block_local_diags: &[Vec<Diagnostic>],
    include_read_state: bool,
    tree_term: WaveTreeTerm,
) -> Result<Vec<BlockVerdict>> {
    let state = wave_projection_state(&mut *conn, wave_id, include_read_state).await?;
    let configured_policy = state.policy;
    // #985 slice 6 PR-B — the tree term. `effective_ceiling = min(ceiling,
    // share)` where `share` is this wave's deterministic slice of the root's
    // `tree_task_budget`, split over the tree's waves in `(created_at, id)`
    // order. Its quota is a function of tree SHAPE, never sibling projection
    // output. Within this wave, immutable occupancy (block in-flight plus
    // non-block live spec rows) is subtracted and block pending rows re-enter
    // as ordered candidates. That keeps "rebuild ≡ incremental" (D.1 #11)
    // true while making upgrade legacy rows consume share without culling
    // them. A shared sibling count would instead be first-come-first-served,
    // path-dependent, and not reconstructible by a rebuild.
    let ceiling = state.ceiling;
    let (tree_share, tree_root_unresolved) = match &tree_term {
        WaveTreeTerm::NotInTree => (None, false),
        WaveTreeTerm::RootUnresolved => {
            // Fail closed. A broken parent link, a cycle, or an over-deep chain
            // means we cannot name the budget this wave draws from; treating
            // "no resolvable tree" as "no tree constraint" would leave a whole
            // subtree unbounded, which is the one outcome the tree bound exists
            // to prevent.
            (None, true)
        }
        WaveTreeTerm::Share(share) => (Some(share.clone()), false),
    };
    let require_gates = state.require_gates;
    let source_cove = state.source_cove;
    let effective_wait = configured_policy.as_deref() == Some("declare-and-wait");
    // unknown_deps knows every in-flight key in the wave, including rows
    // backfilled as legacy.  Ceiling occupancy is intentionally narrower.
    let inflight_keys: Vec<String> = state.inflight.iter().map(|r| r.key.clone()).collect();
    let inflight_key_set: BTreeSet<&str> = inflight_keys.iter().map(String::as_str).collect();
    let ceiling_occupied: i64 = state
        .inflight
        .iter()
        .filter(|r| r.declared_by == "spec" && r.origin == "block")
        .count() as i64;
    let ceiling_capacity = ceiling.saturating_sub(ceiling_occupied).max(0);
    // Block pending rows are projection output and re-enter below as
    // candidates. Everything else is fixed occupancy for this write: all
    // block in-flight rows plus every non-terminal non-block spec row. In
    // particular, 0068 legacy rows consume share without becoming cullable.
    let tree_occupied = ceiling_occupied.saturating_add(state.non_block_live_spec);
    let tree_capacity = tree_share.as_ref().map_or(i64::MAX, |share| {
        share.share.saturating_sub(tree_occupied).max(0)
    });
    let capacity = if tree_root_unresolved
        || tree_share
            .as_ref()
            .is_some_and(|share| share.admission_frozen)
    {
        0
    } else {
        ceiling_capacity.min(tree_capacity) as usize
    };

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
        if tree_root_unresolved {
            diagnostics.push(tree_root_unresolved_diagnostic());
        }
        for (_, dependency) in unknown.iter().filter(|(key, _)| key == &declaration.key) {
            diagnostics.push(Diagnostic::coded(
                "unknown_dependency",
                "depends_on",
                diagnostic_args([("dependency", serde_json::Value::String(dependency.clone()))]),
                vec![],
                None,
                Some("edit_dependencies".into()),
            ));
        }
        if gate_bad.contains(&declaration.key) {
            diagnostics.push(Diagnostic::coded(
                "gate_required",
                "gate",
                BTreeMap::new(),
                vec![],
                None,
                Some("add_gate_or_reason".into()),
            ));
        }
        let mut references = declaration.refs.clone();
        for text in [
            Some(declaration.goal.as_str()),
            declaration.acceptance.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            references.extend(scan_links(text).links.into_iter().filter_map(|link| {
                link.dst_block_id
                    .map(|block| format!("neige://wave/{}#{block}", link.dst_wave_id))
            }));
        }
        references.sort();
        references.dedup();
        for reference in &references {
            let destination = parse_destination(reference);
            let destination_wave = destination.as_ref().map(|(wave, _)| wave.clone());
            let target: Option<(String, String, i64)> = if let Some((dst_wave, dst_block)) =
                destination
            {
                let Some(dst_block) = dst_block else {
                    diagnostics.push(reference_diagnostic(
                        "reference_needs_block",
                        reference,
                        Some(dst_wave),
                    ));
                    continue;
                };
                sqlx::query_as(
                    "SELECT w.cove_id,c.kind,EXISTS(SELECT 1 FROM cards card JOIN json_each(card.payload,'$.blocks') block WHERE card.wave_id=w.id AND card.kind='wave-report' AND json_extract(block.value,'$.id')=?2) FROM waves w JOIN coves c ON c.id=w.cove_id WHERE w.id=?1",
                )
                    .bind(dst_wave).bind(dst_block).fetch_optional(&mut *conn).await?
            } else if let Some(card_id) = reference
                .strip_prefix("neige://card/")
                .filter(|id| !id.is_empty() && !id.contains('/'))
            {
                sqlx::query_as("SELECT w.cove_id,c.kind,1 FROM cards card JOIN waves w ON w.id=card.wave_id JOIN coves c ON c.id=w.cove_id WHERE card.id=?1")
                    .bind(card_id).fetch_optional(&mut *conn).await?
            } else {
                continue;
            };
            match target {
                None => diagnostics.push(reference_diagnostic(
                    "reference_missing",
                    reference,
                    destination_wave.clone(),
                )),
                Some((target_cove, target_kind, _))
                    if target_cove != source_cove && target_kind != "system" =>
                {
                    diagnostics.push(reference_diagnostic(
                        "reference_cross_cove",
                        reference,
                        destination_wave.clone(),
                    ));
                }
                Some((_, _, 0)) => diagnostics.push(reference_diagnostic(
                    "reference_missing",
                    reference,
                    destination_wave,
                )),
                Some(_) => {}
            }
        }
        if effective_wait
            && declaration.declared_by == "spec"
            && !declaration.released_by_user
            && !declaration.tombstone
        {
            diagnostics.push(Diagnostic::coded(
                "declare_and_wait",
                "released_by_user",
                BTreeMap::new(),
                vec![],
                None,
                Some("release_task".into()),
            ));
        }
        verdicts.push(BlockVerdict {
            block_id: declaration.block_id.clone(),
            key: declaration.key.clone(),
            schedulable: declaration.ready && !declaration.tombstone && diagnostics.is_empty(),
            status: None,
            gate_result: None,
            worker_card_id: None,
            child_wave_id: None,
            child_wave_deleted: None,
            diagnostics,
            withdrawal: None,
        });
    }

    // Freeze all persisted declaration columns before ceiling admission. A stale
    // in-flight declaration cannot consume capacity, and a terminal key can never
    // produce a new live row.
    let frozen: Vec<FrozenDeclarationRow> = sqlx::query_as(
        "SELECT status,key,kind,goal,context_json,acceptance_criteria,cwd,depends_on_json,priority,gate_json,declared_by,origin,decl_ready,decl_released_by_user FROM tasks WHERE wave_id=?1 AND status!='pending'",
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
            _kind,
            _goal,
            _context,
            _acceptance,
            _cwd,
            _depends,
            _priority,
            _gate,
            _declared_by,
            origin,
            decl_ready,
            decl_released_by_user,
        )) = frozen_by_key.get(&declaration.key)
        else {
            continue;
        };
        let legacy = origin == "legacy";
        let expected_context = declaration_context_json(declaration, legacy)?;
        let row = frozen_by_key.get(&declaration.key).expect("frozen row");
        let changed = PROJECTION_DRIFT_TASK_FIELDS
            .iter()
            .try_fold(false, |changed, field| {
                Ok::<_, CalmError>(
                    changed
                        || declaration_field_changed(
                            field,
                            row,
                            declaration,
                            &expected_context,
                            legacy,
                        )?,
                )
            })?;
        verdict.withdrawal = if *decl_ready == 1 && !declaration.ready {
            Some(WithdrawalEdge::Ready)
        } else if *decl_released_by_user == 1 && !declaration.released_by_user && effective_wait {
            Some(WithdrawalEdge::ReleasedByUser)
        } else {
            None
        };
        if changed || !matches!(status.as_str(), "dispatched" | "running" | "verifying") {
            verdict.diagnostics.push(Diagnostic::coded(
                if changed {
                    "declaration_changed_in_flight"
                } else {
                    "task_key_completed"
                },
                "key",
                BTreeMap::new(),
                vec![],
                None,
                Some(
                    if changed {
                        "open_worker_output"
                    } else {
                        "create_task_with_new_key"
                    }
                    .into(),
                ),
            ));
            verdict.schedulable = false;
        }
        let stale_causing_diagnostic = changed
            || verdict.withdrawal.is_some()
            || verdict.diagnostics.iter().any(|diagnostic| {
                TASK_BLOCKING_DIAGNOSTIC_PATHS.contains(&diagnostic.path.as_str())
            });
        if stale_causing_diagnostic
            && matches!(status.as_str(), "dispatched" | "running" | "verifying")
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
                status: Some(row.status.clone()),
                gate_result: None,
                worker_card_id: None,
                child_wave_id: None,
                child_wave_deleted: None,
                withdrawal: None,
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
    let admitted: Vec<usize> = candidates.iter().copied().take(capacity).collect();
    let admitted_ids: Vec<String> = declarations
        .iter()
        .enumerate()
        .filter(|(index, declaration)| {
            admitted.contains(index) || inflight_key_set.contains(declaration.key.as_str())
        })
        .map(|(_, declaration)| declaration.block_id.clone())
        .collect();
    // Attribution matters here (§12.2 C: every diagnostic owes the reader a
    // plain sentence AND a next action). When the binding constraint is the
    // tree's shared budget rather than this wave's own ceiling, saying
    // "spec task ceiling reached" sends the reader to raise a ceiling that is
    // not what stopped them — the cause lives in a DIFFERENT wave's budget.
    let tree_bound = tree_share
        .as_ref()
        .filter(|share| share.admission_frozen || tree_capacity <= ceiling_capacity)
        .cloned();
    for index in candidates.into_iter().skip(capacity) {
        let diagnostic = match &tree_bound {
            Some(share) => Diagnostic::coded(
                "tree_budget_exhausted",
                "key",
                diagnostic_args([
                    (
                        "root_wave_id",
                        serde_json::Value::String(share.root_id.clone()),
                    ),
                    ("tree_task_budget", serde_json::Value::from(share.budget)),
                    ("tree_waves", serde_json::Value::from(share.members)),
                    ("share", serde_json::Value::from(share.share)),
                    ("ceiling", serde_json::Value::from(ceiling)),
                    ("occupied", serde_json::Value::from(tree_occupied)),
                    (
                        "admission_order",
                        serde_json::Value::String("document order, then key".into()),
                    ),
                ]),
                admitted_ids.clone(),
                Some(share.root_id.clone()),
                task_diagnostic_action("tree_budget_exhausted").map(str::to_owned),
            ),
            None => Diagnostic::coded(
                "spec_task_ceiling",
                "key",
                diagnostic_args([
                    ("ceiling", serde_json::Value::from(ceiling)),
                    ("occupied", serde_json::Value::from(ceiling_occupied)),
                    (
                        "admission_order",
                        serde_json::Value::String("document order, then key".into()),
                    ),
                ]),
                admitted_ids.clone(),
                Some(wave_id.into()),
                task_diagnostic_action("spec_task_ceiling").map(str::to_owned),
            ),
        };
        verdicts[index].diagnostics.push(diagnostic);
        verdicts[index].schedulable = false;
    }
    if include_read_state {
        attach_task_read_state(&state.task_read_state, wave_id, &mut verdicts);
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
    let tree = super::wave_tree::wave_tree_term(tx, wave_id).await?;
    let tree_cte_queries = tree.tree_cte_queries;
    let verdicts = evaluate_schedulability_with_tree_term(
        tx,
        wave_id,
        declarations,
        block_local_diags,
        false,
        tree.term,
    )
    .await?;
    project_tasks_from_verdicts_tx(tx, wave_id, declarations, verdicts, tree_cte_queries).await
}

/// Projection entry point for a whole-tree rebuild. The caller enumerates the
/// tree once and passes each deterministic share here, avoiding one recursive
/// member walk per wave.
pub async fn project_tasks_with_tree_term_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &str,
    declarations: &[TaskDeclaration],
    block_local_diags: &[Vec<Diagnostic>],
    tree_term: WaveTreeTerm,
) -> Result<TaskProjectionOutcome> {
    let verdicts = evaluate_schedulability_with_tree_term(
        tx,
        wave_id,
        declarations,
        block_local_diags,
        false,
        tree_term,
    )
    .await?;
    project_tasks_from_verdicts_tx(tx, wave_id, declarations, verdicts, 0).await
}

async fn project_tasks_from_verdicts_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &str,
    declarations: &[TaskDeclaration],
    verdicts: Vec<BlockVerdict>,
    tree_cte_queries: u32,
) -> Result<TaskProjectionOutcome> {
    let schedulable_by_key: BTreeMap<_, _> = verdicts
        .iter()
        .filter(|v| !v.key.is_empty())
        .map(|v| (v.key.clone(), v.schedulable))
        .collect();
    let existing: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT id,key,status,claim_context_json FROM tasks WHERE wave_id=?1 AND origin='block'",
    )
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
    let declaration_by_key: BTreeMap<_, _> = declarations
        .iter()
        .map(|declaration| (declaration.key.as_str(), declaration))
        .collect();
    let mut changed = BTreeSet::new();
    let mut kernel_events = Vec::new();
    for (id, key, status, claim_context_json) in &existing {
        let withdrawal_edge = verdict_index_by_key
            .get(key)
            .and_then(|index| verdicts[*index].withdrawal);
        let withdrawal = withdrawal_edge.is_some();
        if !schedulable_by_key.get(key).is_some_and(|value| *value) || withdrawal {
            if matches!(status.as_str(), "dispatched" | "running" | "verifying") {
                let declaration_removed = declaration_by_key
                    .get(key.as_str())
                    .is_none_or(|declaration| declaration.tombstone);
                if withdrawal || declaration_removed {
                    // Withdrawal is a declaration edge, not a content change. Keep
                    // the hash-typed changed_refs clean and describe the edge in the
                    // rationale instead.
                    let mut changed_refs = Vec::new();
                    if declaration_removed {
                        changed_refs.extend(
                            claim_context_json
                                .as_deref()
                                .and_then(|json| {
                                    serde_json::from_str::<Vec<TaskContextRef>>(json).ok()
                                })
                                .unwrap_or_default()
                                .into_iter()
                                .filter(|frozen| frozen.is_root)
                                .map(|frozen| TaskContextChangedRef {
                                    wave_id: frozen.wave_id,
                                    block_id: frozen.block_id,
                                    from_rev: frozen.rev,
                                    from_hash: frozen.hash,
                                    ..Default::default()
                                }),
                        );
                    }
                    kernel_events.extend(
                        mark_context_material_tx(
                            tx,
                            id,
                            wave_id,
                            changed_refs,
                            match withdrawal_edge {
                                Some(WithdrawalEdge::Ready) => {
                                    "task declaration ready was withdrawn"
                                }
                                Some(WithdrawalEdge::ReleasedByUser) => {
                                    "task declaration user release was withdrawn"
                                }
                                None => "task declaration block was removed",
                            },
                        )
                        .await?,
                    );
                }
                let diagnostic = withdrawal_diagnostic(key, status);
                // Declaration-backed rows already received this diagnostic from
                // evaluate_schedulability; this branch only covers deleted blocks.
                if !verdict_index_by_key.contains_key(key) {
                    verdicts.push(BlockVerdict {
                        block_id: String::new(),
                        key: key.clone(),
                        diagnostics: vec![diagnostic],
                        schedulable: false,
                        status: Some(status.clone()),
                        gate_result: None,
                        worker_card_id: None,
                        child_wave_id: None,
                        child_wave_deleted: None,
                        withdrawal: None,
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
                "UPDATE tasks SET origin='block',decl_ready=1,updated_at_ms=?1 WHERE wave_id=?2 AND key=?3 AND origin='legacy' AND status!='pending'",
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
            r#"INSERT INTO tasks(
                   id,wave_id,key,kind,goal,context_json,acceptance_criteria,cwd,
                   depends_on_json,priority,gate_json,status,declared_by,spawn,origin,
                   decl_ready,decl_released_by_user,created_at_ms,updated_at_ms
               ) VALUES(
                   ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'pending',?12,?13,'block',
                   ?14,?15,?16,?16
               )
               ON CONFLICT(wave_id,key) DO UPDATE SET
                   kind=excluded.kind,
                   goal=excluded.goal,
                   context_json=excluded.context_json,
                   acceptance_criteria=excluded.acceptance_criteria,
                   cwd=excluded.cwd,
                   depends_on_json=excluded.depends_on_json,
                   priority=excluded.priority,
                   gate_json=excluded.gate_json,
                   declared_by=excluded.declared_by,
                   spawn=excluded.spawn,
                   origin='block',
                   decl_ready=excluded.decl_ready,
                   decl_released_by_user=excluded.decl_released_by_user,
                   updated_at_ms=excluded.updated_at_ms
               WHERE tasks.status='pending'
                 AND (tasks.origin='block' OR tasks.origin='legacy')
                 AND (
                     tasks.kind IS NOT excluded.kind
                     OR tasks.goal IS NOT excluded.goal
                     OR tasks.context_json IS NOT excluded.context_json
                     OR tasks.acceptance_criteria IS NOT excluded.acceptance_criteria
                     OR tasks.cwd IS NOT excluded.cwd
                     OR tasks.depends_on_json IS NOT excluded.depends_on_json
                     OR tasks.priority IS NOT excluded.priority
                     OR tasks.gate_json IS NOT excluded.gate_json
                     OR tasks.declared_by IS NOT excluded.declared_by
                     OR tasks.spawn IS NOT excluded.spawn
                     OR tasks.origin IS NOT 'block'
                     OR tasks.decl_ready IS NOT excluded.decl_ready
                     OR tasks.decl_released_by_user IS NOT excluded.decl_released_by_user
                 )"#,
        )
        .bind(&id)
        .bind(wave_id)
        .bind(&declaration.key)
        .bind(&declaration.kind)
        .bind(&declaration.goal)
        .bind(context)
        .bind(&declaration.acceptance)
        .bind(&declaration.cwd)
        .bind(depends)
        .bind(declaration.priority)
        .bind(gate)
        .bind(&declaration.declared_by)
        .bind(&declaration.spawn)
        .bind(i64::from(declaration.ready))
        .bind(i64::from(declaration.released_by_user))
        .bind(now)
        .execute(&mut **tx)
        .await?;
        if result.rows_affected() != 0 {
            changed.insert(declaration.key.clone());
        }
    }
    Ok(TaskProjectionOutcome {
        changed_keys: changed.into_iter().collect(),
        diagnostics: verdicts,
        kernel_events,
        tree_cte_queries,
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
            spawn: "in-wave".into(),
            tombstoned_by: None,
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
    async fn read_state_surfaces_truncated_and_stale_reference_diagnostics() {
        let (repo, wave) = setup().await;
        insert_block_task(&repo, &wave, "read-state", "pending").await;
        let claim_context = json!([
            {"wave_id": wave, "block_id": "b_0000", "rev": 1, "hash": "root", "is_root": true},
            {"wave_id": wave, "block_id": "b_feed", "rev": 2, "hash": "ref", "is_root": false}
        ]);
        sqlx::query("UPDATE tasks SET context_closure_truncated=1,context_stale_at_ms=42,claim_context_json=?1,gate_result_json=?2,worker_card_id='worker-output' WHERE wave_id=?3 AND key='read-state'")
            .bind(claim_context.to_string())
            .bind(json!({"passed":false,"failing_step":"test","log_path":"/private/gate.log","log_tail":"raw output","attempt":7}).to_string())
            .bind(&wave)
            .execute(&repo.pool)
            .await
            .unwrap();

        let mut tx = repo.pool.begin().await.unwrap();
        let verdicts = evaluate_schedulability(
            &mut tx,
            &wave,
            &[declaration(0, "read-state")],
            &[vec![]],
            true,
        )
        .await
        .unwrap();

        let verdict = &verdicts[0];
        assert!(!verdict.schedulable);
        assert_eq!(
            verdict.gate_result,
            Some(json!({"passed": false, "failing_step": "test"}))
        );
        assert_eq!(verdict.worker_card_id.as_deref(), Some("worker-output"));
        for code in ["reference_chain_too_large", "context_stale_reference"] {
            let diagnostic = verdict
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.code == code)
                .unwrap_or_else(|| panic!("missing {code}"));
            assert_eq!(diagnostic.related_block_ids, ["b_feed"]);
        }
    }

    #[tokio::test]
    async fn read_state_root_only_context_remains_fail_closed() {
        let (repo, wave) = setup().await;
        insert_block_task(&repo, &wave, "root-only", "pending").await;
        let claim_context = json!([
            {"wave_id": wave, "block_id": "b_0000", "rev": 1, "hash": "root", "is_root": true}
        ]);
        sqlx::query("UPDATE tasks SET context_closure_truncated=1,context_stale_at_ms=42,claim_context_json=?1 WHERE wave_id=?2 AND key='root-only'")
            .bind(claim_context.to_string())
            .bind(&wave)
            .execute(&repo.pool)
            .await
            .unwrap();

        let mut tx = repo.pool.begin().await.unwrap();
        let verdicts = evaluate_schedulability(
            &mut tx,
            &wave,
            &[declaration(0, "root-only")],
            &[vec![]],
            true,
        )
        .await
        .unwrap();

        let verdict = &verdicts[0];
        assert!(!verdict.schedulable);
        for code in ["reference_chain_too_large", "context_stale_reference"] {
            let diagnostic = verdict
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.code == code)
                .unwrap_or_else(|| panic!("missing {code}"));
            assert!(diagnostic.related_block_ids.is_empty());
            assert!(diagnostic.related_wave_id.is_none());
        }
    }

    #[tokio::test]
    async fn declare_and_wait_diagnostic_does_not_link_to_its_own_wave() {
        let (repo, wave) = setup().await;
        sqlx::query("UPDATE waves SET automation_policy='declare-and-wait' WHERE id=?1")
            .bind(&wave)
            .execute(&repo.pool)
            .await
            .unwrap();

        let mut tx = repo.pool.begin().await.unwrap();
        let verdicts = evaluate_schedulability(
            &mut tx,
            &wave,
            &[declaration(0, "approval-needed")],
            &[vec![]],
            true,
        )
        .await
        .unwrap();
        let diagnostic = verdicts[0]
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "declare_and_wait")
            .expect("declare-and-wait diagnostic");
        assert!(diagnostic.related_wave_id.is_none());
        assert!(diagnostic.related_block_ids.is_empty());
    }

    #[tokio::test]
    async fn read_state_groups_related_context_links_by_wave() {
        let (repo, wave) = setup().await;
        insert_block_task(&repo, &wave, "cross-wave", "pending").await;
        let other_wave = "wave_remote".to_owned();
        let claim_context = json!([
            {"wave_id": wave, "block_id": "b_root", "rev": 1, "hash": "root", "is_root": true},
            {"wave_id": wave, "block_id": "b_local", "rev": 2, "hash": "local", "is_root": false},
            {"wave_id": other_wave, "block_id": "b_remote", "rev": 3, "hash": "remote", "is_root": false}
        ]);
        sqlx::query("UPDATE tasks SET context_stale_at_ms=42,claim_context_json=?1 WHERE wave_id=?2 AND key='cross-wave'")
            .bind(claim_context.to_string())
            .bind(&wave)
            .execute(&repo.pool)
            .await
            .unwrap();

        let mut tx = repo.pool.begin().await.unwrap();
        let verdicts = evaluate_schedulability(
            &mut tx,
            &wave,
            &[declaration(0, "cross-wave")],
            &[vec![]],
            true,
        )
        .await
        .unwrap();

        let stale = verdicts[0]
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "context_stale_reference")
            .collect::<Vec<_>>();
        assert_eq!(stale.len(), 2);
        assert!(stale.iter().any(|diagnostic| {
            diagnostic.related_wave_id.is_none() && diagnostic.related_block_ids == ["b_local"]
        }));
        assert!(stale.iter().any(|diagnostic| {
            diagnostic.related_wave_id.as_deref() == Some(other_wave.as_str())
                && diagnostic.related_block_ids == ["b_remote"]
        }));
    }

    #[tokio::test]
    async fn zero_ceiling_diagnostic_has_no_false_block_jump_target() {
        let (repo, wave) = setup().await;
        sqlx::query("UPDATE waves SET spec_task_ceiling=0 WHERE id=?1")
            .bind(&wave)
            .execute(&repo.pool)
            .await
            .unwrap();
        let mut tx = repo.pool.begin().await.unwrap();
        let verdicts = evaluate_schedulability(
            &mut tx,
            &wave,
            &[declaration(0, "capacity-zero")],
            &[vec![]],
            true,
        )
        .await
        .unwrap();
        let ceiling = verdicts[0]
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "spec_task_ceiling")
            .expect("ceiling diagnostic");
        assert!(ceiling.related_block_ids.is_empty());
        assert_eq!(ceiling.related_wave_id.as_deref(), Some(wave.as_str()));
    }

    #[tokio::test]
    async fn acceptance_1_spawn_only_projection_change_updates_row_and_changed_keys() {
        let (repo, wave) = setup().await;
        let mut declaration = declaration(0, "route");
        let mut tx = repo.pool.begin().await.unwrap();
        let first = project_tasks_tx(&mut tx, &wave, &[declaration.clone()], &[vec![]])
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(first.changed_keys, ["route"]);
        declaration.spawn = "sub-wave".into();
        let mut tx = repo.pool.begin().await.unwrap();
        let second = project_tasks_tx(&mut tx, &wave, &[declaration], &[vec![]])
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(second.changed_keys, ["route"]);
        let spawn: String =
            sqlx::query_scalar("SELECT spawn FROM tasks WHERE wave_id=?1 AND key='route'")
                .bind(&wave)
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        assert_eq!(spawn, "sub-wave");
    }

    #[tokio::test]
    async fn acceptance_14b_and_22_read_dto_marks_deleted_child_and_never_exposes_spawn() {
        let (repo, wave) = setup().await;
        insert_block_task(&repo, &wave, "child-link", "running").await;
        sqlx::query("UPDATE tasks SET spawn='sub-wave',child_wave_id='gone-child' WHERE wave_id=?1 AND key='child-link'")
            .bind(&wave).execute(&repo.pool).await.unwrap();
        let mut conn = repo.pool.acquire().await.unwrap();
        let verdicts = evaluate_schedulability(
            &mut conn,
            &wave,
            &[declaration(0, "child-link")],
            &[vec![]],
            true,
        )
        .await
        .unwrap();
        assert_eq!(verdicts[0].child_wave_id.as_deref(), Some("gone-child"));
        assert_eq!(verdicts[0].child_wave_deleted, Some(true));
        let json = serde_json::to_value(&verdicts[0]).unwrap();
        assert!(!json.as_object().unwrap().contains_key("spawn"));
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
        let mut conn = repo.pool.acquire().await.unwrap();
        let diagnostics =
            evaluate_schedulability(&mut conn, &wave, &declarations, &[vec![], vec![]], true)
                .await
                .unwrap();
        assert!(!outcome.diagnostics[0].schedulable);
        assert!(
            outcome.diagnostics[0]
                .diagnostics
                .iter()
                .any(|d| d.message.contains("ceiling"))
        );
        assert!(outcome.diagnostics[1].schedulable);
        assert_eq!(diagnostics[1].status.as_deref(), Some("dispatched"));
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
        let local = vec![vec![Diagnostic::new("payload", "broken")], vec![]];
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

    #[tokio::test]
    async fn rolled_back_material_verdict_leaves_no_row_change_or_event() {
        let (repo, wave) = setup().await;
        insert_block_task(&repo, &wave, "rollback", "running").await;
        let mut tx = repo.pool.begin().await.unwrap();
        let events = mark_context_material_tx(
            &mut tx,
            &format!("{wave}:rollback"),
            &wave,
            Vec::new(),
            "rollback proof",
        )
        .await
        .unwrap();
        assert_eq!(events.len(), 1, "the transaction produced one kernel event");
        tx.rollback().await.unwrap();

        let stale: Option<i64> =
            sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE id=?1")
                .bind(format!("{wave}:rollback"))
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        let persisted_events: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        assert_eq!(stale, None);
        assert_eq!(persisted_events, 0);
    }
}
