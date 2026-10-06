use super::{TOOL_REPORT_COMMIT, TOOL_REPORT_DESCRIBE, TOOL_REPORT_WRITE};
use crate::mcp_server::registry::{
    ToolDescriptor, read_only_annotations, role_gated_write_annotations,
};
use crate::mcp_server::tools::write_args::message_schema;
use crate::model::CardRole;
use crate::track_report::MAX_BATCH_OPS;
use calm_types::report_blocks;
use serde_json::{Value, json};

pub(super) fn kinds_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_DESCRIBE.into(),
        description: include_str!("../../../../prompts/tools/neige_report_describe.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {}
        }),
        annotations: Some(read_only_annotations()),
        roles: &[CardRole::Planner, CardRole::Assistant],
        listed_for: &[CardRole::Planner, CardRole::Assistant],
    }
}

/// The static kind table; the schemas here must stay in lock-step with `calm_types::report_blocks::kinds`.
pub(super) fn kinds_table() -> Value {
    let mut kinds = json!({
        "kinds": [
            {
                "kind": "prose",
                "schema": {
                    "type": "object",
                    "required": ["markdown"],
                    "additionalProperties": false,
                    "properties": {
                        "markdown": { "type": "string", "description": "The block's Markdown source." }
                    }
                },
                "usage": "Free-form Markdown prose. Create or replace via a \
                     `neige_report_commit` `upsert` op passing the content in \
                     the op's `markdown`. Blocks are split at \
                     H1/H2 headings, so a prose block conventionally starts \
                     with one. Prose markdown may NOT embed ```neige-block \
                     fences — data goes in its own block."
            },
            {
                "kind": "chart.candles",
                "schema": {
                    "type": "object",
                    "required": ["symbol", "candles"],
                    "additionalProperties": false,
                    "properties": {
                        "symbol": { "type": "string", "minLength": 1, "maxLength": report_blocks::MAX_STRING_CHARS, "description": "Instrument label, e.g. \"0700.HK\"." },
                        "period": { "type": "string", "enum": ["day", "week", "month"], "description": "Candle period (default day)." },
                        "candles": {
                            "type": "array",
                            "minItems": 2,
                            "maxItems": report_blocks::MAX_CHART_CANDLES,
                            "description": "Inline candle rows, oldest first. You fetch the data yourself and write it in; the reader filters ranges client-side.",
                            "items": {
                                "type": "array",
                                "minItems": 5,
                                "maxItems": 6,
                                "items": { "type": "number" },
                                "description": "[ts_ms, open, high, low, close, volume?]"
                            }
                        },
                        "overlays": {
                            "type": "array",
                            "items": { "type": "string", "enum": ["ma20", "ma60"] },
                            "description": "Moving-average overlays to render."
                        },
                        "caption": { "type": "string", "maxLength": report_blocks::MAX_STRING_CHARS }
                    }
                },
                "usage": "Candlestick chart with inline data — the escape hatch \
                     for data no plugin resolves. For any asset a plugin can \
                     resolve market data for, use `chart.series` instead and \
                     let the kernel fetch the points. Minimal example \
                     — a commit op { \"op\": \"upsert\", \"kind\": \"chart.candles\", \
                     \"payload\": { \"symbol\": \"0700.HK\", \"candles\": \
                     [[1719800000000, 371.2, 380.0, 370.0, 378.4, 12000000], \
                     [1719886400000, 378.4, 382.0, 375.0, 379.8, 9800000]] } }. \
                     Include every candle you want rendered. Limits: at most \
                     5000 candles and 256KB of JSON per block — downsample \
                     older history if you exceed either."
            },
            // Keep this schema in sync with `report_blocks::chart_series::validate_chart_series`.
            {
                "kind": "chart.series",
                "schema": {
                    "type": "object",
                    "required": ["source", "series"],
                    "additionalProperties": false,
                    "properties": {
                        "source": {
                            "type": "string",
                            "maxLength": report_blocks::MAX_STRING_CHARS,
                            "pattern": report_blocks::kinds::LIVE_SOURCE_PATTERN,
                            "description": "`neige://plugin/<plugin_id>/<tool>` — the plugin tool that \
                                resolves the series, e.g. `neige://plugin/dev-neige-market/market.series`. \
                                The plugin need not be installed when the block is written."
                        },
                        "series": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": report_blocks::MAX_CHART_SERIES,
                            "uniqueItems": true,
                            "items": {
                                "type": "string",
                                "maxLength": report_blocks::MAX_STRING_CHARS,
                                "pattern": "^[A-Z]{2,8}:[A-Za-z0-9._-]{1,32}$",
                                "description": "Venue-qualified asset id, e.g. \"US:NVDA\", \"HK:9988\", \
                                    \"CRYPTO:BTC\". The plugin decides what a venue means."
                            },
                            "description": "Assets to draw, 1..8, no literal duplicates."
                        },
                        "field": {
                            "type": "string",
                            "enum": ["close", "open", "high", "low", "volume"],
                            "description": "Value per point (default close). Must be absent when view is candles."
                        },
                        "range": {
                            "type": "string",
                            "enum": ["1M", "3M", "6M", "1Y", "2Y", "5Y"],
                            "description": "Window ending at the cutoff (default 1Y)."
                        },
                        "period": {
                            "type": "string",
                            "enum": ["day", "week", "month"],
                            "description": "Bar period (default day). `range: 1M` cannot combine with `month`."
                        },
                        "view": {
                            "type": "string",
                            "enum": ["line", "normalized", "bar", "candles"],
                            "description": "How to draw (default line). `candles` needs exactly one series."
                        },
                        "as_of": {
                            "type": "string",
                            "pattern": "^\\d{4}-\\d{2}-\\d{2}$",
                            "description": "Cutoff date YYYY-MM-DD; must exist on the calendar. Present = frozen \
                                at that date; absent = live (the window ends at the latest complete day)."
                        },
                        "overlays": {
                            "type": "array",
                            "items": { "type": "string", "enum": ["ma20", "ma60"] },
                            "description": "Moving-average overlays; accepted only with view line or candles."
                        },
                        "caption": { "type": "string", "maxLength": report_blocks::MAX_STRING_CHARS }
                    }
                },
                "usage": "Price chart whose data is NOT inlined: the block names a \
                     plugin tool (`source`) and the assets (`series`), and the \
                     kernel resolves the points itself when the report is read. \
                     Minimal example — a commit op { \"op\": \"upsert\", \"kind\": \
                     \"chart.series\", \"payload\": { \"source\": \
                     \"neige://plugin/dev-neige-market/market.series\", \
                     \"series\": [\"US:NVDA\", \"HK:9988\"], \"range\": \"1Y\" } }. \
                     `as_of` is the cutoff date, not the last bar's date: with it \
                     the chart is frozen at that date; without it the chart is \
                     live and follows the latest complete day. `view: candles` \
                     takes exactly one series and no `field`. Prefer this over \
                     `chart.candles` whenever a plugin can resolve the asset."
            },
            {
                "kind": "table",
                "schema": {
                    "type": "object",
                    "additionalProperties": false,
                    "oneOf": [
                        {
                            "description": "Inline table — the rows live in the block.",
                            "required": ["columns", "rows"],
                            "not": { "required": ["source"] }
                        },
                        {
                            "description": "Live table — the rows come from a plugin-written overlay named by `source`, and the block re-renders whenever that overlay changes. Nothing else may be set: a live table that also carried columns/rows would have two answers to what it shows.",
                            "required": ["source"],
                            "not": { "anyOf": [
                                { "required": ["columns"] },
                                { "required": ["rows"] },
                                { "required": ["highlight"] }
                            ] }
                        }
                    ],
                    "properties": {
                        "source": {
                            "type": "string",
                            "maxLength": report_blocks::MAX_STRING_CHARS,
                            "pattern": report_blocks::kinds::LIVE_SOURCE_PATTERN,
                            "description": "`neige://plugin/<plugin_id>/<overlay_kind>` — the overlay whose payload (itself a `{columns, rows, caption?, highlight?}` document) is rendered here. The plugin need not be installed when the block is written."
                        },
                        "columns": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": report_blocks::MAX_TABLE_COLUMNS,
                            "items": {
                                "type": "object",
                                "required": ["key", "label"],
                                "additionalProperties": false,
                                "properties": {
                                    "key": { "type": "string", "minLength": 1, "maxLength": report_blocks::MAX_STRING_CHARS, "description": "Row-object key; unique per table." },
                                    "label": { "type": "string", "maxLength": report_blocks::MAX_STRING_CHARS, "description": "Rendered column header." },
                                    "align": { "type": "string", "enum": ["left", "right"] }
                                }
                            }
                        },
                        "rows": {
                            "type": "array",
                            "maxItems": report_blocks::MAX_TABLE_ROWS,
                            "items": {
                                "type": "object",
                                "description": "Every key MUST be a declared column `key` (JSON Schema cannot express this — it is enforced server-side). Counter-example: with columns [{\"key\": \"pe\", …}], a row { \"PE\": 18.2 } is rejected with `rows[0].PE: not a declared column key`. Values are string | number | null.",
                                "additionalProperties": { "type": ["string", "number", "null"], "maxLength": report_blocks::MAX_STRING_CHARS }
                            }
                        },
                        "caption": { "type": "string", "maxLength": report_blocks::MAX_STRING_CHARS },
                        "highlight": { "type": "string", "maxLength": report_blocks::MAX_STRING_CHARS, "description": "Row key VALUE to visually highlight." }
                    }
                },
                "usage": "Structured comparison table. Minimal example — \
                     a commit op { \"op\": \"upsert\", \"kind\": \"table\", \"payload\": \
                     { \"columns\": [{ \"key\": \"name\", \"label\": \"公司\" }, \
                     { \"key\": \"pe\", \"label\": \"PE\", \"align\": \"right\" }], \
                     \"rows\": [{ \"name\": \"腾讯\", \"pe\": 18.2 }] } }. Row \
                     keys must be declared column keys — { \"columns\": \
                     [{\"key\": \"pe\", …}], \"rows\": [{ \"PE\": 1 }] } is \
                     rejected. Limits: 32 columns, 500 rows, 2048 chars per \
                     string, 256KB of JSON per block. A LIVE table names its \
                     data instead of carrying it — { \"kind\": \"table\", \
                     \"payload\": { \"source\": \
                     \"neige://plugin/dev-neige-binance/portfolio.holdings\" } } \
                     — and then re-renders on its own as the plugin pushes; \
                     `columns`, `rows` and `highlight` are rejected alongside \
                     `source`."
            },
            {
                "kind": "app",
                "schema": {
                    "type": "object",
                    "required": ["src"],
                    "additionalProperties": false,
                    "properties": {
                        "src": { "type": "string", "maxLength": report_blocks::MAX_STRING_CHARS, "pattern": "^/(?![/\\\\])[^\\\\]*$", "description": "Same-origin absolute path: starts with `/`, not `//`, no backslashes, no scheme — full URLs (https://…) are NOT accepted. Rendered in the sandboxed AppBridge iframe." },
                        "title": { "type": "string", "maxLength": report_blocks::MAX_STRING_CHARS },
                        "height": { "type": "number", "minimum": 120, "maximum": 2000, "description": "Iframe height in px (default chosen by the renderer)." }
                    }
                },
                "usage": "Embed a same-origin mini-app in the report. Minimal \
                     example — a commit op { \"op\": \"upsert\", \"kind\": \"app\", \
                     \"payload\": { \"src\": \"/apps/screener\", \"title\": \
                     \"选股器\", \"height\": 600 } }. `src` must be a \
                     same-origin absolute path (`/…`); full URLs and \
                     backslashes are rejected."
            }
        ]
    });
    // Appended, not inlined: one more element in the literal exceeds `json!`'s recursion limit.
    kinds["kinds"]
        .as_array_mut()
        .expect("kinds array literal")
        .extend([task_kind(), preview_kind(), native_view_kind()]);
    kinds
}

/// Keep this schema in sync with `report_blocks::validate_payload`'s task validation.
fn task_kind() -> Value {
    json!({
        "kind": "task",
        "schema": {
            "type": "object",
            "additionalProperties": false,
            "$defs": {
                "contextValue": {
                    "oneOf": [
                        { "type": "string", "maxLength": report_blocks::MAX_STRING_CHARS },
                        { "type": "array", "items": { "$ref": "#/$defs/contextValue" } },
                        { "type": "object", "additionalProperties": { "$ref": "#/$defs/contextValue" } },
                        { "type": ["number", "boolean", "null"] }
                    ]
                }
            },
            "oneOf": [
                {
                    "description": "Agent task",
                    "required": ["key", "kind", "goal", "ready", "declared_by"],
                    "properties": { "kind": { "enum": ["codex", "claude"] } },
                    "not": { "anyOf": [
                        { "required": ["command"] }, { "required": ["tombstoned_by"] }
                    ] }
                },
                {
                    "description": "Terminal command task",
                    "required": ["key", "kind", "command", "ready", "declared_by"],
                    "properties": { "kind": { "const": "terminal" } },
                    "not": { "anyOf": [
                        { "required": ["goal"] }, { "required": ["tombstoned_by"] }
                    ] }
                },
                {
                    "required": ["key", "tombstone", "declared_by", "tombstoned_by"],
                    "properties": { "tombstone": { "not": { "type": "null" } } },
                    "not": { "anyOf": [
                        { "required": ["kind"] }, { "required": ["goal"] },
                        { "required": ["command"] },
                        { "required": ["acceptance"] }, { "required": ["gate"] },
                        { "required": ["no_gate_reason"] }, { "required": ["depends_on"] },
                        { "required": ["priority"] }, { "required": ["cwd"] },
                        { "required": ["context"] }, { "required": ["refs"] },
                        { "required": ["ready"] }, { "required": ["released_by_user"] },
                        { "required": ["spawn"] }, { "required": ["access"] },
                        { "required": ["head"] }, { "required": ["base"] },
                        { "required": ["start"] }
                    ] }
                }
            ],
            "properties": {
                "key": { "type": "string", "pattern": "^[a-z0-9][a-z0-9._-]{0,63}$" },
                "kind": { "type": "string", "enum": ["codex", "claude", "terminal"] },
                "goal": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": report_blocks::MAX_STRING_CHARS,
                    "pattern": "\\S",
                    "description": "Natural-language objective. Required only for codex/claude tasks; forbidden for terminal tasks."
                },
                "command": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": report_blocks::MAX_STRING_CHARS,
                    "pattern": "\\S",
                    "description": "Exact Shell command passed verbatim as `/bin/sh -c <command>`. Required only for terminal tasks; forbidden for codex/claude tasks."
                },
                "acceptance": { "type": "string", "minLength": 1, "maxLength": report_blocks::MAX_STRING_CHARS, "pattern": "\\S" },
                "gate": {
                    "type": "object",
                    "additionalProperties": false, "required": ["steps"],
                    "properties": {
                        "cwd": { "type": "string", "maxLength": report_blocks::MAX_STRING_CHARS, "pattern": "^[^\\S\\x00-\\x1F\\x7F]*/[^\\x00-\\x1F\\x7F]*$" },
                        "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 7200 },
                        "steps": { "type": "array", "minItems": 1, "items": {
                            "type": "object",
                            "additionalProperties": false, "required": ["name", "cmd"],
                            "properties": {
                                "name": { "type": "string", "minLength": 1, "maxLength": report_blocks::MAX_STRING_CHARS, "pattern": "^(?=.*\\S)[^\\x00-\\x1F\\x7F]*$" },
                                "cmd": { "type": "string", "minLength": 1, "maxLength": report_blocks::MAX_STRING_CHARS, "pattern": "^(?=.*\\S)[^\\x00-\\x1F\\x7F]*$" }
                            }
                        }}
                    }
                },
                "no_gate_reason": { "type": "string", "minLength": 1, "maxLength": report_blocks::MAX_STRING_CHARS, "pattern": "\\S" },
                "depends_on": { "type": "array", "items": { "type": "string", "maxLength": report_blocks::MAX_STRING_CHARS } },
                "priority": {
                    "type": "integer",
                    "minimum": i64::MIN,
                    "maximum": i64::MAX,
                    "default": 0
                },
                "cwd": { "type": "string", "maxLength": report_blocks::MAX_STRING_CHARS, "pattern": "^[^\\S\\x00-\\x1F\\x7F]*/[^\\x00-\\x1F\\x7F]*$" },
                "context": { "$ref": "#/$defs/contextValue", "description": "Arbitrary JSON; every nested string is limited to 2048 characters." },
                "refs": { "type": "array", "items": { "type": "string", "maxLength": report_blocks::MAX_STRING_CHARS, "pattern": "^neige://wave/[^/#]+#b_[0-9a-f]{4}$" } },
                "ready": { "type": "boolean" },
                "declared_by": { "type": "string", "enum": ["spec", "user"] },
                "released_by_user": { "type": "boolean", "default": false },
                "spawn": { "type": "string", "enum": ["in-wave", "sub-wave"], "default": "in-wave" },
                "access": {
                    "type": "string",
                    "enum": ["read_only", "read_write"],
                    "default": "read_write",
                    "description": "`read_only`: a codex/claude task that leaves the checkout unchanged; no gate, runs beside other readers."
                },
                "head": { "type": "string", "pattern": "^[0-9a-f]{40}$", "description": "`read_only` only: the commit the checkout must be at; the launch is refused otherwise." },
                "base": { "type": "string", "pattern": "^[0-9a-f]{40}$", "description": "`read_only` only: the commit a review compares against." },
                "start": {
                    "type": "string",
                    "enum": ["checkout", "upstream"],
                    "default": "checkout",
                    "description": "`upstream` (codex/claude, read_write): start at the upstream the kernel fetches; replay the last done commit."
                },
                "tombstone": { "type": ["object", "null"], "additionalProperties": false, "properties": { "reason": { "type": ["string", "null"], "maxLength": report_blocks::MAX_STRING_CHARS } } },
                "tombstoned_by": { "type": "string", "enum": ["spec", "user"] }
            },
            "description": "Non-tombstones use the required fields above. Tombstones are the closed shape {key,tombstone,declared_by,tombstoned_by}."
        },
        "usage": "Task declaration block. `ready: true` lets the kernel project and schedule it. Use `goal` for codex/claude and `command` for terminal; the two fields are mutually exclusive. The terminal runner passes `command` verbatim to `/bin/sh -c`. Every string nested anywhere in `context` is limited to 2048 characters."
    })
}

/// Keep this schema in sync with `report_blocks::validate_payload`'s preview validation.
fn preview_kind() -> Value {
    json!({
        "kind": "preview",
        "schema": {
            "type": "object",
            "required": ["key"],
            "additionalProperties": false,
            "properties": {
                "key": { "type": "string", "pattern": "^[a-z0-9][a-z0-9_-]{0,63}$", "description": "The `preview_id` you passed to `neige_preview_add`." },
                "title": { "type": "string", "maxLength": report_blocks::MAX_STRING_CHARS },
                "path": { "type": "string", "maxLength": report_blocks::MAX_STRING_CHARS, "pattern": "^/(?![/\\\\])[^\\\\]*$", "description": "Path on the preview (default `/`; `/next/` for a dev-calm FE). Same rules as the `app` block's `src`." },
                "height": { "type": "number", "minimum": 120, "maximum": 2000, "description": "Frame height in px (default chosen by the renderer)." }
            }
        },
        "usage": "Embed a live dev server you added with \
             `neige_preview_add` (its `block_hint` is this block's \
             payload). Minimal example — a commit op { \"op\": \"upsert\", \
             \"kind\": \"preview\", \"payload\": { \"key\": \"fe\", \
             \"title\": \"前端\", \"path\": \"/next/\" } }. The block names \
             the registration by `key`, not a port: it shows an \
             offline or not-registered placeholder until a dev server \
             is registered under that key and answering, and is \
             viewable over LAN http only."
    })
}

pub(super) fn write_markdown_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_WRITE.into(),
        description: include_str!("../../../../prompts/tools/neige_report_write.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["body"],
            "additionalProperties": false,
            "properties": {
                "body": { "type": "string", "description": "Full report Markdown, optionally with `<!-- neige:b_xxxx -->` marker lines." },
                "summary": { "type": "string" },
                "message": optional_message_schema()
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        roles: &[CardRole::Planner, CardRole::Assistant],
        listed_for: &[CardRole::Planner, CardRole::Assistant],
    }
}

/// Read off [`kinds_table`] so the commit op cannot drift from the self-description.
fn block_kind_enum() -> Value {
    let table = kinds_table();
    let kinds = table["kinds"]
        .as_array()
        .expect("kinds_table publishes a kinds array")
        .iter()
        .map(|entry| entry["kind"].clone())
        .collect::<Vec<_>>();
    Value::Array(kinds)
}

fn optional_message_schema() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "description": "Optional audit note: why this write."
    })
}

fn native_view_kind() -> Value {
    let schema = report_blocks::native_view::schema();
    json!({"kind":"view", "schema":schema,
        "usage": include_str!("../../../../prompts/report-kinds/view.md").trim_end()})
}

pub(super) fn commit_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_COMMIT.into(),
        description: include_str!("../../../../prompts/tools/neige_report_commit.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["message"],
            "additionalProperties": false,
            "properties": {
                "message": message_schema(),
                "summary": { "type": "string" },
                "ops": {
                    "type": "array",
                    "maxItems": MAX_BATCH_OPS,
                    "items": {
                        "type": "object",
                        "required": ["op"],
                        "additionalProperties": false,
                        "properties": {
                            "op": { "type": "string", "enum": ["replace", "upsert", "move", "delete"] },
                            "section": { "type": "string" },
                            "id": { "type": "string" },
                            "kind": { "type": "string", "enum": block_kind_enum() },
                            "markdown": { "type": "string" },
                            "payload": { "type": "object", "description": "Data kinds: see neige_report_describe." },
                            "position": { "type": "integer", "minimum": 0 },
                            "to_index": { "type": "integer", "minimum": 0 }
                        }
                    }
                }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        roles: &[CardRole::Planner, CardRole::Assistant],
        listed_for: &[CardRole::Planner, CardRole::Assistant],
    }
}

#[cfg(test)]
mod task_kind_contract_tests {
    use super::*;
    use calm_types::report_blocks::TASK_FIELDS;
    use std::collections::BTreeSet;

    fn task_schema(table: &Value) -> &Value {
        &table["kinds"]
            .as_array()
            .expect("kinds array")
            .iter()
            .find(|kind| kind["kind"] == "task")
            .expect("task kind table entry")["schema"]
    }

    fn assert_required_fields(path: &str, value: &Value, schema: &Value) {
        let object = value
            .as_object()
            .unwrap_or_else(|| panic!("{path}: expected object, got {value}"));
        for field in schema["required"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|field| field.as_str().expect("required field name"))
        {
            assert!(
                object.contains_key(field),
                "{path}: missing required {field}"
            );
        }
    }

    fn assert_value_matches_published_schema(path: &str, value: &Value, schema: &Value) {
        match schema["type"].as_str() {
            Some("object") => {
                let object = value
                    .as_object()
                    .unwrap_or_else(|| panic!("{path}: expected object, got {value}"));
                assert_required_fields(path, value, schema);
                let properties = schema["properties"]
                    .as_object()
                    .expect("published object schema properties");
                for (field, child) in object {
                    let child_schema = properties
                        .get(field)
                        .unwrap_or_else(|| panic!("{path}.{field}: field is not published"));
                    assert_value_matches_published_schema(
                        &format!("{path}.{field}"),
                        child,
                        child_schema,
                    );
                }
            }
            Some("array") => {
                let array = value
                    .as_array()
                    .unwrap_or_else(|| panic!("{path}: expected array, got {value}"));
                for (index, child) in array.iter().enumerate() {
                    assert_value_matches_published_schema(
                        &format!("{path}[{index}]"),
                        child,
                        &schema["items"],
                    );
                }
            }
            Some("string") => assert!(value.is_string(), "{path}: expected string, got {value}"),
            Some("integer") => assert!(value.is_i64(), "{path}: expected integer, got {value}"),
            Some("boolean") => assert!(value.is_boolean(), "{path}: expected boolean, got {value}"),
            other => panic!("{path}: unsupported published schema type {other:?}"),
        }
    }

    #[test]
    fn task_is_advertised_by_the_commit_contract() {
        let table = kinds_table();
        let task = table["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|kind| kind["kind"] == "task")
            .expect("task kind table entry");
        assert_eq!(task["schema"]["additionalProperties"], false);
        assert_eq!(
            task["schema"]["properties"]["declared_by"]["enum"],
            json!(["spec", "user"])
        );
        let properties = &task["schema"]["properties"];
        assert_eq!(properties["acceptance"]["minLength"], 1);
        for field in ["goal", "command", "acceptance", "no_gate_reason"] {
            assert_eq!(properties[field]["pattern"], "\\S");
        }
        let goal_description = properties["goal"]["description"]
            .as_str()
            .expect("task goal description");
        assert!(
            goal_description.contains("Natural-language objective")
                && goal_description.contains("forbidden for terminal"),
            "task goal description must stay agent-only: {goal_description}"
        );
        let command_description = properties["command"]["description"]
            .as_str()
            .expect("task command description");
        assert!(
            command_description.contains("passed verbatim")
                && command_description.contains("Required only for terminal")
                && command_description.contains("forbidden for codex/claude"),
            "task command description must stay terminal-only: {command_description}"
        );
        assert_eq!(properties["priority"]["minimum"], i64::MIN);
        assert_eq!(properties["priority"]["maximum"], i64::MAX);
        for field in ["cwd", "gate"] {
            let cwd = if field == "gate" {
                &properties[field]["properties"]["cwd"]
            } else {
                &properties[field]
            };
            assert!(cwd["pattern"].as_str().unwrap().contains("\\x00-\\x1F"));
            assert!(!cwd["pattern"].as_str().unwrap().starts_with("^/"));
        }
        for field in ["name", "cmd"] {
            assert!(
                properties["gate"]["properties"]["steps"]["items"]["properties"][field]["pattern"]
                    .as_str()
                    .unwrap()
                    .contains("\\x00-\\x1F")
            );
        }
        let usage = task["usage"].as_str().unwrap();
        assert!(usage.contains("ready: true"));
        assert!(
            usage.contains("terminal") && usage.contains("mutually exclusive"),
            "task usage must keep discriminated instruction fields visible: {usage}"
        );

        let commit = commit_descriptor();
        let commit_kinds =
            &commit.input_schema["properties"]["ops"]["items"]["properties"]["kind"]["enum"];
        assert!(
            commit_kinds
                .as_array()
                .unwrap()
                .iter()
                .any(|kind| kind == "task")
        );
        let table_kinds: Vec<Value> = table["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["kind"].clone())
            .collect();
        assert_eq!(commit_kinds, &Value::Array(table_kinds));
    }

    #[test]
    fn task_schema_properties_equal_validator_field_vocabulary() {
        let table = kinds_table();
        let published: BTreeSet<&str> = task_schema(&table)["properties"]
            .as_object()
            .expect("task schema properties")
            .keys()
            .map(String::as_str)
            .collect();
        let validator: BTreeSet<&str> = TASK_FIELDS.iter().copied().collect();
        assert_eq!(published, validator);
    }

    /// The one gate-bearing task payload checked against the published schema (no builtin template file carries a `gate`).
    #[test]
    fn minimal_gate_task_payload_matches_published_task_schema_field_by_field() {
        let payload = json!({
            "key": "minimal-gate",
            "kind": "codex",
            "goal": "exercise the published gate wire shape",
            "depends_on": [],
            "gate": { "steps": [{ "name": "minimal", "cmd": "true" }] },
            "ready": true,
            "declared_by": "spec",
        });
        let table = kinds_table();
        let schema = task_schema(&table);

        assert_required_fields("task", &payload, &schema["oneOf"][0]);
        assert_value_matches_published_schema("task", &payload, schema);
    }
}
