//! The request the kernel derives from a `chart.series` payload. `from_payload` runs at the read end and again when the job is dequeued, from the payload *as it is then*; both derive the same `request_hash`.
//! The window is not part of the fingerprint: a live block's cutoff moves every day.

use chrono::{Datelike, Days, NaiveDate};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use calm_types::report_blocks::kinds::LIVE_SOURCE_PREFIX;
use calm_types::report_blocks::{canonical_json, chart_series_range_days};

pub const CANDLE_FIELDS: [&str; 5] = ["open", "high", "low", "close", "volume"];

/// `frozen` when the payload carries `as_of`, `live` otherwise. Derived, never
/// stored: the presence of `as_of` is already in the fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Live,
    Frozen,
}

impl Mode {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Frozen => "frozen",
        }
    }
}

/// The resolved cutoff window `[start, as_of]`, both `YYYY-MM-DD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub start: NaiveDate,
    pub as_of: NaiveDate,
}

impl Window {
    pub fn start_text(&self) -> String {
        ymd(self.start)
    }
    pub fn as_of_text(&self) -> String {
        ymd(self.as_of)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesRequest {
    pub plugin_id: String,
    pub tool: String,
    pub series: Vec<String>,
    /// Derived from `view`: `candles` → the five OHLCV columns, anything
    /// else → `[field]`.
    pub fields: Vec<String>,
    pub range: String,
    pub period: String,
    /// The payload's own cutoff. `None` is a live block.
    pub as_of: Option<String>,
    pub view: String,
    pub field: String,
    /// `hex(sha256(canonical_json({plugin_id, tool, series, fields, range,
    /// period, as_of})))` — `as_of` only when present.
    pub request_hash: String,
}

fn string_or<'a>(payload: &'a Value, key: &str, default: &'a str) -> Result<&'a str, String> {
    match payload.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(_) => Err(format!("{key}: must be a string")),
    }
}

impl SeriesRequest {
    /// Derive the request from a payload that already passed `validate_chart_series`; errors here are for payloads that bypassed it.
    pub fn from_payload(payload: &Value) -> Result<Self, String> {
        let source = payload
            .get("source")
            .and_then(Value::as_str)
            .ok_or_else(|| "source: missing".to_string())?;
        let rest = source
            .strip_prefix(LIVE_SOURCE_PREFIX)
            .ok_or_else(|| format!("source: must start with {LIVE_SOURCE_PREFIX}"))?;
        let (plugin_id, tool) = match rest.split_once('/') {
            Some((plugin_id, tool)) if !plugin_id.is_empty() && !tool.is_empty() => {
                (plugin_id, tool)
            }
            _ => return Err("source: expected <plugin_id>/<tool>".to_string()),
        };
        if tool.contains('/') {
            return Err("source: expected exactly two segments".to_string());
        }
        let series: Vec<String> = payload
            .get("series")
            .and_then(Value::as_array)
            .ok_or_else(|| "series: missing".to_string())?
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "series: entries must be strings".to_string())
            })
            .collect::<Result<_, _>>()?;
        if series.is_empty() {
            return Err("series: empty".to_string());
        }
        let field = string_or(payload, "field", "close")?.to_string();
        let range = string_or(payload, "range", "1Y")?.to_string();
        if chart_series_range_days(&range).is_none() {
            return Err(format!("range: unknown `{range}`"));
        }
        let period = string_or(payload, "period", "day")?.to_string();
        if !matches!(period.as_str(), "day" | "week" | "month") {
            return Err(format!("period: unknown `{period}`"));
        }
        let view = string_or(payload, "view", "line")?.to_string();
        let as_of = match payload.get("as_of") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => {
                if NaiveDate::parse_from_str(s, "%Y-%m-%d").is_err() {
                    return Err(format!("as_of: not a calendar date `{s}`"));
                }
                Some(s.clone())
            }
            Some(_) => return Err("as_of: must be a string".to_string()),
        };
        let fields: Vec<String> = if view == "candles" {
            CANDLE_FIELDS.iter().map(|f| f.to_string()).collect()
        } else {
            vec![field.clone()]
        };
        let mut fingerprint = json!({
            "plugin_id": plugin_id,
            "tool": tool,
            "series": series,
            "fields": fields,
            "range": range,
            "period": period,
        });
        if let Some(as_of) = &as_of {
            fingerprint["as_of"] = Value::String(as_of.clone());
        }
        let request_hash = sha256_hex(canonical_json(&fingerprint).as_bytes());
        Ok(Self {
            plugin_id: plugin_id.to_string(),
            tool: tool.to_string(),
            series,
            fields,
            range,
            period,
            as_of,
            view,
            field,
            request_hash,
        })
    }

    pub fn mode(&self) -> Mode {
        if self.as_of.is_some() {
            Mode::Frozen
        } else {
            Mode::Live
        }
    }

    /// The cutoff window for a resolution happening at `now_ms`: frozen uses
    /// the payload's `as_of`, live uses yesterday UTC; `start` is
    /// `as_of - RANGE_DAYS[range]` on both. `None` only when the stored
    /// `as_of` is not a calendar date (a payload that bypassed validation).
    pub fn window(&self, now_ms: i64) -> Option<Window> {
        let as_of = match &self.as_of {
            Some(text) => NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()?,
            None => yesterday_utc(now_ms)?,
        };
        let days = chart_series_range_days(&self.range)?;
        let start = as_of.checked_sub_days(Days::new(u64::from(days)))?;
        Some(Window { start, as_of })
    }

    /// Ceiling on the number of points one series may carry for this
    /// `(range, period)`: `RANGE_DAYS / period_days + 2`.
    pub fn max_points(&self) -> usize {
        max_points(&self.range, &self.period)
    }

    /// The `tools/call` arguments. Every field is filled by the kernel; the plugin refuses a request that lacks any of them.
    pub fn tool_arguments(&self, window: &Window, deadline_ms: i64) -> Value {
        json!({
            "series": self.series,
            "fields": self.fields,
            "period": self.period,
            "mode": self.mode().wire(),
            "start": window.start_text(),
            "as_of": window.as_of_text(),
            "deadline_ms": deadline_ms,
        })
    }
}

/// `RANGE_DAYS[range] / period_days + 2`; period_days is 1 / 7 / 30.
pub fn max_points(range: &str, period: &str) -> usize {
    let days = chart_series_range_days(range).unwrap_or(0) as usize;
    let period_days = match period {
        "week" => 7,
        "month" => 30,
        _ => 1,
    };
    days / period_days + 2
}

/// The UTC calendar day before the one containing `now_ms`.
pub fn yesterday_utc(now_ms: i64) -> Option<NaiveDate> {
    chrono::DateTime::from_timestamp_millis(now_ms)?
        .date_naive()
        .checked_sub_days(Days::new(1))
}

pub fn ymd(date: NaiveDate) -> String {
    format!("{:04}-{:02}-{:02}", date.year(), date.month(), date.day())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "neige://plugin/dev-neige-market/market.series";

    fn request(payload: Value) -> SeriesRequest {
        SeriesRequest::from_payload(&payload).expect("payload derives")
    }

    #[test]
    fn fingerprint_ignores_presentation_and_tracks_source() {
        let base = request(json!({ "source": SOURCE, "series": ["US:NVDA"] }));
        assert_eq!(base.fields, vec!["close".to_string()]);
        assert_eq!(base.range, "1Y");
        assert_eq!(base.period, "day");
        assert_eq!(base.mode(), Mode::Live);

        let captioned = request(json!({
            "source": SOURCE, "series": ["US:NVDA"], "caption": "x", "overlays": ["ma20"]
        }));
        assert_eq!(
            captioned.request_hash, base.request_hash,
            "caption/overlays"
        );
        let bar = request(json!({ "source": SOURCE, "series": ["US:NVDA"], "view": "bar" }));
        assert_eq!(
            bar.request_hash, base.request_hash,
            "line<->bar share fields"
        );
        let candles =
            request(json!({ "source": SOURCE, "series": ["US:NVDA"], "view": "candles" }));
        assert_ne!(
            candles.request_hash, base.request_hash,
            "candles widens fields"
        );
        assert_eq!(candles.fields.len(), 5);

        let frozen = request(json!({
            "source": SOURCE, "series": ["US:NVDA"], "as_of": "2026-09-10"
        }));
        assert_ne!(frozen.request_hash, base.request_hash, "as_of enters");
        assert_eq!(frozen.mode(), Mode::Frozen);
        let other_source = request(json!({
            "source": "neige://plugin/other/market.series", "series": ["US:NVDA"]
        }));
        assert_ne!(
            other_source.request_hash, base.request_hash,
            "plugin id enters"
        );
        let other_tool = request(json!({
            "source": "neige://plugin/dev-neige-market/market.other", "series": ["US:NVDA"]
        }));
        assert_ne!(other_tool.request_hash, base.request_hash, "tool enters");
        assert_eq!(base.request_hash.len(), 64);
    }

    #[test]
    fn window_is_relative_to_the_cutoff() {
        let frozen = request(json!({
            "source": SOURCE, "series": ["US:NVDA"], "as_of": "2026-09-10"
        }));
        let window = frozen.window(0).unwrap();
        assert_eq!(window.as_of_text(), "2026-09-10");
        assert_eq!(window.start_text(), "2025-09-09");

        // 2026-09-14T12:00:00Z → yesterday is 2026-09-13.
        let noon = NaiveDate::from_ymd_opt(2026, 9, 14)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let live = request(json!({ "source": SOURCE, "series": ["US:NVDA"], "range": "1M" }));
        let window = live.window(noon).unwrap();
        assert_eq!(window.as_of_text(), "2026-09-13");
        assert_eq!(window.start_text(), "2026-08-13");
        assert_eq!(yesterday_utc(noon).map(ymd).as_deref(), Some("2026-09-13"));
        // One millisecond into the UTC day still counts as that day.
        let just_after_midnight = NaiveDate::from_ymd_opt(2026, 9, 15)
            .unwrap()
            .and_hms_milli_opt(0, 0, 0, 1)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        assert_eq!(
            yesterday_utc(just_after_midnight).map(ymd).as_deref(),
            Some("2026-09-14")
        );
    }

    #[test]
    fn max_points_follows_the_range_table() {
        assert_eq!(max_points("1Y", "day"), 368);
        assert_eq!(max_points("5Y", "day"), 1829);
        assert_eq!(max_points("5Y", "week"), 263);
        assert_eq!(max_points("5Y", "month"), 62);
    }

    #[test]
    fn tool_arguments_carry_the_seven_fields() {
        let live = request(json!({ "source": SOURCE, "series": ["US:NVDA", "HK:9988"] }));
        let window = live.window(1_789_000_000_000).unwrap();
        let args = live.tool_arguments(&window, 42);
        let keys: std::collections::BTreeSet<&str> = args
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let expected: std::collections::BTreeSet<&str> = [
            "series",
            "fields",
            "period",
            "mode",
            "start",
            "as_of",
            "deadline_ms",
        ]
        .into_iter()
        .collect();
        assert_eq!(keys, expected);
        assert_eq!(args["mode"], "live");
        assert_eq!(args["deadline_ms"], 42);
    }

    #[test]
    fn malformed_payloads_are_reported_not_panicked() {
        assert!(SeriesRequest::from_payload(&json!({ "series": ["US:NVDA"] })).is_err());
        assert!(SeriesRequest::from_payload(&json!({ "source": SOURCE })).is_err());
        assert!(
            SeriesRequest::from_payload(&json!({ "source": "https://x", "series": ["US:NVDA"] }))
                .is_err()
        );
        assert!(
            SeriesRequest::from_payload(&json!({
                "source": SOURCE, "series": ["US:NVDA"], "as_of": "2026-02-30"
            }))
            .is_err()
        );
    }
}
