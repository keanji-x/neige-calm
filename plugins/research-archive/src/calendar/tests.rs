use super::*;

fn day(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn hm(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).unwrap()
}

/// Unix ms of an RFC 3339 instant.
fn t(s: &str) -> UnixMs {
    UnixMs(DateTime::parse_from_rfc3339(s).unwrap().timestamp_millis())
}

fn calendar(from: &str, to: &str, days: &[&str]) -> TradingCalendar {
    TradingCalendar {
        fetched_at: 0,
        from: day(from),
        to: day(to),
        trading_days: days.iter().map(|d| day(d)).collect(),
        half_days: vec![],
    }
}

/// March 2026: DST starts on Sunday the 8th. Thanksgiving week: Thursday
/// the 26th of November is closed, Friday the 27th is a half day.
fn sample() -> TradingCalendar {
    let mut c = calendar(
        "2026-03-04",
        "2026-03-12",
        &["2026-03-05", "2026-03-06", "2026-03-09", "2026-03-10"],
    );
    c.half_days = vec![];
    c
}

#[test]
fn the_window_runs_from_window_start_to_the_next_regular_open_across_dst() {
    let c = sample();
    // Friday 17:30 EST (UTC-5) to Monday 09:30 EDT (UTC-4).
    let w = c.window_of(day("2026-03-06"), hm(17, 30)).unwrap();
    assert_eq!(w.start(), t("2026-03-06T22:30:00Z"));
    assert_eq!(w.end(), t("2026-03-09T13:30:00Z"));
    // Monday 17:30 EDT to Tuesday 09:30 EDT.
    let w = c.window_of(day("2026-03-09"), hm(17, 30)).unwrap();
    assert_eq!(w.start(), t("2026-03-09T21:30:00Z"));
    assert_eq!(w.end(), t("2026-03-10T13:30:00Z"));
    // The last known day has no known next day.
    assert_eq!(c.window_of(day("2026-03-10"), hm(17, 30)), None);
    // Not a trading day.
    assert_eq!(c.window_of(day("2026-03-07"), hm(17, 30)), None);
}

#[test]
fn the_window_at_a_time_is_open_closed_or_not_covered() {
    let c = sample();
    let ws = hm(17, 30);
    let friday = AsOf::new(day("2026-03-06")).unwrap();
    // Inside Friday's window, at its start, and over the weekend.
    for at in [
        "2026-03-06T22:30:00Z",
        "2026-03-07T12:00:00Z",
        "2026-03-09T13:29:59.999Z",
    ] {
        let (as_of, _) = c.window_at(t(at), ws).unwrap().unwrap();
        assert_eq!(as_of, friday, "{at}");
    }
    // Between the open and window_start of a trading day: no window.
    for at in [
        "2026-03-09T13:30:00Z",
        "2026-03-09T21:29:59Z",
        "2026-03-06T22:29:59Z",
    ] {
        assert_eq!(c.window_at(t(at), ws), Ok(None), "{at}");
    }
    // Outside the cached range, or no next trading day known.
    for at in [
        "2026-03-03T23:00:00Z",
        "2026-03-13T23:00:00Z",
        "2026-03-10T22:00:00Z",
    ] {
        assert_eq!(c.window_at(t(at), ws), Err(NotCovered(t(at))), "{at}");
    }
    // Before the first known trading day's window, within the range.
    assert_eq!(
        c.window_at(t("2026-03-04T23:00:00Z"), ws),
        Err(NotCovered(t("2026-03-04T23:00:00Z")))
    );
}

#[test]
fn closed_windows_are_those_ending_by_now() {
    let c = sample();
    let ws = hm(17, 30);
    let days = |at: &str| -> Vec<String> {
        c.closed_windows(t(at), ws)
            .into_iter()
            .map(|(d, _)| d.to_string())
            .collect()
    };
    assert_eq!(days("2026-03-06T14:29:59Z"), Vec::<String>::new());
    assert_eq!(days("2026-03-06T14:30:00Z"), ["2026-03-05"]);
    assert_eq!(days("2026-03-09T13:30:00Z"), ["2026-03-05", "2026-03-06"]);
    assert_eq!(
        days("2026-03-12T00:00:00Z"),
        ["2026-03-05", "2026-03-06", "2026-03-09"]
    );
}

#[test]
fn ny_dates_follow_new_york_midnight() {
    assert_eq!(ny_date(t("2026-11-02T04:59:59Z")), day("2026-11-01"));
    assert_eq!(ny_date(t("2026-11-02T05:00:00Z")), day("2026-11-02"));
    assert_eq!(ny_date(t("2026-07-02T03:59:59Z")), day("2026-07-01"));
    assert_eq!(ny_date(t("2026-07-02T04:00:00Z")), day("2026-07-02"));
}

#[test]
fn refresh_is_daily_and_spans_the_sdk_range() {
    let c = TradingCalendar {
        fetched_at: 1_000,
        ..sample()
    };
    assert!(!c.needs_refresh(UnixMs(1_000)));
    assert!(!c.needs_refresh(UnixMs(1_000 + REFRESH_AFTER_MS - 1)));
    assert!(c.needs_refresh(UnixMs(1_000 + REFRESH_AFTER_MS)));
    assert!(c.needs_refresh(UnixMs(999)));
    let (from, to) = TradingCalendar::refresh_range(t("2026-10-13T02:00:00Z"));
    assert_eq!((from, to), (day("2026-10-07"), day("2026-11-06")));
}

#[test]
fn merging_keeps_a_contiguous_history() {
    let old = calendar("2026-03-01", "2026-03-10", &["2026-03-02", "2026-03-09"]);
    let mut fresh = calendar("2026-03-09", "2026-03-20", &["2026-03-10", "2026-03-19"]);
    fresh.fetched_at = 7;
    fresh.half_days = vec![day("2026-03-19")];
    let merged = TradingCalendar::merged(Some(old.clone()), fresh.clone());
    assert_eq!(merged.from, day("2026-03-01"));
    assert_eq!(merged.to, day("2026-03-20"));
    assert_eq!(merged.fetched_at, 7);
    // 03-09 is inside the fresh range, which no longer lists it.
    assert_eq!(
        merged.trading_days,
        [day("2026-03-02"), day("2026-03-10"), day("2026-03-19")]
    );
    assert_eq!(merged.half_days, [day("2026-03-19")]);
    merged.validate().unwrap();
    // Touching ranges merge; a gap keeps the fresh one alone.
    let touching = calendar("2026-03-21", "2026-03-25", &["2026-03-23"]);
    assert_eq!(
        TradingCalendar::merged(Some(fresh.clone()), touching.clone()).from,
        day("2026-03-09")
    );
    let apart = calendar("2026-03-22", "2026-03-25", &["2026-03-23"]);
    assert_eq!(TradingCalendar::merged(Some(fresh), apart.clone()), apart);
    assert_eq!(TradingCalendar::merged(None, old.clone()), old);
}

#[test]
fn the_cache_round_trips_and_refuses_bad_files() {
    let dir = tempfile::tempdir().unwrap();
    assert!(TradingCalendar::load(dir.path()).unwrap().is_none());
    let mut c = sample();
    c.half_days = vec![day("2026-03-06")];
    c.save(dir.path()).unwrap();
    assert_eq!(TradingCalendar::load(dir.path()).unwrap(), Some(c.clone()));
    for bad in [
        TradingCalendar {
            trading_days: vec![day("2026-03-06"), day("2026-03-05")],
            ..c.clone()
        },
        TradingCalendar {
            trading_days: vec![day("2026-03-20")],
            half_days: vec![],
            ..c.clone()
        },
        TradingCalendar {
            half_days: vec![day("2026-03-07")],
            ..c.clone()
        },
        TradingCalendar {
            from: day("2026-03-13"),
            ..c.clone()
        },
    ] {
        bad.save(dir.path()).unwrap();
        assert!(
            matches!(
                TradingCalendar::load(dir.path()),
                Err(CacheError::Invalid { .. })
            ),
            "{bad:?}"
        );
    }
    std::fs::write(TradingCalendar::path(dir.path()), "{").unwrap();
    assert!(TradingCalendar::load(dir.path()).is_err());
}

#[test]
fn a_refresh_fetches_every_gap_back_to_the_needed_history_in_sdk_spans() {
    let now = t("2026-04-13T16:00:00Z");
    let today = (day("2026-04-08"), day("2026-05-08"));
    // A fresh daily refresh: the cache reaches today's range.
    let cache = calendar("2026-04-01", "2026-05-01", &[]);
    assert_eq!(
        TradingCalendar::ranges_to_fetch(Some(&cache), Some(day("2026-04-02")), now),
        [today]
    );
    // After an outage: the gap after the cache, then today's range.
    let cache = calendar("2026-02-25", "2026-03-27", &[]);
    assert_eq!(
        TradingCalendar::ranges_to_fetch(Some(&cache), Some(day("2026-03-02")), now),
        [(day("2026-03-28"), day("2026-04-07")), today]
    );
    // A first run before the cache, and a gap longer than one span.
    let cache = calendar("2026-03-10", "2026-03-12", &[]);
    assert_eq!(
        TradingCalendar::ranges_to_fetch(Some(&cache), Some(day("2026-03-01")), now),
        [
            (day("2026-03-01"), day("2026-03-09")),
            (day("2026-03-13"), day("2026-04-07")),
            today
        ]
    );
    // No cache: back to the first run, cut into 31-day ranges.
    assert_eq!(
        TradingCalendar::ranges_to_fetch(None, Some(day("2026-01-01")), now),
        [
            (day("2026-01-01"), day("2026-01-31")),
            (day("2026-02-01"), day("2026-03-03")),
            (day("2026-03-04"), day("2026-04-03")),
            (day("2026-04-04"), day("2026-04-07")),
            today
        ]
    );
    // Nothing to back-fill before the first run.
    assert_eq!(TradingCalendar::ranges_to_fetch(None, None, now), [today]);
}

#[test]
fn combined_pieces_make_one_contiguous_calendar() {
    let cache = calendar("2026-02-25", "2026-03-27", &["2026-03-02"]);
    let gap = calendar("2026-03-28", "2026-04-07", &["2026-03-30"]);
    let today = calendar("2026-04-08", "2026-05-08", &["2026-04-08"]);
    let all = TradingCalendar::combined(Some(cache), vec![gap, today]);
    assert_eq!((all.from, all.to), (day("2026-02-25"), day("2026-05-08")));
    assert_eq!(
        all.trading_days,
        [day("2026-03-02"), day("2026-03-30"), day("2026-04-08")]
    );
    all.validate().unwrap();
}
