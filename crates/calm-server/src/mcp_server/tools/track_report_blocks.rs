//! The agent report write surface (`neige_report_commit`, `neige_report_write`) plus the read-only `neige_report_describe`.
//! Neither write takes a revision (#1883): the anchors are what this session last read through `neige_report_read`, checked inside
//! the persist transaction against CRDT truth; a mismatch is `-32001` and writes nothing.

use crate::decision_sink::{CardDecisionSink, ReportOpCommit};
use crate::error::CalmError;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolHandler, ToolHandlerFuture, ToolRegistry, require_role_any,
};
use crate::mcp_server::tools::source::reject_unknown_keys;
use crate::mcp_server::tools::track_report::{resolve_report_for_caller, updated_report_doc_rev};
use crate::mcp_server::tools::write_args::{parse_optional_write_args, parse_write_args};
use crate::model::CardRole;
use crate::report_read_ledger::LastRead;
use crate::track_report::{BatchBlockOp, DocAnchor, MAX_BATCH_OPS, ReportBlock, ReportDocOp};
use calm_types::report_blocks;
use serde_json::{Value, json};
use std::sync::Arc;

mod anchors;
mod contracts;

use anchors::{block_anchor, parse_section_op};
use contracts::{commit_descriptor, kinds_descriptor, kinds_table, write_markdown_descriptor};

pub const TOOL_REPORT_DESCRIBE: &str = "neige_report_describe";
pub const TOOL_REPORT_WRITE: &str = "neige_report_write";
pub const TOOL_REPORT_COMMIT: &str = "neige_report_commit";

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

async fn write_markdown(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Planner, CardRole::Assistant])?;
    let tool = TOOL_REPORT_WRITE;
    let obj = require_object(&args, tool)?;
    let message = parse_optional_write_args(&args, tool)?;
    reject_unknown_keys(obj, &["body", "summary", "message"], tool)?;
    let body = required_string(obj, "body", tool)?;
    let summary_override = optional_string(obj, "summary", tool)?;

    let (track, _, report_card, current) = resolve_report_for_caller(&ctx, &identity).await?;
    // A rewrite drops every block its body does not carry, so the session must have seen all of them.
    let if_doc_rev = match ctx
        .read_ledger
        .last_read(&identity.session_id, report_card.id.as_str())
    {
        Some(LastRead {
            doc_rev,
            whole: true,
            ..
        }) => doc_rev,
        _ => {
            return Err(RpcError::invalid_params(format!(
                "{tool}: this session has not read the whole report at its current docRev — \
                 read it with a full neige_report_read (`with_markers: true` keeps the block ids), \
                 then retry"
            )));
        }
    };
    // Omitted summary = keep the existing one, resolved by the persist layer INSIDE the transaction; resolving from `current` here would let a concurrent summary write be silently reverted.
    let op = ReportDocOp::WriteMarkdown {
        summary: summary_override,
        body,
        if_doc_rev,
    };
    let report_card_id = report_card.id.clone();
    let ReportOpCommit { card, warnings, .. } = match CardDecisionSink::from_app_context(&ctx)
        .commit_report_op(&identity, track, report_card, current, op, message)
        .await
    {
        Ok(out) => out,
        Err(e) => return Err(map_commit_err(tool, e)),
    };
    let doc_rev = updated_report_doc_rev(&card, tool)?;
    // The rewrite was anchored on this session's whole read, so, like an own commit, it counts as a
    // read of the whole report at the new docRev. The blocks are the tx's post state, not the body
    // sent: where the write path altered what landed (the contract header is normalized), the
    // ledger holds what landed, and the session re-reads before relying on that block's text.
    let blocks: Vec<ReportBlock> = card
        .payload
        .get("blocks")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| RpcError::internal(format!("{tool}: updated report blocks: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!("{tool}: updated report payload has no blocks"))
        })?;
    let every: Vec<String> = blocks.iter().map(|block| block.id.clone()).collect();
    ctx.read_ledger.record(
        &identity.session_id,
        &identity.card_id,
        report_card_id.as_str(),
        doc_rev,
        &blocks,
        &every,
    );
    Ok(json!({ "updated_at": card.updated_at, "docRev": doc_rev, "warnings": warnings }))
}

/// The one-call update: the whole op list lands as one [`ReportDocOp::Batch`] inside one persist
/// transaction (one docRev bump, one event pair), anchored by what this session last read (#1877).
/// Open to the assistant, whose task-block writes the task guard refuses.
async fn commit(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Planner, CardRole::Assistant])?;
    let tool = TOOL_REPORT_COMMIT;
    let obj = require_object(&args, tool)?;
    let message = parse_write_args(&args, tool)?;
    reject_unknown_keys(obj, &["message", "summary", "ops"], tool)?;
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
    if raw_ops.is_empty() && summary.is_none() {
        return Err(RpcError::invalid_params(format!(
            "{tool}: nothing to commit — pass at least one of `ops`, `summary`"
        )));
    }
    let (track, _, report_card, current) = resolve_report_for_caller(&ctx, &identity).await?;
    let last_read = ctx
        .read_ledger
        .last_read(&identity.session_id, report_card.id.as_str());
    let ops = raw_ops
        .iter()
        .enumerate()
        .map(|(index, raw)| parse_batch_op(raw, index, tool, last_read.as_ref()))
        .collect::<Result<Vec<_>, _>>()?;
    reject_duplicate_block_ids(&ops, tool)?;
    let doc_anchor = match &last_read {
        Some(read) => DocAnchor::LastRead(read.doc_rev),
        None => DocAnchor::Unread,
    };
    let report_card_id = report_card.id.clone();
    let ReportOpCommit {
        card,
        warnings,
        authored,
        doc_anchor_checked,
        ..
    } = CardDecisionSink::from_app_context(&ctx)
        .commit_report_op(
            &identity,
            track,
            report_card,
            current,
            ReportDocOp::Batch {
                doc_anchor,
                summary,
                ops,
            },
            Some(message),
        )
        .await
        .map_err(|e| map_commit_err(tool, e))?;
    let doc_rev = updated_report_doc_rev(&card, tool)?;
    ctx.read_ledger.record_authored(
        &identity.session_id,
        report_card_id.as_str(),
        &authored,
        doc_anchor_checked.then_some(doc_rev),
    );
    // Read off the persisted payload so it is exactly what the next `neige_report_read` would return.
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
    Ok(json!({
        "updated_at": card.updated_at,
        "docRev": doc_rev,
        "blocks": blocks,
        "warnings": warnings,
    }))
}

/// One `ops[i]` of `neige_report_commit`: an id op anchored by the block's rev in `last_read`, or a section op.
fn parse_batch_op(
    raw: &Value,
    index: usize,
    tool: &str,
    last_read: Option<&LastRead>,
) -> Result<BatchBlockOp, RpcError> {
    let at = format!("{tool}: ops[{index}]");
    let obj = raw
        .as_object()
        .ok_or_else(|| RpcError::invalid_params(format!("{at}: must be an object")))?;
    reject_unknown_keys(
        obj,
        &[
            "op", "section", "id", "kind", "markdown", "payload", "position", "to_index",
        ],
        &at,
    )?;
    let op = obj.get("op").and_then(Value::as_str).ok_or_else(|| {
        RpcError::invalid_params(format!(
            "{at}: missing `op` (one of \"replace\", \"upsert\", \"move\", \"delete\")"
        ))
    })?;
    if let Some(section) = optional_string(obj, "section", &at)? {
        return parse_section_op(obj, op, section, &at, last_read);
    }
    match op {
        "upsert" => {
            let id = optional_string(obj, "id", &at)?;
            let (kind, content) = resolve_upsert_content(obj, &at)?;
            let position = optional_index(obj, "position", &at)?;
            let if_rev = match &id {
                Some(_) if position.is_some() => {
                    return Err(RpcError::invalid_params(format!(
                        "{at}: `position` is only valid when creating a new block; use a \
                         `move` op to reorder"
                    )));
                }
                Some(id) => Some(block_anchor(last_read, id, &at)?),
                None => None,
            };
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
            let if_rev = block_anchor(last_read, &id, &at)?;
            Ok(BatchBlockOp::Delete { id, if_rev })
        }
        "replace" => Err(RpcError::invalid_params(format!(
            "{at}: `replace` needs `section` (the H1 heading text) and `markdown`"
        ))),
        other => Err(RpcError::invalid_params(format!(
            "{at}: unknown op `{other}` (one of \"replace\", \"upsert\", \"move\", \"delete\")"
        ))),
    }
}

/// The content of one `upsert` op of `neige_report_commit`.
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
                 `payload` (see neige_report_describe)"
            )));
        }
        let payload = match obj.get("payload") {
            Some(payload @ Value::Object(_)) => payload,
            _ => {
                return Err(RpcError::invalid_params(format!(
                    "{tool}: kind={kind} requires a `payload` object (see \
                     neige_report_describe for its schema)"
                )));
            }
        };
        report_blocks::render_data_block(&kind, payload)
            .map_err(|why| RpcError::invalid_params(format!("{tool}: {why}")))?
    } else {
        return Err(RpcError::invalid_params(format!(
            "{tool}: {}. See neige_report_describe.",
            report_blocks::unknown_kind_message(&kind)
        )));
    };
    Ok((kind, content))
}

/// A content-changing upsert bumps that block's rev, so a second op on the same block would need an `if_rev` the caller cannot know yet.
/// Likewise a section: its replace or delete changes its blocks' revs.
fn reject_duplicate_block_ids(ops: &[BatchBlockOp], tool: &str) -> Result<(), RpcError> {
    let mut seen: std::collections::HashSet<(bool, &str)> = std::collections::HashSet::new();
    for (index, op) in ops.iter().enumerate() {
        let target = match op {
            BatchBlockOp::Upsert { id, .. } => id.as_deref().map(|id| (false, id)),
            BatchBlockOp::Move { id, .. } | BatchBlockOp::Delete { id, .. } => {
                Some((false, id.as_str()))
            }
            BatchBlockOp::ReplaceSection { section, .. }
            | BatchBlockOp::DeleteSection { section, .. } => Some((true, section.as_str())),
        };
        if let Some(target @ (is_section, name)) = target
            && !seen.insert(target)
        {
            let (what, each) = if is_section {
                ("section", "section")
            } else {
                ("block", "block id")
            };
            return Err(RpcError::invalid_params(format!(
                "{tool}: ops[{index}]: {what} `{name}` already addressed by an earlier op — each \
                 {each} may appear at most once per commit"
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
            "neige_report_commit: document revision conflict: current doc_rev is 12, expected \
             if_doc_rev 11 — re-read the report and retry with the current docRev"
                .into(),
        );
        assert_eq!(doc.code, RPC_REV_CONFLICT);
        assert!(
            doc.message
                .starts_with("neige_report_commit: document revision conflict")
        );
        assert_eq!(doc.data, Some(json!({"docRev": 12})));

        let block = rev_conflict_error(
            "neige_report_commit: ops[0]: rev conflict on block b_1: current rev is 7, expected \
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
