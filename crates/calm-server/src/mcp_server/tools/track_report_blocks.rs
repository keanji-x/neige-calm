//! Typed block-level track-report MCP tools (`calm.report.blocks.*`, `write_markdown`, `commit`), the primary write surface.
//! `if_rev` / `if_doc_rev` are checked inside the persist transaction against CRDT truth, never the JSON cache; a mismatch is `-32001` and writes nothing.

use crate::decision_sink::{CardDecisionSink, ReportOpCommit};
use crate::error::CalmError;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolHandler, ToolHandlerFuture, ToolRegistry, require_role,
    require_role_any,
};
use crate::mcp_server::tools::lifecycle_args::{parse_optional_write_args, parse_write_args};
use crate::mcp_server::tools::track_report::{resolve_report_for_caller, updated_report_doc_rev};
use crate::model::{CardRole, TrackLifecycle};
use crate::track_report::{BatchBlockOp, MAX_BATCH_OPS, ReportDocOp};
use calm_types::report_blocks;
use serde_json::{Value, json};
use std::sync::Arc;

mod contracts;

use contracts::{
    commit_descriptor, delete_descriptor, kinds_descriptor, kinds_table, move_descriptor,
    upsert_descriptor, write_markdown_descriptor,
};

pub const TOOL_REPORT_BLOCKS_KINDS: &str = "calm.report.blocks.kinds";
pub const TOOL_REPORT_BLOCKS_UPSERT: &str = "calm.report.blocks.upsert";
pub const TOOL_REPORT_BLOCKS_MOVE: &str = "calm.report.blocks.move";
pub const TOOL_REPORT_BLOCKS_DELETE: &str = "calm.report.blocks.delete";
pub const TOOL_REPORT_WRITE_MARKDOWN: &str = "calm.report.write_markdown";
pub const TOOL_REPORT_COMMIT: &str = "calm.report.commit";

/// JSON-RPC error code for an `if_rev` optimistic-concurrency conflict (kernel-extension range).
pub const RPC_REV_CONFLICT: i64 = -32001;

/// The one `-32001` constructor for every report write. `data` carries the current revisions the message names
/// (`docRev` from `current doc_rev is N`, `rev` from `current rev is N`) so a retry can re-anchor without a full read.
pub(crate) fn rev_conflict_error(message: String) -> RpcError {
    let mut data = serde_json::Map::new();
    if let Some(doc_rev) = number_after(&message, "current doc_rev is ") {
        data.insert("docRev".into(), Value::from(doc_rev));
    }
    if let Some(rev) = number_after(&message, "current rev is ") {
        data.insert("rev".into(), Value::from(rev));
    }
    let mut error = RpcError::custom(RPC_REV_CONFLICT, message);
    if !data.is_empty() {
        error.data = Some(Value::Object(data));
    }
    error
}

fn number_after(text: &str, marker: &str) -> Option<u64> {
    let rest = &text[text.find(marker)? + marker.len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(kinds_descriptor(), wrap(blocks_kinds));
    registry.register(upsert_descriptor(), wrap(blocks_upsert));
    registry.register(move_descriptor(), wrap(blocks_move));
    registry.register(delete_descriptor(), wrap(blocks_delete));
    registry.register(write_markdown_descriptor(), wrap(write_markdown));
    registry.register(commit_descriptor(), wrap(commit));
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

async fn blocks_kinds(
    _ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    _args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Planner, CardRole::Assistant])?;
    Ok(kinds_table())
}

async fn blocks_upsert(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Planner, CardRole::Assistant])?;
    let tool = TOOL_REPORT_BLOCKS_UPSERT;
    let obj = require_object(&args, tool)?;
    let id = optional_string(obj, "id", tool)?;
    let (kind, content) = resolve_upsert_content(obj, tool)?;
    let if_rev = optional_u32(obj, "if_rev", tool)?;
    let position = optional_index(obj, "position", tool)?;
    let if_doc_rev = optional_u64(obj, "if_doc_rev", tool)?;
    let carried = parse_optional_write_args(&args, tool)?;
    planner_only_lifecycle(&identity, carried.lifecycle, tool)?;
    if id.is_some() {
        if if_doc_rev.is_some() {
            return Err(RpcError::invalid_params(format!(
                "{tool}: `if_doc_rev` is not valid when `id` is given; updates with `id` must use \
                 `if_rev` (the block-level rev)"
            )));
        }
        if if_rev.is_none() {
            return Err(RpcError::invalid_params(format!(
                "{tool}: `if_rev` is required when `id` is given (read the \
                 current rev from calm.report.read's blocks index)"
            )));
        }
        if position.is_some() {
            return Err(RpcError::invalid_params(format!(
                "{tool}: `position` is only valid when creating a new block; \
                 use calm.report.blocks.move to reorder"
            )));
        }
    } else if if_rev.is_some() {
        return Err(RpcError::invalid_params(format!(
            "{tool}: `if_rev` without `id` is meaningless — omit it when \
             creating a new block"
        )));
    } else if if_doc_rev.is_none() {
        return Err(RpcError::invalid_params(format!(
            "{tool}: `if_doc_rev` is now required when creating a block; read `docRev` from \
             `calm.report.read`, then retry with that value"
        )));
    }

    let outcome = commit_block_op(
        &ctx,
        &identity,
        tool,
        ReportDocOp::UpsertBlock {
            id,
            kind,
            content,
            if_rev,
            if_doc_rev,
            position,
        },
        carried.message,
        carried.lifecycle,
    )
    .await?;
    let ReportOpCommit {
        card,
        block,
        warnings,
    } = outcome;
    let block = block
        .ok_or_else(|| RpcError::internal(format!("{tool}: upsert produced no block outcome")))?;
    let doc_rev = updated_report_doc_rev(&card, tool)?;
    Ok(json!({
        "id": block.id,
        "rev": block.rev,
        "updated_at": card.updated_at,
        "docRev": doc_rev,
        "warnings": warnings,
    }))
}

async fn blocks_move(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Planner, CardRole::Assistant])?;
    let tool = TOOL_REPORT_BLOCKS_MOVE;
    let obj = require_object(&args, tool)?;
    reject_stray_write_args(obj, tool)?;
    let id = required_string(obj, "id", tool)?;
    let to_index = optional_index(obj, "to_index", tool)?
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: missing `to_index` (integer)")))?;
    let if_doc_rev = optional_u64(obj, "if_doc_rev", tool)?.ok_or_else(|| {
        RpcError::invalid_params(format!(
            "{tool}: `if_doc_rev` is now required; read `docRev` from \
             `calm.report.read`, then retry with that value"
        ))
    })?;

    let ReportOpCommit { card, block, .. } = commit_block_op(
        &ctx,
        &identity,
        tool,
        ReportDocOp::MoveBlock {
            id,
            to_index,
            if_doc_rev,
        },
        None,
        None,
    )
    .await?;
    let block = block
        .ok_or_else(|| RpcError::internal(format!("{tool}: move produced no block outcome")))?;
    let doc_rev = updated_report_doc_rev(&card, tool)?;
    Ok(
        json!({ "id": block.id, "rev": block.rev, "updated_at": card.updated_at, "docRev": doc_rev }),
    )
}

/// The assistant may delete prose blocks here and no task blocks at all; the refusal is enforced one layer down in
/// `track_report_edit_guard::guard_task_declarations` so it covers the whole-document shapes too.
async fn blocks_delete(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Planner, CardRole::Assistant])?;
    let tool = TOOL_REPORT_BLOCKS_DELETE;
    let obj = require_object(&args, tool)?;
    reject_stray_write_args(obj, tool)?;
    let id = required_string(obj, "id", tool)?;
    let if_rev = optional_u32(obj, "if_rev", tool)?.ok_or_else(|| {
        RpcError::invalid_params(format!(
            "{tool}: `if_rev` is required for delete (read the current rev \
             from calm.report.read's blocks index)"
        ))
    })?;

    let ReportOpCommit { card, .. } = commit_block_op(
        &ctx,
        &identity,
        tool,
        ReportDocOp::DeleteBlock { id, if_rev },
        None,
        None,
    )
    .await?;
    let doc_rev = updated_report_doc_rev(&card, tool)?;
    Ok(json!({ "updated_at": card.updated_at, "docRev": doc_rev }))
}

async fn write_markdown(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Planner, CardRole::Assistant])?;
    let tool = TOOL_REPORT_WRITE_MARKDOWN;
    let obj = require_object(&args, tool)?;
    let body = required_string(obj, "body", tool)?;
    let summary_override = optional_string(obj, "summary", tool)?;
    let if_doc_rev = optional_u64(obj, "if_doc_rev", tool)?.ok_or_else(|| {
        RpcError::invalid_params(format!(
            "{tool}: `if_doc_rev` is required (use 0 for a new document)"
        ))
    })?;
    let carried = parse_optional_write_args(&args, tool)?;
    planner_only_lifecycle(&identity, carried.lifecycle, tool)?;

    let (track, _, report_card, current) = resolve_report_for_caller(&ctx, &identity).await?;
    // Omitted summary = keep the existing one, resolved by the persist layer INSIDE the transaction; resolving from `current` here would let a concurrent summary write be silently reverted.
    let op = ReportDocOp::WriteMarkdown {
        summary: summary_override,
        body,
        if_doc_rev,
    };
    let ReportOpCommit { card, warnings, .. } = match CardDecisionSink::from_app_context(&ctx)
        .commit_report_op(
            &identity,
            track,
            report_card,
            current,
            op,
            carried.message,
            carried.lifecycle,
        )
        .await
    {
        Ok(out) => out,
        Err(e) => return Err(map_commit_err(tool, e)),
    };
    let doc_rev = updated_report_doc_rev(&card, tool)?;
    Ok(json!({ "updated_at": card.updated_at, "docRev": doc_rev, "warnings": warnings }))
}

/// The one-call update: every op is parsed with its single-op tool's rules, then the whole list lands as one
/// [`ReportDocOp::Batch`] inside one persist transaction (one doc-rev check, one docRev bump, one event pair).
async fn commit(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    let tool = TOOL_REPORT_COMMIT;
    let obj = require_object(&args, tool)?;
    let write_args = parse_write_args(&args, tool)?;
    let if_doc_rev = optional_u64(obj, "if_doc_rev", tool)?.ok_or_else(|| {
        RpcError::invalid_params(format!(
            "{tool}: `if_doc_rev` is required; read `docRev` from `calm.report.read`"
        ))
    })?;
    let summary = optional_string(obj, "summary", tool)?;
    let raw_ops = match obj.get("ops") {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(ops)) => ops.as_slice(),
        Some(_) => {
            return Err(RpcError::invalid_params(format!(
                "{tool}: `ops` must be an array of op objects"
            )));
        }
    };
    if raw_ops.len() > MAX_BATCH_OPS {
        return Err(RpcError::invalid_params(format!(
            "{tool}: `ops` carries {} entries; at most {MAX_BATCH_OPS} per commit",
            raw_ops.len()
        )));
    }
    if raw_ops.is_empty() && summary.is_none() && write_args.lifecycle.is_none() {
        return Err(RpcError::invalid_params(format!(
            "{tool}: nothing to commit — pass at least one of `ops`, `summary`, `lifecycle`"
        )));
    }
    let ops = raw_ops
        .iter()
        .enumerate()
        .map(|(index, raw)| parse_batch_op(raw, index, tool))
        .collect::<Result<Vec<_>, _>>()?;
    reject_duplicate_block_ids(&ops, tool)?;

    // The response reports the lifecycle transition this commit APPLIED, not the one it requested (a same-state request is a no-op).
    // Known gap: a transition another actor lands between this snapshot and the persist transaction is indistinguishable from ours.
    let (track, _, report_card, current) = resolve_report_for_caller(&ctx, &identity).await?;
    let track_id = track.id.clone();
    let lifecycle_before = track.lifecycle;
    let ReportOpCommit { card, warnings, .. } = CardDecisionSink::from_app_context(&ctx)
        .commit_report_op(
            &identity,
            track,
            report_card,
            current,
            ReportDocOp::Batch {
                if_doc_rev,
                summary,
                ops,
            },
            Some(write_args.message),
            write_args.lifecycle,
        )
        .await
        .map_err(|e| map_commit_err(tool, e))?;
    let doc_rev = updated_report_doc_rev(&card, tool)?;
    // Read off the persisted payload so it is exactly what the next `calm.report.read` would return.
    let blocks = card
        .payload
        .get("blocks")
        .and_then(Value::as_array)
        .ok_or_else(|| RpcError::internal(format!("{tool}: updated report payload has no blocks")))?
        .iter()
        .map(|block| {
            json!({
                "id": block.get("id").cloned().unwrap_or(Value::Null),
                "kind": block.get("kind").cloned().unwrap_or(Value::Null),
                "rev": block.get("rev").cloned().unwrap_or(Value::Null),
            })
        })
        .collect::<Vec<_>>();
    let lifecycle = match write_args.lifecycle {
        Some(_) => {
            let lifecycle_after = ctx
                .repo
                .track_get(track_id.as_str())
                .await
                .map_err(|e| RpcError::internal(format!("{tool}: track re-read: {e}")))?
                .ok_or_else(|| {
                    RpcError::internal(format!(
                        "{tool}: track {} vanished after commit",
                        track_id.as_str()
                    ))
                })?
                .lifecycle;
            if lifecycle_after == lifecycle_before {
                Value::Null
            } else {
                serde_json::to_value(lifecycle_after)
                    .map_err(|e| RpcError::internal(format!("{tool}: serialize lifecycle: {e}")))?
            }
        }
        None => Value::Null,
    };
    Ok(json!({
        "updated_at": card.updated_at,
        "docRev": doc_rev,
        "blocks": blocks,
        "lifecycle": lifecycle,
        "warnings": warnings,
    }))
}

/// One `ops[i]` of `calm.report.commit`, in the single-op tool's argument shape minus `if_doc_rev`.
fn parse_batch_op(raw: &Value, index: usize, tool: &str) -> Result<BatchBlockOp, RpcError> {
    let at = format!("{tool}: ops[{index}]");
    let obj = raw
        .as_object()
        .ok_or_else(|| RpcError::invalid_params(format!("{at}: must be an object")))?;
    if obj.contains_key("if_doc_rev") {
        return Err(RpcError::invalid_params(format!(
            "{at}: `if_doc_rev` belongs on the commit, not on an op"
        )));
    }
    let op = obj.get("op").and_then(Value::as_str).ok_or_else(|| {
        RpcError::invalid_params(format!(
            "{at}: missing `op` (one of \"upsert\", \"move\", \"delete\")"
        ))
    })?;
    match op {
        "upsert" => {
            let id = optional_string(obj, "id", &at)?;
            let (kind, content) = resolve_upsert_content(obj, &at)?;
            let if_rev = optional_u32(obj, "if_rev", &at)?;
            let position = optional_index(obj, "position", &at)?;
            if id.is_some() {
                if if_rev.is_none() {
                    return Err(RpcError::invalid_params(format!(
                        "{at}: `if_rev` is required when `id` is given (read the current rev \
                         from calm.report.read's blocks index)"
                    )));
                }
                if position.is_some() {
                    return Err(RpcError::invalid_params(format!(
                        "{at}: `position` is only valid when creating a new block; use a \
                         `move` op to reorder"
                    )));
                }
            } else if if_rev.is_some() {
                return Err(RpcError::invalid_params(format!(
                    "{at}: `if_rev` without `id` is meaningless — omit it when creating a new \
                     block"
                )));
            }
            Ok(BatchBlockOp::Upsert {
                id,
                kind,
                content,
                if_rev,
                position,
            })
        }
        "move" => {
            let id = required_string(obj, "id", &at)?;
            let to_index = optional_index(obj, "to_index", &at)?.ok_or_else(|| {
                RpcError::invalid_params(format!("{at}: missing `to_index` (integer)"))
            })?;
            Ok(BatchBlockOp::Move { id, to_index })
        }
        "delete" => {
            let id = required_string(obj, "id", &at)?;
            let if_rev = optional_u32(obj, "if_rev", &at)?.ok_or_else(|| {
                RpcError::invalid_params(format!(
                    "{at}: `if_rev` is required for delete (read the current rev from \
                     calm.report.read's blocks index)"
                ))
            })?;
            Ok(BatchBlockOp::Delete { id, if_rev })
        }
        other => Err(RpcError::invalid_params(format!(
            "{at}: unknown op `{other}` (one of \"upsert\", \"move\", \"delete\")"
        ))),
    }
}

/// Shared by `calm.report.blocks.upsert` and every `upsert` op of `calm.report.commit`, so the two cannot drift.
fn resolve_upsert_content(
    obj: &serde_json::Map<String, Value>,
    tool: &str,
) -> Result<(String, String), RpcError> {
    let kind = obj
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: missing `kind` (string)")))?
        .to_string();
    let content = if kind == report_blocks::KIND_PROSE {
        let markdown = match obj.get("markdown") {
            Some(Value::String(s)) => s.clone(),
            None | Some(Value::Null) => match obj.get("payload").and_then(|p| p.get("markdown")) {
                Some(Value::String(s)) => s.clone(),
                _ => {
                    return Err(RpcError::invalid_params(format!(
                        "{tool}: kind=prose requires `markdown` (string), either \
                         top-level or inside `payload`"
                    )));
                }
            },
            Some(_) => {
                return Err(RpcError::invalid_params(format!(
                    "{tool}: `markdown` must be a string if provided"
                )));
            }
        };
        // A prose block may not smuggle data blocks: an embedded ```neige-block fence would splinter into its own block on the next wholesale write.
        report_blocks::check_prose_markdown(&markdown)
            .map_err(|why| RpcError::invalid_params(format!("{tool}: {why}")))?;
        markdown
    } else if report_blocks::is_data_kind(&kind) {
        // Data kinds take a schema-validated `payload` object; the stored content is its canonical fence.
        if !matches!(obj.get("markdown"), None | Some(Value::Null)) {
            return Err(RpcError::invalid_params(format!(
                "{tool}: `markdown` is only valid for kind=prose — pass the {kind} data in \
                 `payload` (see calm.report.blocks.kinds)"
            )));
        }
        let payload = match obj.get("payload") {
            Some(payload @ Value::Object(_)) => payload,
            _ => {
                return Err(RpcError::invalid_params(format!(
                    "{tool}: kind={kind} requires a `payload` object (see \
                     calm.report.blocks.kinds for its schema)"
                )));
            }
        };
        report_blocks::render_data_block(&kind, payload)
            .map_err(|why| RpcError::invalid_params(format!("{tool}: {why}")))?
    } else {
        return Err(RpcError::invalid_params(format!(
            "{tool}: {}. See calm.report.blocks.kinds.",
            report_blocks::unknown_kind_message(&kind)
        )));
    };
    Ok((kind, content))
}

/// Callers have already applied [`planner_only_lifecycle`] (or are planner-only).
async fn commit_block_op(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    tool: &str,
    op: ReportDocOp,
    agent_message: Option<String>,
    lifecycle: Option<TrackLifecycle>,
) -> Result<ReportOpCommit, RpcError> {
    let (track, _, report_card, current) = resolve_report_for_caller(ctx, identity).await?;
    CardDecisionSink::from_app_context(ctx)
        .commit_report_op(
            identity,
            track,
            report_card,
            current,
            op,
            agent_message,
            lifecycle,
        )
        .await
        .map_err(|e| map_commit_err(tool, e))
}

/// `lifecycle` is the planner's field: an assistant that passes it is refused `-32403` before the report is even resolved.
fn planner_only_lifecycle(
    identity: &ToolCallIdentity,
    lifecycle: Option<TrackLifecycle>,
    tool: &str,
) -> Result<(), RpcError> {
    if lifecycle.is_some() && identity.role != CardRole::Planner {
        return Err(RpcError::custom(
            -32403,
            format!(
                "{tool}: forbidden: `lifecycle` is accepted from the Planner role only; \
                 role {:?} must omit it",
                identity.role
            ),
        ));
    }
    Ok(())
}

/// `move` / `delete` carry no `message` and no `lifecycle`; silently dropping either would let a planner believe a rationale was persisted or a track advanced.
fn reject_stray_write_args(
    obj: &serde_json::Map<String, Value>,
    tool: &str,
) -> Result<(), RpcError> {
    for key in ["message", "lifecycle"] {
        if obj.contains_key(key) {
            return Err(RpcError::invalid_params(format!(
                "{tool}: `{key}` is not accepted here; use `calm.report.commit` to carry \
                 a message or a lifecycle transition alongside block ops"
            )));
        }
    }
    Ok(())
}

/// A content-changing upsert bumps that block's rev, so a second op on the same block would need an `if_rev` the caller cannot know yet.
fn reject_duplicate_block_ids(ops: &[BatchBlockOp], tool: &str) -> Result<(), RpcError> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (index, op) in ops.iter().enumerate() {
        let id = match op {
            BatchBlockOp::Upsert { id, .. } => id.as_deref(),
            BatchBlockOp::Move { id, .. } | BatchBlockOp::Delete { id, .. } => Some(id.as_str()),
        };
        if let Some(id) = id
            && !seen.insert(id)
        {
            return Err(RpcError::invalid_params(format!(
                "{tool}: ops[{index}]: block `{id}` already addressed by an earlier op — each \
                 block id may appear at most once per commit"
            )));
        }
    }
    Ok(())
}

fn map_commit_err(tool: &str, e: CalmError) -> RpcError {
    match e {
        CalmError::Conflict(m) => rev_conflict_error(format!("{tool}: {m}")),
        CalmError::BadRequest(m) => RpcError::invalid_params(format!("{tool}: {m}")),
        CalmError::Forbidden(m) => RpcError::custom(-32403, format!("{tool}: forbidden: {m}")),
        other => RpcError::internal(format!("{tool}: {other}")),
    }
}

fn require_object<'a>(
    args: &'a Value,
    tool: &str,
) -> Result<&'a serde_json::Map<String, Value>, RpcError> {
    args.as_object()
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: arguments must be an object")))
}

fn required_string(
    obj: &serde_json::Map<String, Value>,
    key: &str,
    tool: &str,
) -> Result<String, RpcError> {
    obj.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: missing `{key}` (string)")))
}

fn optional_string(
    obj: &serde_json::Map<String, Value>,
    key: &str,
    tool: &str,
) -> Result<Option<String>, RpcError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(RpcError::invalid_params(format!(
            "{tool}: `{key}` must be a string if provided"
        ))),
    }
}

fn optional_u32(
    obj: &serde_json::Map<String, Value>,
    key: &str,
    tool: &str,
) -> Result<Option<u32>, RpcError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .map(Some)
            .ok_or_else(|| {
                RpcError::invalid_params(format!(
                    "{tool}: `{key}` must be a non-negative integer (u32)"
                ))
            }),
    }
}

fn optional_u64(
    obj: &serde_json::Map<String, Value>,
    key: &str,
    tool: &str,
) -> Result<Option<u64>, RpcError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            RpcError::invalid_params(format!(
                "{tool}: `{key}` must be a non-negative integer (u64)"
            ))
        }),
    }
}

fn optional_index(
    obj: &serde_json::Map<String, Value>,
    key: &str,
    tool: &str,
) -> Result<Option<usize>, RpcError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .map(Some)
            .ok_or_else(|| {
                RpcError::invalid_params(format!(
                    "{tool}: `{key}` must be a non-negative integer index"
                ))
            }),
    }
}

#[cfg(test)]
mod rev_conflict_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rev_conflict_error_parses_the_current_revisions_it_names() {
        let doc = rev_conflict_error(
            "calm.report.commit: document revision conflict: current doc_rev is 12, expected \
             if_doc_rev 11 — re-read the report and retry with the current docRev"
                .into(),
        );
        assert_eq!(doc.code, RPC_REV_CONFLICT);
        assert!(
            doc.message
                .starts_with("calm.report.commit: document revision conflict")
        );
        assert_eq!(doc.data, Some(json!({"docRev": 12})));

        let block = rev_conflict_error(
            "calm.report.blocks.upsert: rev conflict on block b_1: current rev is 7, expected \
             if_rev 3; current doc_rev is 12 — re-read the report and retry with the current rev"
                .into(),
        );
        assert_eq!(block.data, Some(json!({"docRev": 12, "rev": 7})));

        let edit = rev_conflict_error(
            "document revision conflict: current doc_rev is 3, expected if_doc_rev 2".into(),
        );
        assert_eq!(edit.data, Some(json!({"docRev": 3})));

        // No `data` at all rather than an empty object a retry might misread as "rev 0".
        let bare = rev_conflict_error("track_report: something else conflicted".into());
        assert_eq!(bare.data, None);
        assert_eq!(bare.message, "track_report: something else conflicted");
    }
}
