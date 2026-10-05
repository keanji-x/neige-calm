//! Which plugins a report references through a live `source`: a view slot, a live `table` or a
//! `chart.series` block. Read by report hydration and by plugin Planner instructions (#2104 K2).

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::Value;

use super::kinds::parse_live_source;
use super::native_view::{LiveSlot, NativeView, RowCell};
use super::{KIND_CHART_SERIES, KIND_TABLE, KIND_VIEW};
use crate::track_report::ReportBlock;

/// The live slots of a stored `view` template, in row and cell order; none when the payload is
/// not a view.
pub fn view_live_slots(payload: &Value) -> Vec<LiveSlot> {
    let Ok(view) = NativeView::deserialize(payload) else {
        return Vec::new();
    };
    view.rows
        .into_iter()
        .flat_map(|row| row.cells)
        .filter_map(|cell| match cell {
            RowCell::Live(slot) => Some(slot),
            RowCell::Inline(_) => None,
        })
        .collect()
}

/// Every plugin id named by a well-formed live `source` in `blocks`. Any other kind, including a
/// retired kind an old report still stores, references nothing.
pub fn referenced_plugin_ids(blocks: &[ReportBlock]) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for block in blocks {
        let sources: Vec<String> = match block.kind.as_str() {
            KIND_VIEW => view_live_slots(&block.payload)
                .into_iter()
                .map(|slot| slot.source)
                .collect(),
            KIND_TABLE | KIND_CHART_SERIES => block
                .payload
                .get("source")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .into_iter()
                .collect(),
            _ => Vec::new(),
        };
        for source in &sources {
            if let Ok((plugin_id, _)) = parse_live_source(source) {
                ids.insert(plugin_id.to_owned());
            }
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn block(kind: &str, payload: Value) -> ReportBlock {
        ReportBlock {
            id: format!("{kind}-block"),
            kind: kind.into(),
            rev: 1,
            payload,
        }
    }

    #[test]
    fn references_come_from_slots_live_tables_and_series_only() {
        let slot = json!({"kind": "live", "id": "s", "expects": "metrics",
                          "source": "neige://plugin/view.plugin/unit"});
        let view = json!({"version": 1, "title": "", "description": "", "snapshot": null,
            "rows": [{"id": "r", "title": "", "layout": "one", "cells": [slot]}]});
        let blocks = [
            block(KIND_VIEW, view),
            block(
                KIND_TABLE,
                json!({"source": "neige://plugin/table.plugin/unit"}),
            ),
            block(
                KIND_CHART_SERIES,
                json!({"source": "neige://plugin/series.plugin/tool", "series": ["US:SPY"]}),
            ),
            block(KIND_TABLE, json!({"source": "neige://plugin/no-kind"})),
            block(
                KIND_TABLE,
                json!({"columns": [], "rows": [], "caption": "neige://plugin/inline/x"}),
            ),
            block(
                "markdown",
                json!({"source": "neige://plugin/markdown.plugin/x"}),
            ),
        ];
        let ids: Vec<String> = referenced_plugin_ids(&blocks).into_iter().collect();
        assert_eq!(ids, ["series.plugin", "table.plugin", "view.plugin"]);
    }
}
