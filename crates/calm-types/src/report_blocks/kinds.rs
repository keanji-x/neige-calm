//! #960 PR3 — the non-prose block-kind vocabulary + payload validation.
//!
//! Three data kinds ship in this slice. Payloads are validated
//! strictly at every write end (`blocks.upsert`, `write_markdown`,
//! and the prose `Replace` shim when a body introduces new fences):
//! unknown fields are rejected, and every violation is reported with
//! a field-level path so the agent can self-correct.
//!
//! | kind | payload |
//! |---|---|
//! | `chart.candles` | `{ symbol, period?, candles: [[ts_ms,o,h,l,c,v?];2..], overlays?, caption? }` |
//! | `table` | `{ columns: [{key,label,align?};1..], rows: [{<key>: string\|number\|null}], caption?, highlight? }` |
//! | `app` | `{ src: same-origin path, title?, height? (120..2000 px) }` |
//!
//! Candle data is inlined by design: the kernel has no market-data
//! source — the agent fetches its own data and writes it in; range
//! switching is client-side filtering.

use serde_json::{Map, Value};

use super::tasks::{GateInput, key_is_valid, validate_gate_shape};
use crate::report_links::parse_destination;

pub const KIND_PROSE: &str = "prose";
pub const KIND_CHART_CANDLES: &str = "chart.candles";
pub const KIND_TABLE: &str = "table";
pub const KIND_APP: &str = "app";
pub const KIND_TASK: &str = "task";

// -- payload size caps (#960 PR3 review round 1) ----------------------
//
// Hard limits enforced by [`validate_payload`] and advertised in the
// `blocks.kinds` JSON Schemas (maxItems / maxLength) so an agent can
// self-limit before the round-trip. Every violation names its limit.

/// Maximum candle rows in a `chart.candles` payload.
pub const MAX_CHART_CANDLES: usize = 5000;
/// Maximum column definitions in a `table` payload.
pub const MAX_TABLE_COLUMNS: usize = 32;
/// Maximum rows in a `table` payload.
pub const MAX_TABLE_ROWS: usize = 500;
/// Maximum length (chars) of any string field in a data payload.
pub const MAX_STRING_CHARS: usize = 2048;
/// Maximum size (bytes) of a payload's canonical JSON rendering —
/// i.e. of the fence interior that lands in the flat `body`.
pub const MAX_CANONICAL_BYTES: usize = 256 * 1024;

/// The non-prose kinds a report may contain, in `blocks.kinds` order.
pub const DATA_KINDS: [&str; 4] = [KIND_CHART_CANDLES, KIND_TABLE, KIND_APP, KIND_TASK];

pub fn is_data_kind(kind: &str) -> bool {
    DATA_KINDS.contains(&kind)
}

/// Markdown-bearing payload fields the kernel declares safe to scan for
/// report links. Structured blocks are opt-in: scanning their canonical
/// JSON fence would corrupt Markdown syntax through JSON escaping.
pub fn scannable_text_fields<'a>(kind: &str, payload: &'a Value) -> Vec<&'a str> {
    let Some(payload) = payload.as_object() else {
        return Vec::new();
    };
    match kind {
        KIND_PROSE => payload
            .get("markdown")
            .and_then(Value::as_str)
            .into_iter()
            .collect(),
        KIND_TASK => ["goal", "acceptance"]
            .into_iter()
            .filter_map(|field| payload.get(field).and_then(Value::as_str))
            .collect(),
        _ => Vec::new(),
    }
}

/// Validate a non-prose payload against its kind's schema. `Err` is a
/// `"; "`-joined list of field-level violations (paths like
/// `candles[3]`), suitable for a `-32602` message verbatim. Unknown
/// kinds are themselves an error.
pub fn validate_payload(kind: &str, payload: &Value) -> Result<(), String> {
    let Some(map) = payload.as_object() else {
        return Err(format!(
            "payload must be a JSON object, got {}",
            type_name(payload)
        ));
    };
    let mut errors = Vec::new();
    match kind {
        KIND_CHART_CANDLES => validate_chart(map, &mut errors),
        KIND_TABLE => validate_table(map, &mut errors),
        KIND_APP => validate_app(map, &mut errors),
        KIND_TASK => validate_task(map, &mut errors),
        other => errors.push(format!(
            "unknown block kind `{other}` — known data kinds: {}",
            DATA_KINDS.join(", ")
        )),
    }
    if errors.is_empty() {
        // Total-size cap, measured once on the canonical rendering
        // (the exact bytes the fence contributes to `body`). Only
        // checked when the shape is otherwise valid — field errors
        // are more actionable than a size number.
        let canonical_len = super::fence::canonical_json(payload).len();
        if canonical_len > MAX_CANONICAL_BYTES {
            errors.push(format!(
                "payload too large: canonical JSON is {canonical_len} bytes, limit is \
                 {MAX_CANONICAL_BYTES} bytes ({}KB) per block",
                MAX_CANONICAL_BYTES / 1024
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

pub const TASK_FIELDS: &[&str] = &[
    "key",
    "kind",
    "goal",
    "acceptance",
    "gate",
    "no_gate_reason",
    "depends_on",
    "priority",
    "cwd",
    "context",
    "refs",
    "ready",
    "declared_by",
    "released_by_user",
    "spawn",
    "tombstone",
    "tombstoned_by",
];

fn validate_task(map: &Map<String, Value>, errors: &mut Vec<String>) {
    reject_unknown(map, TASK_FIELDS, errors);

    let key = match map.get("key") {
        Some(Value::String(key)) if key_is_valid(key) => {
            check_string_cap("key", key, errors);
            Some(key.as_str())
        }
        Some(Value::String(_)) => {
            errors.push("key: must match ^[a-z0-9][a-z0-9._-]{0,63}$".into());
            None
        }
        _ => {
            errors.push("key: required string matching ^[a-z0-9][a-z0-9._-]{0,63}$".into());
            None
        }
    };

    let tombstone = map.get("tombstone").is_some_and(|value| !value.is_null());
    if tombstone {
        for field in TASK_FIELDS
            .iter()
            .filter(|field| !["key", "tombstone", "declared_by", "tombstoned_by"].contains(field))
        {
            if map.contains_key(*field) {
                errors.push(format!("{field}: must be absent from a tombstone task"));
            }
        }
        validate_declared_by(map, errors);
        required_enum(map, "tombstoned_by", &["spec", "user"], errors);
        match &map["tombstone"] {
            Value::Object(value) => {
                reject_unknown(value, &["reason"], errors);
                if let Some(reason) = value.get("reason") {
                    match reason {
                        Value::Null => {}
                        Value::String(reason) => {
                            check_string_cap("tombstone.reason", reason, errors)
                        }
                        _ => errors.push("tombstone.reason: must be a string or null".into()),
                    }
                }
            }
            _ => errors.push("tombstone: must be an object".into()),
        }
        return;
    }

    if map.contains_key("tombstoned_by") {
        errors.push("tombstoned_by: must be absent from a non-tombstone task".into());
    }
    match map.get("kind").and_then(Value::as_str) {
        Some("codex" | "claude" | "terminal") => {}
        _ => errors
            .push("kind: required; must be one of \"codex\" | \"claude\" | \"terminal\"".into()),
    }
    required_non_empty_string(map, "goal", errors);
    optional_non_empty_string(map, "acceptance", errors);
    optional_non_empty_string(map, "no_gate_reason", errors);
    optional_abs_path(map, "cwd", errors);
    if let Some(gate) = map.get("gate") {
        match serde_json::from_value::<GateInput>(gate.clone()) {
            Ok(gate) => {
                if let Err(error) = validate_gate_shape(key.unwrap_or("<invalid>"), &gate) {
                    let detail = error
                        .strip_prefix(&format!("task {}: ", key.unwrap_or("<invalid>")))
                        .unwrap_or(error.as_str());
                    errors.push(detail.to_string());
                }
                check_gate_strings(&gate, errors);
            }
            Err(error) => errors.push(format!("gate: invalid shape: {error}")),
        }
    }
    optional_string_array(map, "depends_on", false, errors);
    if let Some(priority) = map.get("priority")
        && priority.as_i64().is_none()
    {
        errors.push(format!(
            "priority: must be an integer between {} and {}",
            i64::MIN,
            i64::MAX
        ));
    }
    if let Some(context) = map.get("context") {
        check_nested_string_caps("context", context, errors);
    }
    match map.get("refs") {
        None => {}
        Some(Value::Array(refs)) => {
            for (index, reference) in refs.iter().enumerate() {
                match reference.as_str() {
                    Some(reference) => {
                        check_string_cap(&format!("refs[{index}]"), reference, errors);
                        if !matches!(parse_destination(reference), Some((_, Some(_)))) {
                            errors.push(format!("refs[{index}]: must be a neige://wave/<wave>#b_xxxx block reference"));
                        }
                    }
                    None => errors.push(format!("refs[{index}]: must be a string")),
                }
            }
        }
        Some(_) => errors.push("refs: must be an array".into()),
    }
    if !matches!(map.get("ready"), Some(Value::Bool(_))) {
        errors.push("ready: required boolean".into());
    }
    validate_declared_by(map, errors);
    if let Some(value) = map.get("released_by_user")
        && !value.is_boolean()
    {
        errors.push("released_by_user: must be a boolean".into());
    }
    if let Some(value) = map.get("spawn")
        && !value.is_null()
        && !matches!(value.as_str(), Some("in-wave" | "sub-wave"))
    {
        errors.push("spawn: must be one of \"in-wave\" | \"sub-wave\"".into());
    }
    if matches!(map.get("spawn").and_then(Value::as_str), Some("sub-wave"))
        && !matches!(map.get("kind").and_then(Value::as_str), Some("codex"))
    {
        errors.push("spawn: \"sub-wave\" requires kind \"codex\"".into());
    }
}

fn validate_declared_by(map: &Map<String, Value>, errors: &mut Vec<String>) {
    if !matches!(
        map.get("declared_by").and_then(Value::as_str),
        Some("spec" | "user")
    ) {
        errors.push("declared_by: required; must be one of \"spec\" | \"user\"".into());
    }
}

fn required_enum(
    map: &Map<String, Value>,
    field: &str,
    allowed: &[&str],
    errors: &mut Vec<String>,
) {
    if !map
        .get(field)
        .and_then(Value::as_str)
        .is_some_and(|value| allowed.contains(&value))
    {
        errors.push(format!(
            "{field}: required; must be one of {}",
            allowed
                .iter()
                .map(|v| format!("\"{v}\""))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
}

fn required_non_empty_string(map: &Map<String, Value>, field: &str, errors: &mut Vec<String>) {
    match map.get(field) {
        Some(Value::String(value)) if !value.trim().is_empty() => {
            check_string_cap(field, value, errors)
        }
        _ => errors.push(format!("{field}: required non-empty string")),
    }
}

fn optional_non_empty_string(map: &Map<String, Value>, field: &str, errors: &mut Vec<String>) {
    if map.contains_key(field) {
        required_non_empty_string(map, field, errors);
    }
}

fn optional_abs_path(map: &Map<String, Value>, field: &str, errors: &mut Vec<String>) {
    if let Some(value) = map.get(field) {
        match value.as_str() {
            Some(path)
                if path.trim().starts_with('/') && !path.chars().any(|c| c.is_ascii_control()) =>
            {
                check_string_cap(field, path, errors)
            }
            _ => errors.push(format!(
                "{field}: must be an absolute path without ASCII control characters"
            )),
        }
    }
}

fn check_nested_string_caps(path: &str, value: &Value, errors: &mut Vec<String>) {
    match value {
        Value::String(value) => check_string_cap(path, value, errors),
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                check_nested_string_caps(&format!("{path}[{index}]"), value, errors);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                check_string_cap(&format!("{path}.{key}"), key, errors);
                check_nested_string_caps(&format!("{path}.{key}"), value, errors);
            }
        }
        _ => {}
    }
}

fn optional_string_array(
    map: &Map<String, Value>,
    field: &str,
    non_empty: bool,
    errors: &mut Vec<String>,
) {
    if let Some(value) = map.get(field) {
        match value.as_array() {
            Some(values) if !non_empty || !values.is_empty() => {
                for (index, value) in values.iter().enumerate() {
                    match value.as_str() {
                        Some(value) => {
                            check_string_cap(&format!("{field}[{index}]"), value, errors)
                        }
                        None => errors.push(format!("{field}[{index}]: must be a string")),
                    }
                }
            }
            Some(_) => errors.push(format!("{field}: must be non-empty")),
            None => errors.push(format!("{field}: must be an array")),
        }
    }
}

fn check_gate_strings(gate: &GateInput, errors: &mut Vec<String>) {
    if let Some(cwd) = &gate.cwd {
        check_string_cap("gate.cwd", cwd, errors);
    }
    for (index, step) in gate.steps.iter().enumerate() {
        check_string_cap(&format!("gate.steps[{index}].name"), &step.name, errors);
        check_string_cap(&format!("gate.steps[{index}].cmd"), &step.cmd, errors);
    }
}

fn validate_chart(map: &Map<String, Value>, errors: &mut Vec<String>) {
    reject_unknown(
        map,
        &["symbol", "period", "candles", "overlays", "caption"],
        errors,
    );
    match map.get("symbol") {
        Some(Value::String(s)) if !s.is_empty() => check_string_cap("symbol", s, errors),
        Some(_) | None => errors.push("symbol: required non-empty string".into()),
    }
    if let Some(period) = map.get("period") {
        match period.as_str() {
            Some("day" | "week" | "month") => {}
            _ => errors.push("period: must be one of \"day\" | \"week\" | \"month\"".into()),
        }
    }
    match map.get("candles") {
        Some(Value::Array(candles)) => {
            if candles.len() < 2 {
                errors.push(format!(
                    "candles: at least 2 candles required, got {}",
                    candles.len()
                ));
            }
            if candles.len() > MAX_CHART_CANDLES {
                errors.push(format!(
                    "candles: too many rows ({}), limit is {MAX_CHART_CANDLES}",
                    candles.len()
                ));
            }
            for (index, candle) in candles.iter().enumerate() {
                let ok = candle.as_array().is_some_and(|row| {
                    (row.len() == 5 || row.len() == 6) && row.iter().all(Value::is_number)
                });
                if !ok {
                    errors.push(format!(
                        "candles[{index}]: expected [ts_ms, open, high, low, close, volume?] (5 or 6 numbers)"
                    ));
                }
            }
        }
        Some(_) | None => errors.push("candles: required array of candle rows".into()),
    }
    if let Some(overlays) = map.get("overlays") {
        match overlays.as_array() {
            Some(items) => {
                for (index, item) in items.iter().enumerate() {
                    if !matches!(item.as_str(), Some("ma20" | "ma60")) {
                        errors.push(format!("overlays[{index}]: must be \"ma20\" | \"ma60\""));
                    }
                }
            }
            None => errors.push("overlays: must be an array".into()),
        }
    }
    optional_string(map, "caption", errors);
}

fn validate_table(map: &Map<String, Value>, errors: &mut Vec<String>) {
    reject_unknown(map, &["columns", "rows", "caption", "highlight"], errors);
    let mut column_keys: Vec<&str> = Vec::new();
    match map.get("columns") {
        Some(Value::Array(columns)) if !columns.is_empty() => {
            if columns.len() > MAX_TABLE_COLUMNS {
                errors.push(format!(
                    "columns: too many columns ({}), limit is {MAX_TABLE_COLUMNS}",
                    columns.len()
                ));
            }
            for (index, column) in columns.iter().enumerate() {
                let Some(column) = column.as_object() else {
                    errors.push(format!(
                        "columns[{index}]: must be an object {{ key, label, align? }}"
                    ));
                    continue;
                };
                for unknown in column
                    .keys()
                    .filter(|k| !["key", "label", "align"].contains(&k.as_str()))
                {
                    errors.push(format!("columns[{index}].{unknown}: unknown field"));
                }
                match column.get("key") {
                    Some(Value::String(key)) if !key.is_empty() => {
                        if column_keys.contains(&key.as_str()) {
                            errors.push(format!("columns[{index}].key: duplicate key `{key}`"));
                        }
                        check_string_cap(&format!("columns[{index}].key"), key, errors);
                        column_keys.push(key);
                    }
                    _ => errors.push(format!("columns[{index}].key: required non-empty string")),
                }
                match column.get("label") {
                    Some(Value::String(label)) => {
                        check_string_cap(&format!("columns[{index}].label"), label, errors);
                    }
                    _ => errors.push(format!("columns[{index}].label: required string")),
                }
                if let Some(align) = column.get("align")
                    && !matches!(align.as_str(), Some("left" | "right"))
                {
                    errors.push(format!(
                        "columns[{index}].align: must be \"left\" | \"right\""
                    ));
                }
            }
        }
        Some(_) | None => {
            errors.push("columns: required non-empty array of {{ key, label, align? }}".into())
        }
    }
    match map.get("rows") {
        Some(Value::Array(rows)) => {
            if rows.len() > MAX_TABLE_ROWS {
                errors.push(format!(
                    "rows: too many rows ({}), limit is {MAX_TABLE_ROWS}",
                    rows.len()
                ));
            }
            for (index, row) in rows.iter().enumerate() {
                let Some(row) = row.as_object() else {
                    errors.push(format!(
                        "rows[{index}]: must be an object keyed by column keys"
                    ));
                    continue;
                };
                for (key, value) in row {
                    if !column_keys.contains(&key.as_str()) {
                        errors.push(format!("rows[{index}].{key}: not a declared column key"));
                    }
                    match value {
                        Value::String(s) => {
                            check_string_cap(&format!("rows[{index}].{key}"), s, errors);
                        }
                        Value::Number(_) | Value::Null => {}
                        _ => errors.push(format!(
                            "rows[{index}].{key}: must be string | number | null"
                        )),
                    }
                }
            }
        }
        Some(_) | None => errors.push("rows: required array of row objects".into()),
    }
    optional_string(map, "caption", errors);
    optional_string(map, "highlight", errors);
}

fn validate_app(map: &Map<String, Value>, errors: &mut Vec<String>) {
    reject_unknown(map, &["src", "title", "height"], errors);
    match map.get("src") {
        Some(Value::String(src))
            if src.starts_with('/')
                && !src.starts_with("//")
                && !src.contains('\\')
                && !src.chars().any(is_url_hostile_char) =>
        {
            check_string_cap("src", src, errors);
        }
        Some(_) | None => errors.push(
            "src: required same-origin path starting with `/` (no scheme, no `//host`, no \
             backslashes, no control characters — the WHATWG URL parser strips tab/newline/C0 \
             controls before parsing, so `/\\host` or `/\\n/host` would normalize to a \
             protocol-relative URL; percent-encoded forms like `/%0A` are fine)"
                .into(),
        ),
    }
    optional_string(map, "title", errors);
    if let Some(height) = map.get("height") {
        match height.as_f64() {
            Some(px) if (120.0..=2000.0).contains(&px) => {}
            _ => errors.push("height: must be a number between 120 and 2000 (px)".into()),
        }
    }
}

/// Characters the WHATWG URL parser strips or reinterprets before
/// parsing (#960 PR3 review round 2): ASCII C0 controls + DEL
/// (`is_ascii_control` — includes tab/LF/CR) and the C1 control range
/// U+0080..=U+009F. A `src` containing any of these could normalize
/// into a different URL in the browser, so they are rejected outright;
/// visible ASCII (incl. space) and non-control UTF-8 stay legal.
fn is_url_hostile_char(c: char) -> bool {
    c.is_ascii_control() || matches!(c, '\u{80}'..='\u{9f}')
}

fn reject_unknown(map: &Map<String, Value>, allowed: &[&str], errors: &mut Vec<String>) {
    for key in map.keys().filter(|k| !allowed.contains(&k.as_str())) {
        errors.push(format!("{key}: unknown field"));
    }
}

fn optional_string(map: &Map<String, Value>, key: &str, errors: &mut Vec<String>) {
    match map.get(key) {
        None => {}
        Some(Value::String(s)) => check_string_cap(key, s, errors),
        Some(_) => errors.push(format!("{key}: must be a string")),
    }
}

/// Every string field in a data payload is capped at
/// [`MAX_STRING_CHARS`] characters.
fn check_string_cap(path: &str, value: &str, errors: &mut Vec<String>) {
    let chars = value.chars().count();
    if chars > MAX_STRING_CHARS {
        errors.push(format!(
            "{path}: string too long ({chars} chars), limit is {MAX_STRING_CHARS}"
        ));
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
#[path = "kinds_tests.rs"]
mod tests;
