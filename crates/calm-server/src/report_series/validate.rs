//! The kernel-side reply checklist: the kernel re-checks every point at its own boundary, and any failure turns the whole row `unavailable` — a partially wrong reply is never partially stored.
//! Branching is by `(mode, period)` only. `live ∧ day` checks `date ≤ as_of`; every other combination requires each period's end date strictly earlier than `complete_through`.

use chrono::{Datelike, Days, NaiveDate, Weekday};
use serde_json::Value;

use crate::plugin_host::mcp::CallToolResult;

use super::request::{Mode, SeriesRequest, Window, ymd};
use super::{MAX_SERIES_REPLY_BYTES, MAX_SERIES_ROW_BYTES};

const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    pub ts_ms: i64,
    pub date: NaiveDate,
    /// One value per requested field, in request order.
    pub values: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SeriesReply {
    Ok {
        asset: String,
        currency: Option<String>,
        complete_through: NaiveDate,
        points: Vec<Point>,
    },
    NotOk {
        asset: String,
        /// `unknown_asset` | `unavailable`.
        status: String,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedReply {
    pub series: Vec<SeriesReply>,
}

impl ValidatedReply {
    /// The `data` column: series with their points, aligned by index with
    /// the summary.
    pub fn data_json(&self) -> Value {
        let series: Vec<Value> = self
            .series
            .iter()
            .map(|entry| match entry {
                SeriesReply::Ok { asset, points, .. } => {
                    let points: Vec<Value> = points
                        .iter()
                        .map(|point| {
                            let mut row = Vec::with_capacity(1 + point.values.len());
                            row.push(Value::from(point.ts_ms));
                            row.extend(point.values.iter().map(|v| Value::from(*v)));
                            Value::Array(row)
                        })
                        .collect();
                    serde_json::json!({ "asset": asset, "status": "ok", "points": points })
                }
                SeriesReply::NotOk {
                    asset,
                    status,
                    reason,
                } => serde_json::json!({ "asset": asset, "status": status, "reason": reason }),
            })
            .collect();
        serde_json::json!({ "series": series })
    }

    /// Every series `ok` and every `complete_through` strictly later than `as_of` — the frozen pinning
    /// predicate. The caller applies it to frozen requests only.
    pub fn all_complete_past(&self, as_of: NaiveDate) -> bool {
        self.series.iter().all(|entry| match entry {
            SeriesReply::Ok {
                complete_through, ..
            } => *complete_through > as_of,
            SeriesReply::NotOk { .. } => false,
        })
    }
}

fn parse_date(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()
}

/// Last calendar day of `date`'s month.
pub fn month_end(date: NaiveDate) -> NaiveDate {
    let (year, month) = if date.month() == 12 {
        (date.year() + 1, 1)
    } else {
        (date.year(), date.month() + 1)
    };
    NaiveDate::from_ymd_opt(year, month, 1)
        .and_then(|first| first.checked_sub_days(Days::new(1)))
        .unwrap_or(date)
}

/// The end date of the period a point starts: the bar date itself for
/// `day`, the ISO Sunday for `week`, the month's last day for `month`.
pub fn period_end(period: &str, start: NaiveDate) -> NaiveDate {
    match period {
        "week" => start.checked_add_days(Days::new(6)).unwrap_or(start),
        "month" => month_end(start),
        _ => start,
    }
}

fn error_text(result: &CallToolResult) -> String {
    let text: Vec<&str> = result
        .content
        .iter()
        .filter_map(|block| block.text.as_deref())
        .collect();
    if text.is_empty() {
        "tool returned isError".to_string()
    } else {
        text.join(" ")
    }
}

/// Check one `tools/call` reply against the request that produced it.
pub fn validate_reply(
    result: &CallToolResult,
    request: &SeriesRequest,
    window: &Window,
) -> Result<ValidatedReply, String> {
    if result.is_error == Some(true) {
        return Err(format!("plugin error: {}", error_text(result)));
    }
    let serialized = serde_json::to_vec(result).map_err(|e| format!("reply serialize: {e}"))?;
    if serialized.len() > MAX_SERIES_REPLY_BYTES {
        return Err(format!(
            "reply is {} bytes, limit is {MAX_SERIES_REPLY_BYTES}",
            serialized.len()
        ));
    }
    let Some(structured) = result
        .structured_content
        .as_ref()
        .and_then(Value::as_object)
    else {
        return Err("reply structuredContent is not an object".to_string());
    };
    let Some(series) = structured.get("series").and_then(Value::as_array) else {
        return Err("reply has no `series` array".to_string());
    };
    if series.len() != request.series.len() {
        return Err(format!(
            "reply has {} series, request had {}",
            series.len(),
            request.series.len()
        ));
    }
    let mode = request.mode();
    let period = request.period.as_str();
    let relaxed = mode == Mode::Live && period == "day";
    let max_points = request.max_points();
    let width = 1 + request.fields.len();

    let mut out = Vec::with_capacity(series.len());
    for (index, (entry, expected_asset)) in series.iter().zip(&request.series).enumerate() {
        let Some(entry) = entry.as_object() else {
            return Err(format!("series[{index}]: not an object"));
        };
        let asset = entry.get("asset").and_then(Value::as_str).unwrap_or("");
        if asset != expected_asset {
            return Err(format!(
                "series[{index}]: asset `{asset}` does not match request `{expected_asset}`"
            ));
        }
        let status = entry.get("status").and_then(Value::as_str).unwrap_or("");
        match status {
            "unknown_asset" | "unavailable" => {
                let Some(reason) = entry.get("reason").and_then(Value::as_str) else {
                    return Err(format!("series[{index}]: `{status}` without a reason"));
                };
                out.push(SeriesReply::NotOk {
                    asset: asset.to_string(),
                    status: status.to_string(),
                    reason: reason.to_string(),
                });
                continue;
            }
            "ok" => {}
            other => return Err(format!("series[{index}]: unknown status `{other}`")),
        }
        let complete_through = match entry.get("complete_through") {
            Some(Value::String(text)) => parse_date(text).ok_or_else(|| {
                format!("series[{index}]: complete_through `{text}` is not a date")
            })?,
            _ => return Err(format!("series[{index}]: ok without complete_through")),
        };
        let currency = match entry.get("currency") {
            None | Some(Value::Null) => None,
            Some(Value::String(c)) => Some(c.clone()),
            Some(_) => return Err(format!("series[{index}]: currency is not a string")),
        };
        let Some(raw_points) = entry.get("points").and_then(Value::as_array) else {
            return Err(format!("series[{index}]: ok without points"));
        };
        if raw_points.len() < 2 {
            return Err(format!(
                "series[{index}]: ok with {} point(s), need at least 2",
                raw_points.len()
            ));
        }
        if raw_points.len() > max_points {
            return Err(format!(
                "series[{index}]: {} points exceed the {max_points} ceiling for {}/{}",
                raw_points.len(),
                request.range,
                request.period
            ));
        }
        let mut points: Vec<Point> = Vec::with_capacity(raw_points.len());
        for (p, raw) in raw_points.iter().enumerate() {
            let Some(row) = raw.as_array() else {
                return Err(format!("series[{index}].points[{p}]: not an array"));
            };
            if row.len() != width {
                return Err(format!(
                    "series[{index}].points[{p}]: {} entries, expected {width}",
                    row.len()
                ));
            }
            let Some(ts_ms) = row[0].as_i64() else {
                return Err(format!(
                    "series[{index}].points[{p}]: ts_ms is not an integer"
                ));
            };
            if ts_ms % DAY_MS != 0 {
                return Err(format!(
                    "series[{index}].points[{p}]: ts_ms {ts_ms} is not a UTC midnight"
                ));
            }
            if let Some(previous) = points.last()
                && ts_ms <= previous.ts_ms
            {
                return Err(format!(
                    "series[{index}].points[{p}]: ts_ms {ts_ms} is not after {}",
                    previous.ts_ms
                ));
            }
            let mut values = Vec::with_capacity(width - 1);
            for (v, raw_value) in row[1..].iter().enumerate() {
                match raw_value.as_f64() {
                    Some(value) if value.is_finite() => values.push(value),
                    _ => {
                        return Err(format!(
                            "series[{index}].points[{p}][{}]: not a finite number",
                            v + 1
                        ));
                    }
                }
            }
            let Some(date) = chrono::DateTime::from_timestamp_millis(ts_ms).map(|d| d.date_naive())
            else {
                return Err(format!("series[{index}].points[{p}]: ts_ms out of range"));
            };
            if date < window.start || date > window.as_of {
                return Err(format!(
                    "series[{index}].points[{p}]: {} is outside [{}, {}]",
                    ymd(date),
                    window.start_text(),
                    window.as_of_text()
                ));
            }
            match period {
                "week" if date.weekday() != Weekday::Mon => {
                    return Err(format!(
                        "series[{index}].points[{p}]: week period {} does not start on a Monday",
                        ymd(date)
                    ));
                }
                "month" if date.day() != 1 => {
                    return Err(format!(
                        "series[{index}].points[{p}]: month period {} does not start on the 1st",
                        ymd(date)
                    ));
                }
                _ => {}
            }
            let end = period_end(period, date);
            if end > window.as_of {
                return Err(format!(
                    "series[{index}].points[{p}]: period ending {} is after the cutoff {}",
                    ymd(end),
                    window.as_of_text()
                ));
            }
            if !relaxed && end >= complete_through {
                return Err(format!(
                    "series[{index}].points[{p}]: period ending {} is not before complete_through {}",
                    ymd(end),
                    ymd(complete_through)
                ));
            }
            points.push(Point {
                ts_ms,
                date,
                values,
            });
        }
        out.push(SeriesReply::Ok {
            asset: asset.to_string(),
            currency,
            complete_through,
            points,
        });
    }
    let reply = ValidatedReply { series: out };
    let data_len = serde_json::to_vec(&reply.data_json())
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX);
    if data_len > MAX_SERIES_ROW_BYTES {
        return Err(format!(
            "stored data would be {data_len} bytes, limit is {MAX_SERIES_ROW_BYTES}"
        ));
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn period_ends_follow_the_calendar() {
        let monday = NaiveDate::from_ymd_opt(2026, 8, 31).unwrap();
        assert_eq!(ymd(period_end("week", monday)), "2026-09-06");
        let first = NaiveDate::from_ymd_opt(2026, 2, 1).unwrap();
        assert_eq!(ymd(period_end("month", first)), "2026-02-28");
        let leap = NaiveDate::from_ymd_opt(2028, 2, 1).unwrap();
        assert_eq!(ymd(period_end("month", leap)), "2028-02-29");
        let december = NaiveDate::from_ymd_opt(2026, 12, 1).unwrap();
        assert_eq!(ymd(period_end("month", december)), "2026-12-31");
        assert_eq!(ymd(period_end("day", monday)), "2026-08-31");
    }
}
