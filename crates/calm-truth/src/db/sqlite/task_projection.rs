use std::collections::{BTreeMap, BTreeSet};

use calm_types::event::{Event, EventScope, TaskContextChangedRef, TaskContextRef};
use calm_types::ids::{ActorId, WaveId};
use calm_types::report_blocks::tasks::{
    Diagnostic, GateInput, TASK_BLOCKING_DIAGNOSTIC_PATHS, TaskDeclaration, diagnostic_args,
    gate_rule_violations, json_eq, opt_json_eq, unknown_deps,
};
use calm_types::report_links::{parse_destination, scan_links};
use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, Transaction};
use utoipa::ToSchema;

use crate::error::{CalmError, Result};
use crate::model::now_ms;

const DEFAULT_SPEC_TASK_CEILING: i64 = 32;
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
    #[serde(skip)]
    #[schema(ignore)]
    pub withdrawal: Option<WithdrawalEdge>,
}

#[derive(sqlx::FromRow)]
struct TaskReadState {
    key: String,
    status: String,
    gate_result_json: Option<String>,
    worker_card_id: Option<String>,
    context_stale_at_ms: Option<i64>,
    context_closure_truncated: i64,
    claim_context_json: Option<String>,
}

/// Read-time task-table projection for report rendering. This deliberately
/// stays beside the existing projection query instead of introducing a live
/// block data-source abstraction.
pub async fn attach_task_read_state_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &str,
    verdicts: &mut [BlockVerdict],
) -> Result<()> {
    let rows: Vec<TaskReadState> = sqlx::query_as(
        "SELECT key,status,gate_result_json,worker_card_id,context_stale_at_ms,context_closure_truncated,claim_context_json FROM tasks WHERE wave_id=?1",
    )
    .bind(wave_id)
    .fetch_all(&mut **tx)
    .await?;
    let by_key: BTreeMap<_, _> = rows.into_iter().map(|row| (row.key.clone(), row)).collect();
    for verdict in verdicts {
        let Some(row) = by_key.get(&verdict.key) else {
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
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct TaskProjectionOutcome {
    pub changed_keys: Vec<String>,
    pub diagnostics: Vec<BlockVerdict>,
    pub kernel_events: Vec<(ActorId, EventScope, Event)>,
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
    let effective_wait = configured_policy.as_deref() == Some("declare-and-wait");
    // unknown_deps knows every in-flight key in the wave, including rows
    // backfilled as legacy.  Ceiling occupancy is intentionally narrower.
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
            .get(declaration.block_index.unwrap_or(usize::MAX))
            .cloned()
            .unwrap_or_default();
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
                    .bind(dst_wave).bind(dst_block).fetch_optional(&mut **tx).await?
            } else if let Some(card_id) = reference
                .strip_prefix("neige://card/")
                .filter(|id| !id.is_empty() && !id.contains('/'))
            {
                sqlx::query_as("SELECT w.cove_id,c.kind,1 FROM cards card JOIN waves w ON w.id=card.wave_id JOIN coves c ON c.id=w.cove_id WHERE card.id=?1")
                    .bind(card_id).fetch_optional(&mut **tx).await?
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
                Some(wave_id.into()),
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
    .fetch_all(&mut **tx)
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
    let orphaned_inflight: Vec<(String, String)> = sqlx::query_as(
        "SELECT key,status FROM tasks WHERE wave_id=?1 AND origin='block' AND status IN ('dispatched','running','verifying')",
    )
    .bind(wave_id)
    .fetch_all(&mut **tx)
    .await?;
    for (key, status) in orphaned_inflight {
        if !declared_keys.contains(key.as_str()) {
            verdicts.push(BlockVerdict {
                block_id: String::new(),
                diagnostics: vec![withdrawal_diagnostic(&key, &status)],
                key,
                schedulable: false,
                status: Some(status),
                gate_result: None,
                worker_card_id: None,
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
    for index in candidates.into_iter().skip(capacity) {
        verdicts[index].diagnostics.push(Diagnostic::coded(
            "spec_task_ceiling",
            "key",
            diagnostic_args([
                ("ceiling", serde_json::Value::from(ceiling)),
                ("occupied", serde_json::Value::from(occupied)),
                (
                    "admission_order",
                    serde_json::Value::String("document order, then key".into()),
                ),
            ]),
            admitted_ids.clone(),
            Some(wave_id.into()),
            Some("raise_spec_task_ceiling".into()),
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
    let verdicts = evaluate_schedulability_tx(tx, wave_id, declarations, block_local_diags).await?;
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
                // evaluate_schedulability_tx; this branch only covers deleted blocks.
                if !verdict_index_by_key.contains_key(key) {
                    verdicts.push(BlockVerdict {
                        block_id: String::new(),
                        key: key.clone(),
                        diagnostics: vec![diagnostic],
                        schedulable: false,
                        status: Some(status.clone()),
                        gate_result: None,
                        worker_card_id: None,
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
            "INSERT INTO tasks(id,wave_id,key,kind,goal,context_json,acceptance_criteria,cwd,depends_on_json,priority,gate_json,status,declared_by,origin,decl_ready,decl_released_by_user,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'pending',?12,'block',?13,?14,?15,?15) ON CONFLICT(wave_id,key) DO UPDATE SET kind=excluded.kind,goal=excluded.goal,context_json=excluded.context_json,acceptance_criteria=excluded.acceptance_criteria,cwd=excluded.cwd,depends_on_json=excluded.depends_on_json,priority=excluded.priority,gate_json=excluded.gate_json,declared_by=excluded.declared_by,origin='block',decl_ready=excluded.decl_ready,decl_released_by_user=excluded.decl_released_by_user,updated_at_ms=excluded.updated_at_ms WHERE tasks.status='pending' AND (tasks.origin='block' OR tasks.origin='legacy') AND (tasks.kind IS NOT excluded.kind OR tasks.goal IS NOT excluded.goal OR tasks.context_json IS NOT excluded.context_json OR tasks.acceptance_criteria IS NOT excluded.acceptance_criteria OR tasks.cwd IS NOT excluded.cwd OR tasks.depends_on_json IS NOT excluded.depends_on_json OR tasks.priority IS NOT excluded.priority OR tasks.gate_json IS NOT excluded.gate_json OR tasks.declared_by IS NOT excluded.declared_by OR tasks.origin IS NOT 'block' OR tasks.decl_ready IS NOT excluded.decl_ready OR tasks.decl_released_by_user IS NOT excluded.decl_released_by_user)",
        )
        .bind(&id).bind(wave_id).bind(&declaration.key).bind(&declaration.kind)
        .bind(&declaration.goal).bind(context).bind(&declaration.acceptance)
        .bind(&declaration.cwd).bind(depends).bind(declaration.priority).bind(gate)
        .bind(&declaration.declared_by).bind(i64::from(declaration.ready))
        .bind(i64::from(declaration.released_by_user)).bind(now).execute(&mut **tx).await?;
        if result.rows_affected() != 0 {
            changed.insert(declaration.key.clone());
        }
    }
    Ok(TaskProjectionOutcome {
        changed_keys: changed.into_iter().collect(),
        diagnostics: verdicts,
        kernel_events,
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
        let mut verdicts =
            evaluate_schedulability_tx(&mut tx, &wave, &[declaration(0, "read-state")], &[vec![]])
                .await
                .unwrap();
        attach_task_read_state_tx(&mut tx, &wave, &mut verdicts)
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
        let mut verdicts =
            evaluate_schedulability_tx(&mut tx, &wave, &[declaration(0, "cross-wave")], &[vec![]])
                .await
                .unwrap();
        attach_task_read_state_tx(&mut tx, &wave, &mut verdicts)
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
        let verdicts = evaluate_schedulability_tx(
            &mut tx,
            &wave,
            &[declaration(0, "capacity-zero")],
            &[vec![]],
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
    async fn ceiling_i_a_inflight_is_input_and_holds_the_upper_bound() {
        let (repo, wave) = setup().await;
        insert_block_task(&repo, &wave, "k1", "dispatched").await;
        let declarations = vec![declaration(0, "k2"), declaration(1, "k1")];
        let mut tx = repo.pool.begin().await.unwrap();
        let outcome = project_tasks_tx(&mut tx, &wave, &declarations, &[vec![], vec![]])
            .await
            .unwrap();
        let mut diagnostics = outcome.diagnostics.clone();
        attach_task_read_state_tx(&mut tx, &wave, &mut diagnostics)
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
