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

pub const KIND_PROSE: &str = "prose";
pub const KIND_CHART_CANDLES: &str = "chart.candles";
pub const KIND_TABLE: &str = "table";
pub const KIND_APP: &str = "app";

/// The non-prose kinds a report may contain, in `blocks.kinds` order.
pub const DATA_KINDS: [&str; 3] = [KIND_CHART_CANDLES, KIND_TABLE, KIND_APP];

pub fn is_data_kind(kind: &str) -> bool {
    DATA_KINDS.contains(&kind)
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
        other => errors.push(format!(
            "unknown block kind `{other}` — known data kinds: {}",
            DATA_KINDS.join(", ")
        )),
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn validate_chart(map: &Map<String, Value>, errors: &mut Vec<String>) {
    reject_unknown(
        map,
        &["symbol", "period", "candles", "overlays", "caption"],
        errors,
    );
    match map.get("symbol") {
        Some(Value::String(s)) if !s.is_empty() => {}
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
                        column_keys.push(key);
                    }
                    _ => errors.push(format!("columns[{index}].key: required non-empty string")),
                }
                if !matches!(column.get("label"), Some(Value::String(_))) {
                    errors.push(format!("columns[{index}].label: required string"));
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
                    if !matches!(value, Value::String(_) | Value::Number(_) | Value::Null) {
                        errors.push(format!(
                            "rows[{index}].{key}: must be string | number | null"
                        ));
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
        Some(Value::String(src)) if src.starts_with('/') && !src.starts_with("//") => {}
        Some(_) | None => errors.push(
            "src: required same-origin path starting with `/` (no scheme, no `//host`)".into(),
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

fn reject_unknown(map: &Map<String, Value>, allowed: &[&str], errors: &mut Vec<String>) {
    for key in map.keys().filter(|k| !allowed.contains(&k.as_str())) {
        errors.push(format!("{key}: unknown field"));
    }
}

fn optional_string(map: &Map<String, Value>, key: &str, errors: &mut Vec<String>) {
    if let Some(value) = map.get(key)
        && !value.is_string()
    {
        errors.push(format!("{key}: must be a string"));
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
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chart_payload_valid_and_invalid() {
        assert_eq!(
            validate_payload(
                KIND_CHART_CANDLES,
                &json!({
                    "symbol": "0700.HK",
                    "period": "day",
                    "candles": [[1000, 1.0, 2.0, 0.5, 1.5, 100], [2000, 1.5, 2.5, 1.0, 2.0]],
                    "overlays": ["ma20", "ma60"],
                    "caption": "Tencent daily"
                }),
            ),
            Ok(())
        );
        // Field-level errors, all collected.
        let err = validate_payload(
            KIND_CHART_CANDLES,
            &json!({ "candles": [[1, 2]], "period": "year", "extra": 1 }),
        )
        .unwrap_err();
        assert!(err.contains("extra: unknown field"), "{err}");
        assert!(err.contains("symbol: required"), "{err}");
        assert!(err.contains("period: must be one of"), "{err}");
        assert!(err.contains("at least 2 candles"), "{err}");
        assert!(err.contains("candles[0]"), "{err}");
        // Non-object payload.
        assert!(validate_payload(KIND_CHART_CANDLES, &json!([1])).is_err());
    }

    #[test]
    fn table_payload_valid_and_invalid() {
        assert_eq!(
            validate_payload(
                KIND_TABLE,
                &json!({
                    "columns": [
                        { "key": "name", "label": "公司" },
                        { "key": "pe", "label": "PE", "align": "right" }
                    ],
                    "rows": [
                        { "name": "腾讯", "pe": 18.2 },
                        { "name": "阿里", "pe": null }
                    ],
                    "caption": "可比公司",
                    "highlight": "腾讯"
                }),
            ),
            Ok(())
        );
        let err = validate_payload(
            KIND_TABLE,
            &json!({
                "columns": [
                    { "key": "a", "label": "A", "align": "center", "width": 1 },
                    { "key": "a", "label": 3 }
                ],
                "rows": [{ "b": {} }],
            }),
        )
        .unwrap_err();
        assert!(err.contains("columns[0].align"), "{err}");
        assert!(err.contains("columns[0].width: unknown field"), "{err}");
        assert!(err.contains("columns[1].key: duplicate"), "{err}");
        assert!(err.contains("columns[1].label: required string"), "{err}");
        assert!(
            err.contains("rows[0].b: not a declared column key"),
            "{err}"
        );
        assert!(
            err.contains("rows[0].b: must be string | number | null"),
            "{err}"
        );
        assert!(
            validate_payload(KIND_TABLE, &json!({ "columns": [], "rows": [] })).is_err(),
            "empty columns rejected"
        );
    }

    #[test]
    fn app_payload_valid_and_invalid() {
        assert_eq!(
            validate_payload(
                KIND_APP,
                &json!({ "src": "/apps/screener", "title": "选股器", "height": 600 })
            ),
            Ok(())
        );
        assert_eq!(validate_payload(KIND_APP, &json!({ "src": "/x" })), Ok(()));
        for (payload, needle) in [
            (json!({ "src": "https://evil.example/x" }), "src"),
            (json!({ "src": "//evil.example/x" }), "src"),
            (json!({ "src": "relative/path" }), "src"),
            (json!({}), "src"),
            (json!({ "src": "/x", "height": 80 }), "height"),
            (json!({ "src": "/x", "height": 9000 }), "height"),
            (json!({ "src": "/x", "onload": "x" }), "unknown field"),
        ] {
            let err = validate_payload(KIND_APP, &payload).unwrap_err();
            assert!(err.contains(needle), "{payload} → {err}");
        }
    }

    #[test]
    fn unknown_kind_is_an_error() {
        let err = validate_payload("metrics", &json!({})).unwrap_err();
        assert!(err.contains("unknown block kind `metrics`"), "{err}");
        assert!(!is_data_kind("prose"));
        assert!(is_data_kind("chart.candles"));
    }
}
