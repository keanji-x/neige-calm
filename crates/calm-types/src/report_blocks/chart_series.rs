//! The `chart.series` payload: a chart that *names* its data instead of carrying it.
//! Pure functions only: `as_of` is checked against the calendar, never against today.

use serde_json::{Map, Value};

use super::kinds::{
    LIVE_SOURCE_PREFIX, MAX_CHART_SERIES, check_string_cap, optional_string, reject_unknown,
    validate_live_source,
};

/// Calendar days each `range` reaches back from the cutoff, inclusive on both ends: window = `[as_of - days, as_of]`.
pub const RANGE_DAYS: [(&str, u32); 6] = [
    ("1M", 31),
    ("3M", 92),
    ("6M", 183),
    ("1Y", 366),
    ("2Y", 731),
    ("5Y", 1827),
];

/// [`RANGE_DAYS`] as a lookup; `None` for a string that is not a range.
pub fn chart_series_range_days(range: &str) -> Option<u32> {
    RANGE_DAYS
        .iter()
        .find(|(key, _)| *key == range)
        .map(|(_, days)| *days)
}

pub const CHART_SERIES_RANGES: [&str; 6] = ["1M", "3M", "6M", "1Y", "2Y", "5Y"];
pub const CHART_SERIES_FIELDS: [&str; 5] = ["close", "open", "high", "low", "volume"];
pub const CHART_SERIES_PERIODS: [&str; 3] = ["day", "week", "month"];
pub const CHART_SERIES_VIEWS: [&str; 4] = ["line", "normalized", "bar", "candles"];
pub const CHART_SERIES_OVERLAYS: [&str; 2] = ["ma20", "ma60"];

/// Proleptic Gregorian leap-year rule.
pub fn is_leap_year(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

/// Days in `month` (1–12) of `year`; `None` for a month outside 1–12.
pub fn days_in_month(year: u32, month: u32) -> Option<u32> {
    Some(match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => return None,
    })
}

/// `YYYY-MM-DD` → `(year, month, day)` iff the text has exactly that shape AND names a day that
/// exists on the Gregorian calendar; a future date is a valid cutoff.
pub fn parse_ymd(text: &str) -> Option<(u32, u32, u32)> {
    let bytes = text.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        let slice = &bytes[range];
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        slice.iter().try_fold(0u32, |acc, b| {
            acc.checked_mul(10)?.checked_add((b - b'0') as u32)
        })
    };
    let year = digits(0..4)?;
    let month = digits(5..7)?;
    let day = digits(8..10)?;
    let last = days_in_month(year, month)?;
    (1..=last).contains(&day).then_some((year, month, day))
}

/// Shape + calendar check for an `as_of` cutoff; see [`parse_ymd`].
pub fn is_valid_ymd(text: &str) -> bool {
    parse_ymd(text).is_some()
}

/// `^[A-Z]{2,8}:[A-Za-z0-9._-]{1,32}$` — a venue prefix and a symbol; the kernel only checks the shape.
fn is_venue_qualified_asset(text: &str) -> bool {
    let Some((venue, symbol)) = text.split_once(':') else {
        return false;
    };
    (2..=8).contains(&venue.len())
        && venue.bytes().all(|b| b.is_ascii_uppercase())
        && (1..=32).contains(&symbol.len())
        && symbol
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn enum_check<'a>(
    map: &Map<String, Value>,
    key: &str,
    allowed: &[&'a str],
    errors: &mut Vec<String>,
) -> Option<&'a str> {
    let value = map.get(key)?;
    match value.as_str() {
        Some(s) => match allowed.iter().find(|a| **a == s) {
            Some(found) => Some(found),
            None => {
                errors.push(format!(
                    "{key}: must be one of {}",
                    allowed
                        .iter()
                        .map(|a| format!("\"{a}\""))
                        .collect::<Vec<_>>()
                        .join(" | ")
                ));
                None
            }
        },
        None => {
            errors.push(format!("{key}: must be a string"));
            None
        }
    }
}

/// Validate a `chart.series` payload: shape, the `as_of` calendar, and the `(range, period)` combination.
pub(super) fn validate_chart_series(map: &Map<String, Value>, errors: &mut Vec<String>) {
    reject_unknown(
        map,
        &[
            "source", "series", "field", "range", "period", "view", "as_of", "overlays", "caption",
        ],
        errors,
    );

    match map.get("source") {
        Some(Value::String(source)) => {
            check_string_cap("source", source, errors);
            if let Err(e) = validate_live_source(source) {
                errors.push(e);
            }
        }
        Some(_) | None => errors.push(format!(
            "source: required string of the form `{LIVE_SOURCE_PREFIX}<plugin_id>/<tool>`"
        )),
    }

    let mut series_len = 0usize;
    match map.get("series") {
        Some(Value::Array(items)) => {
            series_len = items.len();
            if items.is_empty() {
                errors.push("series: at least 1 asset required".into());
            }
            if items.len() > MAX_CHART_SERIES {
                errors.push(format!(
                    "series: too many assets ({}), limit is {MAX_CHART_SERIES}",
                    items.len()
                ));
            }
            let mut seen: Vec<&str> = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                let Some(asset) = item.as_str() else {
                    errors.push(format!("series[{index}]: must be a string"));
                    continue;
                };
                check_string_cap(&format!("series[{index}]"), asset, errors);
                if !is_venue_qualified_asset(asset) {
                    errors.push(format!(
                        "series[{index}]: must be a venue-qualified asset id matching \
                         ^[A-Z]{{2,8}}:[A-Za-z0-9._-]{{1,32}}$ (e.g. \"US:NVDA\"), got `{asset}`"
                    ));
                }
                if let Some(first) = seen.iter().position(|s| *s == asset) {
                    errors.push(format!(
                        "series[{index}]: duplicate of series[{first}] (`{asset}`)"
                    ));
                }
                seen.push(asset);
            }
        }
        Some(_) | None => errors.push(format!(
            "series: required array of 1..{MAX_CHART_SERIES} venue-qualified asset ids \
             (e.g. [\"US:NVDA\", \"HK:9988\"])"
        )),
    }

    enum_check(map, "field", &CHART_SERIES_FIELDS, errors);
    let range = enum_check(map, "range", &CHART_SERIES_RANGES, errors);
    let period = enum_check(map, "period", &CHART_SERIES_PERIODS, errors);
    // Absent `view` is `line`; only a present-and-valid `view` can be `candles`.
    let view = enum_check(map, "view", &CHART_SERIES_VIEWS, errors).unwrap_or("line");

    if view == "candles" {
        if map.contains_key("field") {
            errors.push("field: must be absent when view is \"candles\"".into());
        }
        if map.get("series").is_some_and(Value::is_array) && series_len != 1 {
            errors.push(format!(
                "view: \"candles\" requires exactly 1 series, got {series_len}"
            ));
        }
    }

    if let Some(overlays) = map.get("overlays") {
        match overlays.as_array() {
            Some(items) => {
                for (index, item) in items.iter().enumerate() {
                    if !item
                        .as_str()
                        .is_some_and(|s| CHART_SERIES_OVERLAYS.contains(&s))
                    {
                        errors.push(format!("overlays[{index}]: must be \"ma20\" | \"ma60\""));
                    }
                }
            }
            None => errors.push("overlays: must be an array".into()),
        }
        // Overlays under a view other than `line` / `candles` are refused, not ignored.
        if !matches!(view, "line" | "candles") {
            errors.push("overlays: apply only to view line|candles".into());
        }
    }

    optional_string(map, "caption", errors);

    if let Some(as_of) = map.get("as_of") {
        match as_of.as_str() {
            Some(text) => {
                check_string_cap("as_of", text, errors);
                if !is_valid_ymd(text) {
                    errors.push(format!(
                        "as_of: must be a calendar date in YYYY-MM-DD form, got `{text}`"
                    ));
                }
            }
            None => errors.push("as_of: must be a string".into()),
        }
    }

    if range == Some("1M") && period == Some("month") {
        errors.push("range 1M cannot hold two complete month periods".into());
    }
}

#[cfg(test)]
mod tests {
    use super::super::kinds::{KIND_CHART_SERIES, validate_payload};
    use super::*;
    use serde_json::json;

    const SOURCE: &str = "neige://plugin/dev-neige-market/market.series";

    fn check(payload: Value) -> Result<(), String> {
        validate_payload(KIND_CHART_SERIES, &payload)
    }

    /// The refusal for `payload`; `name` is the assertion being made.
    fn err_of(name: &str, payload: Value) -> String {
        match check(payload) {
            Err(err) => err,
            Ok(()) => panic!("{name}: payload must be refused"),
        }
    }

    /// `payload` is refused and the refusal names `needle`.
    fn refused(name: &str, payload: Value, needle: &str) {
        let err = err_of(name, payload);
        assert!(
            err.contains(needle),
            "{name} — expected `{needle}` in: {err}"
        );
    }

    #[test]
    fn chart_series_payload_valid_and_invalid() {
        assert_eq!(
            check(json!({ "source": SOURCE, "series": ["US:NVDA"] })),
            Ok(()),
            "minimal payload"
        );
        assert_eq!(
            check(json!({
                "source": SOURCE,
                "series": ["US:NVDA", "HK:9988", "HK:09988", "CRYPTO:BTC-USDT"],
                "field": "close",
                "range": "6M",
                "period": "week",
                "view": "normalized",
                "as_of": "2026-09-10",
                "caption": "Big tech vs BTC"
            })),
            Ok(()),
            "every field; HK:9988 and HK:09988 are two literal entries"
        );
        assert_eq!(
            check(json!({
                "source": SOURCE, "series": ["US:NVDA"], "view": "candles", "overlays": ["ma20", "ma60"]
            })),
            Ok(()),
            "candles with overlays and one series"
        );
        assert_eq!(
            check(json!({ "source": SOURCE, "series": ["US:NVDA"], "overlays": ["ma60"] })),
            Ok(()),
            "overlays under the default (line) view"
        );

        refused(
            "series: required",
            json!({ "source": SOURCE }),
            "series: required",
        );
        refused(
            "source: required",
            json!({ "series": ["US:NVDA"] }),
            "source: required",
        );
        refused(
            "no venue",
            json!({ "source": SOURCE, "series": ["NVDA"] }),
            "series[0]: must be a venue-qualified",
        );
        refused(
            "lowercase venue",
            json!({ "source": SOURCE, "series": ["us:NVDA"] }),
            "series[0]: must be a venue-qualified",
        );
        let nine: Vec<String> = (0..9).map(|i| format!("US:A{i}")).collect();
        refused(
            "nine series",
            json!({ "source": SOURCE, "series": nine }),
            "series: too many assets (9), limit is 8",
        );
        refused(
            "empty series",
            json!({ "source": SOURCE, "series": [] }),
            "series: at least 1 asset",
        );
        refused(
            "duplicate",
            json!({ "source": SOURCE, "series": ["US:NVDA", "HK:9988", "US:NVDA"] }),
            "series[2]: duplicate of series[0]",
        );
        refused(
            "non-string entry",
            json!({ "source": SOURCE, "series": ["US:NVDA", 7] }),
            "series[1]: must be a string",
        );
        refused(
            "source shape",
            json!({ "source": "neige://plugin/only-one-segment", "series": ["US:NVDA"] }),
            "source: expected",
        );
        refused(
            "unknown field",
            json!({ "source": SOURCE, "series": ["US:NVDA"], "points": [] }),
            "points: unknown field",
        );
        refused(
            "field + candles",
            json!({ "source": SOURCE, "series": ["US:NVDA"], "view": "candles", "field": "close" }),
            "field: must be absent when view is \"candles\"",
        );
        refused(
            "candles + 2 series",
            json!({ "source": SOURCE, "series": ["US:NVDA", "HK:9988"], "view": "candles" }),
            "\"candles\" requires exactly 1 series, got 2",
        );
        refused(
            "overlays + bar",
            json!({ "source": SOURCE, "series": ["US:NVDA"], "view": "bar", "overlays": ["ma20"] }),
            "overlays: apply only to view line|candles",
        );
        refused(
            "bad overlay",
            json!({ "source": SOURCE, "series": ["US:NVDA"], "overlays": ["ma50"] }),
            "overlays[0]: must be \"ma20\" | \"ma60\"",
        );
        for (label, value) in [
            ("field", json!("vwap")),
            ("range", json!("10Y")),
            ("period", json!("hour")),
            ("view", json!("area")),
        ] {
            refused(
                &format!("{label} enum"),
                json!({ "source": SOURCE, "series": ["US:NVDA"], label: value }),
                &format!("{label}: must be one of"),
            );
        }

        for bad in ["2026-02-30", "2027-02-29", "2026/09/10"] {
            refused(
                &format!("as_of {bad}"),
                json!({ "source": SOURCE, "series": ["US:NVDA"], "as_of": bad }),
                "as_of: must be a calendar date",
            );
        }
        for good in ["2028-02-29", "2099-01-01"] {
            assert_eq!(
                check(json!({ "source": SOURCE, "series": ["US:NVDA"], "as_of": good })),
                Ok(()),
                "as_of {good} is a legal cutoff (no upper bound: this crate has no clock)"
            );
        }

        refused(
            "1M+month",
            json!({ "source": SOURCE, "series": ["US:NVDA"], "range": "1M", "period": "month" }),
            "range 1M cannot hold two complete month periods",
        );
        assert_eq!(
            check(
                json!({ "source": SOURCE, "series": ["US:NVDA"], "range": "1M", "period": "week" })
            ),
            Ok(()),
            "1M+week"
        );
        assert_eq!(
            check(
                json!({ "source": SOURCE, "series": ["US:NVDA"], "range": "3M", "period": "month" })
            ),
            Ok(()),
            "3M+month"
        );

        let long = "x".repeat(2049);
        refused(
            "caption cap",
            json!({ "source": SOURCE, "series": ["US:NVDA"], "caption": long }),
            "caption: string too long",
        );

        assert!(check(json!([1])).is_err(), "non-object payload");
    }

    #[test]
    fn range_days_table() {
        assert_eq!(chart_series_range_days("1M"), Some(31));
        assert_eq!(chart_series_range_days("3M"), Some(92));
        assert_eq!(chart_series_range_days("6M"), Some(183));
        assert_eq!(chart_series_range_days("1Y"), Some(366));
        assert_eq!(chart_series_range_days("2Y"), Some(731));
        assert_eq!(chart_series_range_days("5Y"), Some(1827));
        assert_eq!(chart_series_range_days("10Y"), None);
        assert_eq!(RANGE_DAYS.len(), 6);
        assert_eq!(
            RANGE_DAYS.map(|(key, _)| key),
            CHART_SERIES_RANGES,
            "one table, one enum"
        );
    }

    #[test]
    fn calendar_leap_year_boundaries() {
        assert!(!is_leap_year(1900), "1900: divisible by 100, not by 400");
        assert!(is_leap_year(2000), "2000: divisible by 400");
        assert!(is_leap_year(2024));
        assert!(!is_leap_year(2100), "2100: divisible by 100, not by 400");
        assert!(!is_valid_ymd("1900-02-29"));
        assert!(is_valid_ymd("2000-02-29"));
        assert!(is_valid_ymd("2024-02-29"));
        assert!(!is_valid_ymd("2100-02-29"));
        assert!(is_valid_ymd("2026-04-30"));
        assert!(!is_valid_ymd("2026-04-31"));
        assert!(!is_valid_ymd("2026-00-10"));
        assert!(!is_valid_ymd("2026-13-01"));
        assert!(!is_valid_ymd("2026-09-00"));
        assert!(!is_valid_ymd("2026-9-10"));
        assert!(!is_valid_ymd("2026-09-10T00:00:00Z"));
        assert!(
            !is_valid_ymd("２０２６-09-10"),
            "full-width digits are not digits"
        );
        assert_eq!(parse_ymd("2026-09-10"), Some((2026, 9, 10)));
        assert_eq!(days_in_month(2026, 13), None);
    }
}
