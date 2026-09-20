//! Resolved `chart.series` data: the background resolver, the `report_series` rows, and the `resolved` projection.
//! Nothing happens when a block is written; the first read enqueues a job, and readers only ever read rows. A frozen block (`as_of` present) is pinned by its first complete resolution; a live block is re-resolved when its row is older than its TTL.

mod admission;
pub(crate) mod hydrate;
pub mod request;
pub mod resolver;
#[cfg(any(test, feature = "fixtures"))]
pub mod seams;
pub mod store;
pub mod summary;
pub mod validate;

use std::time::Duration;

use serde_json::{Value, json};

pub use request::{Mode, SeriesRequest, Window, max_points, yesterday_utc};
pub use resolver::{Enqueue, InflightGuard, Job, ResolveOutcome, SeriesResolver};
pub use store::{Detail, NewRow, SeriesRow};
pub use summary::{Summary, row_is_fresh, row_ttl, summarize};

pub const SERIES_RESOLVE_TIMEOUT: Duration = Duration::from_secs(30);
/// Freshness of an `ok` row whose every series is `ok`.
pub const SERIES_TTL_MS: i64 = 6 * 60 * 60 * 1000;
/// Freshness of an `unavailable` row or an `ok` row with any non-`ok` series.
pub const SERIES_UNAVAILABLE_TTL_MS: i64 = 2 * 60 * 1000;
/// Largest `CallToolResult` the kernel accepts (a checklist item, not a transport bound).
pub const MAX_SERIES_REPLY_BYTES: usize = 2 * 1024 * 1024;
/// Largest serialized `data` column.
pub const MAX_SERIES_ROW_BYTES: usize = 1024 * 1024;
/// `reason` is truncated to this many characters before it is stored.
pub const MAX_REASON_CHARS: usize = 256;

/// The `resolved` projection of one block. `Pending` is the absence of a row; the other two are rows.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolved {
    Pending {
        reason: Option<String>,
    },
    Unavailable {
        reason: String,
        resolved_at: i64,
    },
    Ok {
        as_of: String,
        resolved_at: i64,
        pinned: bool,
        /// The stored `summary` column, verbatim.
        summary: Value,
        /// The stored `data` column, only on a `full` read.
        data: Option<Value>,
    },
}

/// `resolved_at` on the wire: RFC 3339, second precision, UTC.
pub fn resolved_at_text(resolved_at_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(resolved_at_ms)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| resolved_at_ms.to_string())
}

impl Resolved {
    /// From a stored row (`None` = pending without reason).
    pub fn from_row(row: Option<SeriesRow>) -> Self {
        match row {
            None => Self::Pending { reason: None },
            Some(row) if row.status == "ok" => Self::Ok {
                as_of: row.as_of,
                resolved_at: row.resolved_at,
                pinned: row.pinned,
                summary: row.summary.unwrap_or_else(|| json!({ "series": [] })),
                data: row.data,
            },
            Some(row) => Self::Unavailable {
                reason: row.reason.unwrap_or_default(),
                resolved_at: row.resolved_at,
            },
        }
    }

    /// The flattened wire object. `series[j].points` is attached from `data`
    /// when present; the caller adds the block's presentation fields.
    pub fn to_json(&self) -> Value {
        match self {
            Self::Pending { reason } => {
                let mut out = json!({ "status": "pending" });
                if let Some(reason) = reason {
                    out["reason"] = Value::String(reason.clone());
                }
                out
            }
            Self::Unavailable {
                reason,
                resolved_at,
            } => json!({
                "status": "unavailable",
                "reason": reason,
                "resolved_at": resolved_at_text(*resolved_at),
            }),
            Self::Ok {
                as_of,
                resolved_at,
                pinned,
                summary,
                data,
            } => {
                let mut series = summary
                    .get("series")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if let Some(data_series) = data
                    .as_ref()
                    .and_then(|data| data.get("series"))
                    .and_then(Value::as_array)
                {
                    for (entry, stored) in series.iter_mut().zip(data_series) {
                        if let Some(points) = stored.get("points") {
                            entry["points"] = points.clone();
                        }
                    }
                }
                json!({
                    "status": "ok",
                    "as_of": as_of,
                    "resolved_at": resolved_at_text(*resolved_at),
                    "pinned": pinned,
                    "series": series,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_flattens_like_the_design_sample() {
        let pending = Resolved::Pending {
            reason: Some("plugin x is not running".into()),
        }
        .to_json();
        assert_eq!(pending["status"], "pending");
        assert_eq!(pending["reason"], "plugin x is not running");
        let bare = Resolved::Pending { reason: None }.to_json();
        assert!(bare.get("reason").is_none());

        let ok = Resolved::Ok {
            as_of: "2026-09-11".into(),
            resolved_at: 1_789_000_000_000,
            pinned: true,
            summary: json!({ "series": [{ "asset": "US:NVDA", "status": "ok", "n": 2 }] }),
            data: Some(json!({ "series": [{ "asset": "US:NVDA", "points": [[0, 1.0]] }] })),
        }
        .to_json();
        assert_eq!(ok["status"], "ok");
        assert_eq!(ok["pinned"], true);
        assert_eq!(ok["series"][0]["n"], 2);
        assert_eq!(ok["series"][0]["points"], json!([[0, 1.0]]));
        assert_eq!(ok["resolved_at"], "2026-09-10T00:26:40Z");

        let summary_only = Resolved::Ok {
            as_of: "2026-09-11".into(),
            resolved_at: 0,
            pinned: false,
            summary: json!({ "series": [{ "asset": "US:NVDA", "status": "ok" }] }),
            data: None,
        }
        .to_json();
        assert!(summary_only["series"][0].get("points").is_none());
    }
}
