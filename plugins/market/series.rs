//! `market.series` — historical daily, weekly and monthly bars (#1628 S3).
//!
//! This is the resolution backend of a report's `chart.series` block. The
//! kernel derives one request per block and calls this tool in the
//! background; the tool is also visible to agents as
//! `plugin.dev-neige-market_market.series`, and a direct call gets the same
//! contract with **no defaults**: all seven request keys are required, and a
//! missing or malformed one is a `tool_error` before any network request.
//!
//! **Sources.** Tencent's `web.ifzq.gtimg.cn` daily K-line endpoint serves the
//! `US`, `HK`, `SH` and `SZ` venues from one URL (adjusted, `qfq`); Binance
//! klines serve `CRYPTO`. Weekly and monthly bars are aggregated here from the
//! daily ones — the source's own `week`/`month` modes answer only the current
//! bar. There is no fallback source in this slice: an ifzq failure is
//! `unavailable` with its reason (Sina fallback is S3b).
//!
//! **Order of operations per series** (design §2.5 S3 constraint 1, S3.2):
//!
//! 1. **probe** the newest daily bar the source lists (`complete_through`),
//! 2. fetch the window `[start − 14d, as_of]`, paging as the source requires,
//! 3. depth and near-end checks,
//! 4. aggregate and apply the inclusion rule.
//!
//! The probe comes FIRST because it is what certifies a bar as closed: a bar
//! is only emitted under the strict rule when a LATER daily bar was already
//! listed before the window was fetched. Fetching first and probing after
//! would let a still-changing intraday bar be certified by a later probe.
//!
//! **Inclusion** (S3.5, S3.6, spike U9): the one relaxed branch is
//! `mode = live ∧ period = day ∧ venue ∈ {HK, SH, SZ}`, where a bar dated
//! `≤ as_of` (yesterday UTC) has closed hours before the request; see
//! [`venue_relaxes_live_daily`] for why `US` is NOT in that set today. Every
//! other combination — `frozen`, week/month, `CRYPTO`, `US` — requires
//! `period_end < complete_through`.
//!
//! **Cache** (S3.4): fetched pages are kept in memory keyed by source, code,
//! page range and the UTC date they were fetched on, so a page is never reused
//! across a UTC midnight. Each page remembers the probe value observed before
//! it was fetched, and that is the ONLY probe allowed to certify its bars: a
//! period is emitted under the strict rule when its end is before the
//! smallest such value over the pages its bars came from (and before this
//! call's probe). The page covering `as_of` is refetched whenever its
//! observation is behind the current probe, so the bars nearest the cutoff
//! are always certified by the probe just made; older pages are reused and
//! keep certifying their own, far older, bars. The reply's `complete_through`
//! is this call's probe — every emitted period ends before it, because a
//! page's observation never exceeds a later probe of the same source.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use super::{AssetId, Config, Venue, binance_symbol, parse_asset, text_result, tool_error};

/// How far beyond the requested window the source is asked for, on both
/// sides of the depth / near-end checks. Estimate; reviewers may tune it.
pub(super) const SERIES_FETCH_MARGIN_DAYS: i64 = 14;
/// Upper bound on `series` per request — the kernel's `MAX_CHART_SERIES`.
const MAX_SERIES: usize = 8;
/// Hard stop on paging for one series. 5Y of daily bars is about 1260 rows;
/// at 640 per page that is two pages, so eight is a generous ceiling.
const MAX_PAGES: usize = 8;
/// ifzq keeps the NEWEST 640 rows of a window for `sh`/`sz` codes (spike U8);
/// a page this long may have been truncated at its early end.
const IFZQ_PAGE_CAP: usize = 640;
/// The row count asked of ifzq per window request. 3000 answers
/// `param error`; 2000 is accepted, and 5Y is 1827 calendar days.
const IFZQ_ROWS_PER_REQUEST: u32 = 2000;
/// Binance's `limit` ceiling; a page this long may be truncated at its late
/// end (klines are returned from `startTime` forward).
const BINANCE_PAGE_CAP: usize = 1000;
/// Rows asked for by a probe. `us` bare codes answer at most two (a 2011
/// adjustment baseline row plus the newest bar).
const PROBE_ROWS: u32 = 3;
/// A probe row dated earlier than `start − this` is the adjustment baseline
/// row ifzq prepends to `us` answers (2011-06-02), not a recent bar.
const BASELINE_LOOKBACK_DAYS: i64 = 3650;
const MS_PER_DAY: i64 = 86_400_000;
/// ifzq refuses requests without a browser-shaped User-Agent.
const USER_AGENT: &str = "Mozilla/5.0";
/// Ceiling on one response body. 2000 ifzq rows are ~160 KiB; 1000 Binance
/// klines are ~200 KiB.
const MAX_BODY_BYTES: u64 = 4 * 1024 * 1024;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Calendar — pure functions, no clock
// ---------------------------------------------------------------------------

/// Days since 1970-01-01 (UTC). Every date in this module is one of these;
/// `ts_ms = day * MS_PER_DAY` is the UTC midnight the kernel checks for.
type Day = i64;

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Howard Hinnant's `days_from_civil`.
fn days_from_civil(year: i64, month: u32, day: u32) -> Day {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(day: Day) -> (i64, u32, u32) {
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        if m <= 2 { y + 1 } else { y },
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

/// Strict `YYYY-MM-DD`, calendar-checked (leap years, days per month).
fn parse_date(raw: &str) -> Option<Day> {
    let bytes = raw.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        let slice = &raw[range];
        if !slice.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        slice.parse().ok()
    };
    let year = i64::from(digits(0..4)?);
    let month = digits(5..7)?;
    let day = digits(8..10)?;
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

fn format_date(day: Day) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// ISO weekday: Monday = 1 … Sunday = 7. 1970-01-01 was a Thursday.
fn iso_weekday(day: Day) -> i64 {
    (day + 3).rem_euclid(7) + 1
}

/// The Monday of `day`'s ISO week.
fn week_start(day: Day) -> Day {
    day - (iso_weekday(day) - 1)
}

/// The Sunday of `day`'s ISO week.
fn week_end(day: Day) -> Day {
    week_start(day) + 6
}

fn month_start(day: Day) -> Day {
    let (y, m, _) = civil_from_days(day);
    days_from_civil(y, m, 1)
}

fn month_end(day: Day) -> Day {
    let (y, m, _) = civil_from_days(day);
    days_from_civil(y, m, days_in_month(y, m))
}

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Live,
    Frozen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Period {
    Day,
    Week,
    Month,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Open,
    High,
    Low,
    Close,
    Volume,
}

impl Field {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "open" => Some(Self::Open),
            "high" => Some(Self::High),
            "low" => Some(Self::Low),
            "close" => Some(Self::Close),
            "volume" => Some(Self::Volume),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct Request {
    series: Vec<String>,
    fields: Vec<Field>,
    period: Period,
    mode: Mode,
    start: Day,
    as_of: Day,
    deadline_ms: i64,
}

/// Every key is required and checked; nothing is defaulted. The message
/// names the first offending key so a direct caller can fix its call.
fn parse_request(args: &Value) -> Result<Request, String> {
    let Some(obj) = args.as_object() else {
        return Err("arguments must be an object".into());
    };
    let series = match obj.get("series").and_then(Value::as_array) {
        Some(items) if !items.is_empty() && items.len() <= MAX_SERIES => items
            .iter()
            .map(|item| item.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| "`series` must be an array of strings".to_string())?,
        Some(items) => {
            return Err(format!(
                "`series` must name between 1 and {MAX_SERIES} assets, got {}",
                items.len()
            ));
        }
        None => return Err("`series` is required: an array of 1 to 8 asset names".into()),
    };
    let fields = match obj.get("fields").and_then(Value::as_array) {
        Some(items) if !items.is_empty() && items.len() <= 5 => items
            .iter()
            .map(|item| item.as_str().and_then(Field::parse))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                "`fields` may only contain open, high, low, close, volume".to_string()
            })?,
        Some(_) => {
            return Err("`fields` must list 1 to 5 of open, high, low, close, volume".into());
        }
        None => {
            return Err(
                "`fields` is required: an array over open, high, low, close, volume".into(),
            );
        }
    };
    let period = match obj.get("period").and_then(Value::as_str) {
        Some("day") => Period::Day,
        Some("week") => Period::Week,
        Some("month") => Period::Month,
        Some(other) => {
            return Err(format!(
                "`period` must be day, week or month, got `{other}`"
            ));
        }
        None => return Err("`period` is required: day, week or month".into()),
    };
    // No default here on purpose: `live` relaxes the inclusion rule for some
    // venues, and a caller that did not say `live` must not get it.
    let mode = match obj.get("mode").and_then(Value::as_str) {
        Some("live") => Mode::Live,
        Some("frozen") => Mode::Frozen,
        Some(other) => return Err(format!("`mode` must be live or frozen, got `{other}`")),
        None => return Err("`mode` is required: live or frozen (it is never defaulted)".into()),
    };
    let start = match obj.get("start").and_then(Value::as_str) {
        Some(raw) => parse_date(raw)
            .ok_or_else(|| format!("`start` must be a calendar date YYYY-MM-DD, got `{raw}`"))?,
        None => return Err("`start` is required: the window's first day, YYYY-MM-DD".into()),
    };
    let as_of = match obj.get("as_of").and_then(Value::as_str) {
        Some(raw) => parse_date(raw)
            .ok_or_else(|| format!("`as_of` must be a calendar date YYYY-MM-DD, got `{raw}`"))?,
        None => return Err("`as_of` is required: the cutoff day, YYYY-MM-DD".into()),
    };
    if start > as_of {
        return Err("`start` must not be later than `as_of`".into());
    }
    let deadline_ms = match obj.get("deadline_ms") {
        Some(value) => value
            .as_i64()
            .ok_or_else(|| "`deadline_ms` must be an integer (unix milliseconds)".to_string())?,
        None => return Err("`deadline_ms` is required: unix milliseconds".into()),
    };
    Ok(Request {
        series,
        fields,
        period,
        mode,
        start,
        as_of,
        deadline_ms,
    })
}

// ---------------------------------------------------------------------------
// Clock
// ---------------------------------------------------------------------------

/// The plugin's wall clock — frozen at `debug_clock_ms` when that test seam
/// is configured.
fn now_ms(cfg: &Config) -> i64 {
    cfg.debug_clock_ms.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0)
    })
}

// ---------------------------------------------------------------------------
// Bars and sources
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
struct Bar {
    date: Day,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
}

impl Bar {
    fn field(&self, field: Field) -> f64 {
        match field {
            Field::Open => self.open,
            Field::High => self.high,
            Field::Low => self.low,
            Field::Close => self.close,
            Field::Volume => self.volume,
        }
    }

    fn is_finite(&self) -> bool {
        [self.open, self.high, self.low, self.close, self.volume]
            .iter()
            .all(|v| v.is_finite())
    }
}

/// A daily bar together with the probe that may certify it: the newest
/// daily date the source listed BEFORE the page holding this bar was fetched
/// (capped by this call's probe, should a source ever move backwards).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Certified {
    bar: Bar,
    by: Day,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Source {
    Ifzq,
    Binance,
}

/// Which end a source KEEPS when a window holds more rows than one page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Truncation {
    /// ifzq `sh`/`sz`: the newest 640 rows survive (spike U8), so the missing
    /// rows are EARLIER than the page and the next page ends the day before
    /// its earliest row.
    KeepsLatest,
    /// Binance: klines are returned from `startTime` forward up to `limit`,
    /// so the missing rows are LATER and the next page starts the day after
    /// its latest row.
    KeepsEarliest,
}

impl Source {
    fn page_cap(self) -> usize {
        match self {
            Self::Ifzq => IFZQ_PAGE_CAP,
            Self::Binance => BINANCE_PAGE_CAP,
        }
    }

    fn truncation(self) -> Truncation {
        match self {
            Self::Ifzq => Truncation::KeepsLatest,
            Self::Binance => Truncation::KeepsEarliest,
        }
    }
}

/// How one series fails, per item — never the whole request.
#[derive(Debug)]
enum Failure {
    UnknownAsset(String),
    Unavailable(String),
}

/// The next page to ask for after a FULL page spanning `[earliest, latest]`
/// within the requested `[lo, hi]`, or `None` when the window is covered or
/// the source stopped making progress.
fn next_page(
    truncation: Truncation,
    lo: Day,
    hi: Day,
    earliest: Day,
    latest: Day,
) -> Option<(Day, Day)> {
    match truncation {
        Truncation::KeepsLatest if earliest > lo && earliest - 1 < hi => Some((lo, earliest - 1)),
        Truncation::KeepsEarliest if latest < hi && latest + 1 > lo => Some((latest + 1, hi)),
        _ => None,
    }
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Bounded GET with the browser User-Agent ifzq requires. A non-2xx status
/// still yields its body, because Binance spells "unknown symbol" as a 400
/// with a JSON body that says so.
fn http_get(url: &str) -> Result<(u16, String), String> {
    let response = match ureq::get(url)
        .set("User-Agent", USER_AGENT)
        .timeout(HTTP_TIMEOUT)
        .call()
    {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(e) => return Err(format!("GET {url}: {e}")),
    };
    let status = response.status();
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_BODY_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("reading {url}: {e}"))?;
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

/// A source message is quoted back only when it is short plain ASCII; the
/// body is attacker-influenced text that would otherwise ride into a stored
/// reason.
fn quotable(message: &str) -> Option<&str> {
    let ok = message.len() <= 64 && message.bytes().all(|b| b.is_ascii_graphic() || b == b' ');
    ok.then_some(message)
}

/// One ifzq answer: the daily rows and, for a bare `us` code, the
/// exchange-suffixed code the window fetch must use.
#[derive(Debug)]
struct IfzqAnswer {
    bars: Vec<Bar>,
    suffixed: Option<String>,
}

/// `param=<code>,day,<start>,<end>,<n>,qfq`.
fn ifzq_fetch(
    cfg: &Config,
    code: &str,
    window: Option<(Day, Day)>,
    n: u32,
) -> Result<IfzqAnswer, Failure> {
    let (start, end) = match window {
        Some((lo, hi)) => (format_date(lo), format_date(hi)),
        None => (String::new(), String::new()),
    };
    let url = format!(
        "{}/appstock/app/fqkline/get?param={code},day,{start},{end},{n},qfq",
        cfg.tencent_endpoint
    );
    let (status, body) = http_get(&url).map_err(Failure::Unavailable)?;
    if !(200..300).contains(&status) {
        return Err(Failure::Unavailable(format!("GET {url}: HTTP {status}")));
    }
    parse_ifzq(&body, code, &url)
}

/// The parser half of [`ifzq_fetch`]. Rows are
/// `[date, open, close, high, low, volume, …]` — note the o,c,h,l,v order —
/// under `qfqday` (`sh`/`sz`) or `day` (`hk`/`us`); a seventh element may be
/// a dividend note object and is ignored. A row that does not parse is
/// dropped rather than failing the answer.
fn parse_ifzq(body: &str, code: &str, url: &str) -> Result<IfzqAnswer, Failure> {
    let parsed: Value = serde_json::from_str(body)
        .map_err(|e| Failure::Unavailable(format!("{url} returned non-JSON: {e}")))?;
    // `code` is 0 even for a refused request; `msg` is the real signal.
    if let Some(msg) = parsed.get("msg").and_then(Value::as_str)
        && !msg.is_empty()
    {
        return Err(Failure::Unavailable(match quotable(msg) {
            Some(msg) => format!("ifzq refused the request: {msg}"),
            None => "ifzq refused the request".to_string(),
        }));
    }
    let data = parsed.get("data").and_then(Value::as_object);
    let entry = data.and_then(|data| {
        data.get(code).or_else(|| {
            // The source keys the answer by the code it was asked for; a
            // single entry under another spelling is still that answer.
            (data.len() == 1).then(|| data.values().next()).flatten()
        })
    });
    let Some(entry) = entry else {
        return Err(Failure::Unavailable(format!(
            "ifzq listed nothing for `{code}`"
        )));
    };
    let rows = entry
        .get("qfqday")
        .or_else(|| entry.get("day"))
        .and_then(Value::as_array);
    let Some(rows) = rows else {
        return Err(Failure::Unavailable(format!(
            "ifzq answer for `{code}` carried neither `qfqday` nor `day`"
        )));
    };
    let bars = rows
        .iter()
        .filter_map(|row| {
            let row = row.as_array()?;
            if row.len() < 6 {
                return None;
            }
            let bar = Bar {
                date: parse_date(row[0].as_str()?)?,
                open: number(&row[1])?,
                close: number(&row[2])?,
                high: number(&row[3])?,
                low: number(&row[4])?,
                volume: number(&row[5])?,
            };
            bar.is_finite().then_some(bar)
        })
        .collect();
    let suffixed = entry
        .pointer(&format!("/qt/{code}/2"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok(IfzqAnswer { bars, suffixed })
}

/// `/api/v3/klines?symbol=…&interval=1d[&startTime=…&endTime=…]&limit=…`.
fn binance_fetch(
    cfg: &Config,
    symbol: &str,
    window: Option<(Day, Day)>,
    limit: u32,
) -> Result<Vec<Bar>, Failure> {
    let range = match window {
        Some((lo, hi)) => format!(
            "&startTime={}&endTime={}",
            lo * MS_PER_DAY,
            (hi + 1) * MS_PER_DAY - 1
        ),
        None => String::new(),
    };
    let url = format!(
        "{}/api/v3/klines?symbol={symbol}&interval=1d{range}&limit={limit}",
        cfg.binance_endpoint
    );
    let (_, body) = http_get(&url).map_err(Failure::Unavailable)?;
    parse_binance(&body, symbol, &url)
}

/// The parser half of [`binance_fetch`]. A kline is
/// `[openTime, open, high, low, close, volume, closeTime, …]`; its date is
/// the UTC day of `openTime`.
fn parse_binance(body: &str, symbol: &str, url: &str) -> Result<Vec<Bar>, Failure> {
    let parsed: Value = serde_json::from_str(body)
        .map_err(|e| Failure::Unavailable(format!("{url} returned non-JSON: {e}")))?;
    let Some(rows) = parsed.as_array() else {
        // `-1121 Invalid symbol` is "we do not list this" — the asset, not
        // the source, is what is unknown.
        return Err(match parsed.get("code").and_then(Value::as_i64) {
            Some(-1121) => Failure::UnknownAsset(format!("Binance does not list {symbol}")),
            Some(code) => Failure::Unavailable(format!(
                "{symbol}: Binance refused the lookup (code {code})"
            )),
            None => Failure::Unavailable(format!("{symbol}: response carried no klines")),
        });
    };
    Ok(rows
        .iter()
        .filter_map(|row| {
            let row = row.as_array()?;
            if row.len() < 6 {
                return None;
            }
            let bar = Bar {
                date: row[0].as_i64()?.div_euclid(MS_PER_DAY),
                open: number(&row[1])?,
                high: number(&row[2])?,
                low: number(&row[3])?,
                close: number(&row[4])?,
                volume: number(&row[5])?,
            };
            bar.is_finite().then_some(bar)
        })
        .collect())
}

/// Where one asset's bars come from, and how the source spells it.
struct Route {
    source: Source,
    /// The code the probe asks about. For `US` this is the BARE `us<SYM>`,
    /// which is the only spelling that answers the newest bar AND names the
    /// exchange-suffixed code a window fetch needs (spike U8).
    probe_code: String,
    currency: &'static str,
}

fn route(asset: &AssetId) -> Result<Route, Failure> {
    let (source, probe_code, currency) = match asset.venue {
        Venue::Crypto => (Source::Binance, binance_symbol(asset), "USDT"),
        Venue::Us => (Source::Ifzq, format!("us{}", asset.symbol), "USD"),
        Venue::Hk => (Source::Ifzq, format!("hk{:0>5}", asset.symbol), "HKD"),
        Venue::Sh => (Source::Ifzq, format!("sh{}", asset.symbol), "CNY"),
        Venue::Sz => (Source::Ifzq, format!("sz{}", asset.symbol), "CNY"),
        Venue::Cn => {
            return Err(Failure::UnknownAsset(format!(
                "{} names no exchange: record it as SH:{sym} or SZ:{sym}",
                asset.canonical(),
                sym = asset.symbol
            )));
        }
    };
    Ok(Route {
        source,
        probe_code,
        currency,
    })
}

/// What a probe learned: the newest daily bar the source lists, and the code
/// the window fetch must use.
struct Probe {
    complete_through: Day,
    window_code: String,
}

/// Step 1. Always runs, for every mode: `complete_through` is what certifies
/// bars as closed and what the reply reports, and it is a DAILY date whatever
/// `period` is.
fn probe(cfg: &Config, route: &Route, venue: Venue, start: Day) -> Result<Probe, Failure> {
    let (bars, suffixed) = match route.source {
        Source::Ifzq => {
            let answer = ifzq_fetch(cfg, &route.probe_code, None, PROBE_ROWS)?;
            (answer.bars, answer.suffixed)
        }
        Source::Binance => (
            binance_fetch(cfg, &route.probe_code, None, PROBE_ROWS)?,
            None,
        ),
    };
    let baseline_cutoff = start - BASELINE_LOOKBACK_DAYS;
    let complete_through = bars
        .iter()
        .map(|bar| bar.date)
        .filter(|date| *date >= baseline_cutoff)
        .max()
        .ok_or_else(|| Failure::Unavailable("probe returned no recent bar".into()))?;
    let window_code = if venue == Venue::Us {
        // A bare `us` code answers ZERO rows to any request with a window; only
        // the suffixed spelling (`usNVDA.OQ`, `usJPM.N`) has history.
        let Some(suffixed) = suffixed else {
            return Err(Failure::Unavailable("exchange suffix unknown".into()));
        };
        format!("us{suffixed}")
    } else {
        route.probe_code.clone()
    };
    Ok(Probe {
        complete_through,
        window_code,
    })
}

// ---------------------------------------------------------------------------
// Page cache
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PageKey {
    source: Source,
    code: String,
    lo: Day,
    hi: Day,
    /// The UTC date the page was fetched on. Part of the KEY, so a page fetched
    /// at 23:59 is invisible at 00:01: "later time" must never certify
    /// "earlier bytes".
    fetched_on: Day,
}

#[derive(Clone, Debug)]
struct CachedPage {
    bars: Vec<Bar>,
    /// The probe value seen BEFORE this page was fetched — the only probe that
    /// can certify these bars.
    observed_complete_through: Day,
}

static PAGE_CACHE: LazyLock<Mutex<HashMap<PageKey, CachedPage>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn cache_lookup(key: &PageKey) -> Option<CachedPage> {
    PAGE_CACHE
        .lock()
        .ok()
        .and_then(|cache| cache.get(key).cloned())
}

fn cache_store(key: PageKey, page: CachedPage) {
    if let Ok(mut cache) = PAGE_CACHE.lock() {
        // Pages from earlier UTC days can never be hit again.
        let today = key.fetched_on;
        cache.retain(|k, _| k.fetched_on == today);
        cache.insert(key, page);
    }
}

fn fetch_page(
    cfg: &Config,
    source: Source,
    code: &str,
    lo: Day,
    hi: Day,
) -> Result<Vec<Bar>, Failure> {
    match source {
        Source::Ifzq => Ok(ifzq_fetch(cfg, code, Some((lo, hi)), IFZQ_ROWS_PER_REQUEST)?.bars),
        Source::Binance => binance_fetch(cfg, code, Some((lo, hi)), BINANCE_PAGE_CAP as u32),
    }
}

/// The window one series is fetched over.
struct Window<'a> {
    source: Source,
    code: &'a str,
    lo: Day,
    /// Also the cutoff (`as_of`): the page whose `hi` is this one covers it.
    hi: Day,
}

/// Step 2. The window, paged and deduplicated by date. Every bar carries
/// the probe that certifies it: the one made before its page was fetched —
/// this call's for a page fetched now, the stored observation for a reused
/// page (never more than this call's probe).
fn fetch_window(
    cfg: &Config,
    window: &Window<'_>,
    probed: Day,
    today: Day,
) -> Result<Vec<Certified>, Failure> {
    let mut bars: BTreeMap<Day, Certified> = BTreeMap::new();
    let (mut page_lo, mut page_hi) = (window.lo, window.hi);
    for _ in 0..MAX_PAGES {
        let key = PageKey {
            source: window.source,
            code: window.code.to_string(),
            lo: page_lo,
            hi: page_hi,
            fetched_on: today,
        };
        let (page, certified_by) = match cache_lookup(&key) {
            // The page covering the cutoff is reused only when the probe it
            // was fetched under IS this call's probe; otherwise the source
            // has advanced since, and the bars near the cutoff are refetched
            // so the newest ones are certified by the probe just made.
            Some(cached)
                if !(page_hi >= window.hi && cached.observed_complete_through != probed) =>
            {
                (cached.bars, cached.observed_complete_through.min(probed))
            }
            _ => {
                let fetched = fetch_page(cfg, window.source, window.code, page_lo, page_hi)?;
                cache_store(
                    key,
                    CachedPage {
                        bars: fetched.clone(),
                        observed_complete_through: probed,
                    },
                );
                (fetched, probed)
            }
        };
        if page.is_empty() {
            break;
        }
        let earliest = page.iter().map(|b| b.date).min().unwrap_or(page_lo);
        let latest = page.iter().map(|b| b.date).max().unwrap_or(page_hi);
        let full = page.len() >= window.source.page_cap();
        for bar in page {
            bars.entry(bar.date).or_insert(Certified {
                bar,
                by: certified_by,
            });
        }
        if !full {
            break;
        }
        match next_page(
            window.source.truncation(),
            page_lo,
            page_hi,
            earliest,
            latest,
        ) {
            Some((next_lo, next_hi)) => {
                page_lo = next_lo;
                page_hi = next_hi;
            }
            None => break,
        }
    }
    Ok(bars.into_values().collect())
}

// ---------------------------------------------------------------------------
// Aggregation and inclusion
// ---------------------------------------------------------------------------

/// One output bar: a day, an ISO week or a calendar month.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Candle {
    period_start: Day,
    period_end: Day,
    bar: Bar,
    /// The smallest certification over the member bars: a period is proven
    /// closed only by a probe that was made before EVERY one of its bars was
    /// fetched.
    certified_by: Day,
}

fn period_bounds(period: Period, day: Day) -> (Day, Day) {
    match period {
        Period::Day => (day, day),
        Period::Week => (week_start(day), week_end(day)),
        Period::Month => (month_start(day), month_end(day)),
    }
}

/// Aggregate ascending daily bars: open of the first day, close of the last,
/// max high, min low, summed volume.
fn aggregate(bars: &[Certified], period: Period) -> Vec<Candle> {
    let mut out: Vec<Candle> = Vec::new();
    for Certified { bar, by } in bars {
        let (period_start, period_end) = period_bounds(period, bar.date);
        match out.last_mut() {
            Some(last) if last.period_start == period_start => {
                last.bar.high = last.bar.high.max(bar.high);
                last.bar.low = last.bar.low.min(bar.low);
                last.bar.close = bar.close;
                last.bar.volume += bar.volume;
                last.certified_by = last.certified_by.min(*by);
            }
            _ => out.push(Candle {
                period_start,
                period_end,
                bar: Bar {
                    date: period_start,
                    ..*bar
                },
                certified_by: *by,
            }),
        }
    }
    out
}

/// **The only venue-level relaxation in this plugin.**
///
/// A `live` DAILY request may include a bar dated `≤ as_of` (yesterday UTC)
/// without a later bar proving it closed, when the venue's regular session
/// for day D ends hours before D+1 00:00 UTC: HK closes 08:00 UTC, SH/SZ
/// 07:00 UTC (design §2.5 closing-time table, margin ≥ 15h).
///
/// `US` is NOT relaxed (spike U9 was not run: it needs two reads of the same
/// ticker during the after-hours session, 20:00–00:00 UTC in EDT, on both
/// ifzq and Sina, compared against the exchange's regular-session volume, to
/// prove neither source folds after-hours trades into the daily bar; until
/// that evidence exists US takes the strict arm, design §9 U9). `CRYPTO` is
/// never relaxed: Binance's day closes exactly at D+1 00:00 UTC, so the
/// margin is zero and relaxation buys nothing but a clock-skew window.
fn venue_relaxes_live_daily(venue: Venue) -> bool {
    match venue {
        Venue::Hk | Venue::Sh | Venue::Sz => true,
        Venue::Us | Venue::Crypto | Venue::Cn => false,
    }
}

/// The inclusion rule's inputs for one series.
#[derive(Clone, Copy, Debug)]
struct Cutoff {
    mode: Mode,
    period: Period,
    venue: Venue,
    start: Day,
    as_of: Day,
}

impl Cutoff {
    /// The inclusion rule, by `(mode, period, venue)`. The cutoff is compared
    /// against the PERIOD END (a week's Sunday, a month's last day), never
    /// the stored `ts_ms`, so a half-built week is never "before the cutoff".
    /// `complete_through` is the newest daily date the probe certifying this
    /// period saw; the strict arm needs the period to end before it.
    fn includes(&self, period_start: Day, period_end: Day, complete_through: Day) -> bool {
        if self.mode == Mode::Live
            && self.period == Period::Day
            && venue_relaxes_live_daily(self.venue)
        {
            return self.start <= period_end && period_end <= self.as_of;
        }
        period_start >= self.start && period_end <= self.as_of && period_end < complete_through
    }
}

// ---------------------------------------------------------------------------
// One series, end to end
// ---------------------------------------------------------------------------

struct Resolved {
    currency: &'static str,
    complete_through: Day,
    points: Vec<Value>,
}

fn resolve_one(cfg: &Config, req: &Request, raw: &str) -> Result<Resolved, Failure> {
    let asset = parse_asset(raw).ok_or_else(|| {
        Failure::UnknownAsset(
            "not a venue-qualified asset name (CRYPTO, US, HK, SH or SZ)".to_string(),
        )
    })?;
    let route = route(&asset)?;
    let today = now_ms(cfg).div_euclid(MS_PER_DAY);

    // 1. probe — before any window request.
    let probe = probe(cfg, &route, asset.venue, req.start)?;

    // 2. window, with margin, paged.
    let lo = req.start - SERIES_FETCH_MARGIN_DAYS;
    let window = Window {
        source: route.source,
        code: &probe.window_code,
        lo,
        hi: req.as_of,
    };
    let complete_through = probe.complete_through;
    let bars: Vec<Certified> = fetch_window(cfg, &window, complete_through, today)?
        .into_iter()
        .filter(|c| c.bar.date >= lo && c.bar.date <= req.as_of)
        .collect();

    // 4. depth and near-end checks, before anything is aggregated.
    if let Some(earliest) = bars.first()
        && earliest.bar.date > req.start + SERIES_FETCH_MARGIN_DAYS
    {
        return Err(Failure::Unavailable("lookback exceeds source depth".into()));
    }
    let in_window: Vec<Certified> = bars
        .into_iter()
        .filter(|c| c.bar.date >= req.start)
        .collect();
    let latest_in_window = in_window.last().map(|c| c.bar.date);
    if latest_in_window.is_none_or(|latest| latest < req.as_of - SERIES_FETCH_MARGIN_DAYS) {
        return Err(Failure::Unavailable("no data near cutoff".into()));
    }

    // 3. aggregate and include.
    let cutoff = Cutoff {
        mode: req.mode,
        period: req.period,
        venue: asset.venue,
        start: req.start,
        as_of: req.as_of,
    };
    let points: Vec<Value> = aggregate(&in_window, req.period)
        .into_iter()
        .filter(|candle| {
            cutoff.includes(candle.period_start, candle.period_end, candle.certified_by)
        })
        .map(|candle| {
            let mut point = vec![json!(candle.period_start * MS_PER_DAY)];
            point.extend(req.fields.iter().map(|f| json!(candle.bar.field(*f))));
            Value::Array(point)
        })
        .collect();
    if points.len() < 2 {
        return Err(Failure::Unavailable("no data in range".into()));
    }
    Ok(Resolved {
        currency: route.currency,
        complete_through,
        points,
    })
}

/// The `market.series` tool. No Track is needed: the reply depends on the
/// request alone.
pub(super) fn handle(cfg: &Config, args: &Value) -> Value {
    let req = match parse_request(args) {
        Ok(req) => req,
        Err(why) => return tool_error(format!("market.series: {why}")),
    };
    // The kernel gave up on this call already (its own timeout is shorter
    // than the queue this request sat in); answering it would only spend
    // network on a reply nobody reads.
    if req.deadline_ms < now_ms(cfg) {
        return tool_error("deadline exceeded");
    }
    let mut entries = Vec::with_capacity(req.series.len());
    let mut summary = Vec::with_capacity(req.series.len());
    for raw in &req.series {
        let entry = match resolve_one(cfg, &req, raw) {
            Ok(resolved) => {
                summary.push(format!(
                    "{raw}: ok, {} points through {} ({})",
                    resolved.points.len(),
                    format_date(resolved.complete_through),
                    resolved.currency,
                ));
                json!({
                    "asset": raw,
                    "currency": resolved.currency,
                    "status": "ok",
                    "complete_through": format_date(resolved.complete_through),
                    "points": resolved.points,
                })
            }
            Err(Failure::UnknownAsset(reason)) => {
                summary.push(format!("{raw}: unknown_asset ({reason})"));
                json!({ "asset": raw, "status": "unknown_asset", "reason": reason })
            }
            Err(Failure::Unavailable(reason)) => {
                summary.push(format!("{raw}: unavailable ({reason})"));
                json!({ "asset": raw, "status": "unavailable", "reason": reason })
            }
        };
        entries.push(entry);
    }
    text_result(summary.join("; "), json!({ "series": entries }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(raw: &str) -> Day {
        parse_date(raw).expect(raw)
    }

    #[test]
    fn calendar_accepts_real_dates_and_refuses_the_rest() {
        for ok in [
            "2024-02-29",
            "2000-02-29",
            "2026-01-31",
            "2026-04-30",
            "1970-01-01",
        ] {
            assert!(parse_date(ok).is_some(), "{ok}");
        }
        for bad in [
            "2023-02-29",
            "2100-02-29",
            "2026-04-31",
            "2026-13-01",
            "2026-00-10",
            "2026-06-00",
            "2026/06/01",
            "2026-6-1",
            "20260601",
            "",
            "2026-06-01x",
        ] {
            assert!(parse_date(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn civil_round_trips_and_formats() {
        assert_eq!(d("1970-01-01"), 0);
        assert_eq!(d("1970-01-02"), 1);
        assert_eq!(d("1969-12-31"), -1);
        for raw in ["2026-09-13", "2024-02-29", "1999-12-31", "2100-03-01"] {
            assert_eq!(format_date(d(raw)), raw);
        }
    }

    #[test]
    fn iso_week_and_month_bounds() {
        assert_eq!(iso_weekday(0), 4, "1970-01-01 was a Thursday");
        assert_eq!(iso_weekday(d("2026-09-14")), 1, "Monday");
        assert_eq!(iso_weekday(d("2026-09-13")), 7, "Sunday");
        assert_eq!(week_start(d("2026-09-09")), d("2026-09-07"));
        assert_eq!(week_end(d("2026-09-09")), d("2026-09-13"));
        assert_eq!(week_start(d("2026-09-13")), d("2026-09-07"));
        assert_eq!(week_start(d("2026-09-14")), d("2026-09-14"));
        assert_eq!(month_start(d("2024-02-15")), d("2024-02-01"));
        assert_eq!(month_end(d("2024-02-15")), d("2024-02-29"));
        assert_eq!(month_end(d("2023-02-15")), d("2023-02-28"));
        assert_eq!(month_end(d("2026-12-31")), d("2026-12-31"));
    }

    #[test]
    fn paging_follows_the_end_the_source_drops() {
        let (lo, hi) = (d("2021-06-01"), d("2026-09-11"));
        // ifzq kept the newest rows: the next page ends the day before the
        // earliest one seen.
        assert_eq!(
            next_page(Truncation::KeepsLatest, lo, hi, d("2024-01-22"), hi),
            Some((lo, d("2024-01-21")))
        );
        // Covered down to `lo`: nothing more to ask.
        assert_eq!(next_page(Truncation::KeepsLatest, lo, hi, lo, hi), None);
        // Binance kept the earliest rows: the next page starts the day after
        // the latest one seen.
        assert_eq!(
            next_page(Truncation::KeepsEarliest, lo, hi, lo, d("2024-02-25")),
            Some((d("2024-02-26"), hi))
        );
        assert_eq!(next_page(Truncation::KeepsEarliest, lo, hi, lo, hi), None);
        // A page whose rows fall outside the asked range makes no progress.
        assert_eq!(
            next_page(Truncation::KeepsLatest, lo, hi, hi + 5, hi + 9),
            None
        );
    }

    /// Every `(mode, period, venue)` cell: a bar dated exactly
    /// `complete_through` (no later bar exists) inside `[start, as_of]` is
    /// emitted ONLY by the relaxed live-daily arm, and that arm exists only
    /// for HK, SH and SZ.
    #[test]
    fn inclusion_table_is_relaxed_only_for_live_daily_stock_venues() {
        let start = d("2026-08-01");
        let as_of = d("2026-09-11");
        for mode in [Mode::Live, Mode::Frozen] {
            for period in [Period::Day, Period::Week, Period::Month] {
                for venue in [Venue::Crypto, Venue::Us, Venue::Hk, Venue::Sh, Venue::Sz] {
                    let relaxed = mode == Mode::Live
                        && period == Period::Day
                        && matches!(venue, Venue::Hk | Venue::Sh | Venue::Sz);
                    // The last period ending on or before `as_of`.
                    let (period_start, period_end) = match period {
                        Period::Day => (as_of, as_of),
                        Period::Week => (week_start(as_of) - 7, week_end(as_of) - 7),
                        Period::Month => (d("2026-08-01"), d("2026-08-31")),
                    };
                    let cutoff = Cutoff {
                        mode,
                        period,
                        venue,
                        start,
                        as_of,
                    };
                    // The probe sits exactly at the period end: the strict
                    // `<` fails, so only relaxation could admit the period.
                    assert_eq!(
                        cutoff.includes(period_start, period_end, period_end),
                        relaxed,
                        "({mode:?}, {period:?}, {venue:?})"
                    );
                    // With a later daily bar, every cell admits the period.
                    assert!(
                        cutoff.includes(period_start, period_end, period_end + 1),
                        "({mode:?}, {period:?}, {venue:?}) with a later bar"
                    );
                }
            }
        }
        assert!(!venue_relaxes_live_daily(Venue::Us));
        assert!(!venue_relaxes_live_daily(Venue::Crypto));
        assert!(!venue_relaxes_live_daily(Venue::Cn));
    }

    #[test]
    fn strict_arm_compares_the_period_end_not_its_start() {
        // Wednesday 09-09, live weekly, as_of = Tuesday 09-08: the week that
        // STARTS 09-07 (<= as_of) ENDS 09-13 (> as_of) and must not appear.
        let cutoff = Cutoff {
            mode: Mode::Live,
            period: Period::Week,
            venue: Venue::Hk,
            start: d("2026-01-01"),
            as_of: d("2026-09-08"),
        };
        let seen = d("2026-09-08");
        assert!(!cutoff.includes(d("2026-09-07"), d("2026-09-13"), seen));
        assert!(cutoff.includes(d("2026-08-31"), d("2026-09-06"), seen));
        // A period that started before `start` is out even when it ends inside.
        let cutoff = Cutoff {
            mode: Mode::Frozen,
            period: Period::Week,
            venue: Venue::Hk,
            start: d("2026-09-02"),
            as_of: d("2026-09-30"),
        };
        let seen = d("2026-09-30");
        assert!(!cutoff.includes(d("2026-08-31"), d("2026-09-06"), seen));
        assert!(cutoff.includes(d("2026-09-07"), d("2026-09-13"), seen));
    }

    fn bar(date: &str, o: f64, h: f64, l: f64, c: f64, v: f64) -> Bar {
        Bar {
            date: d(date),
            open: o,
            high: h,
            low: l,
            close: c,
            volume: v,
        }
    }

    #[test]
    fn weekly_and_monthly_aggregation() {
        let by = d("2026-09-15");
        let days = [
            Certified {
                bar: bar("2026-09-07", 10.0, 12.0, 9.0, 11.0, 100.0),
                by,
            },
            Certified {
                bar: bar("2026-09-08", 11.0, 15.0, 10.0, 14.0, 200.0),
                by,
            },
            // Fetched under an earlier probe than its week-mates.
            Certified {
                bar: bar("2026-09-11", 14.0, 14.5, 8.0, 9.0, 50.0),
                by: by - 1,
            },
            Certified {
                bar: bar("2026-09-14", 9.0, 9.5, 8.5, 9.2, 10.0),
                by,
            },
        ];
        let weeks = aggregate(&days, Period::Week);
        assert_eq!(weeks.len(), 2);
        assert_eq!(weeks[0].period_start, d("2026-09-07"));
        assert_eq!(weeks[0].period_end, d("2026-09-13"));
        assert_eq!(weeks[0].bar.open, 10.0);
        assert_eq!(weeks[0].bar.high, 15.0);
        assert_eq!(weeks[0].bar.low, 8.0);
        assert_eq!(weeks[0].bar.close, 9.0);
        assert_eq!(weeks[0].bar.volume, 350.0);
        assert_eq!(
            weeks[0].certified_by,
            by - 1,
            "a period is certified by the earliest probe among its bars"
        );
        assert_eq!(weeks[1].period_start, d("2026-09-14"));
        assert_eq!(weeks[1].certified_by, by);
        let months = aggregate(&days, Period::Month);
        assert_eq!(months.len(), 1);
        assert_eq!(months[0].period_start, d("2026-09-01"));
        assert_eq!(months[0].period_end, d("2026-09-30"));
        assert_eq!(months[0].bar.close, 9.2);
        assert_eq!(months[0].bar.volume, 360.0);
        let daily = aggregate(&days, Period::Day);
        assert_eq!(daily.len(), 4);
        assert_eq!(daily[2].period_start, d("2026-09-11"));
        assert_eq!(daily[2].period_end, d("2026-09-11"));
    }

    fn full_request() -> Value {
        json!({
            "series": ["US:NVDA"],
            "fields": ["close"],
            "period": "day",
            "mode": "frozen",
            "start": "2025-09-09",
            "as_of": "2026-09-10",
            "deadline_ms": 1_789_000_000_000i64,
        })
    }

    #[test]
    fn every_request_key_is_required_and_checked() {
        assert!(parse_request(&full_request()).is_ok());
        for key in [
            "series",
            "fields",
            "period",
            "mode",
            "start",
            "as_of",
            "deadline_ms",
        ] {
            let mut args = full_request();
            args.as_object_mut().unwrap().remove(key);
            let err = parse_request(&args).unwrap_err();
            assert!(err.contains(key), "missing {key}: {err}");
        }
        let bad = [
            ("series", json!([])),
            (
                "series",
                json!(["A", "B", "C", "D", "E", "F", "G", "H", "I"]),
            ),
            ("series", json!([1])),
            ("series", json!("US:NVDA")),
            ("fields", json!([])),
            ("fields", json!(["adj_close"])),
            ("period", json!("hour")),
            ("mode", json!("relaxed")),
            ("start", json!("2026-02-30")),
            ("as_of", json!("2026/09/10")),
            ("deadline_ms", json!(1.5)),
            ("deadline_ms", json!("soon")),
        ];
        for (key, value) in bad {
            let mut args = full_request();
            args[key] = value.clone();
            let err = parse_request(&args).unwrap_err();
            assert!(err.contains(key), "{key} = {value}: {err}");
        }
        let mut args = full_request();
        args["start"] = json!("2026-09-11");
        assert!(parse_request(&args).is_err(), "start after as_of");
    }

    #[test]
    fn ifzq_rows_are_reordered_from_o_c_h_l_v_under_either_key() {
        let body = json!({
            "code": 0, "msg": "", "data": { "sh600519": { "qfqday": [
                ["2026-09-10", "1.0", "4.0", "5.0", "0.5", "100"],
                ["2026-09-11", 2.0, 3.0, 6.0, 1.5, 200, {"FHcontent": "x"}],
                ["bad row"],
            ] } }
        })
        .to_string();
        let answer = parse_ifzq(&body, "sh600519", "u").unwrap();
        assert_eq!(answer.bars.len(), 2);
        assert_eq!(answer.bars[0], bar("2026-09-10", 1.0, 5.0, 0.5, 4.0, 100.0));
        assert_eq!(answer.bars[1], bar("2026-09-11", 2.0, 6.0, 1.5, 3.0, 200.0));
        assert_eq!(answer.suffixed, None);
        let body = json!({
            "code": 0, "msg": "", "data": { "usNVDA": {
                "day": [["2011-06-02", "19.02", "19.02", "19.28", "18.84", "19701450"]],
                "qt": { "usNVDA": ["delay", "name", "NVDA.OQ"] }
            } }
        })
        .to_string();
        let answer = parse_ifzq(&body, "usNVDA", "u").unwrap();
        assert_eq!(answer.bars.len(), 1);
        assert_eq!(answer.suffixed.as_deref(), Some("NVDA.OQ"));
        let refused = json!({ "code": 0, "msg": "param error", "data": [] }).to_string();
        match parse_ifzq(&refused, "sh600519", "u") {
            Err(Failure::Unavailable(reason)) => {
                assert!(reason.contains("param error"), "{reason}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn binance_klines_parse_and_unknown_symbol_is_unknown_asset() {
        let body = json!([
            [
                1_757_721_600_000i64,
                "1.0",
                "3.0",
                "0.5",
                "2.0",
                "10",
                1_757_807_999_999i64,
                "x"
            ],
            [1_757_808_000_000i64, 2.0, 4.0, 1.5, 3.0, 20, 0],
            [1_757_894_400_000i64, "nan", "4.0", "1.5", "3.0", "20"],
        ])
        .to_string();
        let bars = parse_binance(&body, "BTCUSDT", "u").unwrap();
        assert_eq!(bars.len(), 2, "the NaN row is dropped");
        assert_eq!(bars[0], bar("2025-09-13", 1.0, 3.0, 0.5, 2.0, 10.0));
        assert_eq!(bars[1].date, d("2025-09-14"));
        let refused = json!({ "code": -1121, "msg": "Invalid symbol." }).to_string();
        assert!(matches!(
            parse_binance(&refused, "NVDAUSDT", "u"),
            Err(Failure::UnknownAsset(_))
        ));
        let other = json!({ "code": -1003, "msg": "Too many requests." }).to_string();
        assert!(matches!(
            parse_binance(&other, "BTCUSDT", "u"),
            Err(Failure::Unavailable(_))
        ));
    }
}
