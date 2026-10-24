//! `neige_report_commit`'s anchors (#1877): what this session last read, from the read ledger.

use super::required_string;
use crate::mcp_server::framing::RpcError;
use crate::track_report::BlockIntent;
use serde_json::Value;

/// `{ op: "replace", section, markdown }` / `{ op: "delete", section }`, anchored by this session's
/// last read of the section (no `if_rev`: a section has no single rev).
pub(super) fn parse_section_op(
    obj: &serde_json::Map<String, Value>,
    op: &str,
    section: String,
    at: &str,
) -> Result<BlockIntent, RpcError> {
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
    if op == "replace" {
        return Ok(BlockIntent::ReplaceSection {
            markdown: required_string(obj, "markdown", at)?,
            section,
        });
    }
    if obj.contains_key("markdown") {
        return Err(RpcError::invalid_params(format!(
            "{at}: a section `delete` takes no `markdown`"
        )));
    }
    Ok(BlockIntent::DeleteSection { section })
}
