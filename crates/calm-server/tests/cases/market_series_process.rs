//! Process-level tests for `market.series` (#1628 S3): the real `market`
//! binary, driven by the fake kernel, against loopback stand-ins for Tencent
//! ifzq and Binance klines.
//!
//! What these pin cannot be pinned from a pure function: the ORDER of the
//! plugin's HTTP requests (probe before window, suffix discovery before the
//! US window), how a window is paged against a source that caps a page, and
//! what the in-memory page cache does across a UTC midnight and across a
//! source that advanced between two calls. The fixtures therefore record
//! every request and can be mutated between calls.
//!
//! Every date below is a real 2026 calendar date: 2026-09-11 is a Friday,
//! 2026-09-13 a Sunday and 2026-09-14 a Monday.

use serde_json::{Value, json};

use crate::support::market_plugin::*;

const FAR_FUTURE_MS: i64 = 9_000_000_000_000;

/// A full request; every test that wants a malformed one starts from this.
fn args(
    series: &[&str],
    fields: &[&str],
    period: &str,
    mode: &str,
    start: &str,
    as_of: &str,
) -> Value {
    json!({
        "series": series,
        "fields": fields,
        "period": period,
        "mode": mode,
        "start": start,
        "as_of": as_of,
        "deadline_ms": FAR_FUTURE_MS,
    })
}

fn frozen_daily_close(series: &[&str], start: &str, as_of: &str) -> Value {
    args(series, &["close"], "day", "frozen", start, as_of)
}

fn entries(reply: &Value) -> Vec<Value> {
    assert_ne!(
        reply.pointer("/result/isError"),
        Some(&json!(true)),
        "{reply:#?}"
    );
    reply
        .pointer("/result/structuredContent/series")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("a series array: {reply:#?}"))
}

fn only(reply: &Value) -> Value {
    let all = entries(reply);
    assert_eq!(all.len(), 1, "{all:#?}");
    all[0].clone()
}

fn status(entry: &Value) -> &str {
    entry["status"].as_str().unwrap_or_default()
}

fn complete_through(entry: &Value) -> &str {
    entry["complete_through"].as_str().unwrap_or_default()
}

fn points(entry: &Value) -> Vec<Vec<f64>> {
    entry["points"]
        .as_array()
        .unwrap_or_else(|| panic!("points: {entry:#?}"))
        .iter()
        .map(|point| {
            point
                .as_array()
                .expect("a point is an array")
                .iter()
                .map(|v| v.as_f64().expect("finite number"))
                .collect()
        })
        .collect()
}

fn point_dates(entry: &Value) -> Vec<String> {
    points(entry)
        .iter()
        .map(|point| date_of_ms(point[0] as i64))
        .collect()
}

fn assert_ok(entry: &Value) {
    assert_eq!(status(entry), "ok", "{entry:#?}");
}

fn assert_unavailable(entry: &Value, reason: &str) {
    assert_eq!(status(entry), "unavailable", "{entry:#?}");
    assert_eq!(entry["reason"].as_str(), Some(reason), "{entry:#?}");
}

fn boot(ifzq: &IfzqServer) -> FakeKernel {
    FakeKernel::boot_series(DEAD_ENDPOINT, &ifzq.endpoint, None)
}

/// The shipped manifest parses through the kernel's validator (a manifest
/// that does not is silently skipped at boot, F14) and exposes the tool as
/// a read-only, open-world one with every request key required.
#[test]
fn shipped_manifest_exposes_market_series_read_only() {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/market/manifest.json"
    ))
    .expect("read the market manifest");
    let manifest = calm_server::plugin_host::Manifest::parse(&raw).expect("manifest validates");
    let tool = manifest
        .exposes_tools
        .iter()
        .find(|tool| tool.name == "market.series")
        .expect("market.series is exposed");
    assert_eq!(tool.kind, None);
    let annotations = tool.annotations.as_ref().expect("annotations");
    assert_eq!(annotations["readOnlyHint"], json!(true));
    assert_eq!(annotations["openWorldHint"], json!(true));
    let schema = tool.input_schema.as_ref().expect("input schema");
    assert_eq!(schema["additionalProperties"], json!(false));
    let mut required: Vec<&str> = schema["required"]
        .as_array()
        .expect("required")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    required.sort_unstable();
    assert_eq!(
        required,
        [
            "as_of",
            "deadline_ms",
            "fields",
            "mode",
            "period",
            "series",
            "start"
        ]
    );
    let config = manifest.config_schema.expect("config schema");
    assert_eq!(
        config["properties"]["tencent_endpoint"]["default"],
        json!("https://web.ifzq.gtimg.cn")
    );
    assert_eq!(
        config["properties"]["debug_clock_ms"]["type"],
        json!("integer")
    );
}

/// `as_of` is a cutoff, not an exclusive bound: with a later bar listed, the
/// bar dated `as_of` itself is in the reply.
#[test]
fn series_as_of_includes_that_days_bar() {
    let ifzq = ifzq_server(IfzqFixture::with_rows(
        "hk00700",
        weekday_bars("2026-06-01", "2026-09-14", 100.0),
    ));
    let mut kernel = boot(&ifzq);
    let reply = kernel.call_tool(
        2,
        "market.series",
        frozen_daily_close(&["HK:700"], "2026-08-01", "2026-09-11"),
        None,
    );
    let entry = only(&reply);
    assert_ok(&entry);
    assert_eq!(complete_through(&entry), "2026-09-14");
    let dates = point_dates(&entry);
    assert_eq!(
        dates.last().map(String::as_str),
        Some("2026-09-11"),
        "{dates:?}"
    );
    assert_eq!(
        dates.first().map(String::as_str),
        Some("2026-08-03"),
        "{dates:?}"
    );
    assert_eq!(entry["currency"], json!("HKD"));
}

/// The window is taken relative to the cutoff, not as "the newest N bars":
/// a cutoff three years back still yields a full window even from a source
/// that caps a windowless request at its newest 640 rows.
#[test]
fn old_as_of_window_is_non_empty() {
    let ifzq = ifzq_server(IfzqFixture::with_rows(
        "sh600519",
        weekday_bars("2019-01-01", "2026-09-11", 1000.0),
    ));
    let mut kernel = boot(&ifzq);
    let reply = kernel.call_tool(
        2,
        "market.series",
        frozen_daily_close(&["SH:600519"], "2022-09-10", "2023-09-11"),
        None,
    );
    let entry = only(&reply);
    assert_ok(&entry);
    assert_eq!(complete_through(&entry), "2026-09-11");
    let dates = point_dates(&entry);
    assert!(dates.len() > 200, "{}", dates.len());
    assert!(
        dates
            .iter()
            .all(|d| d.as_str() >= "2022-09-10" && d.as_str() <= "2023-09-11")
    );
    assert_eq!(dates.last().map(String::as_str), Some("2023-09-11"));
}

/// A 5Y mainland window is longer than ifzq's 640-row page: the plugin pages
/// BACKWARDS by date — each further page ends the day before the earliest
/// row of the previous one — and stitches the pages without duplicates.
#[test]
fn cn_pages_backward_past_the_640_cap() {
    let rows = weekday_bars("2021-01-01", "2026-09-14", 500.0);
    let ifzq = ifzq_server(IfzqFixture::with_rows("sh600519", rows.clone()));
    let mut kernel = boot(&ifzq);
    let reply = kernel.call_tool(
        2,
        "market.series",
        frozen_daily_close(&["SH:600519"], "2021-09-11", "2026-09-11"),
        None,
    );
    let entry = only(&reply);
    assert_ok(&entry);
    let dates = point_dates(&entry);
    let expected: Vec<&str> = rows
        .iter()
        .map(|bar| bar.date.as_str())
        .filter(|d| *d >= "2021-09-11" && *d <= "2026-09-11")
        .collect();
    assert_eq!(
        dates.len(),
        expected.len(),
        "every weekday in the window, once"
    );
    assert_eq!(dates, expected, "ascending, no duplicates, no gaps");

    let windows = ifzq.window_params();
    assert!(windows.len() >= 2, "one page cannot hold 5Y: {windows:?}");
    // Page 1 asked for [start - 14d, as_of]; the source kept its newest 640
    // rows; page 2 must end the day before the earliest of those.
    let first: Vec<&str> = windows[0].split(',').collect();
    assert_eq!(first[2], "2021-08-28");
    assert_eq!(first[3], "2026-09-11");
    let in_first: Vec<&str> = rows
        .iter()
        .map(|bar| bar.date.as_str())
        .filter(|d| *d >= "2021-08-28" && *d <= "2026-09-11")
        .collect();
    let first_page_earliest = in_first[in_first.len() - IFZQ_CN_CAP];
    let day_before = fixture_date(first_page_earliest)
        .pred_opt()
        .unwrap()
        .format("%Y-%m-%d")
        .to_string();
    let second: Vec<&str> = windows[1].split(',').collect();
    assert_eq!(second[2], "2021-08-28");
    assert_eq!(second[3], day_before, "{windows:?}");
}

/// A US window is fetched under the exchange-suffixed code the bare probe
/// revealed; the bare code is never asked for a window (it answers nothing).
#[test]
fn us_window_uses_exchange_suffix_from_probe() {
    let ifzq = ifzq_server(
        IfzqFixture::with_rows("usNVDA.OQ", weekday_bars("2025-01-01", "2026-09-14", 200.0))
            .with_suffix("usNVDA", "NVDA.OQ"),
    );
    let mut kernel = boot(&ifzq);
    let reply = kernel.call_tool(
        2,
        "market.series",
        frozen_daily_close(&["US:NVDA"], "2026-06-01", "2026-09-11"),
        None,
    );
    let entry = only(&reply);
    assert_ok(&entry);
    assert_eq!(entry["currency"], json!("USD"));
    assert_eq!(complete_through(&entry), "2026-09-14");
    let params = ifzq.params();
    assert_eq!(params[0], "usNVDA,day,,,3,qfq", "{params:?}");
    assert_eq!(
        params[1], "usNVDA.OQ,day,2026-05-18,2026-09-11,2000,qfq",
        "{params:?}"
    );
    assert!(
        params
            .iter()
            .all(|p| !(p.starts_with("usNVDA,day,") && !p.starts_with("usNVDA,day,,,"))),
        "a bare US code must never carry a window: {params:?}"
    );
}

/// `complete_through` is the newest DAILY bar the source lists, whatever the
/// period and however the window was filtered — for a weekly request it is
/// later than every point and is not a Monday.
#[test]
fn complete_through_is_unfiltered_latest() {
    let ifzq = ifzq_server(IfzqFixture::with_rows(
        "hk00700",
        weekday_bars("2026-03-02", "2026-09-14", 100.0),
    ));
    let mut kernel = boot(&ifzq);
    let reply = kernel.call_tool(
        2,
        "market.series",
        args(
            &["HK:700"],
            &["close"],
            "week",
            "frozen",
            "2026-04-06",
            "2026-08-31",
        ),
        None,
    );
    let entry = only(&reply);
    assert_ok(&entry);
    assert_eq!(complete_through(&entry), "2026-09-14");
    let dates = point_dates(&entry);
    assert_eq!(dates.last().map(String::as_str), Some("2026-08-24"));
}

/// The probe runs BEFORE the window fetch. The fixture advances a day after
/// the plugin's first request: the cutoff day's bar goes from an intraday
/// value to its close and a later bar appears. The reply is then either
/// "complete through D, D absent" or "complete through D+1, D at its close"
/// — never the intraday value.
#[test]
fn probe_precedes_window_fetch() {
    const V_PARTIAL: f64 = 999.0;
    const V_CLOSE: f64 = 500.0;
    let mut rows = weekday_bars("2026-07-01", "2026-09-11", 100.0);
    rows.last_mut().unwrap().close = V_PARTIAL;
    let mut fixture = IfzqFixture::with_rows("hk00700", rows);
    fixture.after_first = Some(Box::new(|fixture: &mut IfzqFixture| {
        let rows = fixture.rows.get_mut("hk00700").unwrap();
        rows.last_mut().unwrap().close = V_CLOSE;
        rows.extend(weekday_bars("2026-09-14", "2026-09-14", 300.0));
    }));
    let ifzq = ifzq_server(fixture);
    let mut kernel = boot(&ifzq);
    let reply = kernel.call_tool(
        2,
        "market.series",
        frozen_daily_close(&["HK:700"], "2026-08-01", "2026-09-11"),
        None,
    );
    let entry = only(&reply);
    assert_ok(&entry);
    let pts = points(&entry);
    let on_cutoff = pts
        .iter()
        .find(|p| date_of_ms(p[0] as i64) == "2026-09-11")
        .map(|p| p[1]);
    match (complete_through(&entry), on_cutoff) {
        ("2026-09-11", None) => {}
        ("2026-09-14", Some(close)) if close == V_CLOSE => {}
        other => panic!("an intraday value was certified: {other:?}"),
    }
}

/// A probe answering only the 2011 baseline row has shown no recent bar.
#[test]
fn probe_with_only_baseline_row_is_unavailable() {
    let ifzq = ifzq_server(IfzqFixture::default().with_suffix("usNVDA", "NVDA.OQ"));
    let mut kernel = boot(&ifzq);
    let reply = kernel.call_tool(
        2,
        "market.series",
        frozen_daily_close(&["US:NVDA"], "2026-06-01", "2026-09-11"),
        None,
    );
    assert_unavailable(&only(&reply), "probe returned no recent bar");
    assert_eq!(ifzq.params(), vec!["usNVDA,day,,,3,qfq"]);
}

/// A week whose Sunday is after the cutoff is not emitted, however many of
/// its days the source lists.
#[test]
fn aggregated_week_never_partial() {
    let ifzq = ifzq_server(IfzqFixture::with_rows(
        "hk00700",
        weekday_bars("2026-06-01", "2026-09-14", 100.0),
    ));
    let mut kernel = boot(&ifzq);
    let reply = kernel.call_tool(
        2,
        "market.series",
        args(
            &["HK:700"],
            &["close"],
            "week",
            "frozen",
            "2026-06-01",
            "2026-09-09",
        ),
        None,
    );
    let entry = only(&reply);
    assert_ok(&entry);
    assert_eq!(complete_through(&entry), "2026-09-14");
    let dates = point_dates(&entry);
    assert_eq!(dates.first().map(String::as_str), Some("2026-06-01"));
    assert_eq!(dates.last().map(String::as_str), Some("2026-08-31"));
}

/// A period is emitted only once a LATER daily bar proves it closed: the
/// week ending on the cutoff Sunday waits for Monday's bar.
#[test]
fn period_needs_a_later_daily_bar() {
    let ifzq = ifzq_server(IfzqFixture::with_rows(
        "hk00700",
        weekday_bars("2026-06-01", "2026-09-11", 100.0),
    ));
    let mut kernel = boot(&ifzq);
    let request = args(
        &["HK:700"],
        &["close"],
        "week",
        "frozen",
        "2026-06-01",
        "2026-09-13",
    );
    let entry = only(&kernel.call_tool(2, "market.series", request.clone(), None));
    assert_ok(&entry);
    assert_eq!(complete_through(&entry), "2026-09-11");
    assert_eq!(
        point_dates(&entry).last().map(String::as_str),
        Some("2026-08-31")
    );

    let mut rows = weekday_bars("2026-06-01", "2026-09-11", 100.0);
    let friday_close = rows.last().unwrap().close;
    rows.extend(weekday_bars("2026-09-14", "2026-09-14", 900.0));
    ifzq.set_rows("hk00700", rows);
    let entry = only(&kernel.call_tool(3, "market.series", request, None));
    assert_ok(&entry);
    assert_eq!(complete_through(&entry), "2026-09-14");
    let pts = points(&entry);
    let last = pts.last().unwrap();
    assert_eq!(date_of_ms(last[0] as i64), "2026-09-07");
    assert_eq!(last[1], friday_close, "the week closes on Friday's close");
}

/// The cutoff is compared against the period's END: a live weekly request
/// on a Wednesday (cutoff = Tuesday) does not emit the half-built week whose
/// Monday is before the cutoff.
#[test]
fn period_end_not_period_start_is_compared() {
    let ifzq = ifzq_server(IfzqFixture::with_rows(
        "hk00700",
        weekday_bars("2026-06-01", "2026-09-08", 100.0),
    ));
    let mut kernel = boot(&ifzq);
    let reply = kernel.call_tool(
        2,
        "market.series",
        args(
            &["HK:700"],
            &["close"],
            "week",
            "live",
            "2026-06-01",
            "2026-09-08",
        ),
        None,
    );
    let entry = only(&reply);
    assert_ok(&entry);
    assert_eq!(complete_through(&entry), "2026-09-08");
    assert_eq!(
        point_dates(&entry).last().map(String::as_str),
        Some("2026-08-31")
    );
}

/// A source whose newest bar is more than 14 days before the cutoff has a
/// gap at the near end (delisting, a long halt, a truncating source), and the
/// series is refused rather than drawn short.
#[test]
fn truncated_near_end_is_unavailable() {
    let ifzq = ifzq_server(IfzqFixture::with_rows(
        "sh600519",
        weekday_bars("2026-05-01", "2026-08-20", 100.0),
    ));
    let mut kernel = boot(&ifzq);
    let reply = kernel.call_tool(
        2,
        "market.series",
        frozen_daily_close(&["SH:600519"], "2026-06-01", "2026-09-11"),
        None,
    );
    assert_unavailable(&only(&reply), "no data near cutoff");
}

/// The mirror image: the earliest bar more than 14 days after `start`.
#[test]
fn lookback_exceeds_source_depth_is_unavailable() {
    let ifzq = ifzq_server(IfzqFixture::with_rows(
        "sh600519",
        weekday_bars("2026-07-01", "2026-09-14", 100.0),
    ));
    let mut kernel = boot(&ifzq);
    let reply = kernel.call_tool(
        2,
        "market.series",
        frozen_daily_close(&["SH:600519"], "2026-06-01", "2026-09-11"),
        None,
    );
    assert_unavailable(&only(&reply), "lookback exceeds source depth");
}

/// The four-venue fixture for the live/frozen daily cases: every source
/// lists bars through Thursday 2026-09-10 and nothing later.
fn through_thursday() -> (IfzqServer, BinanceServer) {
    let rows = weekday_bars("2026-08-01", "2026-09-10", 100.0);
    let ifzq = ifzq_server(
        IfzqFixture::with_rows("hk00700", rows.clone())
            .add("sh600519", rows.clone())
            .add("sz000001", rows.clone())
            .add("usNVDA.OQ", rows)
            .with_suffix("usNVDA", "NVDA.OQ"),
    );
    let binance = binance_klines_server(BinanceFixture::with_klines(
        "BTCUSDT",
        bars_between("2026-08-01", "2026-09-10", 100.0, false),
    ));
    (ifzq, binance)
}

/// Live daily on HK, SH and SZ includes the cutoff day's bar without a later
/// bar: those sessions close hours before the next UTC day.
#[test]
fn hk_and_cn_live_daily_include_yesterday() {
    let (ifzq, binance) = through_thursday();
    let mut kernel = FakeKernel::boot_series(&binance.endpoint, &ifzq.endpoint, None);
    let reply = kernel.call_tool(
        2,
        "market.series",
        args(
            &["HK:700", "SH:600519", "SZ:000001"],
            &["close"],
            "day",
            "live",
            "2026-08-11",
            "2026-09-10",
        ),
        None,
    );
    for entry in entries(&reply) {
        assert_ok(&entry);
        assert_eq!(complete_through(&entry), "2026-09-10", "{entry:#?}");
        assert_eq!(
            point_dates(&entry).last().map(String::as_str),
            Some("2026-09-10"),
            "{entry:#?}"
        );
    }
}

/// US stays on the strict arm until spike U9 proves neither source folds
/// after-hours trades into the daily bar: the cutoff day's bar waits for a
/// later one.
#[test]
fn us_live_daily_stays_strict_until_u9() {
    let (ifzq, binance) = through_thursday();
    let mut kernel = FakeKernel::boot_series(&binance.endpoint, &ifzq.endpoint, None);
    let reply = kernel.call_tool(
        2,
        "market.series",
        args(
            &["US:NVDA"],
            &["close"],
            "day",
            "live",
            "2026-08-11",
            "2026-09-10",
        ),
        None,
    );
    let entry = only(&reply);
    assert_ok(&entry);
    assert_eq!(complete_through(&entry), "2026-09-10");
    assert_eq!(
        point_dates(&entry).last().map(String::as_str),
        Some("2026-09-09")
    );
}

/// Binance's day closes exactly at the next UTC midnight: no relaxation.
#[test]
fn crypto_live_daily_stays_strict() {
    let (ifzq, binance) = through_thursday();
    let mut kernel = FakeKernel::boot_series(&binance.endpoint, &ifzq.endpoint, None);
    let reply = kernel.call_tool(
        2,
        "market.series",
        args(
            &["CRYPTO:BTC"],
            &["close"],
            "day",
            "live",
            "2026-08-11",
            "2026-09-10",
        ),
        None,
    );
    let entry = only(&reply);
    assert_ok(&entry);
    assert_eq!(entry["currency"], json!("USDT"));
    assert_eq!(complete_through(&entry), "2026-09-10");
    assert_eq!(
        point_dates(&entry).last().map(String::as_str),
        Some("2026-09-09")
    );
}

/// Under `frozen` no venue is relaxed: the same fixture drops the cutoff
/// day's bar for all four.
#[test]
fn frozen_daily_needs_a_later_bar() {
    let (ifzq, binance) = through_thursday();
    let mut kernel = FakeKernel::boot_series(&binance.endpoint, &ifzq.endpoint, None);
    let reply = kernel.call_tool(
        2,
        "market.series",
        frozen_daily_close(
            &["HK:700", "SH:600519", "SZ:000001", "US:NVDA", "CRYPTO:BTC"],
            "2026-08-11",
            "2026-09-10",
        ),
        None,
    );
    let all = entries(&reply);
    assert_eq!(all.len(), 5);
    for entry in all {
        assert_ok(&entry);
        assert_eq!(complete_through(&entry), "2026-09-10", "{entry:#?}");
        assert_eq!(
            point_dates(&entry).last().map(String::as_str),
            Some("2026-09-09"),
            "{entry:#?}"
        );
    }
}

/// Monday 00:30 UTC, live weekly, cutoff = Sunday: the source has not yet
/// published Friday's bar, so last week is a four-day half and is withheld
/// until a later daily bar appears.
#[test]
fn live_week_needs_a_later_daily_bar() {
    let ifzq = ifzq_server(IfzqFixture::with_rows(
        "hk00700",
        weekday_bars("2026-06-01", "2026-09-10", 100.0),
    ));
    let monday_0030 = day_ms("2026-09-14") + 30 * 60 * 1000;
    let mut kernel = FakeKernel::boot_series(DEAD_ENDPOINT, &ifzq.endpoint, Some(monday_0030));
    let request = args(
        &["HK:700"],
        &["close"],
        "week",
        "live",
        "2026-06-01",
        "2026-09-13",
    );
    let entry = only(&kernel.call_tool(2, "market.series", request.clone(), None));
    assert_ok(&entry);
    assert_eq!(complete_through(&entry), "2026-09-10");
    assert_eq!(
        point_dates(&entry).last().map(String::as_str),
        Some("2026-08-31")
    );

    let mut rows = weekday_bars("2026-06-01", "2026-09-11", 100.0);
    let friday_close = rows.last().unwrap().close;
    rows.extend(weekday_bars("2026-09-14", "2026-09-14", 900.0));
    ifzq.set_rows("hk00700", rows);
    let entry = only(&kernel.call_tool(3, "market.series", request, None));
    assert_ok(&entry);
    assert_eq!(complete_through(&entry), "2026-09-14");
    let pts = points(&entry);
    let last = pts.last().unwrap();
    assert_eq!(date_of_ms(last[0] as i64), "2026-09-07");
    assert_eq!(last[1], friday_close);
}

/// A cached page is keyed by the UTC date it was fetched on: the same window
/// asked again after midnight hits the source again.
#[test]
fn cache_page_never_crosses_utc_midnight() {
    let ifzq = ifzq_server(IfzqFixture::with_rows(
        "hk00700",
        weekday_bars("2026-06-01", "2026-09-14", 100.0),
    ));
    let before_midnight = day_ms("2026-09-14") + 23 * 3_600_000;
    let mut kernel = FakeKernel::boot_series(DEAD_ENDPOINT, &ifzq.endpoint, Some(before_midnight));
    let request = frozen_daily_close(&["HK:700"], "2026-08-01", "2026-09-11");
    assert_ok(&only(&kernel.call_tool(
        2,
        "market.series",
        request.clone(),
        None,
    )));
    assert_eq!(ifzq.hits(), 2, "probe + one page");
    assert_ok(&only(&kernel.call_tool(
        3,
        "market.series",
        request.clone(),
        None,
    )));
    assert_eq!(
        ifzq.hits(),
        3,
        "same day, same probe: only the probe goes out"
    );

    let after_midnight = day_ms("2026-09-15") + 30 * 60 * 1000;
    kernel.reinitialize(
        4,
        FakeKernel::series_values(DEAD_ENDPOINT, &ifzq.endpoint, Some(after_midnight)),
    );
    assert_ok(&only(&kernel.call_tool(5, "market.series", request, None)));
    assert_eq!(ifzq.hits(), 5, "past midnight the page is fetched again");
}

/// Same UTC day, source advanced between two calls: the page covering the
/// cutoff is refetched (its observation is behind the new probe) and the
/// reply's `complete_through` moves; pages not covering the cutoff are reused.
#[test]
fn near_end_page_refetched_when_probe_advances() {
    let rows = weekday_bars("2021-01-01", "2026-09-10", 500.0);
    let ifzq = ifzq_server(IfzqFixture::with_rows("sh600519", rows.clone()));
    let clock = day_ms("2026-09-11") + 8 * 3_600_000;
    let mut kernel = FakeKernel::boot_series(DEAD_ENDPOINT, &ifzq.endpoint, Some(clock));
    let request = frozen_daily_close(&["SH:600519"], "2021-09-11", "2026-09-10");
    let entry = only(&kernel.call_tool(2, "market.series", request.clone(), None));
    assert_ok(&entry);
    assert_eq!(complete_through(&entry), "2026-09-10");
    assert_eq!(
        point_dates(&entry).last().map(String::as_str),
        Some("2026-09-09")
    );
    let pages = ifzq.window_params().len();
    assert!(pages >= 2, "5Y of mainland bars needs more than one page");
    assert_eq!(ifzq.hits(), 1 + pages);

    let mut advanced = rows;
    advanced.extend(weekday_bars("2026-09-11", "2026-09-11", 900.0));
    ifzq.set_rows("sh600519", advanced);
    let entry = only(&kernel.call_tool(3, "market.series", request, None));
    assert_ok(&entry);
    assert_eq!(complete_through(&entry), "2026-09-11");
    assert_eq!(
        point_dates(&entry).last().map(String::as_str),
        Some("2026-09-10")
    );
    assert_eq!(
        ifzq.hits(),
        1 + pages + 2,
        "the second call is one probe plus ONE refetched page (the one covering the cutoff)"
    );
    let windows = ifzq.window_params();
    assert_eq!(windows.len(), pages + 1);
    assert_eq!(
        windows[pages], windows[0],
        "the refetched page is the near-end one"
    );
}

/// A request whose deadline has passed is refused without touching the
/// network.
#[test]
fn expired_request_does_not_hit_network() {
    let ifzq = ifzq_server(IfzqFixture::with_rows(
        "hk00700",
        weekday_bars("2026-06-01", "2026-09-14", 100.0),
    ));
    let mut kernel = boot(&ifzq);
    let mut request = frozen_daily_close(&["HK:700"], "2026-08-01", "2026-09-11");
    request["deadline_ms"] = json!(1);
    let reply = kernel.call_tool(2, "market.series", request, None);
    assert_eq!(
        reply.pointer("/result/isError"),
        Some(&json!(true)),
        "{reply:#?}"
    );
    assert!(text_of(&reply).contains("deadline exceeded"), "{reply:#?}");
    assert_eq!(ifzq.hits(), 0);
    assert!(kernel.is_responsive());
}

/// `mode` is never defaulted: a direct call without it is an error before
/// any request goes out.
#[test]
fn missing_mode_is_a_tool_error() {
    let ifzq = ifzq_server(IfzqFixture::with_rows(
        "hk00700",
        weekday_bars("2026-06-01", "2026-09-14", 100.0),
    ));
    let mut kernel = boot(&ifzq);
    let mut request = frozen_daily_close(&["HK:700"], "2026-08-01", "2026-09-11");
    request.as_object_mut().unwrap().remove("mode");
    let reply = kernel.call_tool(2, "market.series", request, None);
    assert_eq!(
        reply.pointer("/result/isError"),
        Some(&json!(true)),
        "{reply:#?}"
    );
    assert!(text_of(&reply).contains("`mode`"), "{reply:#?}");
    assert_eq!(ifzq.hits(), 0);
}

/// Nor is an unknown mode read as anything.
#[test]
fn mode_relaxed_is_a_tool_error() {
    let ifzq = ifzq_server(IfzqFixture::with_rows(
        "hk00700",
        weekday_bars("2026-06-01", "2026-09-14", 100.0),
    ));
    let mut kernel = boot(&ifzq);
    let request = args(
        &["HK:700"],
        &["close"],
        "day",
        "relaxed",
        "2026-08-01",
        "2026-09-11",
    );
    let reply = kernel.call_tool(2, "market.series", request, None);
    assert_eq!(
        reply.pointer("/result/isError"),
        Some(&json!(true)),
        "{reply:#?}"
    );
    assert!(text_of(&reply).contains("`mode`"), "{reply:#?}");
    assert_eq!(ifzq.hits(), 0);
}

/// Every other key is required too, and none of them defaults.
#[test]
fn every_missing_key_is_a_tool_error_without_network() {
    let ifzq = ifzq_server(IfzqFixture::with_rows(
        "hk00700",
        weekday_bars("2026-06-01", "2026-09-14", 100.0),
    ));
    let mut kernel = boot(&ifzq);
    for (id, key) in [
        "series",
        "fields",
        "period",
        "start",
        "as_of",
        "deadline_ms",
    ]
    .into_iter()
    .enumerate()
    {
        let mut request = frozen_daily_close(&["HK:700"], "2026-08-01", "2026-09-11");
        request.as_object_mut().unwrap().remove(key);
        let reply = kernel.call_tool(10 + id as u64, "market.series", request, None);
        assert_eq!(
            reply.pointer("/result/isError"),
            Some(&json!(true)),
            "{key}: {reply:#?}"
        );
        assert!(text_of(&reply).contains(key), "{key}: {reply:#?}");
    }
    assert_eq!(ifzq.hits(), 0);
}

/// `CN:` names no exchange, `XX:` is no venue, and a bare name is a crypto
/// asset Binance does not list: each is `unknown_asset` for that item alone.
#[test]
fn unknown_asset_and_cn_prefix() {
    let ifzq = ifzq_server(IfzqFixture::with_rows(
        "sh600519",
        weekday_bars("2026-06-01", "2026-09-14", 100.0),
    ));
    let binance = binance_klines_server(BinanceFixture::with_klines(
        "BTCUSDT",
        bars_between("2026-06-01", "2026-09-14", 100.0, false),
    ));
    let mut kernel = FakeKernel::boot_series(&binance.endpoint, &ifzq.endpoint, None);
    let reply = kernel.call_tool(
        2,
        "market.series",
        frozen_daily_close(&["CN:600519", "XX:1", "NVDA"], "2026-08-01", "2026-09-11"),
        None,
    );
    let all = entries(&reply);
    assert_eq!(all.len(), 3);
    for (entry, asset) in all.iter().zip(["CN:600519", "XX:1", "NVDA"]) {
        assert_eq!(entry["asset"], json!(asset), "{entry:#?}");
        assert_eq!(status(entry), "unknown_asset", "{entry:#?}");
        assert!(entry["reason"].is_string(), "{entry:#?}");
    }
    assert_eq!(ifzq.hits(), 0, "nothing reached ifzq");
    assert_eq!(
        binance.hits(),
        1,
        "only the bare name was probed, at Binance"
    );
}

/// A window that yields one complete period is not a series.
#[test]
fn fewer_than_two_points_is_unavailable() {
    let ifzq = ifzq_server(IfzqFixture::with_rows(
        "hk00700",
        weekday_bars("2026-07-01", "2026-09-14", 100.0),
    ));
    let mut kernel = boot(&ifzq);
    let reply = kernel.call_tool(
        2,
        "market.series",
        args(
            &["HK:700"],
            &["close"],
            "month",
            "frozen",
            "2026-07-25",
            "2026-09-05",
        ),
        None,
    );
    assert_unavailable(&only(&reply), "no data in range");
}

/// Mainland answers under `qfqday`, Hong Kong under `day`; both carry the
/// source's o,c,h,l,v column order and each value lands in its own field.
#[test]
fn qfqday_and_day_keys_both_parse_with_ochlv_reorder() {
    let sh = weekday_bars("2026-08-01", "2026-09-14", 100.0);
    let hk = weekday_bars("2026-08-01", "2026-09-14", 700.0);
    let ifzq =
        ifzq_server(IfzqFixture::with_rows("sh600519", sh.clone()).add("hk00700", hk.clone()));
    let mut kernel = boot(&ifzq);
    let reply = kernel.call_tool(
        2,
        "market.series",
        args(
            &["SH:600519", "HK:700"],
            &["open", "high", "low", "close", "volume"],
            "day",
            "frozen",
            "2026-08-11",
            "2026-09-11",
        ),
        None,
    );
    let all = entries(&reply);
    for (entry, rows) in all.iter().zip([&sh, &hk]) {
        assert_ok(entry);
        let pts = points(entry);
        let wanted = rows
            .iter()
            .find(|bar| bar.date == "2026-09-01")
            .expect("a fixture bar on 2026-09-01");
        let got = pts
            .iter()
            .find(|p| date_of_ms(p[0] as i64) == "2026-09-01")
            .expect("a point on 2026-09-01");
        assert_eq!(
            &got[1..],
            &[
                wanted.open,
                wanted.high,
                wanted.low,
                wanted.close,
                wanted.volume
            ],
            "{entry:#?}"
        );
    }
}

/// Every `ts_ms` is a UTC midnight, strictly ascending; weekly points fall
/// on Mondays and monthly ones on the first of the month.
#[test]
fn ts_ms_is_utc_midnight_and_ascending() {
    let ifzq = ifzq_server(IfzqFixture::with_rows(
        "hk00700",
        weekday_bars("2025-06-01", "2026-09-14", 100.0),
    ));
    let mut kernel = boot(&ifzq);
    for (id, period) in ["day", "week", "month"].into_iter().enumerate() {
        let reply = kernel.call_tool(
            2 + id as u64,
            "market.series",
            args(
                &["HK:700"],
                &["close"],
                period,
                "frozen",
                "2025-09-01",
                "2026-09-11",
            ),
            None,
        );
        let entry = only(&reply);
        assert_ok(&entry);
        let pts = points(&entry);
        assert!(pts.len() >= 2);
        let mut previous = i64::MIN;
        for point in &pts {
            let ts = point[0] as i64;
            assert_eq!(ts % 86_400_000, 0, "{period}: {ts}");
            assert!(ts > previous, "{period}: {ts} after {previous}");
            previous = ts;
            let date = fixture_date(&date_of_ms(ts));
            match period {
                "week" => assert_eq!(chrono::Datelike::weekday(&date), chrono::Weekday::Mon),
                "month" => assert_eq!(chrono::Datelike::day(&date), 1),
                _ => {}
            }
        }
    }
}

/// One item failing leaves the others answered, in request order, each
/// echoing its request string.
#[test]
fn one_failed_series_does_not_fail_the_others() {
    let ifzq = ifzq_server(
        IfzqFixture::with_rows("usNVDA.OQ", weekday_bars("2026-06-01", "2026-09-14", 200.0))
            .add("hk00700", weekday_bars("2026-06-01", "2026-09-14", 100.0))
            .with_suffix("usNVDA", "NVDA.OQ"),
    );
    let mut kernel = boot(&ifzq);
    let reply = kernel.call_tool(
        2,
        "market.series",
        frozen_daily_close(&["US:NVDA", "XX:1", "HK:700"], "2026-08-01", "2026-09-11"),
        None,
    );
    let all = entries(&reply);
    assert_eq!(all.len(), 3);
    assert_eq!(all[0]["asset"], json!("US:NVDA"));
    assert_ok(&all[0]);
    assert_eq!(all[0]["currency"], json!("USD"));
    assert_eq!(all[1]["asset"], json!("XX:1"));
    assert_eq!(status(&all[1]), "unknown_asset");
    assert_eq!(all[2]["asset"], json!("HK:700"));
    assert_ok(&all[2]);
    assert_eq!(all[2]["currency"], json!("HKD"));
    let text = text_of(&reply);
    assert!(
        text.contains("US:NVDA: ok") && text.contains("XX:1: unknown_asset"),
        "{text}"
    );
}

/// ifzq reports a refused request with `code: 0` and a non-empty `msg`.
#[test]
fn ifzq_msg_error_is_unavailable() {
    let mut fixture =
        IfzqFixture::with_rows("hk00700", weekday_bars("2026-06-01", "2026-09-14", 100.0));
    fixture.refuse_with = Some("param error".into());
    let ifzq = ifzq_server(fixture);
    let mut kernel = boot(&ifzq);
    let reply = kernel.call_tool(
        2,
        "market.series",
        frozen_daily_close(&["HK:700"], "2026-08-01", "2026-09-11"),
        None,
    );
    let entry = only(&reply);
    assert_eq!(status(&entry), "unavailable");
    assert!(
        entry["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("param error"),
        "{entry:#?}"
    );
}
