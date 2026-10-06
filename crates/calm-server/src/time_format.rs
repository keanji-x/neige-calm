//! Shared timestamp formatting for agent-facing views.

use chrono::{Local, SecondsFormat, TimeZone};
use serde_json::Value;

/// A unix-ms time as RFC 3339 with the server's offset and millisecond precision;
/// `None` when it is out of range.
pub(crate) fn rfc3339_local_ms(ms: i64) -> Option<String> {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|at| at.to_rfc3339_opts(SecondsFormat::Millis, false))
}

/// A unix-ms event time as a tool result carries it (agent-commands §4): an RFC 3339 string at the
/// server's offset, millisecond precision; `null` only for a time out of chrono's range.
pub(crate) fn at(ms: i64) -> Value {
    rfc3339_local_ms(ms).map_or(Value::Null, Value::String)
}

/// [`at`] for a time that may be absent (`null`).
pub(crate) fn at_opt(ms: Option<i64>) -> Value {
    ms.map_or(Value::Null, at)
}

/// Rewrites the named unix-ms keys of a serialized REST row in place with [`at`], so a tool can
/// return the row the REST route serves; an absent or `null` key stays as it is. A value that is
/// not an integer is a caller bug: it is kept as it is (never turned into `null`) and fails a
/// debug build, so the time invariant test names it.
pub(crate) fn rewrite_at(row: &mut Value, keys: &[&str]) {
    let Some(object) = row.as_object_mut() else {
        return;
    };
    for key in keys {
        let Some(value) = object.get_mut(*key) else {
            continue;
        };
        match value.as_i64() {
            Some(ms) => *value = at(ms),
            None => debug_assert!(value.is_null(), "{key} is not a unix-ms time: {value}"),
        }
    }
}
