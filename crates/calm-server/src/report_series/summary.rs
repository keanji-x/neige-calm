//! The per-row summary (#1628 D4) and the row TTL that reads it (D3).
//!
//! `summarize` runs once, in the resolver, right before the row is written;
//! both readers (`calm.report.read` and the HTTP route) only deserialize what
//! is stored. `row_ttl` is the one function the read end and the drain-side
//! admission share, so "is this row still fresh" has a single definition.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::request::ymd;
use super::validate::{SeriesReply, ValidatedReply};
use super::{SERIES_TTL_MS, SERIES_UNAVAILABLE_TTL_MS};

/// What the `summary` column holds for an `ok` row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub series: Vec<SeriesSummary>,
}

/// One series' summary. `status` is the plugin's per-series verdict; only
/// `ok` entries carry the numbers, the other two carry `reason`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeriesSummary {
    pub asset: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complete_through: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<usize>,
    /// `[YYYY-MM-DD, value]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first: Option<(String, f64)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<(String, f64)>,
    /// `(last - first) / first * 100`, two decimals; `None` when `first == 0`.
    /// Serialized as `null` in that case so the key is always present on an
    /// `ok` entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_pct: Option<Option<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

fn two_decimals(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// Build the stored summary from a validated reply. `fields` decides which
/// column is "the value": the single requested field, or for candles the
/// `close` column with `high`/`low` taken from their own columns.
pub fn summarize(reply: &ValidatedReply, fields: &[String]) -> Summary {
    let is_candles = fields.len() == 5;
    let value_index = if is_candles {
        1 + fields.iter().position(|f| f == "close").unwrap_or(3)
    } else {
        1
    };
    let high_index = if is_candles {
        1 + fields.iter().position(|f| f == "high").unwrap_or(1)
    } else {
        value_index
    };
    let low_index = if is_candles {
        1 + fields.iter().position(|f| f == "low").unwrap_or(2)
    } else {
        value_index
    };
    let series = reply
        .series
        .iter()
        .map(|entry| match entry {
            SeriesReply::Ok {
                asset,
                currency,
                complete_through,
                points,
            } => {
                // Validation guarantees ≥ 2 points, every value finite.
                let first = &points[0];
                let last = &points[points.len() - 1];
                let first_value = first.values[value_index - 1];
                let last_value = last.values[value_index - 1];
                let change_pct = if first_value == 0.0 {
                    None
                } else {
                    Some(two_decimals(
                        (last_value - first_value) / first_value * 100.0,
                    ))
                };
                let high = points
                    .iter()
                    .map(|p| p.values[high_index - 1])
                    .fold(f64::NEG_INFINITY, f64::max);
                let low = points
                    .iter()
                    .map(|p| p.values[low_index - 1])
                    .fold(f64::INFINITY, f64::min);
                SeriesSummary {
                    asset: asset.clone(),
                    status: "ok".to_string(),
                    currency: currency.clone(),
                    complete_through: Some(ymd(*complete_through)),
                    n: Some(points.len()),
                    first: Some((ymd(first.date), first_value)),
                    last: Some((ymd(last.date), last_value)),
                    change_pct: Some(change_pct),
                    high: Some(high),
                    low: Some(low),
                    reason: None,
                }
            }
            SeriesReply::NotOk {
                asset,
                status,
                reason,
            } => SeriesSummary {
                asset: asset.clone(),
                status: status.clone(),
                currency: None,
                complete_through: None,
                n: None,
                first: None,
                last: None,
                change_pct: None,
                high: None,
                low: None,
                reason: Some(reason.clone()),
            },
        })
        .collect();
    Summary { series }
}

/// How long a row stays fresh (D3, S2.7): an `ok` row whose every series is
/// `ok` lives `SERIES_TTL`; an `unavailable` row, or an `ok` row with any
/// non-`ok` series, lives `SERIES_UNAVAILABLE_TTL`. Pinned rows never expire
/// — callers check `pinned` before asking.
pub fn row_ttl(status: &str, summary: Option<&Value>) -> i64 {
    if status != "ok" {
        return SERIES_UNAVAILABLE_TTL_MS;
    }
    let all_ok = summary
        .and_then(|summary| summary.get("series"))
        .and_then(Value::as_array)
        .is_some_and(|series| {
            series
                .iter()
                .all(|entry| entry.get("status").and_then(Value::as_str) == Some("ok"))
        });
    if all_ok {
        SERIES_TTL_MS
    } else {
        SERIES_UNAVAILABLE_TTL_MS
    }
}

/// Freshness against the injected clock: pinned rows are always fresh.
pub fn row_is_fresh(
    status: &str,
    summary: Option<&Value>,
    pinned: bool,
    resolved_at: i64,
    now_ms: i64,
) -> bool {
    pinned || resolved_at >= now_ms - row_ttl(status, summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ttl_follows_status_and_series_verdicts() {
        let all_ok = json!({ "series": [{ "status": "ok" }, { "status": "ok" }] });
        assert_eq!(row_ttl("ok", Some(&all_ok)), SERIES_TTL_MS);
        let partial = json!({ "series": [{ "status": "ok" }, { "status": "unknown_asset" }] });
        assert_eq!(row_ttl("ok", Some(&partial)), SERIES_UNAVAILABLE_TTL_MS);
        assert_eq!(row_ttl("unavailable", None), SERIES_UNAVAILABLE_TTL_MS);
        assert_eq!(
            row_ttl("ok", None),
            SERIES_UNAVAILABLE_TTL_MS,
            "no summary is not all-ok"
        );
        assert!(
            row_is_fresh("unavailable", None, true, 0, i64::MAX / 2),
            "pinned"
        );
        assert!(row_is_fresh(
            "ok",
            Some(&all_ok),
            false,
            1_000,
            1_000 + SERIES_TTL_MS
        ));
        assert!(!row_is_fresh(
            "ok",
            Some(&all_ok),
            false,
            1_000,
            1_001 + SERIES_TTL_MS
        ));
    }
}
