//! `calm.plan.*` — the spec card's durable per-wave task plan
//! (issue #644, PR-A).
//!
//! The plan lives in the `tasks` table (migration 0041) and is the
//! source of truth for the upcoming kernel scheduler (PR-B) and
//! verification gate (PR-C). In this slice the plan is **inert**:
//! nothing reads it, no dispatch happens, and `calm.task.dispatch`
//! is untouched.
//!
//! ## Tool surface
//!
//! * `calm.plan.upsert` — Spec-only batch write. Whole-batch atomic in
//!   one immediate eventized tx: every task validates (design §4.1
//!   rules 1-5, 7, 8) or nothing lands. Per-key outcomes are
//!   `created` / `updated` / `unchanged`.
//! * `calm.plan.cancel` — Spec-only, pending-only (`§3.1`): canceling
//!   an already-`canceled` task is idempotent success; an in-flight
//!   task returns the 409-style refusal.
//! * `calm.plan.list` — Spec-only read. Gate **commands are not
//!   echoed** (only `{present, steps: [names]}`) — workers must never
//!   see gate bodies, and the listing layer enforces that shape even
//!   for spec callers so a future role widening can't leak them (§6.7).
//!
//! ## Slice guards (PR-A)
//!
//! * Rule 8: any task declaring `gate` is rejected — a declared gate
//!   must always be enforced, and the gate runner only lands in PR-C.
//!   The `gate_json` column and the rule-7 shape validation still ship
//!   here; only acceptance is deferred.
//! * Rule 6 (`require_task_gates` / `no_gate_reason`) is NOT enforced
//!   here (PR-C-only). `no_gate_reason` is accepted and recorded into
//!   `context_json` for auditability, but drives nothing.
//! * `kind = "claude"` is rejected ("not yet supported") — no claude
//!   worker adapter exists and the column CHECK omits the value.
//!
//! ## Scope construction
//!
//! Wave identity is implicit from the calling card (same resolve chain
//! as `wave_state.rs`); it is never a parameter. The `plan.updated`
//! event is wave-scoped with actor `AiSpec`; the in-tx role gate
//! refuses it from worker actors (`role_gate.rs` section 2.5).

use crate::db::sqlite::{task_cancel_tx, task_insert_tx, task_update_pending_tx, tasks_by_wave_tx};
use crate::db::write_with_actor_events_typed;
use crate::error::CalmError;
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    read_only_annotations, require_role, role_gated_write_annotations,
};
use crate::mcp_server::tools::lifecycle_args::{
    lifecycle_schema, message_schema, parse_write_args,
};
use crate::model::{CardRole, Task, TaskKind, TaskStatus, Wave, now_ms};
use crate::wave_lifecycle::{apply_requested_transition_in_tx, auto_promote_draft_in_tx};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

pub const TOOL_PLAN_UPSERT: &str = "calm.plan.upsert";
pub const TOOL_PLAN_CANCEL: &str = "calm.plan.cancel";
pub const TOOL_PLAN_LIST: &str = "calm.plan.list";

/// Gate timeout defaults/caps (design §4.1 rule 7). Validated here even
/// though rule 8 rejects every declared gate until PR-C — the shape
/// contract is part of this slice so PR-C only deletes the guard.
const GATE_TIMEOUT_DEFAULT_SECS: i64 = 1800;
const GATE_TIMEOUT_MAX_SECS: i64 = 7200;

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(plan_upsert_descriptor(), wrap(plan_upsert));
    registry.register(plan_cancel_descriptor(), wrap(plan_cancel));
    registry.register(plan_list_descriptor(), wrap(plan_list));
}

/// Common wrapper that turns a typed async fn into the boxed-future
/// `ToolHandler` the registry expects. Mirrors `emit::wrap`.
fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture { Box::pin(f(ctx, identity, args)) })
}

// ---------------------------------------------------------------------------
// Input shapes + per-task validation (design §4.1)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanTaskInput {
    key: String,
    kind: String,
    goal: String,
    #[serde(default)]
    context: Option<Value>,
    #[serde(default)]
    acceptance_criteria: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    priority: Option<i64>,
    #[serde(default)]
    gate: Option<GateInput>,
    #[serde(default)]
    no_gate_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GateInput {
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout_secs: Option<i64>,
    steps: Vec<GateStepInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GateStepInput {
    name: String,
    cmd: String,
}

/// A batch entry after field-level validation + normalization. The
/// stored row is a pure function of this struct, which is what makes
/// the rule-5 "byte-identical normalized payload" idempotency check
/// well-defined.
#[derive(Debug, Clone)]
struct NormalizedTask {
    key: String,
    kind: TaskKind,
    goal: String,
    /// Canonical serialization (`serde_json` sorts object keys), with
    /// `no_gate_reason` folded in when supplied.
    context_json: String,
    acceptance_criteria: Option<String>,
    cwd: Option<String>,
    /// Sorted + deduped — dependency order is set semantics.
    depends_on: Vec<String>,
    priority: i64,
    /// Always `None` in PR-A: rule 8 rejects every declared gate before
    /// normalization completes. The field exists so PR-C's guard
    /// removal does not reshape this struct.
    gate_json: Option<String>,
}

/// Rule 1 key shape: `^[a-z0-9][a-z0-9._-]{0,63}$` (1..=64 chars).
/// Hand-rolled — the crate has no regex dependency and the grammar is
/// trivial.
fn key_is_valid(key: &str) -> bool {
    let bytes = key.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

/// Rule 7 cwd shape: absolute, non-empty, no ASCII control characters
/// (same check as `codex_adapter::normalize_codex_create_request`).
fn validate_abs_path(field: &str, key: &str, raw: &str) -> Result<String, String> {
    if raw.chars().any(|c| c.is_ascii_control()) {
        return Err(format!(
            "task {key}: {field} must not contain ASCII control characters"
        ));
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "task {key}: {field} must be non-empty when present"
        ));
    }
    if !trimmed.starts_with('/') {
        return Err(format!(
            "task {key}: {field} must be an absolute path (got `{trimmed}`)"
        ));
    }
    Ok(trimmed.to_string())
}

/// Field-level validation for one batch entry (rules 1 partial, 2, 7,
/// 8). Returns the normalized form the resolver + row writer consume.
fn normalize_task_input(input: PlanTaskInput) -> Result<NormalizedTask, String> {
    let key = input.key;
    if !key_is_valid(&key) {
        return Err(format!(
            "invalid task key `{key}`: must match ^[a-z0-9][a-z0-9._-]{{0,63}}$ \
             (lowercase, 1-64 chars)"
        ));
    }

    // Rule 2 — kind vocabulary. `claude` gets the explicit forward-
    // looking refusal; anything else is a typo.
    let kind = match input.kind.as_str() {
        "codex" => TaskKind::Codex,
        "terminal" => TaskKind::Terminal,
        "claude" => {
            return Err(format!(
                "task {key}: kind `claude` is not yet supported (no claude worker \
                 dispatch adapter exists); use `codex` or `terminal`"
            ));
        }
        other => {
            return Err(format!(
                "task {key}: unknown kind `{other}` (expected `codex` or `terminal`)"
            ));
        }
    };

    let goal = input.goal;
    if goal.trim().is_empty() {
        return Err(format!("task {key}: `goal` must be non-empty"));
    }

    // Rule 7 — cwd absolute when present.
    let cwd = match input.cwd.as_deref() {
        None => None,
        Some(raw) => Some(validate_abs_path("cwd", &key, raw)?),
    };

    // Rule 7 — gate shape (validated even though rule 8 rejects below,
    // so a malformed gate surfaces as the precise shape error).
    if let Some(gate) = &input.gate {
        validate_gate_shape(&key, gate)?;
        // Rule 8 — slice guard (PR-A/PR-B window): a declared gate is
        // always enforced, and enforcement lands with task-verify
        // (PR-C). Until then, declaring one is refused outright.
        return Err(format!(
            "task {key}: gates are not yet enforced (lands with task-verify); \
             resubmit without gate or wait for the gate slice"
        ));
    }

    // Rule 6 escape-hatch bookkeeping (enforcement is PR-C-only):
    // `no_gate_reason` is recorded into `context_json` so the audit
    // trail carries it from day one.
    let context = input.context.unwrap_or(Value::Null);
    let context = match input.no_gate_reason {
        None => context,
        Some(reason) => match context {
            Value::Null => json!({ "no_gate_reason": reason }),
            Value::Object(mut map) => {
                map.insert("no_gate_reason".into(), Value::String(reason));
                Value::Object(map)
            }
            other => {
                return Err(format!(
                    "task {key}: `no_gate_reason` requires `context` to be an object \
                     (or omitted) so the reason can be recorded; got {}",
                    crate::mcp_server::tools::lifecycle_args::shape_of(&other)
                ));
            }
        },
    };
    let context_json =
        serde_json::to_string(&context).map_err(|e| format!("task {key}: context: {e}"))?;

    let mut depends_on = input.depends_on;
    depends_on.sort();
    depends_on.dedup();

    Ok(NormalizedTask {
        key,
        kind,
        goal,
        context_json,
        acceptance_criteria: input.acceptance_criteria,
        cwd,
        depends_on,
        priority: input.priority.unwrap_or(0),
        gate_json: None,
    })
}

/// Rule 7 gate shape: non-empty `steps`, non-empty `name`/`cmd` with no
/// ASCII control characters (same check as the codex-create cwd
/// normalization), absolute `cwd` when present, `timeout_secs` in
/// `1..=7200` (default 1800).
fn validate_gate_shape(key: &str, gate: &GateInput) -> Result<(), String> {
    if gate.steps.is_empty() {
        return Err(format!("task {key}: gate.steps must be non-empty"));
    }
    for (i, step) in gate.steps.iter().enumerate() {
        if step.name.trim().is_empty() {
            return Err(format!(
                "task {key}: gate.steps[{i}].name must be non-empty"
            ));
        }
        if step.cmd.trim().is_empty() {
            return Err(format!("task {key}: gate.steps[{i}].cmd must be non-empty"));
        }
        if step.name.chars().any(|c| c.is_ascii_control())
            || step.cmd.chars().any(|c| c.is_ascii_control())
        {
            return Err(format!(
                "task {key}: gate.steps[{i}] must not contain ASCII control characters"
            ));
        }
    }
    if let Some(raw) = gate.cwd.as_deref() {
        validate_abs_path("gate.cwd", key, raw)?;
    }
    let timeout = gate.timeout_secs.unwrap_or(GATE_TIMEOUT_DEFAULT_SECS);
    if timeout <= 0 || timeout > GATE_TIMEOUT_MAX_SECS {
        return Err(format!(
            "task {key}: gate.timeout_secs must be in 1..={GATE_TIMEOUT_MAX_SECS} \
             (default {GATE_TIMEOUT_DEFAULT_SECS}), got {timeout}"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Batch resolution (rules 1 uniqueness, 3, 4, 5)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanOutcome {
    Created,
    Updated,
    Unchanged,
}

impl PlanOutcome {
    fn as_str(self) -> &'static str {
        match self {
            PlanOutcome::Created => "created",
            PlanOutcome::Updated => "updated",
            PlanOutcome::Unchanged => "unchanged",
        }
    }
}

/// Pure resolver for one upsert batch against the wave's current plan.
/// Returns per-entry outcomes in batch order, or the first validation
/// error. Called twice on the write path — once outside the tx for the
/// all-`unchanged` no-write short-circuit, once inside the tx against
/// in-tx state — which is exactly why it must stay side-effect-free.
fn resolve_plan_batch(
    existing: &[Task],
    batch: &[NormalizedTask],
) -> Result<Vec<PlanOutcome>, String> {
    // Rule 1 (uniqueness half) — duplicate keys within the batch.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for t in batch {
        if !seen.insert(t.key.as_str()) {
            return Err(format!("duplicate key `{}` in batch", t.key));
        }
    }

    let existing_by_key: HashMap<&str, &Task> =
        existing.iter().map(|t| (t.key.as_str(), t)).collect();

    // Rule 3 — unknown deps: every dep names an existing wave task or a
    // task in the same batch.
    let known: BTreeSet<&str> = existing_by_key
        .keys()
        .copied()
        .chain(batch.iter().map(|t| t.key.as_str()))
        .collect();
    for t in batch {
        for dep in &t.depends_on {
            if !known.contains(dep.as_str()) {
                return Err(format!(
                    "task {}: unknown dependency `{dep}` (must name an existing wave \
                     task or a task in this batch)",
                    t.key
                ));
            }
        }
    }

    // Rule 5 — mutability: pending rows are freely revisable; a
    // non-pending row only tolerates a byte-identical normalized
    // payload (idempotent retry).
    let mut outcomes = Vec::with_capacity(batch.len());
    for t in batch {
        let outcome = match existing_by_key.get(t.key.as_str()) {
            None => PlanOutcome::Created,
            Some(row) => {
                if task_payload_equal(row, t) {
                    PlanOutcome::Unchanged
                } else if row.status == TaskStatus::Pending {
                    PlanOutcome::Updated
                } else {
                    return Err(format!(
                        "task {} already dispatched; insert a new task instead",
                        t.key
                    ));
                }
            }
        };
        outcomes.push(outcome);
    }

    // Rule 4 — cycle detection over the post-upsert view: existing
    // tasks' frozen deps plus the batch's (which override same-key
    // pending rows).
    let mut graph: BTreeMap<&str, Vec<String>> = existing
        .iter()
        .map(|t| (t.key.as_str(), t.depends_on()))
        .collect();
    for t in batch {
        graph.insert(t.key.as_str(), t.depends_on.clone());
    }
    if let Some(cycle) = find_cycle(&graph) {
        return Err(format!("dependency cycle: {}", cycle.join(" -> ")));
    }

    Ok(outcomes)
}

/// Rule 5 equality: the stored row vs. the candidate's normalized
/// payload. JSON columns compare as parsed `Value`s so formatting can
/// never produce a spurious `updated`.
fn task_payload_equal(row: &Task, cand: &NormalizedTask) -> bool {
    let json_eq = |a: &str, b: &str| -> bool {
        match (
            serde_json::from_str::<Value>(a),
            serde_json::from_str::<Value>(b),
        ) {
            (Ok(av), Ok(bv)) => av == bv,
            _ => a == b,
        }
    };
    let opt_json_eq = |a: &Option<String>, b: &Option<String>| -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => json_eq(a, b),
            _ => false,
        }
    };
    let mut row_deps = row.depends_on();
    row_deps.sort();
    row_deps.dedup();

    row.kind == cand.kind
        && row.goal == cand.goal
        && json_eq(&row.context_json, &cand.context_json)
        && row.acceptance_criteria == cand.acceptance_criteria
        && row.cwd == cand.cwd
        && row_deps == cand.depends_on
        && row.priority == cand.priority
        && opt_json_eq(&row.gate_json, &cand.gate_json)
}

/// DFS cycle finder. Returns the cycle path (first node repeated at the
/// end, e.g. `["a", "b", "a"]`) or `None`. Edges to keys outside the
/// graph are ignored — rule 3 already rejected unknown deps for batch
/// entries, and existing rows' deps were validated at their own write.
fn find_cycle(graph: &BTreeMap<&str, Vec<String>>) -> Option<Vec<String>> {
    const WHITE: u8 = 0;
    const GRAY: u8 = 1;
    const BLACK: u8 = 2;

    fn visit(
        node: &str,
        graph: &BTreeMap<&str, Vec<String>>,
        color: &mut HashMap<String, u8>,
        path: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        color.insert(node.to_string(), GRAY);
        path.push(node.to_string());
        for dep in graph.get(node).into_iter().flatten() {
            if !graph.contains_key(dep.as_str()) {
                continue;
            }
            match color.get(dep.as_str()).copied().unwrap_or(WHITE) {
                GRAY => {
                    // Back-edge — slice the current path from the first
                    // occurrence of `dep` and close the loop.
                    let start = path.iter().position(|k| k == dep).unwrap_or(0);
                    let mut cycle: Vec<String> = path[start..].to_vec();
                    cycle.push(dep.clone());
                    return Some(cycle);
                }
                WHITE => {
                    if let Some(cycle) = visit(dep, graph, color, path) {
                        return Some(cycle);
                    }
                }
                _ => {}
            }
        }
        path.pop();
        color.insert(node.to_string(), BLACK);
        None
    }

    let mut color: HashMap<String, u8> = HashMap::new();
    let mut path: Vec<String> = Vec::new();
    for node in graph.keys() {
        if color.get(*node).copied().unwrap_or(WHITE) == WHITE
            && let Some(cycle) = visit(node, graph, &mut color, &mut path)
        {
            return Some(cycle);
        }
    }
    None
}

/// Build the fresh-row form of a normalized batch entry. Updates reuse
/// the same struct and let `task_update_pending_tx` pick the revisable
/// columns out of it.
fn task_row_from_normalized(wave_id: &str, t: &NormalizedTask, now: i64) -> Task {
    Task {
        id: format!("{wave_id}:{}", t.key),
        wave_id: wave_id.to_string(),
        key: t.key.clone(),
        kind: t.kind,
        goal: t.goal.clone(),
        context_json: t.context_json.clone(),
        acceptance_criteria: t.acceptance_criteria.clone(),
        cwd: t.cwd.clone(),
        depends_on_json: serde_json::to_string(&t.depends_on).unwrap_or_else(|_| "[]".into()),
        priority: t.priority,
        gate_json: t.gate_json.clone(),
        status: TaskStatus::Pending,
        status_detail: None,
        worker_card_id: None,
        gate_result_json: None,
        gate_attempt: 0,
        gate_pid: None,
        gate_pid_starttime: None,
        gate_pid_boot_id: None,
        created_at_ms: now,
        updated_at_ms: now,
        finished_at_ms: None,
    }
}

// ---------------------------------------------------------------------------
// calm.plan.upsert
// ---------------------------------------------------------------------------

fn plan_upsert_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_PLAN_UPSERT.into(),
        description: "Spec-only: create or revise tasks in the wave's durable plan. \
             Batch is whole-batch atomic: every task validates or nothing lands. \
             Tasks are editable while `pending`; re-sending an identical task is an \
             idempotent `unchanged`. `depends_on` names sibling task keys; the kernel \
             schedules ready tasks once the scheduler slice lands. `message` is \
             required and persisted as `agent_message` on the `plan.updated` event. \
             Optional `lifecycle` drives the wave state machine in the same atomic \
             write. NOTE: `gate` is not yet accepted (verification lands with the \
             task-verify slice)."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["tasks", "message"],
            "properties": {
                "tasks": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "required": ["key", "kind", "goal"],
                        "properties": {
                            "key": {
                                "type": "string",
                                "pattern": "^[a-z0-9][a-z0-9._-]{0,63}$",
                                "description": "Stable per-wave task key; also the completion correlation id."
                            },
                            "kind": { "type": "string", "enum": ["codex", "terminal"] },
                            "goal": { "type": "string", "minLength": 1, "description": "codex: goal text; terminal: the command" },
                            "context": { "description": "Optional, any JSON; forwarded to the worker verbatim." },
                            "acceptance_criteria": { "type": ["string", "null"] },
                            "cwd": { "type": ["string", "null"], "description": "Absolute path; terminal worker cwd + future gate default cwd." },
                            "depends_on": { "type": "array", "items": { "type": "string" }, "description": "Sibling task keys that must be done first." },
                            "priority": { "type": "integer", "description": "Higher schedules first; default 0." },
                            "gate": { "type": "object", "description": "NOT yet accepted — gates land with the task-verify slice." },
                            "no_gate_reason": { "type": "string", "description": "Audit note for an ungated codex task; recorded into context." }
                        }
                    }
                },
                "message": message_schema(),
                "lifecycle": lifecycle_schema()
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Spec],
    }
}

async fn plan_upsert(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    let write_args = parse_write_args(&args, "plan_upsert")?;

    let raw_tasks = args
        .get("tasks")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .ok_or_else(|| {
            RpcError::invalid_params("plan_upsert: `tasks` must be a non-empty array")
        })?;

    let mut batch: Vec<NormalizedTask> = Vec::with_capacity(raw_tasks.len());
    for (i, raw) in raw_tasks.iter().enumerate() {
        let input: PlanTaskInput = serde_json::from_value(raw.clone())
            .map_err(|e| RpcError::invalid_params(format!("plan_upsert: tasks[{i}]: {e}")))?;
        let normalized = normalize_task_input(input)
            .map_err(|m| RpcError::invalid_params(format!("plan_upsert: {m}")))?;
        batch.push(normalized);
    }

    let (_card, wave) = resolve_wave_for_identity(&ctx, &identity).await?;
    let wave_id_str = wave.id.as_str().to_string();

    // Pre-tx resolve: validates the batch against current state and
    // short-circuits a pure idempotent retry (all `unchanged`, no
    // lifecycle request) without writing a row or emitting an event.
    // The tx below re-resolves against in-tx state, so this read is a
    // fast path, not the correctness boundary.
    let existing = ctx
        .repo
        .tasks_by_wave(&wave_id_str)
        .await
        .map_err(|e| RpcError::internal(format!("plan_upsert: tasks_by_wave: {e}")))?;
    let pre_outcomes = resolve_plan_batch(&existing, &batch)
        .map_err(|m| RpcError::invalid_params(format!("plan_upsert: {m}")))?;
    if write_args.lifecycle.is_none() && pre_outcomes.iter().all(|o| *o == PlanOutcome::Unchanged) {
        return Ok(results_json(&batch, &pre_outcomes));
    }

    let actor = identity.to_actor_id();
    let scope = EventScope::Wave {
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    let wave_id_typed = wave.id.clone();
    let message = write_args.message.clone();
    let lifecycle = write_args.lifecycle;

    let result = write_with_actor_events_typed::<Vec<PlanOutcome>, _>(
        ctx.repo.as_ref(),
        None,
        &ctx.events,
        &ctx.write,
        move |tx| {
            let batch = batch.clone();
            let wave_id_str = wave_id_str.clone();
            let wave_id_typed = wave_id_typed.clone();
            let actor = actor.clone();
            let scope = scope.clone();
            let message = message.clone();
            Box::pin(async move {
                // Re-resolve against in-tx state — the whole batch
                // validates against the rows it is about to join, and a
                // concurrent writer between the pre-check and this tx
                // surfaces as a clean rollback, never a half-applied
                // batch.
                let existing = tasks_by_wave_tx(tx, &wave_id_str).await?;
                let outcomes =
                    resolve_plan_batch(&existing, &batch).map_err(CalmError::BadRequest)?;

                let now = now_ms();
                let mut changed_keys: Vec<String> = Vec::new();
                for (t, outcome) in batch.iter().zip(&outcomes) {
                    let row = task_row_from_normalized(&wave_id_str, t, now);
                    match outcome {
                        PlanOutcome::Created => {
                            task_insert_tx(tx, &row).await?;
                            changed_keys.push(t.key.clone());
                        }
                        PlanOutcome::Updated => {
                            task_update_pending_tx(tx, &row).await?;
                            changed_keys.push(t.key.clone());
                        }
                        PlanOutcome::Unchanged => {}
                    }
                }

                let mut events = Vec::new();
                if let Some(auto_events) = auto_promote_draft_in_tx(tx, &wave_id_typed).await? {
                    events.extend(
                        auto_events
                            .into_iter()
                            .map(|event| (ActorId::Kernel, scope.clone(), event)),
                    );
                }
                if let Some(target) = lifecycle
                    && let Some(lifecycle_events) = apply_requested_transition_in_tx(
                        tx,
                        &wave_id_typed,
                        target,
                        &actor,
                        message.clone(),
                    )
                    .await?
                {
                    events.extend(
                        lifecycle_events
                            .into_iter()
                            .map(|event| (actor.clone(), scope.clone(), event)),
                    );
                }
                events.push((
                    actor,
                    scope,
                    Event::PlanUpdated {
                        wave_id: wave_id_typed,
                        changed_keys,
                        agent_message: Some(message),
                    },
                ));
                Ok((outcomes, events))
            })
        },
    )
    .await;

    match result {
        Ok((outcomes, _ids)) => Ok(results_json_from_owned(&outcomes, raw_tasks)),
        Err(e) => Err(map_plan_error("plan_upsert", e)),
    }
}

/// Render `{ results: [{key, outcome}] }` in batch order.
fn results_json(batch: &[NormalizedTask], outcomes: &[PlanOutcome]) -> Value {
    let results: Vec<Value> = batch
        .iter()
        .zip(outcomes)
        .map(|(t, o)| json!({ "key": t.key, "outcome": o.as_str() }))
        .collect();
    json!({ "results": results })
}

/// Variant of [`results_json`] for the post-tx path, where the
/// normalized batch was moved into the closure. Keys are re-read from
/// the raw input (already validated; same order).
fn results_json_from_owned(outcomes: &[PlanOutcome], raw_tasks: &[Value]) -> Value {
    let results: Vec<Value> = raw_tasks
        .iter()
        .zip(outcomes)
        .map(|(raw, o)| {
            json!({
                "key": raw.get("key").and_then(Value::as_str).unwrap_or_default(),
                "outcome": o.as_str(),
            })
        })
        .collect();
    json!({ "results": results })
}

// ---------------------------------------------------------------------------
// calm.plan.cancel
// ---------------------------------------------------------------------------

fn plan_cancel_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_PLAN_CANCEL.into(),
        description: "Spec-only: cancel one still-pending task in the wave's plan. \
             Canceling an already-canceled task is an idempotent success. In-flight \
             tasks (dispatched/running/verifying) cannot be interrupted — cancel or \
             rewire their successors instead. `message` is required and persisted as \
             `agent_message` on the `plan.updated` event. Optional `lifecycle` drives \
             the wave state machine in the same atomic write."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["key", "message"],
            "properties": {
                "key": { "type": "string", "minLength": 1 },
                "message": message_schema(),
                "lifecycle": lifecycle_schema()
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Spec],
    }
}

async fn plan_cancel(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    let write_args = parse_write_args(&args, "plan_cancel")?;

    let key = args
        .get("key")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| RpcError::invalid_params("plan_cancel: missing `key` (non-empty)"))?
        .to_string();

    let (_card, wave) = resolve_wave_for_identity(&ctx, &identity).await?;
    let task_id = format!("{}:{key}", wave.id.as_str());
    let task = ctx
        .repo
        .task_get(&task_id)
        .await
        .map_err(|e| RpcError::internal(format!("plan_cancel: task_get: {e}")))?
        .ok_or_else(|| {
            RpcError::invalid_params(format!("plan_cancel: unknown task `{key}` in this wave"))
        })?;

    match task.status {
        // §3.1 — already-canceled is idempotent success, no write, no
        // event (a retry must not re-trigger the scheduler).
        TaskStatus::Canceled => return Ok(json!({ "ok": true })),
        TaskStatus::Pending => {}
        TaskStatus::Dispatched | TaskStatus::Running | TaskStatus::Verifying => {
            return Err(RpcError::custom(
                -32409,
                format!(
                    "plan_cancel: task {key} is in-flight; interrupting running tasks is \
                     out of scope (#644). The worker will finish; its result will be \
                     gated/reported as usual. Cancel or rewire its successors instead."
                ),
            ));
        }
        TaskStatus::Done | TaskStatus::Failed => {
            return Err(RpcError::invalid_params(format!(
                "plan_cancel: task {key} is already {}; only pending tasks can be canceled",
                serde_json::to_value(task.status)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default()
            )));
        }
    }

    let actor = identity.to_actor_id();
    let scope = EventScope::Wave {
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    let wave_id_typed = wave.id.clone();
    let message = write_args.message.clone();
    let lifecycle = write_args.lifecycle;
    let key_for_tx = key.clone();

    let result = write_with_actor_events_typed::<(), _>(
        ctx.repo.as_ref(),
        None,
        &ctx.events,
        &ctx.write,
        move |tx| {
            let task_id = task_id.clone();
            let key = key_for_tx.clone();
            let wave_id_typed = wave_id_typed.clone();
            let actor = actor.clone();
            let scope = scope.clone();
            let message = message.clone();
            Box::pin(async move {
                // Guarded flip — re-checked in-tx so a task that left
                // `pending` between the pre-read and this write rolls
                // back instead of canceling an in-flight run.
                let rows = task_cancel_tx(tx, &task_id, now_ms()).await?;
                if rows == 0 {
                    return Err(CalmError::Conflict(format!(
                        "task {key} changed state concurrently; re-check with \
                         calm.plan.list and retry"
                    )));
                }

                let mut events = Vec::new();
                if let Some(auto_events) = auto_promote_draft_in_tx(tx, &wave_id_typed).await? {
                    events.extend(
                        auto_events
                            .into_iter()
                            .map(|event| (ActorId::Kernel, scope.clone(), event)),
                    );
                }
                if let Some(target) = lifecycle
                    && let Some(lifecycle_events) = apply_requested_transition_in_tx(
                        tx,
                        &wave_id_typed,
                        target,
                        &actor,
                        message.clone(),
                    )
                    .await?
                {
                    events.extend(
                        lifecycle_events
                            .into_iter()
                            .map(|event| (actor.clone(), scope.clone(), event)),
                    );
                }
                events.push((
                    actor,
                    scope,
                    Event::PlanUpdated {
                        wave_id: wave_id_typed,
                        changed_keys: vec![key],
                        agent_message: Some(message),
                    },
                ));
                Ok(((), events))
            })
        },
    )
    .await;

    match result {
        Ok(_) => Ok(json!({ "ok": true })),
        Err(e) => Err(map_plan_error("plan_cancel", e)),
    }
}

// ---------------------------------------------------------------------------
// calm.plan.list
// ---------------------------------------------------------------------------

fn plan_list_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_PLAN_LIST.into(),
        description: "Spec-only: read the wave's full task plan with per-task status. \
             Gate commands are not echoed (only step names); read the worker output \
             for a finished task via the runs views. No event is emitted."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
        annotations: Some(read_only_annotations()),
        visible_to_roles: &[CardRole::Spec],
    }
}

async fn plan_list(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    _args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    let (_card, wave) = resolve_wave_for_identity(&ctx, &identity).await?;
    let tasks = ctx
        .repo
        .tasks_by_wave(wave.id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("plan_list: tasks_by_wave: {e}")))?;

    let tasks_json: Vec<Value> = tasks.iter().map(task_list_entry).collect();
    Ok(json!({ "tasks": tasks_json }))
}

/// One `calm.plan.list` entry. Deliberately a projection, not the row:
/// gate commands are stripped to `{present, steps: [names]}` (§6.7) and
/// the gate bookkeeping columns (`gate_pid*`, `gate_attempt`) never
/// leave the kernel.
fn task_list_entry(t: &Task) -> Value {
    let gate = match t
        .gate_json
        .as_deref()
        .and_then(|g| serde_json::from_str::<Value>(g).ok())
    {
        None => json!({ "present": false, "steps": [] }),
        Some(gate_value) => {
            let names: Vec<Value> = gate_value
                .get("steps")
                .and_then(Value::as_array)
                .map(|steps| {
                    steps
                        .iter()
                        .filter_map(|s| s.get("name").and_then(Value::as_str))
                        .map(|n| Value::String(n.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            json!({ "present": true, "steps": names })
        }
    };
    let gate_result = t
        .gate_result_json
        .as_deref()
        .and_then(|g| serde_json::from_str::<Value>(g).ok())
        .unwrap_or(Value::Null);

    json!({
        "key": t.key,
        "kind": t.kind,
        "goal": t.goal,
        "status": t.status,
        "status_detail": t.status_detail,
        "depends_on": t.depends_on(),
        "priority": t.priority,
        "gate": gate,
        "worker_card_id": t.worker_card_id,
        "gate_result": gate_result,
        "created_at_ms": t.created_at_ms,
        "finished_at_ms": t.finished_at_ms,
    })
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Map tx-layer errors onto the MCP error vocabulary: validation that
/// only the in-tx resolve could catch → `-32602`, concurrent-state
/// conflicts → `-32409`, role-gate refusals → `-32403`, everything
/// else → internal.
fn map_plan_error(tool: &str, e: CalmError) -> RpcError {
    match e {
        CalmError::BadRequest(m) => RpcError::invalid_params(format!("{tool}: {m}")),
        CalmError::Conflict(m) => RpcError::custom(-32409, format!("{tool}: {m}")),
        CalmError::Forbidden(m) => RpcError::custom(-32403, format!("{tool}: forbidden: {m}")),
        other => RpcError::internal(format!("{tool}: {other}")),
    }
}

/// Look up the wave the calling card belongs to. Mirrors
/// `wave_state::resolve_wave_for_identity`: the thread-mapped card must
/// exist while its daemon is active; a missing row is a
/// delete-while-active race surfaced loud as `InternalError`.
async fn resolve_wave_for_identity(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
) -> Result<(crate::model::Card, Wave), RpcError> {
    let card_id_str = identity.card_id.as_str().to_string();
    let card = ctx
        .repo
        .card_get(&card_id_str)
        .await
        .map_err(|e| RpcError::internal(format!("plan: card lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "plan: bound card {card_id_str} not found (deleted mid-connection?)"
            ))
        })?;
    let wave = ctx
        .repo
        .wave_get(card.wave_id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("plan: wave lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "plan: wave {} for card {} not found",
                card.wave_id.as_str(),
                card_id_str
            ))
        })?;
    Ok((card, wave))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_task(key: &str) -> PlanTaskInput {
        PlanTaskInput {
            key: key.into(),
            kind: "codex".into(),
            goal: "do the thing".into(),
            context: None,
            acceptance_criteria: None,
            cwd: None,
            depends_on: vec![],
            priority: None,
            gate: None,
            no_gate_reason: None,
        }
    }

    fn normalized(key: &str, deps: &[&str]) -> NormalizedTask {
        let mut t = raw_task(key);
        t.depends_on = deps.iter().map(|s| s.to_string()).collect();
        normalize_task_input(t).expect("normalize")
    }

    fn pending_row(key: &str, deps: &[&str]) -> Task {
        task_row_from_normalized("wave-1", &normalized(key, deps), 1)
    }

    // -------------------------------------------------------- rule 1: key

    #[test]
    fn key_regex_accepts_and_rejects_per_design() {
        for ok in [
            "a",
            "impl-parser",
            "a.b_c-d",
            "0task",
            "x".repeat(64).as_str(),
        ] {
            assert!(key_is_valid(ok), "should accept `{ok}`");
        }
        for bad in [
            "",
            "-leading-dash",
            ".leading-dot",
            "_leading-underscore",
            "Upper",
            "has space",
            "ünicode",
            "x".repeat(65).as_str(),
        ] {
            assert!(!key_is_valid(bad), "should reject `{bad}`");
        }
    }

    #[test]
    fn duplicate_key_in_batch_rejected() {
        let batch = vec![normalized("a", &[]), normalized("a", &[])];
        let err = resolve_plan_batch(&[], &batch).expect_err("dup key");
        assert!(err.contains("duplicate key `a`"), "err = {err}");
    }

    // -------------------------------------------------------- rule 2: kind

    #[test]
    fn kind_claude_rejected_with_not_yet_supported() {
        let mut t = raw_task("a");
        t.kind = "claude".into();
        let err = normalize_task_input(t).expect_err("claude rejected");
        assert!(err.contains("not yet supported"), "err = {err}");
    }

    #[test]
    fn unknown_kind_rejected() {
        let mut t = raw_task("a");
        t.kind = "banana".into();
        let err = normalize_task_input(t).expect_err("unknown kind");
        assert!(err.contains("unknown kind `banana`"), "err = {err}");
    }

    // -------------------------------------------------------- rule 3: deps

    #[test]
    fn unknown_dep_rejected_and_batch_or_existing_deps_accepted() {
        let existing = vec![pending_row("old", &[])];
        let err =
            resolve_plan_batch(&existing, &[normalized("a", &["ghost"])]).expect_err("unknown dep");
        assert!(err.contains("unknown dependency `ghost`"), "err = {err}");

        // Dep on an existing wave task and on a same-batch sibling both pass.
        let outcomes = resolve_plan_batch(
            &existing,
            &[normalized("a", &["old", "b"]), normalized("b", &[])],
        )
        .expect("valid deps");
        assert_eq!(outcomes, vec![PlanOutcome::Created, PlanOutcome::Created]);
    }

    // -------------------------------------------------------- rule 4: cycles

    #[test]
    fn cycle_rejected_with_path_in_error() {
        let batch = vec![
            normalized("a", &["b"]),
            normalized("b", &["c"]),
            normalized("c", &["a"]),
        ];
        let err = resolve_plan_batch(&[], &batch).expect_err("cycle");
        assert!(err.contains("dependency cycle:"), "err = {err}");
        // The path names every participant and closes the loop.
        for k in ["a", "b", "c"] {
            assert!(err.contains(k), "cycle path misses `{k}`: {err}");
        }
        assert!(err.contains(" -> "), "err = {err}");
    }

    #[test]
    fn self_dependency_is_a_cycle() {
        let err = resolve_plan_batch(&[], &[normalized("a", &["a"])]).expect_err("self dep");
        assert!(err.contains("dependency cycle: a -> a"), "err = {err}");
    }

    #[test]
    fn cycle_through_existing_rows_detected() {
        // Existing pending row `old` depends on batch task `new`; the
        // batch makes `new` depend on `old` → cycle across the
        // post-upsert view.
        let existing = vec![pending_row("old", &["new"])];
        let err = resolve_plan_batch(&existing, &[normalized("new", &["old"])])
            .expect_err("cross-set cycle");
        assert!(err.contains("dependency cycle:"), "err = {err}");
    }

    // -------------------------------------------------------- rule 5: mutability

    #[test]
    fn pending_row_revisable_and_identical_is_unchanged() {
        let existing = vec![pending_row("a", &[])];

        let identical = resolve_plan_batch(&existing, &[normalized("a", &[])]).expect("ok");
        assert_eq!(identical, vec![PlanOutcome::Unchanged]);

        let mut revised = normalized("a", &[]);
        revised.goal = "do the other thing".into();
        let updated = resolve_plan_batch(&existing, &[revised]).expect("ok");
        assert_eq!(updated, vec![PlanOutcome::Updated]);
    }

    #[test]
    fn non_pending_identical_unchanged_and_different_rejected() {
        let mut row = pending_row("a", &[]);
        row.status = TaskStatus::Running;
        let existing = vec![row];

        let identical = resolve_plan_batch(&existing, &[normalized("a", &[])]).expect("ok");
        assert_eq!(identical, vec![PlanOutcome::Unchanged]);

        let mut revised = normalized("a", &[]);
        revised.priority = 9;
        let err = resolve_plan_batch(&existing, &[revised]).expect_err("immutable");
        assert!(
            err.contains("task a already dispatched; insert a new task instead"),
            "err = {err}"
        );
    }

    // -------------------------------------------------------- rule 7: cwd + gate shape

    #[test]
    fn relative_cwd_rejected_absolute_accepted() {
        let mut t = raw_task("a");
        t.cwd = Some("relative/path".into());
        let err = normalize_task_input(t).expect_err("relative cwd");
        assert!(err.contains("absolute path"), "err = {err}");

        let mut t = raw_task("a");
        t.cwd = Some("/abs/path".into());
        let ok = normalize_task_input(t).expect("absolute cwd");
        assert_eq!(ok.cwd.as_deref(), Some("/abs/path"));
    }

    #[test]
    fn cwd_with_control_chars_rejected() {
        let mut t = raw_task("a");
        t.cwd = Some("/abs/pa\nth".into());
        let err = normalize_task_input(t).expect_err("control char cwd");
        assert!(err.contains("ASCII control"), "err = {err}");
    }

    fn gate(steps: Vec<GateStepInput>, timeout: Option<i64>, cwd: Option<&str>) -> GateInput {
        GateInput {
            cwd: cwd.map(str::to_string),
            timeout_secs: timeout,
            steps,
        }
    }

    fn step(name: &str, cmd: &str) -> GateStepInput {
        GateStepInput {
            name: name.into(),
            cmd: cmd.into(),
        }
    }

    #[test]
    fn gate_shape_violations_rejected() {
        // Empty steps.
        let err = validate_gate_shape("a", &gate(vec![], None, None)).expect_err("empty steps");
        assert!(err.contains("gate.steps must be non-empty"), "err = {err}");

        // Empty cmd.
        let err = validate_gate_shape("a", &gate(vec![step("fmt", "  ")], None, None))
            .expect_err("empty cmd");
        assert!(err.contains("cmd must be non-empty"), "err = {err}");

        // Control characters in cmd (same check as codex_adapter).
        let err = validate_gate_shape("a", &gate(vec![step("fmt", "cargo\u{7}fmt")], None, None))
            .expect_err("control char");
        assert!(err.contains("ASCII control"), "err = {err}");

        // Timeout over the cap.
        let err = validate_gate_shape("a", &gate(vec![step("t", "true")], Some(7201), None))
            .expect_err("timeout cap");
        assert!(err.contains("1..=7200"), "err = {err}");

        // Relative gate cwd.
        let err = validate_gate_shape("a", &gate(vec![step("t", "true")], None, Some("rel/path")))
            .expect_err("relative gate cwd");
        assert!(err.contains("absolute path"), "err = {err}");

        // A well-shaped gate passes shape validation (rule 8 rejection
        // happens at the normalize layer, not here).
        validate_gate_shape(
            "a",
            &gate(vec![step("t", "cargo test")], Some(600), Some("/repo")),
        )
        .expect("valid shape");
    }

    // -------------------------------------------------------- rule 8: slice guard

    #[test]
    fn any_declared_gate_rejected_until_task_verify_lands() {
        let mut t = raw_task("a");
        t.gate = Some(gate(vec![step("test", "cargo test")], None, None));
        let err = normalize_task_input(t).expect_err("rule 8");
        assert!(
            err.contains("gates are not yet enforced (lands with task-verify)"),
            "err = {err}"
        );
    }

    // -------------------------------------------------------- no_gate_reason

    #[test]
    fn no_gate_reason_recorded_into_context_json() {
        let mut t = raw_task("a");
        t.context = Some(json!({ "hint": "x" }));
        t.no_gate_reason = Some("docs-only change".into());
        let n = normalize_task_input(t).expect("normalize");
        let ctx: Value = serde_json::from_str(&n.context_json).unwrap();
        assert_eq!(ctx["no_gate_reason"], "docs-only change");
        assert_eq!(ctx["hint"], "x");

        // Missing context still records the reason.
        let mut t = raw_task("a");
        t.no_gate_reason = Some("r".into());
        let n = normalize_task_input(t).expect("normalize");
        let ctx: Value = serde_json::from_str(&n.context_json).unwrap();
        assert_eq!(ctx["no_gate_reason"], "r");

        // Non-object context cannot carry the reason — rejected loud.
        let mut t = raw_task("a");
        t.context = Some(json!("a string"));
        t.no_gate_reason = Some("r".into());
        let err = normalize_task_input(t).expect_err("non-object context");
        assert!(
            err.contains("requires `context` to be an object"),
            "err = {err}"
        );
    }

    // -------------------------------------------------------- normalization

    #[test]
    fn depends_on_sorted_and_deduped_for_idempotency() {
        let stored = {
            let n = normalized("a", &["c", "b", "c"]);
            assert_eq!(n.depends_on, vec!["b", "c"]);
            task_row_from_normalized("wave-1", &n, 1)
        };
        // Re-sending the same deps in a different order is `unchanged`.
        let outcomes = resolve_plan_batch(
            &[pending_row("b", &[]), pending_row("c", &[]), stored],
            &[normalized("a", &["b", "c"])],
        )
        .expect("ok");
        assert_eq!(outcomes, vec![PlanOutcome::Unchanged]);
    }

    #[test]
    fn list_entry_never_echoes_gate_commands() {
        let mut row = pending_row("a", &[]);
        row.gate_json = Some(
            json!({
                "steps": [
                    { "name": "fmt", "cmd": "cargo fmt --check" },
                    { "name": "test", "cmd": "cargo test --secret-flag" }
                ],
                "timeout_secs": 600
            })
            .to_string(),
        );
        let entry = task_list_entry(&row);
        assert_eq!(entry["gate"]["present"], true);
        assert_eq!(entry["gate"]["steps"], json!(["fmt", "test"]));
        let rendered = entry.to_string();
        assert!(
            !rendered.contains("cargo fmt") && !rendered.contains("secret-flag"),
            "gate cmd leaked: {rendered}"
        );
    }
}
