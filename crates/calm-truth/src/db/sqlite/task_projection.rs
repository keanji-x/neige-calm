use std::collections::{BTreeMap, BTreeSet};

use calm_types::event::{Event, EventScope, TaskContextChangedRef, TaskContextRef};
use calm_types::ids::{ActorId, TrackId};
use calm_types::report_blocks::tasks::{
    Diagnostic, GateInput, TASK_BLOCKING_DIAGNOSTIC_PATHS, TaskDeclaration, diagnostic_args,
    gate_rule_violations, json_eq, opt_json_eq, task_diagnostic_action, unknown_deps,
};
use calm_types::report_links::{format_track_destination, parse_destination, scan_links};
use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, SqliteConnection, Transaction};
use utoipa::ToSchema;

use super::track_tree::{
    DEFAULT_PLANNER_TASK_CEILING, MAX_TREE_TASK_BUDGET, TrackTreeTerm, deterministic_share,
    effective_limit,
};
use crate::error::{CalmError, Result};
use crate::model::now_ms;

/// Persisted task columns compared for in-flight declaration drift. `refs` is
/// resolved through the frozen context/index rather than stored on `tasks`;
/// `no_gate_reason` only affects gate validation, so neither belongs to this
/// direct column comparison.
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
) -> Result<bool> {
    let (_, _, kind, goal, context, acceptance, cwd, depends, _, gate, _, _, _) = row;
    Ok(match field {
        "kind" => kind != &declaration.kind,
        "goal" => goal != &declaration.goal,
        "context" => !context_eq(context, expected_context),
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
    i64,
    i64,
);

/// Which declaration edge was withdrawn, in the order the rationale prefers.
///
/// **`Ready` outranks `ReleasedByUser`, and the variant order is that rule.**
/// `evaluate_schedulability_with_tree_term` already picks this way for a single
/// block — it tests `decl_ready` first and only falls to the release edge in
/// the `else if` — because unsetting `ready` withdraws the declaration itself
/// while unsetting `released_by_user` only withdraws the wait release, which is
/// the narrower of the two. #1160 review ② makes the key-level fold agree:
/// folding across several blocks used to take the *document-order first* edge,
/// so a key declared by a `ready=false` block and a `released_by_user=false`
/// block reported a different rationale depending on which block came first —
/// the same block-order bug this change set exists to remove. Taking the `min`
/// makes one document produce one rationale, and makes the one-block and
/// many-block answers the same rule instead of two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum WithdrawalEdge {
    Ready,
    ReleasedByUser,
}

fn declaration_context_json(declaration: &TaskDeclaration) -> Result<String> {
    serde_json::to_string(&declaration.context)
        .map_err(|e| CalmError::Internal(format!("serialize task context: {e}")))
}

fn context_eq(actual: &str, expected: &str) -> bool {
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
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[schema(rename_all = "camelCase")]
pub enum TaskPendingReason {
    DependencyBlocked {
        message: String,
        dependencies: Vec<String>,
    },
    BudgetQueued {
        message: String,
        #[schema(rename = "occupiedTaskBudget")]
        occupied_task_budget: i64,
        #[schema(rename = "effectiveTaskBudget")]
        effective_task_budget: i64,
    },
    NotAdmitted {
        message: String,
        #[schema(rename = "diagnosticCodes")]
        diagnostic_codes: Vec<String>,
        actions: Vec<String>,
    },
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
    /// Issue #1147 slice ① / #1149 — the failure classifier plus its
    /// human reason tail (`"spawn-failed: track … is not a git
    /// repository"`). Without it a failed task reads as a bare
    /// classifier on every surface and the real diagnosis stays buried
    /// in the operation's `phase_detail_json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_card_id: Option<String>,
    /// Written after claim, so exposing it preserves the #1030 read-state
    /// exception. `spawn` must never be added beside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_track_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_track_deleted: Option<bool>,
    /// Server-owned explanation for a task that has not started. The tagged
    /// variants keep dependency readiness, scheduler budget, and projection
    /// admission distinct so clients never need to reconstruct scheduler
    /// policy from `schedulable`, diagnostics, or environment defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_reason: Option<TaskPendingReason>,
    #[serde(skip)]
    #[schema(ignore)]
    pub withdrawal: Option<WithdrawalEdge>,
}

#[derive(Deserialize)]
struct TaskReadState {
    key: String,
    status: String,
    status_detail: Option<String>,
    gate_result_json: Option<String>,
    worker_card_id: Option<String>,
    child_track_id: Option<String>,
    child_track_deleted: i64,
    context_stale_at_ms: Option<i64>,
    context_closure_truncated: i64,
    claim_context_json: Option<String>,
    depends_on: Vec<String>,
}

/// Live (non-tombstoned) declaration block ids per key, in document order.
///
/// This is the read-path twin of `calm-server`'s
/// `TaskContextService::resolve_task_closure` (`crates/calm-server/src/task_context.rs`,
/// the `let root = match live.as_slice()` arm): `[root] => Ok`,
/// `[] if tombstoned => RootTombstoned`, `[] => RootAbsent`,
/// `_ => DuplicateLiveKey`. The dispatch side has always failed closed on every
/// shape but `[root]` — the scheduler leaves such a task pending instead of
/// claiming it. #1160 applies the same verdict on the read path, so an
/// ambiguous key answers `status: null` on the wire instead of stamping one
/// row's run onto every block that happens to carry the key.
///
/// `calm-truth` cannot depend on `calm-server`, so the rule is stated twice;
/// both sites name each other. Do not add a third spelling.
fn live_declaration_blocks_by_key(declarations: &[TaskDeclaration]) -> BTreeMap<&str, Vec<&str>> {
    declarations
        .iter()
        // The empty key is grouped like any other. `key` is not required to be
        // non-empty on the wire, and `UNIQUE (track_id, key)` means two blocks
        // declaring `""` still share exactly one row — the very ambiguity this
        // index exists to name. Dropping them here would hand both blocks the
        // same run again.
        .filter(|declaration| !declaration.tombstone)
        .fold(BTreeMap::new(), |mut grouped, declaration| {
            grouped
                .entry(declaration.key.as_str())
                .or_default()
                .push(declaration.block_id.as_str());
            grouped
        })
}

/// Attach the task-table state carried by `track_projection_state`'s single
/// statement. This deliberately stays beside the projection query instead of
/// introducing a live block data-source abstraction.
///
/// #1160 — run state is attached only where the declaration index gives one
/// unambiguous owner. `tasks` is keyed by `(track_id, key)` and carries no block
/// identity, so when *several* live declarations claim a key nothing in the
/// data says which block owns the run; every field below then stays `None`
/// rather than being copied onto each candidate. See `owned` for the arms.
fn attach_task_read_state(
    rows: &[TaskReadState],
    track_id: &str,
    declarations: &[TaskDeclaration],
    verdicts: &mut [BlockVerdict],
) {
    let by_key: BTreeMap<_, _> = rows.iter().map(|row| (row.key.as_str(), row)).collect();
    let live_blocks = live_declaration_blocks_by_key(declarations);
    for verdict in verdicts {
        let Some(row) = by_key.get(verdict.key.as_str()) else {
            continue;
        };
        // Only *duplication* is ambiguous. `resolve_task_closure` returns a
        // single answer for the other two shapes too, and this mirrors them:
        //
        // - `[root]` — the one live declaration owns the run.
        // - `[]` / absent — no live declaration. `RootAbsent` /
        //   `RootTombstoned` on the dispatch side. Two verdicts can reach this
        //   arm and they are told apart by block id, not by guessing:
        //     * a *tombstoned* declaration carries its own block id, and must
        //       stay bare — that is #1160 case 1;
        //     * the `block_id: ""` verdict the loop above synthesises for a
        //       hard-deleted block is the only carrier the still-live row has
        //       left, and it is unique (the synthesis is guarded by "no
        //       declaration mentions this key at all"). Refusing it would leave
        //       a verdict this function's caller already stamped `status` on
        //       with no worker card for its own `open_worker_output` action,
        //       no `status_detail`, no child track, and neither reference
        //       diagnostic — the §6.5 withdrawal row would name a run nobody
        //       can open.
        // - two or more — genuinely undecidable, `DuplicateLiveKey`.
        let owned = match live_blocks.get(verdict.key.as_str()).map(Vec::as_slice) {
            Some([root]) => *root == verdict.block_id,
            None | Some([]) => verdict.block_id.is_empty(),
            Some(_) => false,
        };
        if !owned {
            continue;
        }
        verdict.status = Some(row.status.clone());
        verdict.status_detail = row.status_detail.clone();
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
        verdict.child_track_id = row.child_track_id.clone();
        verdict.child_track_deleted = row
            .child_track_id
            .as_ref()
            .map(|_| row.child_track_deleted != 0);
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
                            .entry(item.track_id.to_string())
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
                .map(|(related_track_id, block_ids)| {
                    let related_track_id =
                        (related_track_id != track_id).then_some(related_track_id);
                    (related_track_id, block_ids)
                })
                .collect::<Vec<_>>()
        };
        if row.context_closure_truncated != 0 {
            verdict
                .diagnostics
                .extend(related_context_blocks().into_iter().map(
                    |(related_track_id, related_block_ids)| {
                        Diagnostic::coded(
                            "reference_chain_too_large",
                            "refs",
                            BTreeMap::new(),
                            related_block_ids,
                            related_track_id,
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
        let restore_check_available =
            matches!(row.status.as_str(), "dispatched" | "running" | "verifying");
        if row.context_stale_at_ms.is_some()
            && !already_declaration_stale
            && restore_check_available
        {
            verdict
                .diagnostics
                .extend(related_context_blocks().into_iter().map(
                    |(related_track_id, related_block_ids)| {
                        Diagnostic::coded(
                            "context_stale_reference",
                            "refs",
                            diagnostic_args([(
                                "status",
                                serde_json::Value::String(row.status.clone()),
                            )]),
                            related_block_ids,
                            related_track_id,
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

fn readable_action(action: &str) -> String {
    match action {
        "raise_planner_task_ceiling" => "raise planner ceiling".into(),
        "raise_tree_task_budget" => "raise tree budget".into(),
        "add_gate_or_reason" => "add gate or reason".into(),
        "edit_dependencies" => "edit dependencies".into(),
        "release_task" => "release task".into(),
        other => other.replace('_', " "),
    }
}

/// Finish the read verdict with the scheduler-facing explanation. All inputs
/// come from `track_projection_state`'s one statement: the nullable track
/// override, every task status, and every persisted dependency list. The only
/// outside value is the already server-resolved environment default.
fn attach_task_pending_reasons(
    state: &TrackProjectionState,
    declarations: &[TaskDeclaration],
    task_budget_default: i64,
    verdicts: &mut [BlockVerdict],
) {
    let by_key: BTreeMap<_, _> = state
        .task_read_state
        .iter()
        .map(|row| (row.key.as_str(), row))
        .collect();
    let done: BTreeSet<_> = state
        .task_read_state
        .iter()
        .filter(|row| row.status == "done")
        .map(|row| row.key.as_str())
        .collect();
    let occupied = state
        .task_read_state
        .iter()
        .filter(|row| matches!(row.status.as_str(), "dispatched" | "running" | "verifying"))
        .count() as i64;
    let effective_budget = state.task_budget.unwrap_or(task_budget_default).max(0);

    for (index, verdict) in verdicts.iter_mut().enumerate() {
        if verdict.status.as_deref() == Some("pending") {
            let Some(row) = by_key.get(verdict.key.as_str()) else {
                continue;
            };
            let dependencies: Vec<String> = row
                .depends_on
                .iter()
                .filter(|dependency| !done.contains(dependency.as_str()))
                .cloned()
                .collect();
            if !dependencies.is_empty() {
                let terminal_dependencies: Vec<_> = dependencies
                    .iter()
                    .filter_map(|dependency| {
                        let status = by_key.get(dependency.as_str())?.status.as_str();
                        matches!(status, "failed" | "canceled")
                            .then_some((dependency.as_str(), status))
                    })
                    .collect();
                let message = match terminal_dependencies.as_slice() {
                    [] => match dependencies.as_slice() {
                        [dependency] => format!("Waiting for `{dependency}`"),
                        many => format!("Waiting for {} dependencies", many.len()),
                    },
                    [(dependency, status)] => {
                        format!("Blocked by `{dependency}` ({status}); revise dependencies")
                    }
                    many => {
                        format!(
                            "Blocked by {} terminal dependencies; revise dependencies",
                            many.len()
                        )
                    }
                };
                verdict.pending_reason = Some(TaskPendingReason::DependencyBlocked {
                    message,
                    dependencies,
                });
            } else if occupied >= effective_budget {
                verdict.pending_reason = Some(TaskPendingReason::BudgetQueued {
                    message: format!("Queued {occupied}/{effective_budget}"),
                    occupied_task_budget: occupied,
                    effective_task_budget: effective_budget,
                });
            }
            continue;
        }

        let Some(declaration) = declarations.get(index) else {
            continue;
        };
        if verdict.status.is_some()
            || verdict.schedulable
            || !declaration.ready
            || declaration.tombstone
            || verdict.diagnostics.is_empty()
        {
            continue;
        }
        let diagnostic_codes = verdict
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.clone())
            .collect::<Vec<_>>();
        let actions = verdict
            .diagnostics
            .iter()
            .filter_map(|diagnostic| diagnostic.action.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let explanation = if actions.is_empty() {
            verdict
                .diagnostics
                .first()
                .map(|diagnostic| diagnostic.code.replace('_', " "))
                .unwrap_or_else(|| "inspect diagnostics".into())
        } else {
            actions
                .iter()
                .map(|action| readable_action(action))
                .collect::<Vec<_>>()
                .join(" and ")
        };
        verdict.pending_reason = Some(TaskPendingReason::NotAdmitted {
            message: format!("Not admitted · {explanation}"),
            diagnostic_codes,
            actions,
        });
    }
}

#[derive(Debug, Clone, Default)]
pub struct TaskProjectionOutcome {
    pub changed_keys: Vec<String>,
    pub diagnostics: Vec<BlockVerdict>,
    pub kernel_events: Vec<(ActorId, EventScope, Event)>,
    /// Recursive tree statements used to obtain this projection's tree term.
    /// Whole-tree callers use this countable seam to prevent an accidental
    /// return to one full member walk per projected track.
    pub tree_cte_queries: u32,
}

/// Single-winner material verdict. Callers that commit this transaction must
/// merge the returned kernel events into that same eventized write.
pub async fn mark_context_material_tx(
    tx: &mut Transaction<'_, Sqlite>,
    task_id: &str,
    track_id: &str,
    changed_refs: Vec<TaskContextChangedRef>,
    rationale: &str,
) -> Result<Vec<(ActorId, EventScope, Event)>> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT t.key,w.area_id FROM tasks t JOIN tracks w ON w.id=t.track_id WHERE t.id=?1 AND t.track_id=?2",
    )
    .bind(task_id)
    .bind(track_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((task_key, area_id)) = row else {
        tracing::warn!(
            task_id,
            track_id,
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
        EventScope::Track {
            track: TrackId::from(track_id),
            area: area_id.into(),
        },
        Event::TaskContextAdvanced {
            track_id: TrackId::from(track_id),
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

fn reference_diagnostic(code: &str, reference: &str, track_id: Option<String>) -> Diagnostic {
    let related_block_ids = parse_destination(reference)
        .and_then(|(_, block)| block)
        .into_iter()
        .collect();
    Diagnostic::coded(
        code,
        "refs",
        diagnostic_args([("reference", serde_json::Value::String(reference.into()))]),
        related_block_ids,
        track_id,
        Some("relink_reference".into()),
    )
}

/// One in-flight `tasks` row, as carried by [`track_projection_state`]'s
/// `json_group_array`. Shape mirrors the three columns the predicate needs.
#[derive(Deserialize)]
struct InflightTaskRow {
    key: String,
    status: String,
    declared_by: String,
}

/// One normalized reference lookup passed to [`track_projection_state`].
///
/// Declarations stay Rust-owned. SQL receives only the relational lookup keys
/// it needs, as one JSON parameter, and returns facts; the verdict remains the
/// single Rust predicate shared by reads and writes.
#[derive(Serialize)]
#[serde(tag = "lookup_kind", rename_all = "snake_case")]
enum ReferenceLookupRequest {
    TrackBlock {
        reference: String,
        track_id: String,
        block_id: String,
    },
    Card {
        reference: String,
        card_id: String,
    },
}

impl ReferenceLookupRequest {
    fn reference(&self) -> &str {
        match self {
            Self::TrackBlock { reference, .. } | Self::Card { reference, .. } => reference,
        }
    }
}

#[derive(Deserialize)]
struct ReferenceTargetRow {
    reference: String,
    target_area: Option<String>,
    target_kind: Option<String>,
    block_exists: i64,
}

fn declaration_references(declaration: &TaskDeclaration) -> Vec<String> {
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
                .map(|block| format_track_destination(&link.dst_track_id, Some(&block)))
        }));
    }
    references.sort();
    references.dedup();
    references
}

fn reference_lookup_request(reference: &str) -> Option<ReferenceLookupRequest> {
    if let Some((track_id, Some(block_id))) = parse_destination(reference) {
        return Some(ReferenceLookupRequest::TrackBlock {
            reference: reference.into(),
            track_id,
            block_id,
        });
    }
    reference
        .strip_prefix("neige://card/")
        .filter(|id| !id.is_empty() && !id.contains('/'))
        .map(|card_id| ReferenceLookupRequest::Card {
            reference: reference.into(),
            card_id: card_id.into(),
        })
}

/// Everything the schedulability verdict needs from `tracks` + the in-flight
/// `tasks` rows, read in ONE statement (#1016).
struct TrackProjectionState {
    policy: Option<String>,
    ceiling: i64,
    require_gates: bool,
    /// Area of the track being projected — the source side of the
    /// cross-area reference check.
    source_area: String,
    inflight: Vec<InflightTaskRow>,
    task_read_state: Vec<TaskReadState>,
    task_budget: Option<i64>,
    frozen: Vec<FrozenDeclarationRow>,
    reference_targets: BTreeMap<String, ReferenceTargetRow>,
}

#[derive(sqlx::FromRow)]
struct TrackProjectionStateRow {
    automation_policy: Option<String>,
    planner_task_ceiling: Option<i64>,
    require_task_gates: i64,
    area_id: String,
    inflight_json: String,
    task_read_state_json: String,
    task_budget: Option<i64>,
    frozen_json: String,
    reference_targets_json: String,
}

/// Materializes every database fact used by the local schedulability verdict
/// in a SINGLE statement.
///
/// This used to be four statements — policy/ceiling/gates, the in-flight key
/// list, the ceiling-occupancy `count(*)`, and `SELECT area_id` — which is
/// fine inside the write path's IMMEDIATE transaction but mixes database
/// versions on the read path, where the caller runs in autocommit. A ceiling
/// read at t0 against an occupancy counted at t1 can report a capacity that
/// never existed, and the orphaned-in-flight scan could contradict the
/// in-flight key list. One statement is one implicit transaction, so all of
/// it now comes from one version. It also removes the `SELECT area_id ...
/// fetch_one` that turned a concurrently deleted track into a 500 (it is the
/// `NotFound` below, i.e. a 404, on the same row that carries the policy).
///
/// The in-flight key set, ceiling occupancy, orphan candidates, frozen
/// declarations, task-budget override, dependency rows, and reference targets
/// are captured by the same statement.
async fn track_projection_state(
    conn: &mut SqliteConnection,
    track_id: &str,
    include_read_state: bool,
    reference_requests: &[ReferenceLookupRequest],
    after_statement: impl std::future::Future<Output = ()>,
) -> Result<TrackProjectionState> {
    let reference_requests_json = serde_json::to_string(reference_requests)
        .map_err(|e| CalmError::Internal(format!("serialize task reference lookups: {e}")))?;
    let row: Option<TrackProjectionStateRow> = sqlx::query_as(
        r#"WITH reference_input(reference,lookup_kind,track_id,block_id,card_id) AS (
               SELECT json_extract(value,'$.reference'),
                      json_extract(value,'$.lookup_kind'),
                      json_extract(value,'$.track_id'),
                      json_extract(value,'$.block_id'),
                      json_extract(value,'$.card_id')
                 FROM json_each(?2)
           )
           SELECT w.automation_policy, w.planner_task_ceiling, w.require_task_gates, w.area_id,
                  w.task_budget,
                  (SELECT json_group_array(json_object(
                       'key', t.key, 'status', t.status,
                       'declared_by', t.declared_by))
                   FROM tasks t
                   WHERE t.track_id = w.id
                       AND t.status IN ('dispatched','running','verifying')) AS inflight_json,
                   CASE WHEN ?3 != 0 THEN (SELECT json_group_array(json_object(
                        'key', t.key, 'status', t.status,
                        'status_detail', t.status_detail,
                       'gate_result_json', t.gate_result_json,
                       'worker_card_id', t.worker_card_id,
                       'child_track_id', t.child_track_id,
                       'child_track_deleted', CASE WHEN t.child_track_id IS NOT NULL
                         AND NOT EXISTS(SELECT 1 FROM tracks child WHERE child.id=t.child_track_id)
                         THEN 1 ELSE 0 END,
                       'context_stale_at_ms', t.context_stale_at_ms,
                       'context_closure_truncated', t.context_closure_truncated,
                        'claim_context_json', t.claim_context_json,
                        'depends_on', json(t.depends_on_json)))
                     FROM tasks t WHERE t.track_id = w.id)
                   ELSE '[]' END AS task_read_state_json,
                   (SELECT json_group_array(json_array(
                        t.status,t.key,t.kind,t.goal,t.context_json,
                        t.acceptance_criteria,t.cwd,t.depends_on_json,t.priority,
                        t.gate_json,t.declared_by,t.decl_ready,t.decl_released_by_user))
                      FROM tasks t
                     WHERE t.track_id = w.id AND t.status != 'pending') AS frozen_json,
                   (SELECT json_group_array(json_object(
                        'reference', r.reference,
                        'target_area', target.area_id,
                        'target_kind', target_area.kind,
                        'block_exists', CASE
                            WHEN target.id IS NULL THEN 0
                            WHEN r.lookup_kind = 'card' THEN 1
                            ELSE EXISTS(
                                SELECT 1
                                  FROM cards report
                                  JOIN json_each(report.payload,'$.blocks') block
                                 WHERE report.track_id = target.id
                                   AND report.kind = 'track-report'
                                   AND json_extract(block.value,'$.id') = r.block_id)
                        END))
                      FROM reference_input r
                      LEFT JOIN cards referenced_card
                        ON r.lookup_kind = 'card' AND referenced_card.id = r.card_id
                      LEFT JOIN tracks target
                        ON target.id = CASE r.lookup_kind
                            WHEN 'card' THEN referenced_card.track_id
                            ELSE r.track_id
                        END
                      LEFT JOIN areas target_area ON target_area.id = target.area_id)
                     AS reference_targets_json
           FROM tracks w WHERE w.id = ?1"#,
    )
    .bind(track_id)
    .bind(reference_requests_json)
    .bind(i64::from(include_read_state))
    .fetch_optional(&mut *conn)
    .await?;
    // Keep the test seam immediately after the one statement. A future
    // extraction of frozen/reference facts into another query must therefore
    // cross this t0/t1 boundary and trip the concurrency regressions.
    after_statement.await;
    let row = row.ok_or_else(|| CalmError::NotFound(format!("track {track_id}")))?;
    let reference_targets =
        serde_json::from_str::<Vec<ReferenceTargetRow>>(&row.reference_targets_json)?
            .into_iter()
            .map(|target| (target.reference.clone(), target))
            .collect();
    Ok(TrackProjectionState {
        policy: row.automation_policy,
        ceiling: effective_limit(row.planner_task_ceiling, DEFAULT_PLANNER_TASK_CEILING),
        require_gates: row.require_task_gates != 0,
        source_area: row.area_id,
        inflight: serde_json::from_str(&row.inflight_json)?,
        task_read_state: serde_json::from_str(&row.task_read_state_json)?,
        task_budget: row.task_budget,
        frozen: serde_json::from_str(&row.frozen_json)?,
        reference_targets,
    })
}

/// The single DB-aware schedulability predicate used by writes, rebuilds and reads.
///
/// Takes a bare connection rather than a transaction (#1016): the WRITE path
/// hands it `&mut **tx` so its verdict stays atomic with the projection it
/// drives, while the read-only path (`read.rs::task_diagnostics`) hands it a
/// pooled connection in AUTOCOMMIT mode.
///
/// Consistency on the READ path: policy, ceiling, local in-flight occupancy,
/// frozen declarations, task read state, source area and every normalized
/// reference target are materialized by ONE autocommit statement
/// ([`track_projection_state`]). It is therefore one SQLite snapshot without
/// taking the writer slot or introducing the shared-cache deferred-reader
/// deadlock pinned by `deferred_read_tx_deadlock_repro`.
///
/// [`track_tree_term`] still runs before that statement. Its bounded tree
/// walks are a separate tree-budget term, while all local/reference facts in
/// the #1027 tear are version-locked here. The WRITE path remains fully atomic
/// because its caller supplies the enclosing IMMEDIATE transaction.
/// Every declaration of a track whose tree root cannot be resolved is
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
        None,
    )
}

pub async fn evaluate_schedulability(
    conn: &mut SqliteConnection,
    track_id: &str,
    declarations: &[TaskDeclaration],
    block_local_diags: &[Vec<Diagnostic>],
    include_read_state: bool,
) -> Result<Vec<BlockVerdict>> {
    let tree = super::track_tree::track_tree_term(&mut *conn, track_id).await?;
    evaluate_schedulability_with_tree_term(
        conn,
        track_id,
        declarations,
        block_local_diags,
        tree.term,
        TaskReadOptions {
            include_state: include_read_state,
            task_budget_default: None,
        },
    )
    .await
}

/// Read-side form of [`evaluate_schedulability`]. `task_budget_default` is the
/// server-resolved `NEIGE_TRACK_TASK_BUDGET` value; the repository combines it
/// with the nullable per-track override and returns the finished diagnosis.
/// Write paths intentionally call the sibling above because pending reasons
/// are presentation metadata, never projection inputs.
pub async fn evaluate_schedulability_with_task_budget_default(
    conn: &mut SqliteConnection,
    track_id: &str,
    declarations: &[TaskDeclaration],
    block_local_diags: &[Vec<Diagnostic>],
    task_budget_default: i64,
) -> Result<Vec<BlockVerdict>> {
    let tree = super::track_tree::track_tree_term(&mut *conn, track_id).await?;
    evaluate_schedulability_with_tree_term(
        conn,
        track_id,
        declarations,
        block_local_diags,
        tree.term,
        TaskReadOptions {
            include_state: true,
            task_budget_default: Some(task_budget_default),
        },
    )
    .await
}

#[derive(Clone, Copy)]
struct TaskReadOptions {
    include_state: bool,
    task_budget_default: Option<i64>,
}

async fn evaluate_schedulability_with_tree_term(
    conn: &mut SqliteConnection,
    track_id: &str,
    declarations: &[TaskDeclaration],
    block_local_diags: &[Vec<Diagnostic>],
    tree_term: TrackTreeTerm,
    read: TaskReadOptions,
) -> Result<Vec<BlockVerdict>> {
    evaluate_schedulability_with_tree_term_after_snapshot(
        conn,
        track_id,
        declarations,
        block_local_diags,
        tree_term,
        read,
        std::future::ready(()),
    )
    .await
}

/// Test-only interleaving seam. `after_snapshot` runs after the one fact query
/// has completed and before any verdict consumes it, so concurrency tests can
/// mutate the database at a proven t0/t1 boundary without copying the
/// production predicate or relying on scheduler timing.
#[cfg(test)]
pub(super) async fn evaluate_schedulability_after_snapshot_for_test(
    conn: &mut SqliteConnection,
    track_id: &str,
    declarations: &[TaskDeclaration],
    block_local_diags: &[Vec<Diagnostic>],
    include_read_state: bool,
    after_snapshot: impl std::future::Future<Output = ()>,
) -> Result<Vec<BlockVerdict>> {
    let tree = super::track_tree::track_tree_term(&mut *conn, track_id).await?;
    evaluate_schedulability_with_tree_term_after_snapshot(
        conn,
        track_id,
        declarations,
        block_local_diags,
        tree.term,
        TaskReadOptions {
            include_state: include_read_state,
            task_budget_default: None,
        },
        after_snapshot,
    )
    .await
}

async fn evaluate_schedulability_with_tree_term_after_snapshot(
    conn: &mut SqliteConnection,
    track_id: &str,
    declarations: &[TaskDeclaration],
    block_local_diags: &[Vec<Diagnostic>],
    tree_term: TrackTreeTerm,
    read: TaskReadOptions,
    after_snapshot: impl std::future::Future<Output = ()>,
) -> Result<Vec<BlockVerdict>> {
    let references_by_declaration = declarations
        .iter()
        .map(declaration_references)
        .collect::<Vec<_>>();
    let reference_requests = references_by_declaration
        .iter()
        .flatten()
        .filter_map(|reference| reference_lookup_request(reference))
        .fold(BTreeMap::new(), |mut requests, request| {
            requests
                .entry(request.reference().to_owned())
                .or_insert(request);
            requests
        })
        .into_values()
        .collect::<Vec<_>>();
    let state = track_projection_state(
        &mut *conn,
        track_id,
        read.include_state,
        &reference_requests,
        after_snapshot,
    )
    .await?;
    // #985 slice 6 PR-B — the tree term. `effective_ceiling = min(ceiling,
    // share)` where `share` is this track's deterministic slice of the root's
    // `tree_task_budget`, split over the tree's tracks in `(created_at, id)`
    // order. Its quota is a function of tree SHAPE, never sibling projection
    // output. Within this track, immutable in-flight occupancy is subtracted and
    // pending rows re-enter as ordered candidates. That keeps
    // "rebuild ≡ incremental" (D.1 #11) true. A shared sibling count would
    // instead be first-come-first-served,
    // path-dependent, and not reconstructible by a rebuild.
    let ceiling = state.ceiling;
    let (tree_share, tree_root_unresolved) = match &tree_term {
        TrackTreeTerm::RootUnresolved => {
            // Fail closed. A broken parent link, a cycle, or an over-deep chain
            // means we cannot name the budget this track draws from; treating
            // "no resolvable tree" as "no tree constraint" would leave a whole
            // subtree unbounded, which is the one outcome the tree bound exists
            // to prevent.
            (None, true)
        }
        TrackTreeTerm::Share(share) => (Some(share.clone()), false),
    };
    let require_gates = state.require_gates;
    let source_area = state.source_area.as_str();
    let effective_wait = state.policy.as_deref() == Some("declare-and-wait");
    // unknown_deps knows every in-flight key in the track.
    let inflight_keys: Vec<String> = state.inflight.iter().map(|r| r.key.clone()).collect();
    let inflight_key_set: BTreeSet<&str> = inflight_keys.iter().map(String::as_str).collect();
    let ceiling_occupied: i64 = state
        .inflight
        .iter()
        .filter(|r| r.declared_by == "spec")
        .count() as i64;
    let ceiling_capacity = ceiling.saturating_sub(ceiling_occupied).max(0);
    // Pending rows are projection output and re-enter below as candidates.
    // In-flight planner rows are fixed occupancy for this write.
    let tree_occupied = ceiling_occupied;
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
    for (declaration, references) in declarations.iter().zip(&references_by_declaration) {
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
        for reference in references {
            let destination = parse_destination(reference);
            let destination_track = destination.as_ref().map(|(track, _)| track.clone());
            if let Some((dst_track, None)) = destination {
                diagnostics.push(reference_diagnostic(
                    "reference_needs_block",
                    reference,
                    Some(dst_track),
                ));
                continue;
            }
            if reference_lookup_request(reference).is_none() {
                continue;
            }
            let target = state.reference_targets.get(reference).ok_or_else(|| {
                CalmError::Internal(format!(
                    "task reference snapshot omitted normalized lookup {reference:?}"
                ))
            })?;
            match (
                target.target_area.as_deref(),
                target.target_kind.as_deref(),
                target.block_exists,
            ) {
                (None, _, _) | (_, None, _) => diagnostics.push(reference_diagnostic(
                    "reference_missing",
                    reference,
                    destination_track.clone(),
                )),
                (Some(target_area), Some(target_kind), _)
                    if target_area != source_area && target_kind != "system" =>
                {
                    diagnostics.push(reference_diagnostic(
                        "reference_cross_area",
                        reference,
                        destination_track.clone(),
                    ));
                }
                (Some(_), Some(_), 0) => diagnostics.push(reference_diagnostic(
                    "reference_missing",
                    reference,
                    destination_track,
                )),
                (Some(_), Some(_), _) => {}
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
            status_detail: None,
            gate_result: None,
            worker_card_id: None,
            child_track_id: None,
            child_track_deleted: None,
            pending_reason: None,
            diagnostics,
            withdrawal: None,
        });
    }

    // Freeze all persisted declaration columns before ceiling admission. A stale
    // in-flight declaration cannot consume capacity, and a terminal key can never
    // produce a new live row.
    let frozen_by_key: BTreeMap<_, _> = state
        .frozen
        .iter()
        .cloned()
        .map(|row| (row.1.clone(), row))
        .collect();
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
            decl_ready,
            decl_released_by_user,
        )) = frozen_by_key.get(&declaration.key)
        else {
            continue;
        };
        let expected_context = declaration_context_json(declaration)?;
        let row = frozen_by_key.get(&declaration.key).expect("frozen row");
        let changed = PROJECTION_DRIFT_TASK_FIELDS
            .iter()
            .try_fold(false, |changed, field| {
                Ok::<_, CalmError>(
                    changed
                        || declaration_field_changed(field, row, declaration, &expected_context)?,
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
    // Same rows the in-flight key list came from (one statement, one version).
    for row in &state.inflight {
        if !declared_keys.contains(row.key.as_str()) {
            verdicts.push(BlockVerdict {
                block_id: String::new(),
                diagnostics: vec![withdrawal_diagnostic(&row.key, &row.status)],
                key: row.key.clone(),
                schedulable: false,
                status: Some(row.status.clone()),
                status_detail: None,
                gate_result: None,
                worker_card_id: None,
                child_track_id: None,
                child_track_deleted: None,
                pending_reason: None,
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
    // Attribution matters here (§12.2 C): recovery must name every setting
    // that actually binds this admission. A strict minimum names one knob;
    // equality names BOTH, because raising either one alone leaves the other
    // at the same minimum. An overage freeze always binds the tree and also
    // binds the local ceiling when that ceiling has no remaining slot.
    let tree_bound = tree_share
        .as_ref()
        .filter(|share| share.admission_frozen || tree_capacity < ceiling_capacity)
        .cloned();
    let tied_bounds = tree_share
        .as_ref()
        .filter(|share| !share.admission_frozen && tree_capacity == ceiling_capacity)
        .cloned();
    for index in candidates.into_iter().skip(capacity) {
        let tree_diagnostic = |share: &super::track_tree::TreeShare, bounds_tied: bool| {
            // The target must gain a genuinely free slot, not merely catch up
            // to immutable occupancy. In a self-overage freeze the first B
            // that increases `share` can still leave share == occupancy.
            let occupied_or_current_share = share.share.max(tree_occupied);
            let minimum_for_target =
                (share.budget.saturating_add(1)..=MAX_TREE_TASK_BUDGET).find(|budget| {
                    deterministic_share(*budget, share.members, share.member_index)
                        > occupied_or_current_share
                });
            let minimum_tree_task_budget = if share.admission_frozen {
                minimum_for_target
                    .zip(share.minimum_budget_to_unfreeze)
                    .map(|(target_minimum, unfreeze_minimum)| target_minimum.max(unfreeze_minimum))
            } else {
                minimum_for_target
            };
            let mut args = diagnostic_args([
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
                    "admission_frozen",
                    serde_json::Value::from(share.admission_frozen),
                ),
                ("bounds_tied", serde_json::Value::from(bounds_tied)),
                (
                    "admission_order",
                    serde_json::Value::String("document order, then key".into()),
                ),
            ]);
            if let Some(minimum) = minimum_tree_task_budget {
                args.insert(
                    "minimum_tree_task_budget".into(),
                    serde_json::Value::from(minimum),
                );
            }
            Diagnostic::coded(
                "tree_budget_exhausted",
                "key",
                args,
                admitted_ids.clone(),
                Some(share.root_id.clone()),
                minimum_tree_task_budget
                    .and_then(|_| task_diagnostic_action("tree_budget_exhausted"))
                    .map(str::to_owned),
            )
        };
        let ceiling_diagnostic = |tree_context: Option<&super::track_tree::TreeShare>,
                                  admission_frozen: bool,
                                  raise_available: bool| {
            // The new ceiling must clear both the configured bound and fixed
            // in-flight occupancy. When an operator lowered the ceiling below
            // occupancy this is `occupied + 1`; otherwise it preserves the
            // ordinary `ceiling + 1` minimum.
            let minimum_planner_task_ceiling = ceiling.max(ceiling_occupied).saturating_add(1);
            let mut args = diagnostic_args([
                ("ceiling", serde_json::Value::from(ceiling)),
                ("occupied", serde_json::Value::from(ceiling_occupied)),
                (
                    "minimum_planner_task_ceiling",
                    serde_json::Value::from(minimum_planner_task_ceiling),
                ),
                (
                    "admission_order",
                    serde_json::Value::String("document order, then key".into()),
                ),
            ]);
            if let Some(share) = tree_context {
                args.insert(
                    "admission_frozen".into(),
                    serde_json::Value::from(admission_frozen),
                );
                args.insert(
                    "bounds_tied".into(),
                    serde_json::Value::from(!admission_frozen),
                );
                args.insert(
                    "root_wave_id".into(),
                    serde_json::Value::String(share.root_id.clone()),
                );
            }
            if !raise_available {
                args.insert(
                    "capacity_raise_unavailable".into(),
                    serde_json::Value::from(true),
                );
            }
            Diagnostic::coded(
                "planner_task_ceiling",
                "key",
                args,
                admitted_ids.clone(),
                Some(track_id.into()),
                raise_available
                    .then(|| task_diagnostic_action("planner_task_ceiling"))
                    .flatten()
                    .map(str::to_owned),
            )
        };
        if let Some(share) = &tree_bound {
            let tree = tree_diagnostic(share, false);
            if share.admission_frozen && ceiling_capacity == 0 {
                verdicts[index].diagnostics.push(ceiling_diagnostic(
                    Some(share),
                    true,
                    tree.action.is_some(),
                ));
            }
            verdicts[index].diagnostics.push(tree);
        } else if let Some(share) = &tied_bounds {
            let tree = tree_diagnostic(share, true);
            verdicts[index].diagnostics.push(ceiling_diagnostic(
                Some(share),
                false,
                tree.action.is_some(),
            ));
            verdicts[index].diagnostics.push(tree);
        } else {
            verdicts[index]
                .diagnostics
                .push(ceiling_diagnostic(None, false, true));
        }
        verdicts[index].schedulable = false;
    }
    if read.include_state {
        attach_task_read_state(
            &state.task_read_state,
            track_id,
            declarations,
            &mut verdicts,
        );
        if let Some(task_budget_default) = read.task_budget_default {
            attach_task_pending_reasons(&state, declarations, task_budget_default, &mut verdicts);
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
    track_id: &str,
    declarations: &[TaskDeclaration],
    block_local_diags: &[Vec<Diagnostic>],
) -> Result<TaskProjectionOutcome> {
    let tree = super::track_tree::track_tree_term(tx, track_id).await?;
    let tree_cte_queries = tree.tree_cte_queries;
    let verdicts = evaluate_schedulability_with_tree_term(
        tx,
        track_id,
        declarations,
        block_local_diags,
        tree.term,
        TaskReadOptions {
            include_state: false,
            task_budget_default: None,
        },
    )
    .await?;
    project_tasks_from_verdicts_tx(tx, track_id, declarations, verdicts, tree_cte_queries).await
}

/// Projection entry point for a whole-tree rebuild. The caller enumerates the
/// tree once and passes each deterministic share here, avoiding one recursive
/// member walk per track.
pub async fn project_tasks_with_tree_term_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
    declarations: &[TaskDeclaration],
    block_local_diags: &[Vec<Diagnostic>],
    tree_term: TrackTreeTerm,
) -> Result<TaskProjectionOutcome> {
    let verdicts = evaluate_schedulability_with_tree_term(
        tx,
        track_id,
        declarations,
        block_local_diags,
        tree_term,
        TaskReadOptions {
            include_state: false,
            task_budget_default: None,
        },
    )
    .await?;
    project_tasks_from_verdicts_tx(tx, track_id, declarations, verdicts, 0).await
}

async fn project_tasks_from_verdicts_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
    declarations: &[TaskDeclaration],
    verdicts: Vec<BlockVerdict>,
    tree_cte_queries: u32,
) -> Result<TaskProjectionOutcome> {
    // #1160 — these three indexes ask row-level questions ("is this *key*
    // still backed by the document?"), but used to be built as last-wins
    // `BTreeMap` collects, so with several blocks on one key the answer
    // depended on document order. Each is now an explicit fold over every
    // block carrying the key. The dimension stays `key`: `tasks` rows are
    // keyed by `(track_id, key)` and `block_id` is not a durable identity.
    //
    // `schedulable` folds with `any`: the row is still wanted as long as *one*
    // live block can produce it. Folding with `all` would let a tombstone (or
    // a diagnosed twin) of a key that is still declared delete the pending row
    // / raise a withdrawal.
    let schedulable_by_key = verdicts.iter().filter(|v| !v.key.is_empty()).fold(
        BTreeMap::<String, bool>::new(),
        |mut folded, verdict| {
            *folded.entry(verdict.key.clone()).or_default() |= verdict.schedulable;
            folded
        },
    );
    let existing: Vec<(String, String, String, Option<String>)> =
        sqlx::query_as("SELECT id,key,status,claim_context_json FROM tasks WHERE track_id=?1")
            .bind(track_id)
            .fetch_all(&mut **tx)
            .await?;
    let mut verdicts = verdicts;
    // All verdict slots of a key, in document order.
    let verdict_indexes_by_key = verdicts.iter().enumerate().fold(
        BTreeMap::<String, Vec<usize>>::new(),
        |mut folded, (index, verdict)| {
            if !verdict.key.is_empty() {
                folded.entry(verdict.key.clone()).or_default().push(index);
            }
            folded
        },
    );
    // A key is removed exactly when no live block declares it any more —
    // a tombstone standing beside a live re-declaration removes nothing.
    let live_declarations_by_key = live_declaration_blocks_by_key(declarations);
    let mut changed = BTreeSet::new();
    let mut kernel_events = Vec::new();
    for (id, key, status, claim_context_json) in &existing {
        let verdict_indexes = verdict_indexes_by_key
            .get(key)
            .map(Vec::as_slice)
            .unwrap_or_default();
        // Withdrawal folds with `all`: the key is withdrawn only when *every*
        // block carrying it withdrew. One block that is still ready keeps the
        // key claimed — otherwise a tombstone left beside a live
        // re-declaration would read as a withdrawal of the new declaration.
        //
        // The edge itself is the *strongest* one, by `WithdrawalEdge`'s own
        // ordering — not the first one in document order, which was still a
        // block-order dependency (#1160 review ②): two withdrawing blocks with
        // different edges sent a different rationale for `[X, Y]` than for
        // `[Y, X]`. See `WithdrawalEdge` for why `Ready` is the stronger.
        let withdrawal_edge = (!verdict_indexes.is_empty()
            && verdict_indexes
                .iter()
                .all(|index| verdicts[*index].withdrawal.is_some()))
        .then(|| {
            verdict_indexes
                .iter()
                .filter_map(|index| verdicts[*index].withdrawal)
                .min()
        })
        .flatten();
        let withdrawal = withdrawal_edge.is_some();
        if !schedulable_by_key.get(key).is_some_and(|value| *value) || withdrawal {
            if matches!(status.as_str(), "dispatched" | "running" | "verifying") {
                let declaration_removed = live_declarations_by_key
                    .get(key.as_str())
                    .is_none_or(Vec::is_empty);
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
                                    track_id: frozen.track_id,
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
                            track_id,
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
                if verdict_indexes.is_empty() {
                    verdicts.push(BlockVerdict {
                        block_id: String::new(),
                        key: key.clone(),
                        diagnostics: vec![diagnostic],
                        schedulable: false,
                        status: Some(status.clone()),
                        status_detail: None,
                        gate_result: None,
                        worker_card_id: None,
                        child_track_id: None,
                        child_track_deleted: None,
                        pending_reason: None,
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
        if !verdict.schedulable {
            continue;
        }
        let id = format!("{track_id}:{}", declaration.key);
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
                   id,track_id,key,kind,goal,context_json,acceptance_criteria,cwd,
                   depends_on_json,priority,gate_json,status,declared_by,spawn,
                   decl_ready,decl_released_by_user,created_at_ms,updated_at_ms
               ) VALUES(
                   ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'pending',?12,?13,
                   ?14,?15,?16,?16
               )
               ON CONFLICT(track_id,key) DO UPDATE SET
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
                   decl_ready=excluded.decl_ready,
                   decl_released_by_user=excluded.decl_released_by_user,
                   updated_at_ms=excluded.updated_at_ms
               WHERE tasks.status='pending'
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
                     OR tasks.decl_ready IS NOT excluded.decl_ready
                     OR tasks.decl_released_by_user IS NOT excluded.decl_released_by_user
                 )"#,
        )
        .bind(&id)
        .bind(track_id)
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
    use calm_types::track_report::ReportBlock;
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

    /// The shape `report_blocks::tasks` produces for a deleted task block:
    /// `tombstone` set, `ready` absent (so `false`).
    fn tombstone_declaration(index: usize, key: &str) -> TaskDeclaration {
        TaskDeclaration {
            tombstone: true,
            ready: false,
            ..declaration(index, key)
        }
    }

    async fn setup() -> (super::super::SqlxRepo, String) {
        let repo = super::super::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let track = "track-projection".to_string();
        sqlx::query("INSERT INTO areas(id,name,color,sort,kind,created_at,updated_at) VALUES('area-projection','c','#000',0,'user',0,0)")
            .execute(&repo.pool).await.unwrap();
        // #1147 S1 — this fixture used to inline `cwd='/'`, which made it a
        // second writer of a column design D1 reserves for
        // `track_workspace_write_tx`. It is converted rather than exempted:
        // a fixture that bypasses a production invariant is exactly the shape
        // that lets the invariant rot. The projection assertions never read
        // the workspace, so the value is the same `/` routed properly.
        sqlx::query("INSERT INTO tracks(id,area_id,title,sort,lifecycle,created_at,updated_at,planner_task_ceiling,require_task_gates) VALUES(?1,'area-projection','w',0,'draft',0,0,1,0)")
            .bind(&track).execute(&repo.pool).await.unwrap();
        let mut tx = repo.pool.begin().await.unwrap();
        crate::db::sqlite::track_workspace::track_workspace_write_tx(
            &mut tx,
            &track,
            &crate::model::TrackWorkspace {
                kind: crate::model::TrackWorkspaceKind::Attached,
                path: "/".into(),
                frozen_at: Some(0),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (repo, track)
    }

    async fn insert_block_task(
        repo: &super::super::SqlxRepo,
        track: &str,
        key: &str,
        status: &str,
    ) {
        sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,depends_on_json,priority,status,declared_by,claim_context_json,context_closure_truncated,decl_ready,decl_released_by_user,context_verify_failures,spawn,child_track_id,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,'codex',?4,'{}','[]',0,?5,'spec',NULL,0,0,0,0,'in-wave',NULL,0,0)")
            .bind(format!("{track}:{key}")).bind(track).bind(key).bind(format!("goal {key}")).bind(status)
            .execute(&repo.pool).await.unwrap();
    }

    #[tokio::test]
    async fn read_state_surfaces_truncated_and_stale_reference_diagnostics() {
        let (repo, track) = setup().await;
        insert_block_task(&repo, &track, "read-state", "running").await;
        let claim_context = json!([
            {"track_id": track, "block_id": "b_0000", "rev": 1, "hash": "root", "is_root": true},
            {"track_id": track, "block_id": "b_feed", "rev": 2, "hash": "ref", "is_root": false}
        ]);
        sqlx::query("UPDATE tasks SET context_closure_truncated=1,context_stale_at_ms=42,claim_context_json=?1,gate_result_json=?2,worker_card_id='worker-output' WHERE track_id=?3 AND key='read-state'")
            .bind(claim_context.to_string())
            .bind(json!({"passed":false,"failing_step":"test","log_path":"/private/gate.log","log_tail":"raw output","attempt":7}).to_string())
            .bind(&track)
            .execute(&repo.pool)
            .await
            .unwrap();

        let mut tx = repo.pool.begin().await.unwrap();
        let verdicts = evaluate_schedulability(
            &mut tx,
            &track,
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
        let (repo, track) = setup().await;
        insert_block_task(&repo, &track, "root-only", "running").await;
        let claim_context = json!([
            {"track_id": track, "block_id": "b_0000", "rev": 1, "hash": "root", "is_root": true}
        ]);
        sqlx::query("UPDATE tasks SET context_closure_truncated=1,context_stale_at_ms=42,claim_context_json=?1 WHERE track_id=?2 AND key='root-only'")
            .bind(claim_context.to_string())
            .bind(&track)
            .execute(&repo.pool)
            .await
            .unwrap();

        let mut tx = repo.pool.begin().await.unwrap();
        let verdicts = evaluate_schedulability(
            &mut tx,
            &track,
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
            assert!(diagnostic.related_track_id.is_none());
        }
    }

    #[tokio::test]
    async fn declare_and_wait_diagnostic_does_not_link_to_its_own_track() {
        let (repo, track) = setup().await;
        sqlx::query("UPDATE tracks SET automation_policy='declare-and-wait' WHERE id=?1")
            .bind(&track)
            .execute(&repo.pool)
            .await
            .unwrap();

        let mut tx = repo.pool.begin().await.unwrap();
        let verdicts = evaluate_schedulability(
            &mut tx,
            &track,
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
        assert!(diagnostic.related_track_id.is_none());
        assert!(diagnostic.related_block_ids.is_empty());
    }

    #[tokio::test]
    async fn read_state_groups_related_context_links_by_track() {
        let (repo, track) = setup().await;
        insert_block_task(&repo, &track, "cross-track", "running").await;
        let other_track = "track_remote".to_owned();
        let claim_context = json!([
            {"track_id": track, "block_id": "b_root", "rev": 1, "hash": "root", "is_root": true},
            {"track_id": track, "block_id": "b_local", "rev": 2, "hash": "local", "is_root": false},
            {"track_id": other_track, "block_id": "b_remote", "rev": 3, "hash": "remote", "is_root": false}
        ]);
        sqlx::query("UPDATE tasks SET context_stale_at_ms=42,claim_context_json=?1 WHERE track_id=?2 AND key='cross-track'")
            .bind(claim_context.to_string())
            .bind(&track)
            .execute(&repo.pool)
            .await
            .unwrap();

        let mut tx = repo.pool.begin().await.unwrap();
        let verdicts = evaluate_schedulability(
            &mut tx,
            &track,
            &[declaration(0, "cross-track")],
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
            diagnostic.related_track_id.is_none() && diagnostic.related_block_ids == ["b_local"]
        }));
        assert!(stale.iter().any(|diagnostic| {
            diagnostic.related_track_id.as_deref() == Some(other_track.as_str())
                && diagnostic.related_block_ids == ["b_remote"]
        }));
    }

    #[tokio::test]
    async fn terminal_stale_row_only_offers_the_new_key_action() {
        let (repo, track) = setup().await;
        insert_block_task(&repo, &track, "terminal-stale", "failed").await;
        sqlx::query(
            "UPDATE tasks SET context_stale_at_ms=42,claim_context_json='[]' \
             WHERE track_id=?1 AND key='terminal-stale'",
        )
        .bind(&track)
        .execute(&repo.pool)
        .await
        .unwrap();

        let mut tx = repo.pool.begin().await.unwrap();
        let verdicts = evaluate_schedulability(
            &mut tx,
            &track,
            &[declaration(0, "terminal-stale")],
            &[vec![]],
            true,
        )
        .await
        .unwrap();

        let codes = verdicts[0]
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>();
        assert!(codes.contains(&"task_key_completed"));
        assert!(!codes.contains(&"context_stale_reference"));
    }

    #[tokio::test]
    async fn zero_ceiling_diagnostic_has_no_false_block_jump_target() {
        let (repo, track) = setup().await;
        sqlx::query("UPDATE tracks SET planner_task_ceiling=0 WHERE id=?1")
            .bind(&track)
            .execute(&repo.pool)
            .await
            .unwrap();
        let mut tx = repo.pool.begin().await.unwrap();
        let verdicts = evaluate_schedulability(
            &mut tx,
            &track,
            &[declaration(0, "capacity-zero")],
            &[vec![]],
            true,
        )
        .await
        .unwrap();
        let ceiling = verdicts[0]
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "planner_task_ceiling")
            .expect("ceiling diagnostic");
        assert!(ceiling.related_block_ids.is_empty());
        assert_eq!(ceiling.related_track_id.as_deref(), Some(track.as_str()));
    }

    #[tokio::test]
    async fn acceptance_1_spawn_only_projection_change_updates_row_and_changed_keys() {
        let (repo, track) = setup().await;
        let mut declaration = declaration(0, "route");
        let mut tx = repo.pool.begin().await.unwrap();
        let first = project_tasks_tx(&mut tx, &track, &[declaration.clone()], &[vec![]])
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(first.changed_keys, ["route"]);
        declaration.spawn = "sub-wave".into();
        let mut tx = repo.pool.begin().await.unwrap();
        let second = project_tasks_tx(&mut tx, &track, &[declaration], &[vec![]])
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(second.changed_keys, ["route"]);
        let spawn: String =
            sqlx::query_scalar("SELECT spawn FROM tasks WHERE track_id=?1 AND key='route'")
                .bind(&track)
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        assert_eq!(spawn, "sub-wave");
    }

    #[tokio::test]
    async fn acceptance_14b_and_22_read_dto_marks_deleted_child_and_never_exposes_spawn() {
        let (repo, track) = setup().await;
        insert_block_task(&repo, &track, "child-link", "running").await;
        sqlx::query("UPDATE tasks SET spawn='sub-wave',child_track_id='gone-child' WHERE track_id=?1 AND key='child-link'")
            .bind(&track).execute(&repo.pool).await.unwrap();
        let mut conn = repo.pool.acquire().await.unwrap();
        let verdicts = evaluate_schedulability(
            &mut conn,
            &track,
            &[declaration(0, "child-link")],
            &[vec![]],
            true,
        )
        .await
        .unwrap();
        assert_eq!(verdicts[0].child_track_id.as_deref(), Some("gone-child"));
        assert_eq!(verdicts[0].child_track_deleted, Some(true));
        let json = serde_json::to_value(&verdicts[0]).unwrap();
        assert!(!json.as_object().unwrap().contains_key("spawn"));
    }

    #[tokio::test]
    async fn ceiling_i_a_inflight_is_input_and_holds_the_upper_bound() {
        let (repo, track) = setup().await;
        insert_block_task(&repo, &track, "k1", "dispatched").await;
        let declarations = vec![declaration(0, "k2"), declaration(1, "k1")];
        let mut tx = repo.pool.begin().await.unwrap();
        let outcome = project_tasks_tx(&mut tx, &track, &declarations, &[vec![], vec![]])
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let mut conn = repo.pool.acquire().await.unwrap();
        let diagnostics =
            evaluate_schedulability(&mut conn, &track, &declarations, &[vec![], vec![]], true)
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
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE track_id=?1 AND status IN ('pending','dispatched','running','verifying')")
            .bind(&track).fetch_one(&repo.pool).await.unwrap();
        assert_eq!(
            count, 1,
            "invariant 7b: no pending row may exceed occupied capacity"
        );
    }

    #[tokio::test]
    async fn ceiling_i_b_diagnosed_pending_is_output_and_does_not_cause_jitter() {
        let (repo, track) = setup().await;
        insert_block_task(&repo, &track, "k1", "pending").await;
        let declarations = vec![declaration(0, "k1"), declaration(1, "k2")];
        let local = vec![vec![Diagnostic::new("payload", "broken")], vec![]];
        for _ in 0..2 {
            let mut tx = repo.pool.begin().await.unwrap();
            let outcome = project_tasks_tx(&mut tx, &track, &declarations, &local)
                .await
                .unwrap();
            tx.commit().await.unwrap();
            assert!(!outcome.diagnostics[0].schedulable);
            assert!(outcome.diagnostics[1].schedulable);
            let keys: Vec<(String,)> =
                sqlx::query_as("SELECT key FROM tasks WHERE track_id=?1 ORDER BY key")
                    .bind(&track)
                    .fetch_all(&repo.pool)
                    .await
                    .unwrap();
            assert_eq!(keys, vec![("k2".into(),)]);
        }
    }

    #[tokio::test]
    async fn rolled_back_material_verdict_leaves_no_row_change_or_event() {
        let (repo, track) = setup().await;
        insert_block_task(&repo, &track, "rollback", "running").await;
        let mut tx = repo.pool.begin().await.unwrap();
        let events = mark_context_material_tx(
            &mut tx,
            &format!("{track}:rollback"),
            &track,
            Vec::new(),
            "rollback proof",
        )
        .await
        .unwrap();
        assert_eq!(events.len(), 1, "the transaction produced one kernel event");
        tx.rollback().await.unwrap();

        let stale: Option<i64> =
            sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE id=?1")
                .bind(format!("{track}:rollback"))
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

    /// #1160 case 2 — two live blocks claim one key while the row is in
    /// flight. `tasks` has a single row for `(track_id, key)` and nothing in
    /// the data says which block owns that run, so neither verdict may carry
    /// it. Mirrors `resolve_task_closure`'s `DuplicateLiveKey`.
    #[tokio::test]
    async fn duplicate_live_declarations_never_stamp_run_state() {
        let (repo, track) = setup().await;
        insert_block_task(&repo, &track, "contested", "running").await;
        sqlx::query(
            "UPDATE tasks SET worker_card_id='card-contested' WHERE track_id=?1 AND key='contested'",
        )
        .bind(&track)
        .execute(&repo.pool)
        .await
        .unwrap();

        // Declarations *and* diagnostics from the production producer: a
        // contested key never reaches the projection with an empty diagnostic
        // list, it reaches it carrying `duplicate_key` on both blocks.
        let (declarations, block_local_diags) =
            calm_types::report_blocks::tasks::project_task_declarations(&[
                live_task_block(0, "contested"),
                live_task_block(1, "contested"),
            ]);
        assert!(
            block_local_diags
                .iter()
                .flatten()
                .any(|d| d.code == "duplicate_key"),
            "fixture must carry the diagnostic this document shape produces"
        );

        let mut conn = repo.pool.acquire().await.unwrap();
        let verdicts =
            evaluate_schedulability(&mut conn, &track, &declarations, &block_local_diags, true)
                .await
                .unwrap();

        assert_eq!(verdicts.len(), 2);
        for verdict in &verdicts {
            assert_eq!(
                verdict.status, None,
                "block {} must not claim the contested run",
                verdict.block_id
            );
            assert_eq!(verdict.worker_card_id, None);
        }
    }

    /// #1160 case 1 — a tombstoned block and a live block carry the same key.
    /// Exactly one live declaration exists, so it takes the run state; the
    /// tombstone must stay bare.
    #[tokio::test]
    async fn tombstoned_declaration_never_stamps_run_state() {
        let (repo, track) = setup().await;
        insert_block_task(&repo, &track, "redeclared", "running").await;
        sqlx::query(
            "UPDATE tasks SET worker_card_id='card-redeclared' WHERE track_id=?1 AND key='redeclared'",
        )
        .bind(&track)
        .execute(&repo.pool)
        .await
        .unwrap();

        let mut conn = repo.pool.acquire().await.unwrap();
        let verdicts = evaluate_schedulability(
            &mut conn,
            &track,
            &[
                declaration(0, "redeclared"),
                tombstone_declaration(1, "redeclared"),
            ],
            &[vec![], vec![]],
            true,
        )
        .await
        .unwrap();

        assert_eq!(verdicts.len(), 2);
        assert_eq!(verdicts[0].status.as_deref(), Some("running"));
        assert_eq!(
            verdicts[0].worker_card_id.as_deref(),
            Some("card-redeclared")
        );
        assert_eq!(
            verdicts[1].status, None,
            "the tombstoned block must not claim the run"
        );
        assert_eq!(verdicts[1].worker_card_id, None);
    }

    /// #1160 review ① — the run of a *hard-deleted* block still has to be
    /// readable.
    ///
    /// `evaluate_schedulability` synthesises a `block_id: ""` verdict for an
    /// in-flight row no declaration carries any more, and deliberately stamps
    /// `status` on it by hand. The first cut of the uniqueness rule then asked
    /// `live_blocks.get(key) == Some([root])`, which a deleted block can never
    /// satisfy, so everything `attach_task_read_state` adds — the worker card
    /// the `open_worker_output` action needs, the failure detail, the child
    /// track, and both reference diagnostics — silently fell off that verdict
    /// while `status` stayed. The key is not ambiguous here: it has *zero*
    /// live declarations, which `resolve_task_closure` answers `RootAbsent`,
    /// one single answer, and there is exactly one verdict to give it to.
    #[tokio::test]
    async fn deleted_declaration_run_state_survives_on_the_synthetic_verdict() {
        let (repo, track) = setup().await;
        insert_block_task(&repo, &track, "deleted-block", "running").await;
        sqlx::query(
            "UPDATE tasks SET worker_card_id='card-deleted',status_detail='running: step 2',\
             child_track_id='child-of-deleted',context_closure_truncated=1,\
             claim_context_json=?2 WHERE track_id=?1 AND key='deleted-block'",
        )
        .bind(&track)
        .bind(
            json!([{"track_id": &track, "block_id": "b_dead", "rev": 1, "hash": "h", "is_root": true},
                   {"track_id": &track, "block_id": "b_ref", "rev": 1, "hash": "h", "is_root": false}])
            .to_string(),
        )
        .execute(&repo.pool)
        .await
        .unwrap();

        let mut conn = repo.pool.acquire().await.unwrap();
        // The document no longer declares the key at all — the block was hard
        // deleted, and `delete_block` leaves no tombstone behind.
        let verdicts = evaluate_schedulability(&mut conn, &track, &[], &[], true)
            .await
            .unwrap();

        assert_eq!(verdicts.len(), 1, "one synthetic verdict for the live row");
        let verdict = &verdicts[0];
        assert_eq!(verdict.block_id, "");
        assert_eq!(verdict.status.as_deref(), Some("running"));
        assert_eq!(
            verdict.worker_card_id.as_deref(),
            Some("card-deleted"),
            "without the card id the `open_worker_output` action is a dead link"
        );
        assert_eq!(verdict.status_detail.as_deref(), Some("running: step 2"));
        assert_eq!(verdict.child_track_id.as_deref(), Some("child-of-deleted"));
        assert_eq!(verdict.child_track_deleted, Some(true));
        let codes = verdict
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>();
        assert!(
            codes.contains(&"reference_chain_too_large"),
            "diagnostics were {codes:?}"
        );
        assert!(
            codes.contains(&"context_stale_declaration"),
            "diagnostics were {codes:?}"
        );
    }

    /// #1160 review ① — the same fix must not hand a *tombstoned* block the
    /// run through the new zero-live-declarations arm. A tombstone carries a
    /// real block id, so `block_id: ""` is what separates the two.
    #[tokio::test]
    async fn tombstone_only_document_still_never_stamps_run_state() {
        let (repo, track) = setup().await;
        insert_block_task(&repo, &track, "only-tombstone", "running").await;
        sqlx::query(
            "UPDATE tasks SET worker_card_id='card-only-tombstone' WHERE track_id=?1 AND key='only-tombstone'",
        )
        .bind(&track)
        .execute(&repo.pool)
        .await
        .unwrap();

        let mut conn = repo.pool.acquire().await.unwrap();
        let verdicts = evaluate_schedulability(
            &mut conn,
            &track,
            &[tombstone_declaration(0, "only-tombstone")],
            &[vec![]],
            true,
        )
        .await
        .unwrap();

        assert_eq!(
            verdicts.len(),
            1,
            "the key is declared, so nothing is synthesised"
        );
        assert_eq!(verdicts[0].block_id, "b_0000");
        assert_eq!(verdicts[0].status, None);
        assert_eq!(verdicts[0].worker_card_id, None);
    }

    /// A task block as the report card actually stores it, so the declarations
    /// *and* the block-local diagnostics below both come out of the production
    /// producer instead of being asserted into existence. Two live blocks
    /// sharing a key really do carry `duplicate_key`; a tombstone standing
    /// beside a re-declaration really does carry
    /// `tombstone_blocks_redeclaration`. A fixture that passes `&[vec![]]`
    /// hands the projection a document shape the kernel never produces.
    fn task_block(index: usize, payload: serde_json::Value) -> ReportBlock {
        ReportBlock {
            id: format!("b_{index:04x}"),
            kind: calm_types::report_blocks::KIND_TASK.into(),
            rev: 0,
            payload,
        }
    }

    fn live_task_block(index: usize, key: &str) -> ReportBlock {
        task_block(
            index,
            json!({"key": key, "kind": "codex", "goal": format!("goal {key}"),
                   "ready": true, "declared_by": "spec", "no_gate_reason": "not needed"}),
        )
    }

    fn tombstone_task_block(index: usize, key: &str) -> ReportBlock {
        task_block(
            index,
            json!({"key": key, "tombstone": {}, "tombstoned_by": "user", "declared_by": "spec"}),
        )
    }

    /// The rationale carried by each `task.context_advanced` this projection
    /// emitted, which is the only field the folds under test decide.
    fn context_advanced_rationales(outcome: &TaskProjectionOutcome) -> Vec<String> {
        outcome
            .kernel_events
            .iter()
            .map(|(_, _, event)| match event {
                Event::TaskContextAdvanced {
                    rationale, verdict, ..
                } => {
                    assert_eq!(verdict, "material");
                    rationale.clone()
                }
                other => panic!("unexpected kernel event {other:?}"),
            })
            .collect()
    }

    async fn project_blocks(
        repo: &super::super::SqlxRepo,
        track: &str,
        blocks: &[ReportBlock],
    ) -> TaskProjectionOutcome {
        let (declarations, block_local_diags) =
            calm_types::report_blocks::tasks::project_task_declarations(blocks);
        let mut tx = repo.pool.begin().await.unwrap();
        let outcome = project_tasks_tx(&mut tx, track, &declarations, &block_local_diags)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        outcome
    }

    /// #1160 — `declaration_by_key` used to be a last-wins `BTreeMap`, so the
    /// same document content produced different kernel events depending on
    /// whether the tombstone block sat before or after the live one.
    ///
    /// **This is the negative half of a pair.** On its own an equality between
    /// two orders proves nothing — any constant predicate satisfies it — so it
    /// asserts the *value*: a key that still has a live declaration is not
    /// removed, therefore zero events, in both orders.
    /// `tombstone_only_document_advances_the_in_flight_context` is the positive
    /// half, and the two together pin the predicate rather than its symmetry.
    #[tokio::test]
    async fn tombstone_beside_live_redeclaration_emits_no_kernel_event_in_either_order() {
        async fn rationales_for(blocks: [ReportBlock; 2]) -> Vec<String> {
            let (repo, track) = setup().await;
            insert_block_task(&repo, &track, "reordered", "running").await;
            let outcome = project_blocks(&repo, &track, &blocks).await;
            context_advanced_rationales(&outcome)
        }

        let live = live_task_block(0, "reordered");
        let tombstone = tombstone_task_block(1, "reordered");
        // The document really is diagnosed — the projection is fed the shape
        // the kernel produces for it, not an empty diagnostic list.
        let (_, diags) = calm_types::report_blocks::tasks::project_task_declarations(&[
            live.clone(),
            tombstone.clone(),
        ]);
        assert!(
            diags
                .iter()
                .flatten()
                .any(|d| d.code == "tombstone_blocks_redeclaration"),
            "fixture must carry the diagnostic this document shape produces"
        );

        let tombstone_first = rationales_for([tombstone.clone(), live.clone()]).await;
        let live_first = rationales_for([live, tombstone]).await;
        assert_eq!(
            tombstone_first,
            Vec::<String>::new(),
            "a live re-declaration means the key was not removed"
        );
        assert_eq!(
            live_first,
            Vec::<String>::new(),
            "a live re-declaration means the key was not removed"
        );
    }

    /// #1160 — the positive half. Every live declaration of the key is gone,
    /// so the in-flight row's frozen context is material and the kernel must
    /// say so.
    #[tokio::test]
    async fn tombstone_only_document_advances_the_in_flight_context() {
        let (repo, track) = setup().await;
        insert_block_task(&repo, &track, "reordered", "running").await;
        let outcome = project_blocks(&repo, &track, &[tombstone_task_block(0, "reordered")]).await;
        assert_eq!(
            context_advanced_rationales(&outcome),
            ["task declaration block was removed"]
        );
        let stale: Option<i64> =
            sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE id=?1")
                .bind(format!("{track}:reordered"))
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        assert!(stale.is_some(), "the row itself carries the verdict");
    }

    /// One live block on the contested key `contended`, carrying whichever of
    /// the two declaration edges the caller wants withdrawn.
    fn contended_task_block(index: usize, ready: bool, released_by_user: bool) -> ReportBlock {
        task_block(
            index,
            json!({"key": "contended", "kind": "codex", "goal": "goal contended",
                   "ready": ready, "released_by_user": released_by_user,
                   "declared_by": "spec", "no_gate_reason": "not needed"}),
        )
    }

    /// Project `blocks` over an in-flight row whose stored declaration carried
    /// *both* edges, under the policy that makes the release edge exist at all,
    /// and return the `task.context_advanced` rationales.
    async fn withdrawal_rationales_for(blocks: [ReportBlock; 2]) -> Vec<String> {
        let (repo, track) = setup().await;
        insert_block_task(&repo, &track, "contended", "running").await;
        // The row was established by a declaration that carried *both*
        // edges; withdrawing either one is what the fold has to name.
        sqlx::query(
            "UPDATE tasks SET decl_ready=1,decl_released_by_user=1 WHERE track_id=?1 AND key='contended'",
        )
        .bind(&track)
        .execute(&repo.pool)
        .await
        .unwrap();
        // The release edge only exists under this policy.
        sqlx::query("UPDATE tracks SET automation_policy='declare-and-wait' WHERE id=?1")
            .bind(&track)
            .execute(&repo.pool)
            .await
            .unwrap();
        let outcome = project_blocks(&repo, &track, &blocks).await;
        context_advanced_rationales(&outcome)
    }

    /// #1160 review ② — the withdrawal rationale used to be `verdict_indexes[0]`,
    /// i.e. the first block in document order, so two blocks withdrawing
    /// *different* edges sent a different rationale for `[X, Y]` than for
    /// `[Y, X]` — the same block-order bug this change set removes elsewhere.
    ///
    /// Both orders expect the *same* rationale here, which is the point of the
    /// case but also its blind spot: an implementation that ignored the edges
    /// and answered `Ready` unconditionally would satisfy it. That mutant is
    /// killed by
    /// `withdrawal_rationale_names_the_release_edge_when_it_is_the_only_one`
    /// below, whose expectation is the *other* variant.
    #[tokio::test]
    async fn withdrawal_rationale_is_edge_ordered_not_block_ordered() {
        // X dropped `ready` → `WithdrawalEdge::Ready`.
        let x = contended_task_block(0, false, true);
        // Y kept `ready` but dropped the user release → `ReleasedByUser`.
        let y = contended_task_block(1, true, false);
        let (_, diags) =
            calm_types::report_blocks::tasks::project_task_declarations(&[x.clone(), y.clone()]);
        assert!(
            diags.iter().flatten().any(|d| d.code == "duplicate_key"),
            "two live blocks on one key really do carry duplicate_key"
        );

        let expected = ["task declaration ready was withdrawn".to_string()];
        assert_eq!(
            withdrawal_rationales_for([x.clone(), y.clone()]).await,
            expected
        );
        assert_eq!(withdrawal_rationales_for([y, x]).await, expected);
    }

    /// #1160 review ② (second round) — the counter-example that makes the fold
    /// falsifiable. Both live blocks keep `ready` and drop only the user
    /// release, so `Ready` is not an edge *any* block withdrew and the only
    /// honest rationale is the release one. A fold hard-coded to
    /// `Some(WithdrawalEdge::Ready)` — which the paired case above cannot see —
    /// reports "ready was withdrawn" about a declaration that is still ready.
    #[tokio::test]
    async fn withdrawal_rationale_names_the_release_edge_when_it_is_the_only_one() {
        let x = contended_task_block(0, true, false);
        let y = contended_task_block(1, true, false);
        let expected = ["task declaration user release was withdrawn".to_string()];
        assert_eq!(
            withdrawal_rationales_for([x.clone(), y.clone()]).await,
            expected
        );
        assert_eq!(withdrawal_rationales_for([y, x]).await, expected);
    }
}
