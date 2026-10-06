//! Shared timestamp formatting for agent-facing views.

use chrono::{Local, SecondsFormat, TimeZone};

/// A unix-ms time as RFC 3339 with the server's offset and millisecond precision;
/// `None` when it is out of range.
pub(crate) fn rfc3339_local_ms(ms: i64) -> Option<String> {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|at| at.to_rfc3339_opts(SecondsFormat::Millis, false))
}
