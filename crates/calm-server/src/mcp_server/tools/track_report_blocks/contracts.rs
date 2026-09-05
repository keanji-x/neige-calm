use super::{
    TOOL_REPORT_BLOCKS_DELETE, TOOL_REPORT_BLOCKS_KINDS, TOOL_REPORT_BLOCKS_MOVE,
    TOOL_REPORT_BLOCKS_UPSERT, TOOL_REPORT_WRITE_MARKDOWN,
};
use crate::mcp_server::registry::{
    ToolDescriptor, read_only_annotations, role_gated_write_annotations,
};
use crate::model::CardRole;
use calm_types::report_blocks;
use serde_json::{Value, json};

pub(super) fn kinds_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_BLOCKS_KINDS.into(),
        description: "Planner-only: list the block kinds a track report can \
             contain. Returns `{ kinds: [{ kind, schema, usage }] }` \
             where `schema` is the JSON Schema of that kind's payload. \
             Kinds: `prose` (markdown), `chart.candles` (inline candle \
             chart), `table` (comparison table), `app` (embedded \
             same-origin mini-app), `task` (validated task declaration; \
             projection lands in a later slice). Creating or moving blocks \
             requires `if_doc_rev`; read `docRev` from `calm.report.read`."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
        annotations: Some(read_only_annotations()),
        // #1189 — the block channel is the assistant's report write
        // surface (discovery only; the handler's `require_role` relaxes in S2).
        visible_to_roles: &[CardRole::Planner, CardRole::Assistant],
    }
}

/// The static kind table — the single self-description source a planner
/// agent discovers the block vocabulary from. Payload validation for
/// the data kinds lives in `calm_types::report_blocks::kinds` (the
/// schemas here must stay in lock-step with it).
pub(super) fn kinds_table() -> Value {
    json!({
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
                "usage": "Free-form Markdown prose. Create or replace via \
                     `calm.report.blocks.upsert` passing the content in the \
                     top-level `markdown` argument. Blocks are split at \
                     H1/H2 headings, so a prose block conventionally starts \
                     with one. Prose markdown may NOT embed ```neige-block \
                     fences — data goes in its own block. Creating requires \
                     `if_doc_rev`; read `docRev` from `calm.report.read`."
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
                "usage": "Candlestick chart with inline data. Minimal example \
                     — calm.report.blocks.upsert { \"kind\": \"chart.candles\", \
                     \"payload\": { \"symbol\": \"0700.HK\", \"candles\": \
                     [[1719800000000, 371.2, 380.0, 370.0, 378.4, 12000000], \
                     [1719886400000, 378.4, 382.0, 375.0, 379.8, 9800000]] } }. \
                     The kernel has no market-data source: include every \
                     candle you want rendered. Limits: at most 5000 candles \
                     and 256KB of JSON per block — downsample older history \
                     if you exceed either."
            },
            {
                "kind": "table",
                "schema": {
                    "type": "object",
                    "required": ["columns", "rows"],
                    "additionalProperties": false,
                    "properties": {
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
                     calm.report.blocks.upsert { \"kind\": \"table\", \"payload\": \
                     { \"columns\": [{ \"key\": \"name\", \"label\": \"公司\" }, \
                     { \"key\": \"pe\", \"label\": \"PE\", \"align\": \"right\" }], \
                     \"rows\": [{ \"name\": \"腾讯\", \"pe\": 18.2 }] } }. Row \
                     keys must be declared column keys — { \"columns\": \
                     [{\"key\": \"pe\", …}], \"rows\": [{ \"PE\": 1 }] } is \
                     rejected. Limits: 32 columns, 500 rows, 2048 chars per \
                     string, 256KB of JSON per block."
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
                     example — calm.report.blocks.upsert { \"kind\": \"app\", \
                     \"payload\": { \"src\": \"/apps/screener\", \"title\": \
                     \"选股器\", \"height\": 600 } }. `src` must be a \
                     same-origin absolute path (`/…`); full URLs and \
                     backslashes are rejected."
            },
            // Keep this schema in sync with `report_blocks::validate_payload`'s
            // task validation. Any constraint changed here must be changed there,
            // and vice versa.
            {
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
                                { "required": ["spawn"] }
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
                            "type": "object", "additionalProperties": false, "required": ["steps"],
                            "properties": {
                                "cwd": { "type": "string", "maxLength": report_blocks::MAX_STRING_CHARS, "pattern": "^[^\\S\\x00-\\x1F\\x7F]*/[^\\x00-\\x1F\\x7F]*$" },
                                "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 7200 },
                                "steps": { "type": "array", "minItems": 1, "items": {
                                    "type": "object", "additionalProperties": false, "required": ["name", "cmd"],
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
                        "tombstone": { "type": ["object", "null"], "additionalProperties": false, "properties": { "reason": { "type": ["string", "null"], "maxLength": report_blocks::MAX_STRING_CHARS } } },
                        "tombstoned_by": { "type": "string", "enum": ["spec", "user"] }
                    },
                    "description": "Non-tombstones use the required fields above. Tombstones are the closed shape {key,tombstone,declared_by,tombstoned_by}."
                },
                "usage": "Task declaration block. Set `ready: true` to opt into projection once task projection ships in slice 3b; this slice validates and stores declarations but does not project or schedule them. Use `goal` for codex/claude and `command` for terminal; the two fields are mutually exclusive. The terminal runner passes `command` verbatim to `/bin/sh -c`. Every string nested anywhere in `context` is limited to 2048 characters."
            }
        ]
    })
}

pub(super) fn upsert_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_BLOCKS_UPSERT.into(),
        description: "Planner-only: create or replace ONE report block. \
             Without `id`: creates a new block (appended at the end, \
             or inserted at `position`) and REQUIRES `if_doc_rev`; read \
             `docRev` from `calm.report.read`. With `id`: replaces that \
             block's content and REQUIRES `if_rev` (the rev you read); \
             a mismatch returns error -32001 (rev conflict) and writes \
             nothing — re-read and retry. Kinds (see \
             calm.report.blocks.kinds for payload schemas): `prose` \
             takes its content in `markdown`; `chart.candles` / \
             `table` / `app` / `task` take a schema-validated `payload` object \
             (and must NOT pass `markdown`). Returns `{ id, rev, \
             updated_at, docRev }` — keep the returned rev for your next edit \
             of the same block. Get ids/revs from `calm.report.read`'s \
             `blocks` index. The report summary is not touched."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["kind"],
            "properties": {
                "id": { "type": "string", "description": "Existing block id to replace. Omit to create a new block." },
                "kind": { "type": "string", "enum": ["prose", "chart.candles", "table", "app", "task"], "description": "Block kind." },
                "markdown": { "type": "string", "description": "Prose content (kind=prose only)." },
                "payload": { "type": "object", "description": "Kind-specific payload: required for data kinds; for prose, `{ markdown }` is accepted as an alternative to the top-level `markdown`." },
                "if_rev": { "type": "integer", "minimum": 0, "description": "Required when `id` is given: the block rev you last read." },
                "if_doc_rev": { "type": "integer", "minimum": 0, "description": "Required when creating: read docRev from calm.report.read." },
                "position": { "type": "integer", "minimum": 0, "description": "Insertion index for a NEW block (default: append)." }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        // #1189 — the block channel is the assistant's report write
        // surface (discovery only; the handler's `require_role` relaxes in S2).
        visible_to_roles: &[CardRole::Planner, CardRole::Assistant],
    }
}

pub(super) fn move_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_BLOCKS_MOVE.into(),
        description: "Planner-only: move a report block to `to_index` (its \
             final 0-based index in document order). Content and rev \
             are untouched — ordering is not content. `if_doc_rev` is \
             REQUIRED because ordering is document-wide; read `docRev` \
             from `calm.report.read` (mismatch → error -32001). Returns \
             `{ id, rev, updated_at, docRev }`."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["id", "to_index", "if_doc_rev"],
            "properties": {
                "id": { "type": "string" },
                "to_index": { "type": "integer", "minimum": 0 },
                "if_doc_rev": { "type": "integer", "minimum": 0, "description": "Required document revision; read docRev from calm.report.read." }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        // #1189 — the block channel is the assistant's report write
        // surface (discovery only; the handler's `require_role` relaxes in S2).
        visible_to_roles: &[CardRole::Planner, CardRole::Assistant],
    }
}

pub(super) fn delete_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_BLOCKS_DELETE.into(),
        description: "Planner-only: delete a report block. `if_rev` is \
             REQUIRED (destructive op): pass the rev you last read; a \
             mismatch returns error -32001 (rev conflict) and deletes \
             nothing. Returns `{ updated_at, docRev }`."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["id", "if_rev"],
            "properties": {
                "id": { "type": "string" },
                "if_rev": { "type": "integer", "minimum": 0, "description": "The revision of this specific report block; not the document-wide docRev." }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        // #1189 — the block channel is the assistant's report write
        // surface (discovery only; the handler's `require_role` relaxes in S2).
        visible_to_roles: &[CardRole::Planner, CardRole::Assistant],
    }
}

pub(super) fn write_markdown_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_WRITE_MARKDOWN.into(),
        description: "Planner-only: the id-preserving whole-document write \
             — wholesale-replace the report from full-document \
             Markdown. Prefer this over `calm.report.write` for any \
             full rewrite: that tool re-derives block ids \
             best-effort, this one keeps them. The body MAY contain \
             the `<!-- neige:b_xxxx -->` marker lines that \
             `calm.report.read { with_markers: true }` emits — each \
             marker pins the block that follows it to that existing \
             block id (its rev bumps only if the content changed). \
             Marker lines are ALWAYS stripped server-side and never \
             stored; blocks without markers are re-matched \
             best-effort. Non-prose blocks appear in the body as \
             ```neige-block <kind>``` fences: keep a fence verbatim to \
             preserve that block, edit its JSON to update it (rev+1), \
             drop it to delete it — every fence must be well-formed \
             and schema-valid or the whole write is rejected (-32602). \
             Use `calm.report.blocks.*` for targeted \
             edits; use this for large restructurings. Takes no \
             `message`/`lifecycle`. Omitting \
             `summary` keeps the existing one. Returns \
             `{ updated_at, docRev }`."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["body", "if_doc_rev"],
            "properties": {
                "body": { "type": "string", "description": "Full report Markdown, optionally with `<!-- neige:b_xxxx -->` marker lines." },
                "if_doc_rev": { "type": "integer", "minimum": 0, "description": "The document-wide docRev returned by calm.report.read; not a block rev." },
                "summary": { "type": "string" }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        // #1189 — the block channel is the assistant's report write
        // surface (discovery only; the handler's `require_role` relaxes in S2).
        visible_to_roles: &[CardRole::Planner, CardRole::Assistant],
    }
}

#[cfg(test)]
mod task_kind_contract_tests {
    use super::*;
    use crate::mcp_server::tools::plan::{
        GateInput, GateStepInput, PlanTaskInput, plan_template_task_block_payload,
    };
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
    fn task_is_advertised_by_both_block_tool_contracts() {
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

        let kinds = kinds_descriptor();
        assert!(kinds.description.contains("`task`"));
        let upsert = upsert_descriptor();
        assert!(upsert.description.contains("/ `task`"));
        assert!(
            upsert.input_schema["properties"]["kind"]["enum"]
                .as_array()
                .unwrap()
                .iter()
                .any(|kind| kind == "task")
        );
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

    #[test]
    fn minimal_template_gate_wire_matches_published_task_schema_field_by_field() {
        let payload = plan_template_task_block_payload(&PlanTaskInput {
            key: "minimal-gate".into(),
            kind: "codex".into(),
            goal: "exercise the published gate wire shape".into(),
            context: None,
            acceptance_criteria: None,
            cwd: None,
            depends_on: vec![],
            priority: None,
            gate: Some(GateInput {
                cwd: None,
                timeout_secs: None,
                steps: vec![GateStepInput {
                    name: "minimal".into(),
                    cmd: "true".into(),
                }],
            }),
            no_gate_reason: None,
        });
        let table = kinds_table();
        let schema = task_schema(&table);

        assert_required_fields("task", &payload, &schema["oneOf"][0]);
        assert_value_matches_published_schema("task", &payload, schema);
    }
}
