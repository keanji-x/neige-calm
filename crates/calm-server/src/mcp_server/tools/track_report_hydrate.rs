//! Read-only hydration of chart series and typed Track overlay references.
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
use calm_types::report_blocks::kinds::validate_inline_table_overlay;
use calm_types::report_blocks::live_refs::view_live_slots;
use calm_types::report_blocks::native_view::{LiveSlot, validate_unit};
use calm_types::report_blocks::{KIND_CHART_SERIES, KIND_TABLE, KIND_VIEW};
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
    let overlays = if blocks.iter().any(|block| {
        is_overlay_block(block) && modes.get(&block.id).copied() != Some(ResolveMode::None)
    }) {
        ctx.repo
            .overlays_for("track", track_id)
            .await
            .map_err(|_| "overlay storage unavailable")
    } else {
        Ok(Vec::new())
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
        } else if is_overlay_block(block) {
            entry["resolved"] = match &overlays {
                Ok(overlays) => hydrate_overlay(track_id, block, mode, overlays),
                Err(reason) => json!({ "status": "unavailable", "reason": reason }),
            };
        }
        index.push(entry);
    }
    index
}

fn is_overlay_block(block: &ReportBlock) -> bool {
    match block.kind.as_str() {
        KIND_TABLE => block.payload.get("source").is_some_and(Value::is_string),
        KIND_VIEW => !view_live_slots(&block.payload).is_empty(),
        // Any other kind, including a retired kind an old report still stores, is listed without
        // `resolved`; the read never fails on it.
        _ => false,
    }
}

/// Exact Track/plugin/kind lookup; never resolve by plugin name alone.
fn find_overlay<'a>(
    track_id: &str,
    source: &str,
    overlays: &'a [crate::model::Overlay],
) -> Result<Option<&'a crate::model::Overlay>, &'static str> {
    let (plugin_id, kind) = source
        .strip_prefix(LIVE_SOURCE_PREFIX)
        .and_then(|rest| rest.split_once('/'))
        .filter(|(plugin_id, kind)| {
            !plugin_id.is_empty() && !kind.is_empty() && !kind.contains('/')
        })
        .ok_or("source is not a plugin overlay")?;
    Ok(overlays.iter().find(|overlay| {
        overlay.entity_kind == "track"
            && overlay.entity_id == track_id
            && overlay.plugin_id == plugin_id
            && overlay.kind == kind
    }))
}

fn hydrate_overlay(
    track_id: &str,
    block: &ReportBlock,
    mode: ResolveMode,
    overlays: &[crate::model::Overlay],
) -> Value {
    if block.kind == KIND_VIEW {
        return hydrate_live_slots(track_id, &view_live_slots(&block.payload), mode, overlays);
    }
    let source = block
        .payload
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let overlay = match find_overlay(track_id, source, overlays) {
        Err(reason) => return json!({ "status": "unavailable", "reason": reason }),
        Ok(None) => return json!({ "status": "pending" }),
        Ok(Some(overlay)) => overlay,
    };
    // A live reference, mixed view/table object, or malformed row is not an inline table.
    if validate_inline_table_overlay(&overlay.payload).is_err() {
        return json!({ "status": "unavailable", "reason": "overlay payload is not an inline table",
                       "resolved_at": resolved_at_text(overlay.updated_at) });
    }
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

/// Each slot of a template view resolves and validates on its own; a failing slot degrades only itself.
fn hydrate_live_slots(
    track_id: &str,
    slots: &[LiveSlot],
    mode: ResolveMode,
    overlays: &[crate::model::Overlay],
) -> Value {
    let cells: Vec<Value> = slots
        .iter()
        .map(|slot| hydrate_slot(slot, find_overlay(track_id, &slot.source, overlays), mode))
        .collect();
    let status = if cells.iter().all(|cell| cell["status"] == "ok") {
        "ok"
    } else {
        "partial"
    };
    json!({ "status": status, "validation": "presentation", "cells": cells })
}

fn hydrate_slot(
    slot: &LiveSlot,
    found: Result<Option<&crate::model::Overlay>, &'static str>,
    mode: ResolveMode,
) -> Value {
    let mut out = json!({ "id": slot.id, "source": slot.source });
    let overlay = match found {
        Err(reason) => {
            out["status"] = json!("unavailable");
            out["reason"] = json!(reason);
            return out;
        }
        Ok(None) => {
            out["status"] = json!("pending");
            return out;
        }
        Ok(Some(overlay)) => overlay,
    };
    out["resolved_at"] = json!(resolved_at_text(overlay.updated_at));
    if let Err(reason) = validate_unit(slot.expects, &overlay.payload) {
        out["status"] = json!("unavailable");
        out["reason"] = json!(reason);
        return out;
    }
    out["status"] = json!("ok");
    // `validate_unit` bounded `observedAt` to a non-negative integer millisecond time.
    if let Some(observed_at) = overlay.payload["snapshot"]["observedAt"].as_f64() {
        out["observed_at"] = json!(resolved_at_text(observed_at as i64));
    }
    if mode == ResolveMode::Full {
        out["data"] = overlay.payload.clone();
    }
    out
}
