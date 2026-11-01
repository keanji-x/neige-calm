//! Bounded preview; the immutable before/after indexes retain complete evidence.
use crate::decision_sink::ReportOpCommit;
use crate::mcp_server::framing::RpcError;
use serde_json::{Value, json};

pub(super) fn render(out: ReportOpCommit, tool: &str) -> Result<Value, RpcError> {
    let doc_rev = super::updated_report_doc_rev(&out.card, tool)?;
    let total = out.changes.len();
    let mut bytes = 2; // JSON array brackets; each row also reserves its separator.
    let changes: Vec<Value> = out
        .changes
        .iter()
        .take(32)
        .map(|change| serde_json::to_value(change).expect("block change serialization"))
        .take_while(|change| {
            bytes += change.to_string().len() + 1;
            bytes <= 8 * 1024
        })
        .collect();
    Ok(json!({
        "updated_at": crate::time_format::at(out.card.updated_at),
        "doc_rev": doc_rev,
        "anchor_status": crate::report_read_ledger::anchor_line(&out.anchor),
        "anchor": out.anchor,
        "changes_total": total,
        "changes_complete": changes.len() == total,
        "changes": changes,
        "history": out.history,
        "full_index": {"tool":"neige_report_read", "arguments":{"detail":"index", "commit":out.history.after}},
        "warnings": out.warnings,
    }))
}
