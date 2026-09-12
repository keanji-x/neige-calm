//! Issue #960 PR2 — typed block-level track-report MCP tools.
//!
//! The report is a CRDT block map (`crate::track_report_doc`); these
//! tools are the primary write surface. `calm.report.write`/`edit`
//! remain as prose-only compatibility shims in `track_report.rs`.
//!
//! ## Tool surface
//!
//! | Tool | Shape | Notes |
//! |---|---|---|
//! | `calm.report.blocks.kinds`  | `{}` | Self-describing kind vocabulary (static). |
//! | `calm.report.blocks.upsert` | `{ id?, kind, markdown?, payload?, if_rev?, if_doc_rev?, position?, message?, lifecycle? }` | Create (`id` absent + mandatory `if_doc_rev`) or replace (`id` + mandatory `if_rev`). Returns `{ id, rev, updated_at, docRev }`. |
//! | `calm.report.blocks.move`   | `{ id, to_index, if_doc_rev }` | Reorder; rev untouched. A `message`/`lifecycle` key is refused (-32602). |
//! | `calm.report.blocks.delete` | `{ id, if_rev }` | `if_rev` mandatory. A `message`/`lifecycle` key is refused (-32602). |
//! | `calm.report.write_markdown`| `{ body, summary?, if_doc_rev, message?, lifecycle? }` | The id-preserving whole-document write: guarded full-document Markdown, optionally carrying `<!-- neige:b_xxxx -->` marker lines that pin block identity. Markers are stripped server-side and never stored. |
//! | `calm.report.commit`        | `{ if_doc_rev, message, ops?, summary?, lifecycle? }` | Planner-only: one user-intent update — an ordered list of block ops (`upsert`/`move`/`delete`, each in its single-op shape minus `if_doc_rev`) + optional summary + optional lifecycle, under ONE `if_doc_rev`; each existing block id at most once per commit (-32602). Returns `{ updated_at, docRev, blocks: [{ id, kind, rev }], lifecycle }` — `lifecycle` is the transition applied (null when none, incl. a same-state request). |
//!
//! ## Concurrency contract
//!
//! Block `if_rev` and document `if_doc_rev` are checked **inside the
//! persist transaction against the appropriate CRDT truth**
//! (`ReportDoc::block_rev` / `ReportDoc::doc_rev`), never against the JSON
//! cache. A mismatch returns JSON-RPC error `-32001` ("rev conflict",
//! carrying both revs), the transaction aborts, nothing is written and
//! no events are emitted. A successful op keeps the dual-event
//! invariant: exactly one `CardUpdated` + one `TrackReportEdited`
//! (flat-projection `body_before/after`). The block ops never touch
//! `summary`; `calm.report.commit` is the one block-channel write that
//! may (its optional `summary`), still inside the same single
//! transaction and the same event pair.
//!
//! ## Authorization
//!
//! `require_role_any([Planner, Assistant])` at the entry (#1189 §3.2b),
//! track binding via the caller's own card
//! (`track_report::resolve_report_for_caller`), and the write funnels
//! through `CardDecisionSink::commit_report_op` so recorder-shadow
//! gating and `EditAuthor` attribution stay uniform.
//!
//! The block channel — not `calm.report.write`/`.edit` — is where the
//! assistant role lives. `blocks.upsert` and `write_markdown` accept an
//! optional `message` (persisted as `agent_message`) and, for a
//! **Planner** caller only, an optional `lifecycle`; an assistant passing
//! `lifecycle` is refused at the entry (`-32403`) before anything is
//! resolved — that is the §3.2 dividing line, and `calm.report.commit`
//! (which always carries a message and may carry a lifecycle) is
//! planner-only outright. What the channel being open does *not* mean
//! is that an assistant may write anything a planner may: the sink
//! attributes its ops as `EditAuthor::Assistant` and suppresses
//! auto-promote, and `track_report_edit_guard` refuses any op of its
//! that would touch a task declaration block. A Worker token is still
//! refused here outright.

use crate::decision_sink::CardDecisionSink;
use crate::error::CalmError;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolHandler, ToolHandlerFuture, ToolRegistry, require_role,
    require_role_any,
};
use crate::mcp_server::tools::lifecycle_args::{parse_optional_write_args, parse_write_args};
use crate::mcp_server::tools::track_report::{resolve_report_for_caller, updated_report_doc_rev};
use crate::model::{CardRole, TrackLifecycle};
use crate::track_report::{BatchBlockOp, BlockOpOutcome, MAX_BATCH_OPS, ReportDocOp};
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

/// JSON-RPC error code for an `if_rev` optimistic-concurrency
/// conflict (kernel-extension range; see framing.rs).
pub const RPC_REV_CONFLICT: i64 = -32001;

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(kinds_descriptor(), wrap(blocks_kinds));
    registry.register(upsert_descriptor(), wrap(blocks_upsert));
    registry.register(move_descriptor(), wrap(blocks_move));
    registry.register(delete_descriptor(), wrap(blocks_delete));
    registry.register(write_markdown_descriptor(), wrap(write_markdown));
    registry.register(commit_descriptor(), wrap(commit));
}

/// Boxed-future wrapper, same shape as the other tool modules.
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

// ---------------------------------------------------------------------------
// calm.report.blocks.kinds
// ---------------------------------------------------------------------------

async fn blocks_kinds(
    _ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    _args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Planner, CardRole::Assistant])?;
    Ok(kinds_table())
}

// ---------------------------------------------------------------------------
// calm.report.blocks.upsert
// ---------------------------------------------------------------------------

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
    let (card, block) = outcome;
    let block = block
        .ok_or_else(|| RpcError::internal(format!("{tool}: upsert produced no block outcome")))?;
    let doc_rev = updated_report_doc_rev(&card, tool)?;
    Ok(
        json!({ "id": block.id, "rev": block.rev, "updated_at": card.updated_at, "docRev": doc_rev }),
    )
}

// ---------------------------------------------------------------------------
// calm.report.blocks.move
// ---------------------------------------------------------------------------

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

    let (card, block) = commit_block_op(
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

// ---------------------------------------------------------------------------
// calm.report.blocks.delete
// ---------------------------------------------------------------------------

/// #1189 ruling — the assistant may delete *prose* blocks here, and no
/// task blocks at all. Deleting one is not a right it grows into later:
/// it cannot create a task block in the first place (`author_name`
/// gives `EditAuthor::Assistant` no attribution name), so "delete the
/// ones I declared" is the empty set, and everything it *could* reach is
/// a declaration some planner or user made. The refusal is enforced one
/// layer down, in `track_report_edit_guard::guard_task_declarations`, so
/// it covers the whole-document shapes too — this entry point stays open
/// so the prose case works.
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

    let (card, _none) = commit_block_op(
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

// ---------------------------------------------------------------------------
// calm.report.write_markdown
// ---------------------------------------------------------------------------

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
    // Omitted summary = keep the existing one. The op carries `None`
    // and the persist layer resolves it against the doc INSIDE the
    // transaction — resolving from the `current` snapshot here would
    // let a concurrent summary write be silently reverted (TOCTOU,
    // #960 PR2 review).
    let op = ReportDocOp::WriteMarkdown {
        summary: summary_override,
        body,
        if_doc_rev,
    };
    let (card, _none) = match CardDecisionSink::from_app_context(&ctx)
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
    Ok(json!({ "updated_at": card.updated_at, "docRev": doc_rev }))
}

// ---------------------------------------------------------------------------
// calm.report.commit
// ---------------------------------------------------------------------------

/// Planner feedback #1 — the one-call update: `{ ops, summary?, lifecycle?,
/// message, if_doc_rev }`. Every op is parsed with the same rules as its
/// single-op tool (content via [`resolve_upsert_content`], `if_rev` shape
/// rules, `to_index`), then the whole list lands as one
/// [`ReportDocOp::Batch`] inside one persist transaction: one doc-rev
/// check, one docRev bump, one `CardUpdated` + one `TrackReportEdited`,
/// plus the lifecycle events when a transition applies.
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

    // Resolved here rather than through `commit_block_op` because the
    // response reports the lifecycle transition this commit APPLIED, not
    // the one it requested: `apply_requested_transition_in_tx` is a no-op
    // for a same-state request, so the honest value is the persisted
    // lifecycle after the write when it differs from the snapshot before
    // it. Known gap: a transition another actor lands between this
    // snapshot and the persist transaction is indistinguishable from ours.
    let (track, _, report_card, current) = resolve_report_for_caller(&ctx, &identity).await?;
    let track_id = track.id.clone();
    let lifecycle_before = track.lifecycle;
    let (card, _none) = CardDecisionSink::from_app_context(&ctx)
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
    // The response index is read off the persisted payload — the doc's
    // own post-op snapshot — so it is exactly what the next
    // `calm.report.read` would return.
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
    }))
}

/// One `ops[i]` of `calm.report.commit`: `{ op: "upsert" | "move" |
/// "delete", ... }` in the single-op tool's argument shape, minus
/// `if_doc_rev` (the batch carries one for all).
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

/// Resolve `(kind, content)` from an upsert-shaped object: `prose` takes
/// its markdown top-level or in `payload.markdown` and is checked for
/// smuggled fences; data kinds take a schema-validated `payload` and
/// store its canonical fence. Shared by `calm.report.blocks.upsert` and
/// every `upsert` op of `calm.report.commit`, so the two cannot drift.
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
        // Prose content: top-level `markdown`, falling back to
        // `payload.markdown` (the shape blocks.kinds documents).
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
        // A prose block may not smuggle data blocks: an embedded
        // ```neige-block fence (well-formed or typo'd) would either
        // splinter into its own block on the next wholesale write or
        // silently persist a malformed data block as prose. The rule
        // itself lives in `calm_types::report_blocks` — shared with the
        // #955 proposal lowering so the two cannot drift.
        report_blocks::check_prose_markdown(&markdown)
            .map_err(|why| RpcError::invalid_params(format!("{tool}: {why}")))?;
        markdown
    } else if report_blocks::is_data_kind(&kind) {
        // Data kinds take a schema-validated `payload` object; the
        // stored content is its canonical fence (deterministic pretty
        // JSON — design §3.5).
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

// ---------------------------------------------------------------------------
// Shared plumbing
// ---------------------------------------------------------------------------

/// Resolve the caller's report and run one block-level op through the
/// decision sink (same identity/attribution path as `report.write`).
/// `agent_message` / `lifecycle` ride the same persist call; callers
/// have already applied [`planner_only_lifecycle`] (or are planner-only).
async fn commit_block_op(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    tool: &str,
    op: ReportDocOp,
    agent_message: Option<String>,
    lifecycle: Option<TrackLifecycle>,
) -> Result<(crate::model::Card, Option<BlockOpOutcome>), RpcError> {
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

/// §3.2 — `lifecycle` is the planner's field. The block tools are open to
/// the assistant role, so the refusal is per-argument rather than
/// per-tool: an assistant that passes `lifecycle` is refused `-32403`
/// before the report is even resolved, and nothing is written.
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

/// `calm.report.blocks.move` / `.delete` carry no `message` and no
/// `lifecycle` — the prompt says so, and silently dropping either would
/// let a planner believe a rationale was persisted or a track advanced.
/// Refused `-32602` naming the key, before anything is resolved.
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

/// Ops in a batch apply in order to the doc as the previous ops left it,
/// and a content-changing upsert bumps that block's rev — so a second op
/// on the same block would need an `if_rev` the caller cannot know yet.
/// Rather than publish intra-batch rev arithmetic, each existing block id
/// may appear at most once per commit (`-32602` at parse time).
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

/// `CalmError` → JSON-RPC for the block tools:
///   * `Conflict` (if_rev mismatch) → `-32001`, message keeps the
///     "rev conflict … current … expected" detail;
///   * `BadRequest` (unknown id, bad index) → `-32602`;
///   * `Forbidden` (recorder gate / lifecycle) → `-32403` (parity with
///     `commit_report_write_for_identity`);
///   * anything else → internal.
fn map_commit_err(tool: &str, e: CalmError) -> RpcError {
    match e {
        CalmError::Conflict(m) => RpcError::custom(RPC_REV_CONFLICT, format!("{tool}: {m}")),
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
