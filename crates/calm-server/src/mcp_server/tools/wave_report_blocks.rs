//! Issue #960 PR2 — typed block-level wave-report MCP tools.
//!
//! The report is a CRDT block map (`crate::wave_report_doc`); these
//! tools are the primary write surface. `calm.report.write`/`edit`
//! remain as prose-only compatibility shims in `wave_report.rs`.
//!
//! ## Tool surface
//!
//! | Tool | Shape | Notes |
//! |---|---|---|
//! | `calm.report.blocks.kinds`  | `{}` | Self-describing kind vocabulary (static). |
//! | `calm.report.blocks.upsert` | `{ id?, kind, markdown?, payload?, if_rev?, if_doc_rev?, position? }` | Create (`id` absent + mandatory `if_doc_rev`) or replace (`id` + mandatory `if_rev`). Returns `{ id, rev, updated_at, docRev }`. |
//! | `calm.report.blocks.move`   | `{ id, to_index, if_doc_rev }` | Reorder; rev untouched. |
//! | `calm.report.blocks.delete` | `{ id, if_rev }` | `if_rev` mandatory. |
//! | `calm.report.write_markdown`| `{ body, summary?, if_doc_rev }` | The id-preserving whole-document write: guarded full-document Markdown, optionally carrying `<!-- neige:b_xxxx -->` marker lines that pin block identity. Markers are stripped server-side and never stored. |
//!
//! ## Concurrency contract
//!
//! Block `if_rev` and document `if_doc_rev` are checked **inside the
//! persist transaction against the appropriate CRDT truth**
//! (`ReportDoc::block_rev` / `ReportDoc::doc_rev`), never against the JSON
//! cache. A mismatch returns JSON-RPC error `-32001` ("rev conflict",
//! carrying both revs), the transaction aborts, nothing is written and
//! no events are emitted. A successful op keeps the dual-event
//! invariant: exactly one `CardUpdated` + one `WaveReportEdited`
//! (flat-projection `body_before/after`), and never touches `summary`.
//!
//! ## Authorization
//!
//! Identical to `calm.report.write`: `require_role(Spec)` at the entry,
//! wave binding via the caller's spec card
//! (`wave_report::resolve_report_for_caller`), and the write funnels
//! through `CardDecisionSink::commit_report_op` so recorder-shadow
//! gating and `EditAuthor::Spec` attribution stay uniform.

use crate::decision_sink::CardDecisionSink;
use crate::error::CalmError;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolHandler, ToolHandlerFuture, ToolRegistry, require_role,
};
use crate::mcp_server::tools::wave_report::{resolve_report_for_caller, updated_report_doc_rev};
use crate::model::CardRole;
use crate::wave_report::{BlockOpOutcome, ReportDocOp};
use calm_types::report_blocks;
use serde_json::{Value, json};
use std::sync::Arc;

mod contracts;

use contracts::{
    delete_descriptor, kinds_descriptor, kinds_table, move_descriptor, upsert_descriptor,
    write_markdown_descriptor,
};

pub const TOOL_REPORT_BLOCKS_KINDS: &str = "calm.report.blocks.kinds";
pub const TOOL_REPORT_BLOCKS_UPSERT: &str = "calm.report.blocks.upsert";
pub const TOOL_REPORT_BLOCKS_MOVE: &str = "calm.report.blocks.move";
pub const TOOL_REPORT_BLOCKS_DELETE: &str = "calm.report.blocks.delete";
pub const TOOL_REPORT_WRITE_MARKDOWN: &str = "calm.report.write_markdown";

/// JSON-RPC error code for an `if_rev` optimistic-concurrency
/// conflict (kernel-extension range; see framing.rs).
pub const RPC_REV_CONFLICT: i64 = -32001;

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(kinds_descriptor(), wrap(blocks_kinds));
    registry.register(upsert_descriptor(), wrap(blocks_upsert));
    registry.register(move_descriptor(), wrap(blocks_move));
    registry.register(delete_descriptor(), wrap(blocks_delete));
    registry.register(write_markdown_descriptor(), wrap(write_markdown));
}

/// Boxed-future wrapper, same shape as the other tool modules.
fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture { Box::pin(f(ctx, identity, args)) })
}

// ---------------------------------------------------------------------------
// calm.report.blocks.kinds
// ---------------------------------------------------------------------------

async fn blocks_kinds(
    _ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    _args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
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
    require_role(&identity, CardRole::Spec)?;
    let tool = TOOL_REPORT_BLOCKS_UPSERT;
    let obj = require_object(&args, tool)?;
    let id = optional_string(obj, "id", tool)?;
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
    let if_rev = optional_u32(obj, "if_rev", tool)?;
    let position = optional_index(obj, "position", tool)?;
    let if_doc_rev = optional_u64(obj, "if_doc_rev", tool)?;
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
    require_role(&identity, CardRole::Spec)?;
    let tool = TOOL_REPORT_BLOCKS_MOVE;
    let obj = require_object(&args, tool)?;
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

async fn blocks_delete(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    let tool = TOOL_REPORT_BLOCKS_DELETE;
    let obj = require_object(&args, tool)?;
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
    require_role(&identity, CardRole::Spec)?;
    let tool = TOOL_REPORT_WRITE_MARKDOWN;
    let obj = require_object(&args, tool)?;
    let body = required_string(obj, "body", tool)?;
    let summary_override = optional_string(obj, "summary", tool)?;
    let if_doc_rev = optional_u64(obj, "if_doc_rev", tool)?.ok_or_else(|| {
        RpcError::invalid_params(format!(
            "{tool}: `if_doc_rev` is required (use 0 for a new document)"
        ))
    })?;

    let (wave, _, report_card, current) = resolve_report_for_caller(&ctx, &identity).await?;
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
        .commit_report_op(&identity, wave, report_card, current, op, None, None)
        .await
    {
        Ok(out) => out,
        Err(e) => return Err(map_commit_err(tool, e)),
    };
    let doc_rev = updated_report_doc_rev(&card, tool)?;
    Ok(json!({ "updated_at": card.updated_at, "docRev": doc_rev }))
}

// ---------------------------------------------------------------------------
// Shared plumbing
// ---------------------------------------------------------------------------

/// Resolve the caller's report and run one block-level op through the
/// decision sink (same identity/attribution path as `report.write`).
async fn commit_block_op(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    tool: &str,
    op: ReportDocOp,
) -> Result<(crate::model::Card, Option<BlockOpOutcome>), RpcError> {
    let (wave, _, report_card, current) = resolve_report_for_caller(ctx, identity).await?;
    CardDecisionSink::from_app_context(ctx)
        .commit_report_op(identity, wave, report_card, current, op, None, None)
        .await
        .map_err(|e| map_commit_err(tool, e))
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
