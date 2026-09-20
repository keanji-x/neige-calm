//! The `resolved` projection `calm.report.read` attaches to its block index (`chart.series` rows, live `table` overlays).
//! Everything here is a database read plus, for a `chart.series` block without a fresh row, one in-memory `enqueue`; nothing calls a plugin, nothing writes.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::AppContext;
use crate::mcp_server::tool_visibility::{TrackPluginScope, plugin_scope_for_track};
use crate::report_series::hydrate::hydrate_chart_series;
use crate::report_series::{Detail, resolved_at_text};
use calm_types::report_blocks::kinds::LIVE_SOURCE_PREFIX;
use calm_types::report_blocks::{KIND_CHART_SERIES, KIND_TABLE};
use calm_types::track_report::ReportBlock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolveMode {
    Summary,
    Full,
    None,
}

/// Absent / null → every block gets the default summary.
pub(crate) fn parse_resolve_arg(
    args: &Value,
    tool: &str,
) -> Result<HashMap<String, ResolveMode>, RpcError> {
    let mut out = HashMap::new();
    match args.get("resolve") {
        None | Some(Value::Null) => {}
        Some(Value::Object(map)) => {
            for (block_id, mode) in map {
                let mode = match mode.as_str() {
                    Some("full") => ResolveMode::Full,
                    Some("none") => ResolveMode::None,
                    _ => {
                        return Err(RpcError::invalid_params(format!(
                            "{tool}: `resolve.{block_id}` must be \"full\" or \"none\""
                        )));
                    }
                };
                out.insert(block_id.clone(), mode);
            }
        }
        Some(_) => {
            return Err(RpcError::invalid_params(format!(
                "{tool}: `resolve` must be an object of block id to \"full\" | \"none\""
            )));
        }
    }
    Ok(out)
}

/// The block index with `resolved` attached where it applies.
pub(crate) async fn hydrated_block_index(
    ctx: &Arc<AppContext>,
    track_id: &str,
    blocks: &[ReportBlock],
    modes: &HashMap<String, ResolveMode>,
) -> Vec<Value> {
    let needs_scope = blocks.iter().any(|block| {
        block.kind == KIND_CHART_SERIES && modes.get(&block.id).copied() != Some(ResolveMode::None)
    });
    // One scope resolution per read, and only when a series block may need enqueueing.
    let scope = if needs_scope {
        Some(plugin_scope_for_track(ctx, Some(track_id)).await)
    } else {
        None
    };
    let overlays = if blocks.iter().any(is_live_table) {
        ctx.repo
            .overlays_for("track", track_id)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut index = Vec::with_capacity(blocks.len());
    for block in blocks {
        let mut entry = json!({ "id": block.id, "kind": block.kind, "rev": block.rev });
        let mode = modes
            .get(&block.id)
            .copied()
            .unwrap_or(ResolveMode::Summary);
        if mode == ResolveMode::None {
            index.push(entry);
            continue;
        }
        if block.kind == KIND_CHART_SERIES {
            let scope = scope.as_ref().unwrap_or(&TrackPluginScope::All);
            let detail = match mode {
                ResolveMode::Full => Detail::Full,
                ResolveMode::Summary | ResolveMode::None => Detail::Summary,
            };
            entry["resolved"] =
                hydrate_chart_series(ctx, track_id, block, detail, Some(scope)).await;
        } else if is_live_table(block) {
            entry["resolved"] = hydrate_live_table(track_id, block, mode, &overlays);
        }
        index.push(entry);
    }
    index
}

fn is_live_table(block: &ReportBlock) -> bool {
    block.kind == KIND_TABLE && block.payload.get("source").is_some_and(Value::is_string)
}

/// Same rule the frontend applies (`liveTableOverlayPayload`).
fn hydrate_live_table(
    track_id: &str,
    block: &ReportBlock,
    mode: ResolveMode,
    overlays: &[crate::model::Overlay],
) -> Value {
    let source = block
        .payload
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let target = source
        .strip_prefix(LIVE_SOURCE_PREFIX)
        .and_then(|rest| rest.split_once('/'))
        .filter(|(plugin_id, kind)| {
            !plugin_id.is_empty() && !kind.is_empty() && !kind.contains('/')
        });
    let Some((plugin_id, kind)) = target else {
        return json!({ "status": "unavailable", "reason": "source is not a plugin overlay" });
    };
    let Some(overlay) = overlays.iter().find(|overlay| {
        overlay.entity_kind == "track"
            && overlay.entity_id == track_id
            && overlay.plugin_id == plugin_id
            && overlay.kind == kind
    }) else {
        return json!({ "status": "pending" });
    };
    let columns = overlay.payload.get("columns").and_then(Value::as_array);
    let rows = overlay.payload.get("rows").and_then(Value::as_array);
    let (Some(columns), Some(rows)) = (columns, rows) else {
        return json!({
            "status": "unavailable",
            "reason": "overlay payload is not a table",
            "resolved_at": resolved_at_text(overlay.updated_at),
        });
    };
    let mut out = json!({
        "status": "ok",
        "resolved_at": resolved_at_text(overlay.updated_at),
        "columns": columns.len(),
        "rows": rows.len(),
    });
    if let Some(caption) = overlay.payload.get("caption").and_then(Value::as_str) {
        out["caption"] = Value::String(caption.to_string());
    }
    if mode == ResolveMode::Full {
        out["table"] = overlay.payload.clone();
    }
    out
}
