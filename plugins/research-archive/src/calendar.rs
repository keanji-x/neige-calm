//! The US trading calendar and the capture window (#2575, "Model: Window").
//!
//! The job owns a cache file, `<data_dir>/calendar/us.json`, holding the US
//! trading days and half days of a contiguous date range and the wall time
//! it was fetched. The cache is refreshed at most once a day through
//! `QuoteContext::trading_days` ([`fetch`]), which needs the token and the
//! quote WebSocket.
//!
//! **The window of trading day D** runs from D at `window_start` New York
//! local time to the next trading day at 09:30 New York local time, the
//! regular open. Both ends are converted with the `America/New_York` zone,
//! so daylight saving time is handled. The window does not depend on half
//! days; they are kept because the calendar call returns them.

pub mod fetch;

use std::path::{Path, PathBuf};

use chrono::{DateTime, Days, NaiveDate, NaiveTime, TimeZone};
use chrono_tz::America::New_York;
use serde::{Deserialize, Serialize};

use crate::files;
use crate::types::{AsOf, UnixMs, Window};

/// The regular session opens at 09:30 New York.
pub fn regular_open() -> NaiveTime {
    NaiveTime::from_hms_opt(9, 30, 0).expect("a valid time")
}

/// The cache is refreshed when it is at least this old.
pub const REFRESH_AFTER_MS: i64 = 24 * 3_600_000;

/// A refresh asks for the days from this many days before today (New York)…
const REFRESH_BACK_DAYS: u64 = 5;
/// …through this many days after that first day. The SDK asks for the same
/// span itself (`rust/src/quote/core.rs:1174-1176` at v5.2.0).
const REFRESH_SPAN_DAYS: u64 = 30;

/// The cache file, as stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TradingCalendar {
    /// Wall time of the fetch that last wrote the file, Unix ms.
    pub fetched_at: i64,
    /// The first and last day the calendar answers for, inclusive.
    pub from: NaiveDate,
    pub to: NaiveDate,
    /// US trading days in `[from, to]`, ascending.
    pub trading_days: Vec<NaiveDate>,
    /// US half trading days in `[from, to]`, ascending; each is a trading day.
    pub half_days: Vec<NaiveDate>,
}

/// Why a cache file was not usable.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("cannot read the calendar cache {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("the calendar cache {path} is not valid: {reason}")]
    Invalid { path: PathBuf, reason: String },
}

/// The calendar cannot say whether a window is open at a time: the time, or
/// the trading day before or after it, lies outside the cached range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the trading calendar does not cover {0:?}")]
pub struct NotCovered(pub UnixMs);

/// The New York local date of `t`.
pub fn ny_date(t: UnixMs) -> NaiveDate {
    DateTime::from_timestamp_millis(t.0)
        .expect("a stored instant is in chrono's range")
        .with_timezone(&New_York)
        .date_naive()
}

/// The instant of New York local `time` on `day`. A time inside the spring
/// gap (02:00-03:00) does not exist; the window times (09:30 and a
/// `window_start` at or after 16:00) never fall in it.
pub fn ny_instant(day: NaiveDate, time: NaiveTime) -> Option<UnixMs> {
    New_York
        .from_local_datetime(&day.and_time(time))
        .earliest()
        .map(|t| UnixMs(t.timestamp_millis()))
}

impl TradingCalendar {
    /// `<data_dir>/calendar/us.json`.
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("calendar").join("us.json")
    }

    /// The cache, or `None` when the file does not exist.
    pub fn load(data_dir: &Path) -> Result<Option<TradingCalendar>, CacheError> {
        let path = Self::path(data_dir);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(CacheError::Read { path, source }),
        };
        let invalid = |reason: String| CacheError::Invalid {
            path: path.clone(),
            reason,
        };
        let calendar: TradingCalendar =
            serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
        calendar.validate().map_err(invalid)?;
        Ok(Some(calendar))
    }

    /// Write the cache atomically ([`files::replace`]), creating its
    /// directory (mode 0700) when it is missing.
    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        let path = Self::path(data_dir);
        if let Some(dir) = path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        let bytes = serde_json::to_vec_pretty(self).expect("a calendar always serializes");
        files::replace(&path, &bytes)
            .map_err(|e| std::io::Error::new(e.source.kind(), e.to_string()))
    }

    /// Days ascending and unique inside `[from, to]`; half days are trading
    /// days.
    pub fn validate(&self) -> Result<(), String> {
        if self.from > self.to {
            return Err(format!("from {} is after to {}", self.from, self.to));
        }
        for (name, days) in [
            ("trading_days", &self.trading_days),
            ("half_days", &self.half_days),
        ] {
            if days.windows(2).any(|w| w[0] >= w[1]) {
                return Err(format!("{name} is not ascending and unique"));
            }
            if days.iter().any(|d| *d < self.from || *d > self.to) {
                return Err(format!("{name} has a day outside [from, to]"));
            }
        }
        if let Some(d) = self
            .half_days
            .iter()
            .find(|d| self.trading_days.binary_search(d).is_err())
        {
            return Err(format!("half day {d} is not a trading day"));
        }
        Ok(())
    }

    /// Whether the cache is due for its daily refresh at `now`: it was
    /// fetched at least a day ago, or its fetch time is in the future.
    pub fn needs_refresh(&self, now: UnixMs) -> bool {
        now.0 < self.fetched_at || now.0 - self.fetched_at >= REFRESH_AFTER_MS
    }

    /// The `[from, to]` a refresh at `now` asks for.
    pub fn refresh_range(now: UnixMs) -> (NaiveDate, NaiveDate) {
        let from = ny_date(now) - Days::new(REFRESH_BACK_DAYS);
        (from, from + Days::new(REFRESH_SPAN_DAYS))
    }

    /// `fresh` combined with what `old` knows outside `fresh`'s range, when
    /// the two ranges overlap or touch, so the calendar stays contiguous
    /// back to the first refresh. Disjoint ranges keep `fresh` alone.
    pub fn merged(old: Option<TradingCalendar>, fresh: TradingCalendar) -> TradingCalendar {
        let Some(old) = old else { return fresh };
        let touches = old.to + Days::new(1) >= fresh.from && fresh.to + Days::new(1) >= old.from;
        if !touches {
            return fresh;
        }
        let outside = |d: &NaiveDate| *d < fresh.from || *d > fresh.to;
        let mut trading_days: Vec<_> = old.trading_days.into_iter().filter(outside).collect();
        trading_days.extend(fresh.trading_days);
        trading_days.sort();
        let mut half_days: Vec<_> = old.half_days.into_iter().filter(outside).collect();
        half_days.extend(fresh.half_days);
        half_days.sort();
        TradingCalendar {
            fetched_at: fresh.fetched_at,
            from: old.from.min(fresh.from),
            to: old.to.max(fresh.to),
            trading_days,
            half_days,
        }
    }

    /// The window of trading day `day`, when `day` and the trading day after
    /// it are known.
    pub fn window_of(&self, day: NaiveDate, window_start: NaiveTime) -> Option<Window> {
        let at = self.trading_days.binary_search(&day).ok()?;
        let next = *self.trading_days.get(at + 1)?;
        let start = ny_instant(day, window_start)?;
        let end = ny_instant(next, regular_open())?;
        Window::new(start, end).ok()
    }

    /// The window open at `now`, with its trading day, or `None` when no
    /// window is open. `NotCovered` when the cache cannot tell.
    pub fn window_at(
        &self,
        now: UnixMs,
        window_start: NaiveTime,
    ) -> Result<Option<(AsOf, Window)>, NotCovered> {
        let today = ny_date(now);
        if today < self.from || today > self.to {
            return Err(NotCovered(now));
        }
        // The latest trading day whose window has started.
        let day = self
            .trading_days
            .iter()
            .rev()
            .find(|d| ny_instant(**d, window_start).is_some_and(|s| s <= now))
            .ok_or(NotCovered(now))?;
        let window = self.window_of(*day, window_start).ok_or(NotCovered(now))?;
        if !window.contains(now) {
            return Ok(None);
        }
        let as_of = AsOf::new(*day).map_err(|_| NotCovered(now))?;
        Ok(Some((as_of, window)))
    }

    /// Every known trading day whose window has closed by `now`
    /// (`window_end ≤ now`), ascending, with its window.
    pub fn closed_windows(&self, now: UnixMs, window_start: NaiveTime) -> Vec<(AsOf, Window)> {
        self.trading_days
            .iter()
            .filter_map(|d| {
                let window = self.window_of(*d, window_start)?;
                let as_of = AsOf::new(*d).ok()?;
                (window.end() <= now).then_some((as_of, window))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
