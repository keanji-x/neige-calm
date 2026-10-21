//! The calendar refresh: its error mapping (only credential failures are
//! `auth`, and only a variant name is kept), the back-fill of a long gap,
//! and the fallback to a cache that still covers now.

use crate::archive::security_count;
use std::sync::Arc;

use chrono::{Datelike, NaiveDate};
use longbridge::oauth::OAuth;
use research_archive::calendar::TradingCalendar;
use research_archive::calendar::fetch::{RefreshError, TradingDays, refresh_error};
use research_archive::capture;
use research_archive::clock::Clock;
use research_archive::config::Config;
use research_archive::state::{ExitKind, Outcome};
use research_archive::types::UnixMs;

use crate::archive::{Archive, Knobs, t};
use crate::fake::Reply;
use crate::fake_clock::{FakeClock, TimedServer};
use crate::hermetic;
use crate::support::raw;

#[test]
fn only_credential_failures_are_auth_and_only_names_are_kept() {
    use longbridge::httpclient::HttpClientError as E;
    let cases = [
        (
            longbridge::Error::OAuth("CANARY expired".into()),
            true,
            "oauth",
        ),
        (
            longbridge::Error::HttpClient(E::InvalidAccessToken),
            true,
            "http:invalid_access_token",
        ),
        (
            longbridge::Error::HttpClient(E::OAuth("CANARY".into())),
            true,
            "http:oauth",
        ),
        (
            longbridge::Error::HttpClient(E::RequestTimeout),
            false,
            "http:request_timeout",
        ),
        (
            longbridge::Error::UnknownCommand(1),
            false,
            "unknown_command",
        ),
    ];
    for (err, auth, kind) in cases {
        let mapped = refresh_error(err);
        let text = mapped.to_string();
        assert!(!text.contains("CANARY"), "{text}");
        match mapped {
            RefreshError::Auth(k) => assert!(auth && k == kind, "{k}"),
            RefreshError::Failed(k) => assert!(!auth && k == kind, "{k}"),
        }
    }
}

/// A trading-days source that answers every weekday, records the ranges it
/// was asked for, and fails every call when `fail`, or the calls whose
/// index (from 0) is in `fail_calls`; a call in `hang_calls` never answers.
#[derive(Default)]
pub struct WeekdayDays {
    pub asked: std::sync::Mutex<Vec<Vec<(NaiveDate, NaiveDate)>>>,
    pub fail: bool,
    pub fail_calls: Vec<usize>,
    pub hang_calls: Vec<usize>,
}

impl TradingDays for WeekdayDays {
    async fn fetch(
        &self,
        _oauth: &OAuth,
        _config: &Config,
        ranges: &[(NaiveDate, NaiveDate)],
        now: UnixMs,
    ) -> Result<Vec<TradingCalendar>, RefreshError> {
        let call = {
            let mut asked = self.asked.lock().unwrap();
            asked.push(ranges.to_vec());
            asked.len() - 1
        };
        if self.hang_calls.contains(&call) {
            std::future::pending::<()>().await;
        }
        if self.fail || self.fail_calls.contains(&call) {
            return Err(RefreshError::Failed("ws:connect_timeout".into()));
        }
        Ok(ranges
            .iter()
            .map(|&(from, to)| TradingCalendar {
                fetched_at: now.0,
                from,
                to,
                trading_days: from
                    .iter_days()
                    .take_while(|d| *d <= to)
                    .filter(|d| d.weekday().number_from_monday() <= 5)
                    .collect(),
                half_days: vec![],
            })
            .collect())
    }
}

async fn capture_with(a: &Archive, clock: &Arc<FakeClock>, days: &WeekdayDays) -> Outcome {
    capture::run_with(&a.config(), Arc::clone(clock), days, None).await
}

fn day(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

/// After a run, the job is offline 40 days. The next start fetches today's
/// range first, then the history back to the first run, in ranges the SDK
/// accepts, so every trading day in the gap whose window has closed is
/// marked `missed`.
#[test]
fn a_40_day_outage_marks_every_closed_day_missed() {
    hermetic::run(async {
        let clock = FakeClock::at(t("2026-03-02T23:00:00Z"));
        let server = TimedServer::start(clock.clone(), |_, _| Reply::data(r#"{"v":1}"#)).await;
        let a = Archive::new(&server.server.url, &Knobs::default());
        a.token("ap_m_CALENDAR-TOKEN", None, 4_102_444_800);
        let days = WeekdayDays::default();
        assert_eq!(
            capture_with(&a, &clock, &days).await.kind,
            ExitKind::Complete
        );
        assert_eq!(
            days.asked.lock().unwrap()[0],
            [(day("2026-02-25"), day("2026-03-27"))]
        );

        // Monday 12:00 EDT, 42 days later: Friday's window has closed,
        // Monday's has not opened.
        clock.set(t("2026-04-13T16:00:00Z"));
        let outcome = capture_with(&a, &clock, &days).await;
        assert_eq!(outcome.kind, ExitKind::NoWindow, "{outcome:?}");
        let asked = days.asked.lock().unwrap().clone();
        assert_eq!(asked[1], [(day("2026-04-08"), day("2026-05-08"))]);
        assert_eq!(
            asked[2],
            [
                (day("2026-03-02"), day("2026-04-01")),
                (day("2026-04-02"), day("2026-04-07")),
            ]
        );
        let mut conn = raw(&a.data_dir.join("archive.sqlite3"), false).await;
        let missed: Vec<String> = sqlx::query_scalar(
            "SELECT r.as_of FROM run r JOIN run_end e ON e.run_id = r.run_id
             WHERE e.outcome = 'missed' ORDER BY r.as_of",
        )
        .fetch_all(&mut conn)
        .await
        .unwrap();
        let expected: Vec<String> = day("2026-03-03")
            .iter_days()
            .take_while(|d| *d <= day("2026-04-10"))
            .filter(|d| d.weekday().number_from_monday() <= 5)
            .map(|d| d.to_string())
            .collect();
        assert_eq!(expected.len(), 29);
        assert_eq!(missed, expected);
        let cache = TradingCalendar::load(&a.data_dir).unwrap().unwrap();
        assert_eq!(
            (cache.from, cache.to),
            (day("2026-03-02"), day("2026-05-08"))
        );
        assert!(a.last_exit().get("missed_scan_incomplete").is_none());
    })
}

/// The same outage, but the history back-fill fails. Today's range was
/// fetched and saved first, so the start is not blocked: it marks what the
/// calendar covers, records `missed_scan_incomplete`, and the next start
/// retries the back-fill and marks the rest.
#[test]
fn a_failed_backfill_only_warns_and_is_retried() {
    hermetic::run(async {
        let clock = FakeClock::at(t("2026-03-02T23:00:00Z"));
        let server = TimedServer::start(clock.clone(), |_, _| Reply::data(r#"{"v":1}"#)).await;
        let a = Archive::new(&server.server.url, &Knobs::default());
        a.token("ap_m_CALENDAR-TOKEN", None, 4_102_444_800);
        let days = WeekdayDays {
            fail_calls: vec![2],
            ..WeekdayDays::default()
        };
        assert_eq!(
            capture_with(&a, &clock, &days).await.kind,
            ExitKind::Complete
        );

        clock.set(t("2026-04-13T16:00:00Z"));
        let outcome = capture_with(&a, &clock, &days).await;
        assert_eq!(outcome.kind, ExitKind::NoWindow, "{outcome:?}");
        assert!(outcome.missed_scan_incomplete);
        assert_eq!(a.last_exit()["missed_scan_incomplete"], true);
        let cache = TradingCalendar::load(&a.data_dir).unwrap().unwrap();
        assert_eq!(
            (cache.from, cache.to),
            (day("2026-04-08"), day("2026-05-08"))
        );
        assert_eq!(missed(&a).await, ["2026-04-08", "2026-04-09", "2026-04-10"]);

        // The next start, an hour later: the cache is fresh, the back-fill
        // is retried and the rest of the gap is marked.
        clock.set(t("2026-04-13T17:00:00Z"));
        let outcome = capture_with(&a, &clock, &days).await;
        assert_eq!(outcome.kind, ExitKind::NoWindow, "{outcome:?}");
        assert!(!outcome.missed_scan_incomplete);
        assert_eq!(days.asked.lock().unwrap().len(), 4);
        assert_eq!(missed(&a).await.len(), 29);
    })
}

/// The days marked `missed`, ascending.
async fn missed(a: &Archive) -> Vec<String> {
    let mut conn = raw(&a.data_dir.join("archive.sqlite3"), false).await;
    sqlx::query_scalar(
        "SELECT r.as_of FROM run r JOIN run_end e ON e.run_id = r.run_id
         WHERE e.outcome = 'missed' ORDER BY r.as_of",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap()
}

/// A refresh that fails (not for auth) falls back to the day-old cache
/// while it still tells whether a window is open now; without such a cache
/// the start is `FATAL`.
#[test]
fn a_failed_refresh_uses_a_cache_that_still_covers_now() {
    hermetic::run(async {
        let clock = FakeClock::at(t("2026-03-03T23:00:00Z"));
        let server = TimedServer::start(clock.clone(), |_, _| Reply::data(r#"{"v":1}"#)).await;
        let a = Archive::new(&server.server.url, &Knobs::default());
        a.token("ap_m_CALENDAR-TOKEN", None, 4_102_444_800);
        a.calendar(
            UnixMs(clock.now().0 - 2 * 86_400_000),
            "2026-02-26",
            "2026-03-12",
            &["2026-03-02", "2026-03-03", "2026-03-04"],
        );
        let failing = WeekdayDays {
            fail: true,
            ..WeekdayDays::default()
        };
        let outcome = capture_with(&a, &clock, &failing).await;
        assert_eq!(outcome.kind, ExitKind::Complete, "{outcome:?}");
        assert_eq!(failing.asked.lock().unwrap().len(), 1);

        // A cache that ends before now cannot answer: FATAL, no request.
        clock.set(t("2026-03-20T23:00:00Z"));
        let before = server.arrivals().len();
        let outcome = capture_with(&a, &clock, &failing).await;
        assert_eq!(outcome.kind, ExitKind::Fatal, "{outcome:?}");
        assert_eq!(server.arrivals().len(), before);
    })
}

/// Codex: the history back-fill runs only on a start with no window open.
/// The calendar lacks the first run's day and every history request would
/// hang; a start inside a window does not ask for it, captures with full
/// coverage, and records that the missed-day scan is incomplete.
#[test]
fn a_start_in_a_window_never_waits_on_the_history() {
    hermetic::run(async {
        let clock = FakeClock::at(t("2026-03-02T23:00:00Z"));
        let server = TimedServer::start(clock.clone(), |_, _| Reply::data(r#"{"v":1}"#)).await;
        let knobs = Knobs {
            attempt_timeout_minutes: 1,
            ..Knobs::default()
        };
        let a = Archive::new(&server.server.url, &knobs);
        a.token("ap_m_CALENDAR-TOKEN", None, 4_102_444_800);
        let days = WeekdayDays::default();
        assert_eq!(
            capture_with(&a, &clock, &days).await.kind,
            ExitKind::Complete
        );

        // Tuesday's window; the cache now starts after the first run.
        clock.set(t("2026-03-03T23:00:00Z"));
        a.calendar(
            clock.now(),
            "2026-03-03",
            "2026-03-12",
            &["2026-03-03", "2026-03-04", "2026-03-05"],
        );
        let hanging = WeekdayDays {
            hang_calls: vec![0],
            ..WeekdayDays::default()
        };
        let outcome = capture_with(&a, &clock, &hanging).await;
        assert_eq!(outcome.kind, ExitKind::Complete, "{outcome:?}");
        assert!(outcome.missed_scan_incomplete);
        assert!(hanging.asked.lock().unwrap().is_empty());
        let coverage = a
            .store()
            .await
            .coverage(research_archive::types::RunId(2))
            .await
            .unwrap();
        assert_eq!(coverage.len(), security_count() + 6);
        assert!(coverage.iter().all(|p| p.has_ok));
    })
}
