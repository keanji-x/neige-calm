//! #960 PR3 — the deterministic fenced flat representation of
//! non-prose report blocks (design §3.5).
//!
//! A non-prose block appears in the flat `body` projection as a
//! ```` ```neige-block <kind> ```` fence whose interior is the block's
//! payload as **canonical JSON** (keys sorted bytewise, 2-space
//! indent, scalar-only arrays inlined). The fence carries **no id and
//! no rev** (design D9): the dispatcher fingerprints `body` with
//! SHA256 and the VCS snapshot diffs it, so metadata would pollute
//! both, while canonical pretty JSON turns parameter edits into
//! line-level diffs.
//!
//! Read side is lenient: [`parse_fence`] only accepts a slice that is
//! *exactly* one well-formed fence (unindented ```` ```neige-block
//! <kind> ```` opener, a JSON **object** interior, an unindented
//! ```` ``` ```` closer); anything else reads as prose. Write ends
//! must reject malformed fences explicitly
//! (`super::invalid_neige_fences`).
//!
//! [`render_fence`] ∘ [`parse_fence`] is idempotent: rendering a
//! parsed payload reproduces the canonical fence byte for byte.

use serde_json::Value;

/// A parsed `neige-block` fence: the block kind from the info string
/// plus the JSON object payload.
#[derive(Debug, Clone, PartialEq)]
pub struct NonProseFence {
    pub kind: String,
    pub payload: Value,
}

/// `Some(kind)` iff `line` (ending-stripped or raw) is an unindented
/// `neige-block` fence opener: exactly three backticks, the literal
/// info word, one space, and a `[a-z0-9._-]+` kind token.
pub fn neige_open_kind(line: &str) -> Option<&str> {
    let kind = line.strip_prefix("```neige-block ")?;
    let kind = kind.strip_suffix('\n').unwrap_or(kind);
    let kind = kind.strip_suffix('\r').unwrap_or(kind);
    (!kind.is_empty()
        && kind
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-')))
    .then_some(kind)
}

/// Parse `raw` iff it is exactly one well-formed `neige-block` fence:
/// opener line, a JSON **object** interior, and a bare ```` ``` ````
/// closing line (final newline optional — a fence at EOF may lack it).
/// Returns `None` for everything else — the caller treats the slice
/// as prose (lenient read).
pub fn parse_fence(raw: &str) -> Option<NonProseFence> {
    let (first_line, rest) = raw.split_once('\n')?;
    let kind = neige_open_kind(first_line)?;
    let rest = rest.strip_suffix('\n').unwrap_or(rest);
    let (interior, close) = match rest.rsplit_once('\n') {
        Some((interior, close)) => (interior, close),
        None => ("", rest),
    };
    let close = close.strip_suffix('\r').unwrap_or(close);
    if close != "```" {
        return None;
    }
    let payload: Value = serde_json::from_str(interior).ok()?;
    payload.is_object().then(|| NonProseFence {
        kind: kind.to_string(),
        payload,
    })
}

/// The canonical fence text for a non-prose block: what the CRDT
/// stores in the block's `text` field and what the flat `body`
/// projection therefore contains. Deterministic: byte-identical for
/// semantically-equal payloads.
pub fn render_fence(kind: &str, payload: &Value) -> String {
    format!("```neige-block {kind}\n{}\n```\n", canonical_json(payload))
}

/// Deterministic pretty JSON: object keys sorted bytewise, 2-space
/// indent, `": "` separators, arrays of scalars inlined on one line
/// (so a candle row is one line and a parameter edit is a line-level
/// diff), composite arrays and objects broken across lines. Idempotent
/// under parse→render (serde_json numbers round-trip exactly).
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_value(value, 0, &mut out);
    out
}

fn write_value(value: &Value, indent: usize, out: &mut String) {
    match value {
        Value::Object(map) if map.is_empty() => out.push_str("{}"),
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.push_str("{\n");
            for (position, key) in keys.iter().enumerate() {
                pad(indent + 1, out);
                out.push_str(&scalar_text(&Value::String((*key).clone())));
                out.push_str(": ");
                write_value(&map[*key], indent + 1, out);
                if position + 1 < keys.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            pad(indent, out);
            out.push('}');
        }
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Array(items) if items.iter().all(is_scalar) => {
            out.push('[');
            for (position, item) in items.iter().enumerate() {
                if position > 0 {
                    out.push_str(", ");
                }
                out.push_str(&scalar_text(item));
            }
            out.push(']');
        }
        Value::Array(items) => {
            out.push_str("[\n");
            for (position, item) in items.iter().enumerate() {
                pad(indent + 1, out);
                write_value(item, indent + 1, out);
                if position + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            pad(indent, out);
            out.push(']');
        }
        scalar => out.push_str(&scalar_text(scalar)),
    }
}

fn is_scalar(value: &Value) -> bool {
    !matches!(value, Value::Object(_) | Value::Array(_))
}

fn scalar_text(value: &Value) -> String {
    serde_json::to_string(value).expect("scalar JSON serialization cannot fail")
}

fn pad(indent: usize, out: &mut String) {
    for _ in 0..indent {
        out.push_str("  ");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    #[test]
    fn open_kind_accepts_only_the_canonical_opener() {
        assert_eq!(
            neige_open_kind("```neige-block chart.candles"),
            Some("chart.candles")
        );
        assert_eq!(neige_open_kind("```neige-block app\r"), Some("app"));
        for reject in [
            "```neige-block ",      // empty kind
            "```neige-block Chart", // uppercase
            "```neige-block a b",   // space in kind
            "````neige-block app",  // four backticks
            " ```neige-block app",  // indented
            "```neige-blocks app",  // wrong info word
            "~~~neige-block app",   // tilde fence
        ] {
            assert_eq!(neige_open_kind(reject), None, "{reject:?}");
        }
    }

    #[test]
    fn parse_accepts_exactly_one_well_formed_fence() {
        let raw = "```neige-block app\n{\"src\": \"/x\"}\n```\n";
        let fence = parse_fence(raw).expect("well-formed fence parses");
        assert_eq!(fence.kind, "app");
        assert_eq!(fence.payload, json!({ "src": "/x" }));
        // Final newline optional (fence at EOF).
        assert!(parse_fence("```neige-block app\n{\"src\": \"/x\"}\n```").is_some());
        // CRLF interior tolerated (JSON whitespace).
        assert!(parse_fence("```neige-block app\r\n{\"src\": \"/x\"}\r\n```\r\n").is_some());

        for reject in [
            "```neige-block app\nnot json\n```\n",     // invalid JSON
            "```neige-block app\n[1, 2]\n```\n",       // non-object payload
            "```neige-block app\n{}\n````\n",          // over-long closer
            "```neige-block app\n{}\n``` \n",          // trailing space on closer
            "```neige-block app\n{}\n```\ntrailing\n", // content after closer
            "```neige-block app\n{}\n",                // unterminated
            "```neige-block app\n```\n",               // empty interior
            "prefix\n```neige-block app\n{}\n```\n",   // content before opener
        ] {
            assert!(parse_fence(reject).is_none(), "{reject:?}");
        }
    }

    #[test]
    fn render_is_canonical_and_parse_render_round_trips() {
        // Key order and formatting in the input do not matter.
        let a =
            json!({ "symbol": "0700.HK", "candles": [[1, 2.0, 3, 1, 2, 100], [2, 2, 4, 2, 3]] });
        let b: Value = serde_json::from_str(
            "{\"candles\":[[1,2.0,3,1,2,100],[2,2,4,2,3]],\"symbol\":\"0700.HK\"}",
        )
        .unwrap();
        let rendered = render_fence("chart.candles", &a);
        assert_eq!(rendered, render_fence("chart.candles", &b));
        assert_eq!(
            rendered,
            "```neige-block chart.candles\n{\n  \"candles\": [\n    [1, 2.0, 3, 1, 2, 100],\n    [2, 2, 4, 2, 3]\n  ],\n  \"symbol\": \"0700.HK\"\n}\n```\n",
        );
        // Idempotence: parse the rendered fence, render again → same bytes.
        let parsed = parse_fence(&rendered).expect("canonical fence parses");
        assert_eq!(parsed.kind, "chart.candles");
        assert_eq!(render_fence(&parsed.kind, &parsed.payload), rendered);
    }

    #[test]
    fn canonical_json_layout_rules() {
        assert_eq!(canonical_json(&json!({})), "{}");
        assert_eq!(canonical_json(&json!([])), "[]");
        assert_eq!(
            canonical_json(&json!([1, "x", null, true])),
            "[1, \"x\", null, true]"
        );
        assert_eq!(
            canonical_json(&json!({ "b": 1, "a": { "z": [], "y": "s" } })),
            "{\n  \"a\": {\n    \"y\": \"s\",\n    \"z\": []\n  },\n  \"b\": 1\n}",
        );
        assert_eq!(
            canonical_json(&json!([{ "k": 1 }])),
            "[\n  {\n    \"k\": 1\n  }\n]",
        );
    }

    fn arbitrary_json() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(|n| json!(n)),
            any::<f64>()
                .prop_filter("finite", |f| f.is_finite())
                .prop_map(|n| json!(n)),
            "[a-z甲乙\" \\\\]{0,8}".prop_map(Value::String),
        ];
        leaf.prop_recursive(4, 32, 6, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
                prop::collection::hash_map("[a-z甲]{0,4}", inner, 0..6)
                    .prop_map(|m| Value::Object(m.into_iter().collect())),
            ]
        })
    }

    proptest! {
        #[test]
        fn canonical_json_parse_render_is_idempotent(value in arbitrary_json()) {
            let rendered = canonical_json(&value);
            let reparsed: Value = serde_json::from_str(&rendered).expect("canonical JSON parses");
            prop_assert_eq!(&reparsed, &value, "parse(render(v)) == v");
            prop_assert_eq!(canonical_json(&reparsed), rendered, "render is idempotent");
        }
    }
}
