//! `neige_report_commit`'s anchors (#1877): what this session last read, from the read ledger.

use super::required_string;
use crate::mcp_server::framing::RpcError;
use crate::report_read_ledger::LastRead;
use crate::track_report::{BatchBlockOp, SectionRead};
use serde_json::Value;

/// `{ op: "replace", section, markdown }` / `{ op: "delete", section }`, anchored by this session's
/// last read of the section (no `if_rev`: a section has no single rev).
pub(super) fn parse_section_op(
    obj: &serde_json::Map<String, Value>,
    op: &str,
    section: String,
    at: &str,
    last_read: Option<&LastRead>,
) -> Result<BatchBlockOp, RpcError> {
    if !matches!(op, "replace" | "delete") {
        return Err(RpcError::invalid_params(format!(
            "{at}: op `{op}` does not take `section` (only \"replace\" and \"delete\" do)"
        )));
    }
    if let Some(key) = ["id", "kind", "payload", "position", "to_index"]
        .into_iter()
        .find(|key| obj.contains_key(*key))
    {
        return Err(RpcError::invalid_params(format!(
            "{at}: `{key}` is not accepted on a section op; it is anchored by this session's \
             last read of the section"
        )));
    }
    let read = match last_read.and_then(|read| read.sections.get(&section)) {
        Some(seen) => SectionRead::Seen(seen.clone()),
        None => SectionRead::Unseen,
    };
    if op == "replace" {
        return Ok(BatchBlockOp::ReplaceSection {
            markdown: required_string(obj, "markdown", at)?,
            section,
            read,
        });
    }
    if obj.contains_key("markdown") {
        return Err(RpcError::invalid_params(format!(
            "{at}: a section `delete` takes no `markdown`"
        )));
    }
    Ok(BatchBlockOp::DeleteSection { section, read })
}

/// An id op's `if_rev`: the rev this session last read of that block.
pub(super) fn block_anchor(
    last_read: Option<&LastRead>,
    id: &str,
    at: &str,
) -> Result<u32, RpcError> {
    last_read
        .and_then(|read| read.blocks.get(id).copied())
        .ok_or_else(|| {
            RpcError::invalid_params(format!(
                "{at}: block `{id}` has not been read by this session — read it first \
                 (neige_report_read {{ blocks: [\"{id}\"] }})"
            ))
        })
}
