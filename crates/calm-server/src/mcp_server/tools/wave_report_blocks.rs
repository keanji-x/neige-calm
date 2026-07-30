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
//! | `calm.report.blocks.upsert` | `{ id?, kind, markdown?, payload?, if_rev?, position? }` | Create (`id` absent) or replace (`id` + mandatory `if_rev`). Returns `{ id, rev, updated_at }`. |
//! | `calm.report.blocks.move`   | `{ id, to_index, if_rev? }` | Reorder; rev untouched. |
//! | `calm.report.blocks.delete` | `{ id, if_rev }` | `if_rev` mandatory. |
//! | `calm.report.write_markdown`| `{ body, summary? }` | Escape hatch: full-document Markdown, optionally carrying `<!-- neige:b_xxxx -->` marker lines that pin block identity. Markers are stripped server-side and never stored. |
//!
//! ## Concurrency contract
//!
//! `if_rev` is checked **inside the persist transaction against the
//! CRDT truth** (`ReportDoc::block_rev`), never against the JSON
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
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    read_only_annotations, require_role, role_gated_write_annotations,
};
use crate::mcp_server::tools::wave_report::resolve_report_for_caller;
use crate::model::CardRole;
use crate::wave_report::{BlockOpOutcome, ReportDocOp};
use serde_json::{Value, json};
use std::sync::Arc;

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

fn kinds_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_BLOCKS_KINDS.into(),
        description: "Spec-only: list the block kinds a wave report can \
             contain. Returns `{ kinds: [{ kind, schema, usage }] }` \
             where `schema` is the JSON Schema of that kind's payload. \
             Currently only `prose` exists; richer kinds \
             (chart.candles, table, app) arrive in a later release."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
        annotations: Some(read_only_annotations()),
        visible_to_roles: &[CardRole::Spec],
    }
}

/// The static kind table. PR3 extends this with `chart.candles` /
/// `table` / `app`; keep it the single source the tool serves.
fn kinds_table() -> Value {
    json!({
        "kinds": [
            {
                "kind": "prose",
                "schema": {
                    "type": "object",
                    "required": ["markdown"],
                    "properties": {
                        "markdown": { "type": "string", "description": "The block's Markdown source." }
                    }
                },
                "usage": "Free-form Markdown prose. Create or replace via \
                     `calm.report.blocks.upsert` passing the content in the \
                     top-level `markdown` argument. Blocks are split at \
                     H1/H2 headings, so a prose block conventionally starts \
                     with one."
            }
        ]
    })
}

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

fn upsert_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_BLOCKS_UPSERT.into(),
        description: "Spec-only: create or replace ONE report block. \
             Without `id`: creates a new block (appended at the end, \
             or inserted at `position`). With `id`: replaces that \
             block's content and REQUIRES `if_rev` (the rev you read); \
             a mismatch returns error -32001 (rev conflict) and writes \
             nothing — re-read and retry. `kind` must be `prose` for \
             now (see calm.report.blocks.kinds); prose content goes in \
             `markdown`. Returns `{ id, rev, updated_at }` — keep the \
             returned rev for your next edit of the same block. Get \
             ids/revs from `calm.report.read`'s `blocks` index. The \
             report summary is not touched."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["kind"],
            "properties": {
                "id": { "type": "string", "description": "Existing block id to replace. Omit to create a new block." },
                "kind": { "type": "string", "description": "Block kind; only `prose` is accepted today." },
                "markdown": { "type": "string", "description": "Prose content (required for kind=prose)." },
                "payload": { "type": "object", "description": "Kind-specific payload; for prose, `{ markdown }` (alternative to the top-level `markdown`)." },
                "if_rev": { "type": "integer", "minimum": 0, "description": "Required when `id` is given: the block rev you last read." },
                "position": { "type": "integer", "minimum": 0, "description": "Insertion index for a NEW block (default: append)." }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Spec],
    }
}

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
    if kind != "prose" {
        return Err(RpcError::invalid_params(format!(
            "{tool}: unsupported kind `{kind}` — only `prose` exists today; \
             chart.candles/table/app land in a later release (PR3). \
             See calm.report.blocks.kinds."
        )));
    }
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
    let if_rev = optional_u32(obj, "if_rev", tool)?;
    let position = optional_index(obj, "position", tool)?;
    if id.is_some() {
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
    }

    let outcome = commit_block_op(
        &ctx,
        &identity,
        tool,
        ReportDocOp::UpsertBlock {
            id,
            kind,
            markdown,
            if_rev,
            position,
        },
    )
    .await?;
    let (card, block) = outcome;
    let block = block
        .ok_or_else(|| RpcError::internal(format!("{tool}: upsert produced no block outcome")))?;
    Ok(json!({ "id": block.id, "rev": block.rev, "updated_at": card.updated_at }))
}

// ---------------------------------------------------------------------------
// calm.report.blocks.move
// ---------------------------------------------------------------------------

fn move_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_BLOCKS_MOVE.into(),
        description: "Spec-only: move a report block to `to_index` (its \
             final 0-based index in document order). Content and rev \
             are untouched — ordering is not content. Optional \
             `if_rev` guards against concurrent edits of the same \
             block (mismatch → error -32001, nothing written). Returns \
             `{ id, rev, updated_at }`."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["id", "to_index"],
            "properties": {
                "id": { "type": "string" },
                "to_index": { "type": "integer", "minimum": 0 },
                "if_rev": { "type": "integer", "minimum": 0 }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Spec],
    }
}

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
    let if_rev = optional_u32(obj, "if_rev", tool)?;

    let (card, block) = commit_block_op(
        &ctx,
        &identity,
        tool,
        ReportDocOp::MoveBlock {
            id,
            to_index,
            if_rev,
        },
    )
    .await?;
    let block = block
        .ok_or_else(|| RpcError::internal(format!("{tool}: move produced no block outcome")))?;
    Ok(json!({ "id": block.id, "rev": block.rev, "updated_at": card.updated_at }))
}

// ---------------------------------------------------------------------------
// calm.report.blocks.delete
// ---------------------------------------------------------------------------

fn delete_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_BLOCKS_DELETE.into(),
        description: "Spec-only: delete a report block. `if_rev` is \
             REQUIRED (destructive op): pass the rev you last read; a \
             mismatch returns error -32001 (rev conflict) and deletes \
             nothing. Returns `{ updated_at }`."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["id", "if_rev"],
            "properties": {
                "id": { "type": "string" },
                "if_rev": { "type": "integer", "minimum": 0 }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Spec],
    }
}

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
    Ok(json!({ "updated_at": card.updated_at }))
}

// ---------------------------------------------------------------------------
// calm.report.write_markdown
// ---------------------------------------------------------------------------

fn write_markdown_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_WRITE_MARKDOWN.into(),
        description: "Spec-only escape hatch: wholesale-replace the \
             report from full-document Markdown. The body MAY contain \
             the `<!-- neige:b_xxxx -->` marker lines that \
             `calm.report.read { with_markers: true }` emits — each \
             marker pins the block that follows it to that existing \
             block id (its rev bumps only if the content changed). \
             Marker lines are ALWAYS stripped server-side and never \
             stored; blocks without markers are re-matched \
             best-effort. Prefer `calm.report.blocks.*` for targeted \
             edits; use this for large restructurings. Omitting \
             `summary` keeps the existing one. Returns \
             `{ updated_at }`."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["body"],
            "properties": {
                "body": { "type": "string", "description": "Full report Markdown, optionally with `<!-- neige:b_xxxx -->` marker lines." },
                "summary": { "type": "string" }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Spec],
    }
}

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

    let (wave, _, report_card, current) = resolve_report_for_caller(&ctx, &identity).await?;
    // Omitted summary = keep the existing one. The op carries `None`
    // and the persist layer resolves it against the doc INSIDE the
    // transaction — resolving from the `current` snapshot here would
    // let a concurrent summary write be silently reverted (TOCTOU,
    // #960 PR2 review).
    let op = ReportDocOp::WriteMarkdown {
        summary: summary_override,
        body,
    };
    let (card, _none) = match CardDecisionSink::from_app_context(&ctx)
        .commit_report_op(&identity, wave, report_card, current, op, None, None)
        .await
    {
        Ok(out) => out,
        Err(e) => return Err(map_commit_err(tool, e)),
    };
    Ok(json!({ "updated_at": card.updated_at }))
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
