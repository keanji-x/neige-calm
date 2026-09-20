//! `calm.plan.*` — the planner card's durable per-track task plan.
//! `calm.plan.list` never echoes gate commands, only `{present, steps: [names]}`.

use crate::db::sqlite::{task_cancel_tx, task_get_tx};
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
use crate::model::TaskKind;
use crate::model::{CardRole, Task, TaskStatus, Track, now_ms};
use crate::track_lifecycle::{apply_requested_transition_in_tx, auto_promote_draft_in_tx};
use calm_types::report_blocks::tasks::GATE_TIMEOUT_MAX_SECS;
pub use calm_types::report_blocks::tasks::{
    GateInput, GateStepInput, key_is_valid, validate_gate_shape,
};
#[cfg(test)]
use calm_types::report_blocks::tasks::{TaskDeclaration, dup_keys, find_cycle, unknown_deps};
use serde::Deserialize;
use serde_json::{Value, json};
#[cfg(test)]
use std::collections::BTreeMap;
use std::sync::Arc;

mod list;
mod recovery_guidance;

pub const TOOL_PLAN_UPSERT: &str = "calm.plan.upsert";
pub const TOOL_PLAN_CANCEL: &str = "calm.plan.cancel";
pub const TOOL_PLAN_LIST: &str = "calm.plan.list";
pub const TOOL_PLAN_RECOVER: &str = "calm.plan.recover";

/// Gate timeout defaults/caps; the task-verify adapter re-clamps at run time.
pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(plan_upsert_descriptor(), wrap(plan_upsert));
    registry.register(plan_cancel_descriptor(), wrap(plan_cancel));
    registry.register(plan_list_descriptor(), wrap(plan_list));
    registry.register(plan_recover_descriptor(), wrap(plan_recover));
}

fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture {
        let result = f(ctx, identity, args);
        Box::pin(async move {
            result
                .await
                .map(crate::mcp_server::result::ToolResult::structured)
        })
    })
}

#[cfg(test)]
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanTaskInput {
    pub key: String,
    pub kind: String,
    pub goal: String,
    #[serde(default)]
    pub context: Option<Value>,
    #[serde(default)]
    pub acceptance_criteria: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub gate: Option<GateInput>,
    #[serde(default)]
    pub no_gate_reason: Option<String>,
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct NormalizedTask {
    key: String,
    kind: TaskKind,
    goal: String,
    acceptance_criteria: Option<String>,
    cwd: Option<String>,
    /// Sorted + deduped — dependency order is set semantics.
    depends_on: Vec<String>,
    priority: i64,
    /// Canonical gate serialization; deterministic per input so the idempotency check covers gates.
    gate_json: Option<String>,
    has_no_gate_reason: bool,
}

/// Absolute, non-empty, no ASCII control characters.
#[cfg(test)]
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

#[cfg(test)]
fn normalize_task_input(input: PlanTaskInput) -> Result<NormalizedTask, String> {
    let key = input.key;
    if !key_is_valid(&key) {
        return Err(format!(
            "invalid task key `{key}`: must match ^[a-z0-9][a-z0-9._-]{{0,63}}$ \
             (lowercase, 1-64 chars)"
        ));
    }

    let kind = match input.kind.as_str() {
        "codex" => TaskKind::Codex,
        "claude" => TaskKind::Claude,
        "terminal" => TaskKind::Terminal,
        other => {
            return Err(format!(
                "task {key}: unknown kind `{other}` (expected `codex`, `claude`, or `terminal`)"
            ));
        }
    };

    let goal = input.goal;
    if goal.trim().is_empty() {
        return Err(format!("task {key}: `goal` must be non-empty"));
    }

    let cwd = match input.cwd.as_deref() {
        None => None,
        Some(raw) => Some(validate_abs_path("cwd", &key, raw)?),
    };

    let gate_json = match &input.gate {
        None => None,
        Some(gate) => Some(normalize_gate(&key, gate)?),
    };
    let no_gate_reason = match input.no_gate_reason {
        None => None,
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(format!(
                    "task {key}: `no_gate_reason` must be a non-empty reason \
                     (it is the audited justification for skipping a verification gate)"
                ));
            }
            Some(trimmed.to_string())
        }
    };
    let has_no_gate_reason = no_gate_reason.is_some();

    let context = input.context.unwrap_or(Value::Null);
    if no_gate_reason.is_some() {
        match context {
            Value::Null | Value::Object(_) => {}
            other => {
                return Err(format!(
                    "task {key}: `no_gate_reason` requires `context` to be an object \
                     (or omitted) so the reason can be recorded; got {}",
                    crate::mcp_server::tools::lifecycle_args::shape_of(&other)
                ));
            }
        }
    }

    let mut depends_on = input.depends_on;
    depends_on.sort();
    depends_on.dedup();

    Ok(NormalizedTask {
        key,
        kind,
        goal,
        acceptance_criteria: input.acceptance_criteria,
        cwd,
        depends_on,
        priority: input.priority.unwrap_or(0),
        gate_json,
        has_no_gate_reason,
    })
}

/// Canonical `gate_json` is a pure function of the input (`None` omitted, fixed key order) so byte-identical idempotency covers gates.
#[cfg(test)]
fn normalize_gate(key: &str, gate: &GateInput) -> Result<String, String> {
    validate_gate_shape(key, gate)?;
    let mut obj = serde_json::Map::new();
    if let Some(raw) = gate.cwd.as_deref() {
        obj.insert(
            "cwd".into(),
            Value::String(validate_abs_path("gate.cwd", key, raw)?),
        );
    }
    if let Some(timeout) = gate.timeout_secs {
        obj.insert("timeout_secs".into(), json!(timeout));
    }
    obj.insert(
        "steps".into(),
        Value::Array(
            gate.steps
                .iter()
                .map(|s| json!({ "name": s.name, "cmd": s.cmd }))
                .collect(),
        ),
    );
    serde_json::to_string(&Value::Object(obj)).map_err(|e| format!("task {key}: gate: {e}"))
}

#[cfg(test)]
fn validate_new_batch(batch: &[NormalizedTask]) -> Result<(), String> {
    let batch_declarations: Vec<TaskDeclaration> = batch
        .iter()
        .map(declaration_from_normalized)
        .collect::<Result<_, _>>()?;
    if let Some(key) = dup_keys(&batch_declarations).first() {
        return Err(format!("duplicate key `{key}` in batch"));
    }

    if let Some((key, dependency)) = unknown_deps(&batch_declarations, &[]).first() {
        return Err(format!(
            "task {key}: unknown dependency `{dependency}` (must name an existing track \
             task in this template)"
        ));
    }

    let graph: BTreeMap<String, Vec<String>> = batch
        .iter()
        .map(|t| (t.key.clone(), t.depends_on.clone()))
        .collect();
    if let Some(cycle) = find_cycle(&graph) {
        return Err(format!("dependency cycle: {}", cycle.join(" -> ")));
    }

    Ok(())
}

#[cfg(test)]
fn declaration_from_normalized(task: &NormalizedTask) -> Result<TaskDeclaration, String> {
    let gate = task
        .gate_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|error| format!("task {}: invalid normalized gate_json: {error}", task.key))?;
    Ok(TaskDeclaration {
        source: calm_types::report_blocks::tasks::TaskDeclarationSource::ValidationOnly,
        block_index: None,
        block_id: String::new(),
        key: task.key.clone(),
        kind: match task.kind {
            TaskKind::Codex => "codex",
            TaskKind::Claude => "claude",
            TaskKind::Terminal => "terminal",
        }
        .into(),
        goal: task.goal.clone(),
        acceptance: task.acceptance_criteria.clone(),
        gate,
        no_gate_reason: task.has_no_gate_reason.then(String::new),
        depends_on: task.depends_on.clone(),
        context: serde_json::json!({}),
        cwd: task.cwd.clone(),
        priority: task.priority,
        refs: Vec::new(),
        declared_by: "spec".into(),
        released_by_user: false,
        spawn: "in-wave".into(),
        tombstoned_by: None,
        ready: true,
        tombstone: false,
    })
}

#[cfg(test)]
fn task_row_from_normalized(track_id: &str, t: &NormalizedTask, now: i64) -> Task {
    Task {
        id: format!("{track_id}:{}", t.key),
        track_id: track_id.to_string(),
        key: t.key.clone(),
        kind: t.kind,
        goal: t.goal.clone(),
        context_json: "null".into(),
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
        running_deadline_ms: None,
        context_stale_at_ms: None,
        declared_by: "spec".into(),
        spawn: "in-wave".into(),
        created_at_ms: now,
        updated_at_ms: now,
        finished_at_ms: None,
    }
}

fn plan_upsert_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_PLAN_UPSERT.into(),
        description: include_str!("../../../prompts/tools/calm.plan.upsert.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["tasks", "message"],
            "properties": {
                "tasks": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "oneOf": [
                            {
                                "required": ["key", "kind", "goal"],
                                "properties": { "kind": { "enum": ["codex", "claude"] } },
                                "not": { "required": ["command"] }
                            },
                            {
                                "required": ["key", "kind", "command"],
                                "properties": { "kind": { "const": "terminal" } },
                                "not": { "required": ["goal"] }
                            }
                        ],
                        "properties": {
                            "key": {
                                "type": "string",
                                "pattern": "^[a-z0-9][a-z0-9._-]{0,63}$",
                                "description": "Stable per-track task key; also the completion correlation id."
                            },
                            "kind": { "type": "string", "enum": ["codex", "claude", "terminal"] },
                            "goal": { "type": "string", "minLength": 1, "description": "Natural-language objective; required only for codex/claude tasks and forbidden for terminal tasks." },
                            "command": { "type": "string", "minLength": 1, "description": "Exact Shell command passed verbatim as `/bin/sh -c <command>`; required only for terminal tasks and forbidden for codex/claude tasks." },
                            "context": { "description": "Optional, any JSON; forwarded to the worker verbatim." },
                            "acceptance_criteria": { "type": ["string", "null"] },
                            "cwd": { "type": ["string", "null"], "description": "Absolute path; terminal worker cwd + gate default cwd." },
                            "depends_on": { "type": "array", "items": { "type": "string" }, "description": "Sibling task keys that must be done first." },
                            "priority": { "type": "integer", "description": "Higher schedules first; default 0." },
                            "gate": {
                                "type": "object",
                                "required": ["steps"],
                                "description": "Verification the kernel runs after the worker reports done; declare one for every agent task. Steps run in order, first non-zero exit fails the gate, and steps must be re-runnable (kernel restarts re-run the gate).",
                                "properties": {
                                    "steps": {
                                        "type": "array",
                                        "minItems": 1,
                                        "items": {
                                            "type": "object",
                                            "required": ["name", "cmd"],
                                            "properties": {
                                                "name": { "type": "string", "minLength": 1, "description": "Step label; the failing step is attributed in the gate result." },
                                                "cmd": { "type": "string", "minLength": 1, "description": "Shell command; must be re-runnable. Non-zero exit fails the gate." }
                                            }
                                        }
                                    },
                                    "cwd": { "type": ["string", "null"], "description": "Absolute path; defaults to task.cwd, else the track cwd." },
                                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": GATE_TIMEOUT_MAX_SECS, "description": "Whole-gate timeout in seconds; default 1800, max 7200. Timeout fails the gate." }
                                }
                            },
                            "no_gate_reason": { "type": "string", "minLength": 1, "description": "Escape hatch: justifies an ungated agent task on a track with `require_task_gates`; recorded into context for audit. Must be a non-empty reason (whitespace-only is rejected)." }
                        }
                    }
                },
                "message": message_schema(),
                "lifecycle": lifecycle_schema()
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[],
    }
}

async fn plan_upsert(
    _ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    _args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    Ok(json!({
        "error": "calm.plan.upsert was retired (#985); no task declaration was written",
        "migration": {
            "use": "calm.report.blocks.upsert",
            "shape": "{ kind: \"task\", payload: { key, kind, goal (codex/claude) | command (terminal), acceptance?, depends_on?, priority?, gate?, ready: true, declared_by: \"spec\" }, if_doc_rev }",
            "notes": "Read docRev with calm.report.read. The kernel projects ready task blocks, schedules tasks, and runs verification gates; use calm.plan.list for status."
        }
    }))
}

fn plan_cancel_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_PLAN_CANCEL.into(),
        description: include_str!("../../../prompts/tools/calm.plan.cancel.md")
            .trim_end()
            .to_string(),
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
        visible_to_roles: &[CardRole::Planner],
    }
}

async fn plan_cancel(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    plan_cancel_impl(ctx, identity, args, || async {}).await
}

async fn plan_cancel_impl<F, Fut>(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
    after_pre_read: F,
) -> Result<Value, RpcError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    require_role(&identity, CardRole::Planner)?;
    let write_args = parse_write_args(&args, "plan_cancel")?;

    let key = args
        .get("key")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| RpcError::invalid_params("plan_cancel: missing `key` (non-empty)"))?
        .to_string();

    let (_card, track) = resolve_track_for_identity(&ctx, &identity).await?;
    let task = ctx
        .repo
        .task_current_get(track.id.as_str(), &key)
        .await
        .map_err(|e| RpcError::internal(format!("plan_cancel: task_get: {e}")))?
        .ok_or_else(|| {
            RpcError::invalid_params(format!("plan_cancel: unknown task `{key}` in this track"))
        })?;

    let task_id = task.id.clone();

    // A `lifecycle` equal to the track's current state is the same-state idempotency shortcut: it would apply nothing.
    let lifecycle_is_noop = write_args
        .lifecycle
        .is_none_or(|target| target == track.lifecycle);

    match task.status {
        // Already-canceled is idempotent success: no write, no event (a retry must not re-trigger the scheduler). A real lifecycle request still falls through into the tx.
        TaskStatus::Canceled if lifecycle_is_noop => {
            return Ok(json!({ "ok": true }));
        }
        TaskStatus::Canceled | TaskStatus::Pending => {}
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

    // Fixtures advance the row here to exercise the guarded UPDATE; production supplies a no-op future.
    after_pre_read().await;

    let actor = identity.to_actor_id();
    let scope = EventScope::Track {
        track: track.id.clone(),
        area: track.area_id.clone(),
    };
    let track_id_typed = track.id.clone();
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
            let track_id_typed = track_id_typed.clone();
            let actor = actor.clone();
            let scope = scope.clone();
            let message = message.clone();
            Box::pin(async move {
                // Re-checked in-tx so a task that left `pending` between the pre-read and this write rolls back instead of canceling an in-flight run.
                let current =
                    crate::db::sqlite::task_current_get_tx(tx, track_id_typed.as_str(), &key)
                        .await?;
                if current.as_ref().map(|task| task.id.as_str()) != Some(task_id.as_str()) {
                    return Err(CalmError::Conflict(
                        "current execution changed; refresh calm.plan.list".into(),
                    ));
                }
                let rows = task_cancel_tx(tx, &task_id, now_ms()).await?;
                if rows == 0 {
                    // Disambiguate the 0-row flip: a concurrent `canceled` is the idempotent path (no `plan.updated`); anything else is a real concurrent state change.
                    let now_canceled = task_get_tx(tx, &task_id)
                        .await?
                        .is_some_and(|t| t.status == TaskStatus::Canceled);
                    if !now_canceled {
                        return Err(CalmError::Conflict(format!(
                            "task {key} changed state concurrently; re-check with \
                             calm.plan.list and retry"
                        )));
                    }
                }

                let mut events = Vec::new();
                if let Some(auto_events) = auto_promote_draft_in_tx(tx, &track_id_typed).await? {
                    events.extend(
                        auto_events
                            .into_iter()
                            .map(|event| (ActorId::Kernel, scope.clone(), event)),
                    );
                }
                if let Some(target) = lifecycle
                    && let Some(lifecycle_events) = apply_requested_transition_in_tx(
                        tx,
                        &track_id_typed,
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
                // Idempotent re-cancel changed nothing — suppress `plan.updated` so a retry can't re-trigger the scheduler.
                if rows > 0 {
                    events.push((
                        actor,
                        scope,
                        Event::PlanUpdated {
                            track_id: track_id_typed,
                            changed_keys: vec![key.clone()],
                            agent_message: Some(message),
                        },
                    ));
                }
                // An empty batch means a concurrent writer turned the request into a no-op mid-flight; `write_with_actor_events` rejects empty batches, so surface a retryable conflict.
                if events.is_empty() {
                    return Err(CalmError::Conflict(format!(
                        "task {key} or track changed state concurrently; retry"
                    )));
                }
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

/// Fixtures-only deterministic seam for the cancel pre-read/write race.
#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub async fn plan_cancel_after_pre_read_for_test<F, Fut>(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
    after_pre_read: F,
) -> Result<Value, RpcError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    plan_cancel_impl(ctx, identity, args, after_pre_read).await
}

fn plan_list_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_PLAN_LIST.into(),
        description: include_str!("../../../prompts/tools/calm.plan.list.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {"detail":{"type":"string","enum":["summary","full"]},"key":{"type":"string","minLength":1,"description":"One exact current task key in this Track; no prefix matching or whitespace normalization."}},
            "additionalProperties":false
        }),
        annotations: Some(read_only_annotations()),
        visible_to_roles: &[CardRole::Planner],
    }
}

async fn plan_list(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    let args = list::Args::parse(&args)?;
    let (_card, track) = resolve_track_for_identity(&ctx, &identity).await?;
    let actor = identity.to_actor_id();
    let task_budget_default = ctx.task_budget_default;
    crate::db::write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let as_of_ms = now_ms();
            let mut tasks_json = Vec::new();
            let mut after_key = None;
            loop {
                let allocations = if let Some(key) = &args.key {
                    vec![crate::db::sqlite::task_attempt_current_tx(tx, track.id.as_str(), key)
                        .await?
                        .ok_or_else(|| CalmError::BadRequest(format!("current execution unavailable for exact key `{key}` in this Track")))?]
                } else {
                    crate::db::sqlite::task_attempt_current_by_track_tx(
                        tx, track.id.as_str(), after_key.as_deref(), 128,
                    ).await?
                };
                let full_page = allocations.len() == 128;
                for allocation in allocations {
                    let task = crate::db::sqlite::task_get_tx(tx, &allocation.attempt_id).await?;
                    let (view, refusal) = crate::task_recovery::task_recovery_view_with_refusal_tx(
                        tx,
                        &track.id,
                        &allocation.key,
                        &actor,
                        task_budget_default,
                    )
                    .await?;
                    let mut entry = if args.summary {
                        match &task {
                            Some(task) => json!({"key":allocation.key,"kind":task.kind,
                                "status_detail":task.status_detail,
                                "gate_result":task.gate_result_json.as_deref().and_then(|raw| serde_json::from_str::<Value>(raw).ok())}),
                            None => json!({"key":allocation.key,"task_projection":"unavailable"}),
                        }
                    } else {
                        task.as_ref().map(task_list_entry).unwrap_or_else(
                            || json!({"id":allocation.attempt_id,"key":allocation.key}),
                        )
                    };
                    let current = view.current.ok_or_else(|| {
                        CalmError::Internal(
                            "allocated task has no current execution history".into(),
                        )
                    })?;
                    entry["activity"] = serde_json::to_value(
                        crate::isolated_codex::activity::read_tx(
                            tx,
                            task.as_ref(),
                            &current.attempt_id,
                            track.id.as_str(),
                            as_of_ms,
                        )
                        .await?,
                    )?;
                    if let Some(task) = &task {
                        entry["file_delivery"] = crate::file_delivery::view_tx(tx, task).await?;
                    }
                    entry["attempt_id"] = json!(current.attempt_id);
                    entry["generation"] = json!(current.generation);
                    entry["status"] = json!(current.status);
                    entry["blocking_reason"] = json!(current.blocking_reason);
                    entry["recovery"] = serde_json::to_value(view.recovery)?;
                    // Absent (no key) when the attempt never held a lease.
                    let worktree_facts = match task
                        .as_ref()
                        .and_then(|task| task.worker_card_id.as_deref())
                    {
                        Some(worker_card_id) => {
                            crate::operation::workspace_lease::facts::worker_worktree_facts_tx(
                                tx,
                                worker_card_id,
                            )
                            .await?
                        }
                        None => None,
                    };
                    if let Some(facts) = &worktree_facts {
                        entry["worktree"] = serde_json::to_value(facts)?;
                    }
                    // Given for every current attempt, `pending`/`failed` included (D8).
                    if let Some(task) = &task {
                        entry["candidate"] = serde_json::to_value(
                            crate::git_candidate::view::candidate_view_tx(tx, task, worktree_facts.as_ref()).await?,
                        )?;
                    }
                    // MCP-only: `guidance` exists only here; the REST wire type is unchanged.
                    if let (Some(refused), Some(task)) = (&refusal, &task) {
                        entry["recovery"]["guidance"] =
                            recovery_guidance::guidance_tx(tx, task, refused, worktree_facts)
                                .await?;
                    }
                    tasks_json.push(if args.summary { list::summary(&entry) } else { entry });
                    after_key = Some(allocation.key);
                }
                if args.key.is_some() || !full_page {
                    break;
                }
            }
            Ok(json!({ "tasks": tasks_json }))
        })
    })
    .await
    .map_err(|error| map_plan_error("plan_list", error))
}

/// A projection, not the row: gate commands are stripped to `{present, steps: [names]}` and gate bookkeeping columns never leave the kernel.
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

    let mut entry = json!({
        "id": t.id,
        "key": t.key,
        "kind": t.kind,
        "status": t.status,
        "status_detail": t.status_detail,
        "depends_on": t.depends_on(),
        "priority": t.priority,
        "gate": gate,
        "worker_card_id": t.worker_card_id,
        "gate_result": gate_result,
        "created_at_ms": t.created_at_ms,
        "finished_at_ms": t.finished_at_ms,
    });
    let instruction_field = if t.kind == TaskKind::Terminal {
        "command"
    } else {
        "goal"
    };
    entry[instruction_field] = json!(t.goal);
    entry
}

fn plan_recover_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_PLAN_RECOVER.into(),
        description: include_str!("../../../prompts/tools/calm.plan.recover.md")
            .trim_end()
            .to_string(),
        input_schema: json!({"type": "object", "additionalProperties": false,
        "required": ["key", "expected_attempt_id", "idempotency_key", "reason"],
        "properties": {
            "key": {"type": "string", "minLength": 1},
            "expected_attempt_id": {"type": "string", "minLength": 1},
            "idempotency_key": {"type": "string", "minLength": 1, "maxLength": 200},
            "reason": {"type": "string", "minLength": 1, "maxLength": 4000}
        }}),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Planner],
    }
}

async fn plan_recover(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Args {
        key: String,
        expected_attempt_id: String,
        idempotency_key: String,
        reason: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|error| RpcError::invalid_params(error.to_string()))?;
    let (_, track) = resolve_track_for_identity(&ctx, &identity).await?;
    let receipt = crate::task_recovery::recover_failed_task(
        crate::task_recovery::RecoveryContext {
            repo: ctx.repo.as_ref(),
            events: &ctx.events,
            write: &ctx.write,
        },
        track.id.as_str(),
        &args.key,
        calm_types::task_recovery::TaskRecoveryRequest {
            expected_attempt_id: args.expected_attempt_id,
            idempotency_key: args.idempotency_key,
            reason: args.reason,
        },
        identity.to_actor_id(),
    )
    .await
    .map_err(|error| map_plan_error("plan_recover", error))?;
    let statement =
        crate::task_recovery::executor_statement_for_receipt(ctx.repo.as_ref(), &receipt)
            .await
            .map_err(|error| map_plan_error("plan_recover", error))?;
    let mut response =
        serde_json::to_value(receipt).map_err(|error| RpcError::internal(error.to_string()))?;
    response["executor_environment"] = statement.environment;
    response["recover_changes"] = json!(statement.recover_changes);
    Ok(response)
}

fn map_plan_error(tool: &str, e: CalmError) -> RpcError {
    match e {
        CalmError::BadRequest(m) => RpcError::invalid_params(format!("{tool}: {m}")),
        CalmError::Conflict(m) => RpcError::custom(-32409, format!("{tool}: {m}")),
        CalmError::Forbidden(m) => RpcError::custom(-32403, format!("{tool}: forbidden: {m}")),
        other => RpcError::internal(format!("{tool}: {other}")),
    }
}

/// A missing thread-mapped card while its daemon is active is a delete-while-active race, surfaced as `InternalError`.
async fn resolve_track_for_identity(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
) -> Result<(crate::model::Card, Track), RpcError> {
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
    let track = ctx
        .repo
        .track_get(card.track_id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("plan: track lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "plan: track {} for card {} not found",
                card.track_id.as_str(),
                card_id_str
            ))
        })?;
    Ok((card, track))
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
        task_row_from_normalized("track-1", &normalized(key, deps), 1)
    }

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
        let err = validate_new_batch(&batch).expect_err("dup key");
        assert!(err.contains("duplicate key `a`"), "err = {err}");
    }

    #[test]
    fn kind_claude_normalizes_to_taskkind_claude() {
        let mut t = raw_task("a");
        t.kind = "claude".into();
        let normalized = normalize_task_input(t).expect("claude accepted");
        assert_eq!(normalized.kind, TaskKind::Claude);
    }

    #[test]
    fn upsert_schema_kind_enum_includes_claude() {
        let descriptor = plan_upsert_descriptor();
        let enum_values = descriptor
            .input_schema
            .pointer("/properties/tasks/items/properties/kind/enum")
            .and_then(Value::as_array)
            .expect("kind enum");

        assert!(
            enum_values
                .iter()
                .any(|value| value.as_str() == Some("claude")),
            "calm.plan.upsert kind enum must advertise claude: {enum_values:?}"
        );
    }

    #[test]
    fn upsert_schema_discriminates_agent_goals_from_terminal_commands() {
        let descriptor = plan_upsert_descriptor();
        let item = &descriptor.input_schema["properties"]["tasks"]["items"];
        let goal_description = descriptor
            .input_schema
            .pointer("/properties/tasks/items/properties/goal/description")
            .and_then(Value::as_str)
            .expect("goal description");

        assert!(
            goal_description.contains("Natural-language objective")
                && goal_description.contains("forbidden for terminal"),
            "calm.plan.upsert goal description must stay agent-only: {goal_description}"
        );
        let command_description = item["properties"]["command"]["description"]
            .as_str()
            .expect("command description");
        assert!(
            command_description.contains("passed verbatim")
                && command_description.contains("required only for terminal")
                && command_description.contains("forbidden for codex/claude"),
            "calm.plan.upsert command description must stay terminal-only: {command_description}"
        );
        assert_eq!(
            item["oneOf"][0]["properties"]["kind"]["enum"],
            json!(["codex", "claude"])
        );
        assert_eq!(item["oneOf"][1]["properties"]["kind"]["const"], "terminal");
    }

    #[test]
    fn plan_list_uses_the_same_kind_discriminated_instruction_fields() {
        let agent = task_row_from_normalized(
            "track",
            &normalize_task_input(raw_task("agent")).unwrap(),
            1,
        );
        let agent = task_list_entry(&agent);
        assert_eq!(agent["goal"], "do the thing");
        assert!(agent.get("command").is_none());

        let mut terminal = raw_task("terminal");
        terminal.kind = "terminal".into();
        terminal.goal = "cargo test".into();
        let terminal =
            task_row_from_normalized("track", &normalize_task_input(terminal).unwrap(), 1);
        let terminal = task_list_entry(&terminal);
        assert_eq!(terminal["command"], "cargo test");
        assert!(terminal.get("goal").is_none());
    }

    #[test]
    fn unknown_kind_rejected() {
        let mut t = raw_task("a");
        t.kind = "banana".into();
        let err = normalize_task_input(t).expect_err("unknown kind");
        assert!(err.contains("unknown kind `banana`"), "err = {err}");
        assert!(
            err.contains("codex") && err.contains("claude") && err.contains("terminal"),
            "err = {err}"
        );
    }

    #[test]
    fn unknown_dep_rejected_and_same_batch_dep_accepted() {
        let err = validate_new_batch(&[normalized("a", &["ghost"])]).expect_err("unknown dep");
        assert!(err.contains("unknown dependency `ghost`"), "err = {err}");

        validate_new_batch(&[normalized("a", &["b"]), normalized("b", &[])])
            .expect("same-batch sibling dependency");
    }

    #[test]
    fn cycle_rejected_with_path_in_error() {
        let batch = vec![
            normalized("a", &["b"]),
            normalized("b", &["c"]),
            normalized("c", &["a"]),
        ];
        let err = validate_new_batch(&batch).expect_err("cycle");
        assert!(err.contains("dependency cycle:"), "err = {err}");
        for k in ["a", "b", "c"] {
            assert!(err.contains(k), "cycle path misses `{k}`: {err}");
        }
        assert!(err.contains(" -> "), "err = {err}");
    }

    #[test]
    fn self_dependency_is_a_cycle() {
        let err = validate_new_batch(&[normalized("a", &["a"])]).expect_err("self dep");
        assert!(err.contains("dependency cycle: a -> a"), "err = {err}");
    }

    #[test]
    fn resolver_and_task_block_diagnostics_are_equivalent_for_batch_rules() {
        use calm_types::report_blocks::tasks::project_task_declarations;
        use calm_types::track_report::ReportBlock;

        // Exhaust the 4^3 dependency graphs over three keys.
        let choices: [Option<&str>; 4] = [None, Some("a"), Some("b"), Some("c")];
        for a in choices {
            for b in choices {
                for c in choices {
                    let dependencies = [a, b, c];
                    let batch: Vec<NormalizedTask> = ["a", "b", "c"]
                        .into_iter()
                        .zip(dependencies)
                        .map(|(key, dependency)| {
                            normalized(key, &dependency.into_iter().collect::<Vec<_>>())
                        })
                        .collect();
                    let blocks: Vec<ReportBlock> = batch.iter().enumerate().map(|(index, task)| ReportBlock {
                        id: format!("b_{index:04x}"), kind: "task".into(), rev: 0,
                        payload: json!({"key":task.key,"kind":"codex","goal":"do it","depends_on":task.depends_on,"ready":true,"declared_by":"spec"}),
                    }).collect();
                    let diagnostics = project_task_declarations(&blocks).1;
                    assert_eq!(
                        validate_new_batch(&batch).is_err(),
                        diagnostics.iter().any(|items| !items.is_empty()),
                        "dependencies={dependencies:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn resolver_reports_the_first_duplicate_in_batch_order() {
        let batch = ["z", "z", "a", "a"].map(|key| normalized(key, &[]));
        assert_eq!(
            validate_new_batch(&batch).unwrap_err(),
            "duplicate key `z` in batch"
        );
    }

    #[test]
    fn normalized_declaration_gate_round_trips_and_invalid_json_fails() {
        let mut task = normalized("a", &[]);
        task.gate_json = Some(r#"{"steps":[{"name":"test","cmd":"cargo test"}]}"#.into());
        let declaration = declaration_from_normalized(&task).expect("valid gate JSON");
        assert_eq!(declaration.gate.unwrap().steps[0].cmd, "cargo test");

        task.gate_json = Some("{".into());
        let error = declaration_from_normalized(&task).unwrap_err();
        assert!(error.contains("invalid normalized gate_json"), "{error}");
    }

    #[test]
    fn empty_or_whitespace_goal_rejected() {
        for bad in ["", "   ", "\t\n"] {
            let mut t = raw_task("a");
            t.goal = bad.into();
            let err = normalize_task_input(t).expect_err("empty goal");
            assert!(
                err.contains("`goal` must be non-empty"),
                "goal {bad:?}: err = {err}"
            );
        }
    }

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
        let err = validate_gate_shape("a", &gate(vec![], None, None)).expect_err("empty steps");
        assert!(err.contains("gate.steps must be non-empty"), "err = {err}");

        let err = validate_gate_shape("a", &gate(vec![step("fmt", "  ")], None, None))
            .expect_err("empty cmd");
        assert!(err.contains("cmd must be non-empty"), "err = {err}");

        let err = validate_gate_shape("a", &gate(vec![step("fmt", "cargo\u{7}fmt")], None, None))
            .expect_err("control char");
        assert!(err.contains("ASCII control"), "err = {err}");

        let err = validate_gate_shape("a", &gate(vec![step("t", "true")], Some(7201), None))
            .expect_err("timeout cap");
        assert!(err.contains("1..=7200"), "err = {err}");

        for bad in [0, -1] {
            let err = validate_gate_shape("a", &gate(vec![step("t", "true")], Some(bad), None))
                .expect_err("non-positive timeout");
            assert!(err.contains("1..=7200"), "timeout {bad}: err = {err}");
        }

        let err = validate_gate_shape("a", &gate(vec![step("t", "true")], None, Some("rel/path")))
            .expect_err("relative gate cwd");
        assert!(err.contains("absolute path"), "err = {err}");

        validate_gate_shape(
            "a",
            &gate(vec![step("t", "cargo test")], Some(600), Some("/repo")),
        )
        .expect("valid shape");
    }

    #[test]
    fn declared_gate_accepted_and_stored_canonically() {
        let mut t = raw_task("a");
        t.gate = Some(gate(
            vec![step("test", "cargo test"), step("fmt", "cargo fmt --check")],
            Some(600),
            Some("  /repo "),
        ));
        let n = normalize_task_input(t).expect("gate accepted in PR-C");
        let gate_json = n.gate_json.expect("gate stored");
        let parsed: crate::operation::task_verify_adapter::GateSpec =
            serde_json::from_str(&gate_json).expect("stored bytes parse as GateSpec");
        assert_eq!(parsed.cwd.as_deref(), Some("/repo"), "gate.cwd is trimmed");
        assert_eq!(parsed.timeout_secs, Some(600));
        assert_eq!(parsed.steps.len(), 2);
        assert_eq!(parsed.steps[0].name, "test");
        assert_eq!(parsed.steps[0].cmd, "cargo test");

        let mut t2 = raw_task("a");
        t2.gate = Some(gate(
            vec![step("test", "cargo test"), step("fmt", "cargo fmt --check")],
            Some(600),
            Some("  /repo "),
        ));
        let n2 = normalize_task_input(t2).expect("normalize");
        assert_eq!(n2.gate_json.as_deref(), Some(gate_json.as_str()));

        let mut t3 = raw_task("a");
        t3.gate = Some(gate(vec![step("test", "cargo test")], None, None));
        let n3 = normalize_task_input(t3).expect("normalize");
        let bytes = n3.gate_json.expect("gate stored");
        assert!(!bytes.contains("cwd"), "absent cwd omitted: {bytes}");
        assert!(
            !bytes.contains("timeout_secs"),
            "absent timeout omitted: {bytes}"
        );

        let mut t4 = raw_task("a");
        t4.gate = Some(gate(vec![], None, None));
        let err = normalize_task_input(t4).expect_err("empty steps");
        assert!(err.contains("gate.steps must be non-empty"), "err = {err}");
    }

    #[test]
    fn no_gate_reason_requires_object_or_omitted_context() {
        let mut t = raw_task("a");
        t.context = Some(json!({ "hint": "x" }));
        t.no_gate_reason = Some("docs-only change".into());
        normalize_task_input(t).expect("object context");

        let mut t = raw_task("a");
        t.no_gate_reason = Some("r".into());
        normalize_task_input(t).expect("omitted context");

        let mut t = raw_task("a");
        t.context = Some(json!("a string"));
        t.no_gate_reason = Some("r".into());
        let err = normalize_task_input(t).expect_err("non-object context");
        assert!(
            err.contains("requires `context` to be an object"),
            "err = {err}"
        );
    }

    #[test]
    fn no_gate_reason_blank_rejected_valid_reason_trimmed() {
        for blank in ["", " ", "  \t\n "] {
            let mut t = raw_task("a");
            t.no_gate_reason = Some(blank.into());
            let err = normalize_task_input(t).expect_err("blank reason");
            assert!(
                err.contains("`no_gate_reason` must be a non-empty reason"),
                "err for {blank:?} = {err}"
            );
        }

        let mut t = raw_task("a");
        t.no_gate_reason = Some("  docs-only change  ".into());
        let n = normalize_task_input(t).expect("normalize");
        assert!(n.has_no_gate_reason);
    }

    #[test]
    fn depends_on_sorted_and_deduped_for_manifest_validation() {
        let n = normalized("a", &["c", "b", "c"]);
        assert_eq!(n.depends_on, vec!["b", "c"]);
        validate_new_batch(&[n, normalized("b", &[]), normalized("c", &[])])
            .expect("normalized sibling dependencies");
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
