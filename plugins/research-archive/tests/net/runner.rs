//! The capture runner in process, on a fake clock, against the fake server,
//! under the hermetic harness (`capture::run` builds the `OAuth` client and
//! the source):
//! the window and deadline rules (I1), retries and the attempt cap (I7),
//! the order of membership and fetch, resumption, missed days, and the
//! serialized recorder.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use research_archive::capture;
use research_archive::clock::Clock;
use research_archive::state::{ExitKind, Outcome};
use research_archive::types::{RunId, UnixMs};

use crate::archive::{Archive, Knobs, members, t};
use crate::fake::{Reply, Seen};
use crate::fake_clock::{FakeClock, TimedServer};
use crate::hermetic;
use crate::support::{endpoint, key, raw};

/// The test calendar: Thursday 2026-02-26 to Thursday 2026-03-12.
pub const DAYS: &[&str] = &[
    "2026-02-27",
    "2026-03-02",
    "2026-03-03",
    "2026-03-04",
    "2026-03-05",
    "2026-03-06",
    "2026-03-09",
    "2026-03-10",
];

/// Tuesday 2026-03-03's window: 17:30 EST to Wednesday 09:30 EST.
pub fn tue_start() -> UnixMs {
    t("2026-03-03T22:30:00Z")
}
pub fn tue_end() -> UnixMs {
    t("2026-03-04T14:30:00Z")
}

/// A body for every security endpoint, and `index` for constituents.
pub fn ok_reply(seen: &Seen, index: &[&str]) -> Reply {
    if seen.path == "/v1/quote/index-constituents" {
        Reply::data(&members(index))
    } else {
        Reply::data(r#"{"v":1}"#)
    }
}

/// An archive with a far-future token and a calendar fetched at `now`.
pub fn archive(url: &str, knobs: &Knobs, now: UnixMs) -> Archive {
    let a = Archive::new(url, knobs);
    a.token("ap_m_RUNNER-TOKEN", None, 4_102_444_800);
    a.calendar(now, "2026-02-26", "2026-03-12", DAYS);
    a
}

pub async fn capture(a: &Archive, clock: &Arc<FakeClock>) -> Outcome {
    capture::run(&a.config(), Arc::clone(clock)).await
}

/// `(key, endpoint_id, attempt, fetched_at, status)` of every row, by
/// `fetched_at` then attempt.
pub async fn rows(a: &Archive) -> Vec<(String, String, i64, i64, String)> {
    let mut conn = raw(&a.data_dir.join("archive.sqlite3"), false).await;
    sqlx::query_as(
        "SELECT key, endpoint_id, attempt, fetched_at, status FROM observation
         ORDER BY fetched_at, attempt",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap()
}

pub async fn run_end(a: &Archive, run: i64) -> Option<(String, Option<String>, i64)> {
    let mut conn = raw(&a.data_dir.join("archive.sqlite3"), false).await;
    sqlx::query_as("SELECT outcome, reason, finished_at FROM run_end WHERE run_id = ?")
        .bind(run)
        .fetch_optional(&mut conn)
        .await
        .unwrap()
}

#[test]
fn a_full_run_freezes_first_then_completes_every_pair() {
    hermetic::run(async {
        let clock = FakeClock::at(tue_start());
        clock.advance(60_000);
        let server =
            TimedServer::start(clock.clone(), |s, _| ok_reply(s, &["AAPL.US", "MSFT.US"])).await;
        let knobs = Knobs {
            indexes: vec![".SPX.US"],
            ..Knobs::default()
        };
        let a = archive(&server.server.url, &knobs, clock.now());
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::Complete, "{outcome:?}");
        assert_eq!(outcome.as_of.unwrap().to_string(), "2026-03-03");
        assert_eq!(a.last_exit()["kind"], "complete");
        assert_eq!(a.last_exit()["code"], 0);

        let arrivals = server.arrivals();
        // AAPL.US (config and member), MSFT.US: 6 each; .SPX.US: 1.
        assert_eq!(arrivals.len(), 13);
        assert_eq!(arrivals[0].path, "/v1/quote/index-constituents");
        let store = a.store().await;
        let coverage = store.coverage(RunId(1)).await.unwrap();
        assert_eq!(coverage.len(), 13);
        assert!(coverage.iter().all(|p| p.has_ok && p.attempts == 1));
        // I4: the universe was persisted before any per-key fetch.
        let mut conn = raw(&a.data_dir.join("archive.sqlite3"), false).await;
        let frozen_at: i64 = sqlx::query_scalar("SELECT frozen_at FROM run_membership")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        for (k, e, _, at, _) in rows(&a).await {
            if e != "index_constituents" {
                assert!(at >= frozen_at, "{k} {e} at {at} before the freeze");
            }
        }
        assert_eq!(run_end(&a, 1).await.unwrap().0, "complete");
        // Another start in the same window finds the run ended.
        let again = capture(&a, &clock).await;
        assert_eq!(again.kind, ExitKind::AlreadyEnded);
        assert_eq!(server.arrivals().len(), 13);
    })
}

/// I7: every attempt is a row and retries stay inside the window. A
/// transient failure is retried after backoff; throttles do not count
/// toward the cap of 2; a pair that keeps failing stops at the cap.
#[test]
fn i7_retries_record_every_attempt_and_throttles_do_not_count() {
    hermetic::run(async {
        let clock = FakeClock::at(tue_start());
        clock.advance(60_000);
        let consensus = Arc::new(AtomicUsize::new(0));
        let forecast = Arc::new(AtomicUsize::new(0));
        let (c, f) = (consensus.clone(), forecast.clone());
        let server = TimedServer::start(clock.clone(), move |s, _| match s.path.as_str() {
        "/v1/quote/financial-consensus-detail" => match c.fetch_add(1, Ordering::SeqCst) {
            0 => Reply::new(503, "<html>busy</html>"),
            _ => Reply::data(r#"{"eps":"1.25"}"#),
        },
        "/v1/quote/forecast-eps" => match f.fetch_add(1, Ordering::SeqCst) {
            0..=2 => Reply::new(
                200,
                r#"{"code":429002,"message":"rate limit of 1-second interval has been reached, please retry after: 0.2s"}"#,
            ),
            _ => Reply::data(r#"{"eps":"2"}"#),
        },
        "/v1/quote/institution-ratings/detail" => Reply::new(503, "<html>down</html>"),
        _ => Reply::data(r#"{"v":1}"#),
    })
    .await;
        let knobs = Knobs {
            attempt_cap: 2,
            ..Knobs::default()
        };
        let a = archive(&server.server.url, &knobs, clock.now());
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::CompleteWithFailures, "{outcome:?}");
        assert_eq!(outcome.failure.as_deref(), Some("http_status:503"));
        assert_eq!(a.last_exit()["failure"], "http_status:503");

        let store = a.store().await;
        let statuses = |e: &'static str| {
            let store = store.clone();
            async move {
                store
                    .attempts(RunId(1), &key("AAPL.US"), endpoint(e))
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|(n, at, s)| (n, at, s.as_str()))
                    .collect::<Vec<_>>()
            }
        };
        let rows = statuses("consensus").await;
        assert_eq!(
            rows.iter().map(|r| r.2).collect::<Vec<_>>(),
            ["transient", "ok"]
        );
        // The retry waited out the 1-minute backoff.
        assert!(rows[1].1.0 - rows[0].1.0 >= 60_000, "{rows:?}");
        let read = store
            .read_at(&key("AAPL.US"), endpoint("consensus"), clock.now())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read.body, br#"{"eps":"1.25"}"#);
        assert_eq!(read.fetched_at, rows[1].1);

        let rows = statuses("forecast_eps").await;
        assert_eq!(
            rows.iter().map(|r| r.2).collect::<Vec<_>>(),
            ["throttled", "throttled", "throttled", "ok"]
        );
        // Each throttle waited the 0.2 s the server named.
        assert!(
            rows.windows(2).all(|w| w[1].1.0 - w[0].1.0 >= 200),
            "{rows:?}"
        );

        let rows = statuses("rating_detail").await;
        assert_eq!(
            rows.iter().map(|r| r.2).collect::<Vec<_>>(),
            ["transient", "transient"]
        );
        assert_eq!(run_end(&a, 1).await.unwrap().0, "complete");
    })
}

/// I1: no catalog request outside the window, and no back-fill after it
/// closes. A pair whose retry would start after the window end is left,
/// the run ends `partial` at the window end, and the next start, after the
/// window, finds no window and requests nothing.
#[test]
fn i1_the_window_closing_ends_the_run_partial_and_nothing_is_fetched_after() {
    hermetic::run(async {
        let clock = FakeClock::at(UnixMs(tue_end().0 - 90_000));
        let server = TimedServer::start(clock.clone(), |s, _| match s.path.as_str() {
            "/v1/quote/financial-consensus-detail" => Reply::new(503, "<html>busy</html>"),
            _ => Reply::data(r#"{"v":1}"#),
        })
        .await;
        let a = archive(&server.server.url, &Knobs::default(), clock.now());
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::Partial, "{outcome:?}");
        assert_eq!(outcome.code(), research_archive::exit::PARTIAL);
        assert_eq!(a.last_exit()["kind"], "partial");
        assert_eq!(a.last_exit()["failure"], "http_status:503");
        let (end, reason, finished_at) = run_end(&a, 1).await.unwrap();
        assert_eq!(
            (end.as_str(), reason.as_deref()),
            ("partial", Some("window closed"))
        );
        assert!(finished_at >= tue_end().0);

        let arrivals = server.arrivals();
        let consensus = arrivals
            .iter()
            .filter(|r| r.path == "/v1/quote/financial-consensus-detail")
            .count();
        // At -90 s and, after the 1 min backoff, at -30 s; the next retry (4
        // min later) would start after the window end.
        assert_eq!(consensus, 2, "{arrivals:?}");
        for r in &arrivals {
            assert!(
                r.at < tue_end(),
                "a request at {:?} after the window end",
                r.at
            );
        }
        for (_, _, _, at, _) in rows(&a).await {
            assert!(at < tue_end().0);
        }

        // The next start, an hour after the window: no window, no request.
        clock.set(UnixMs(tue_end().0 + 3_600_000));
        a.calendar(clock.now(), "2026-02-26", "2026-03-12", DAYS);
        let next = capture(&a, &clock).await;
        assert_eq!(next.kind, ExitKind::NoWindow, "{next:?}");
        assert_eq!(next.code(), 0);
        assert_eq!(server.arrivals().len(), arrivals.len());
        assert_eq!(a.last_exit()["kind"], "no_window");
    })
}

/// A response that arrives after the window closed is not recorded: the
/// run stops and ends `partial`.
#[test]
fn a_response_after_the_window_end_is_not_recorded() {
    hermetic::run(async {
        let clock = FakeClock::at(UnixMs(tue_end().0 - 60_000));
        let server = TimedServer::start(clock.clone(), |s, clock| {
            if s.path == "/v1/quote/valuation" {
                clock.set(UnixMs(tue_end().0 + 1));
            }
            Reply::data(r#"{"v":1}"#)
        })
        .await;
        let a = archive(&server.server.url, &Knobs::default(), clock.now());
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::Partial, "{outcome:?}");
        let recorded = rows(&a).await;
        assert!(recorded.iter().all(|r| r.1 != "valuation"), "{recorded:?}");
        assert!(recorded.iter().all(|r| r.3 < tue_end().0));
        assert_eq!(run_end(&a, 1).await.unwrap().0, "partial");
    })
}

/// A request that hangs is dropped at the deadline instead of holding the
/// run past it.
#[test]
fn a_hanging_request_is_dropped_at_the_deadline() {
    hermetic::run(async {
        let clock = FakeClock::at(UnixMs(tue_end().0 - 2_000));
        let server = TimedServer::start(clock.clone(), |s, _| {
            let reply = Reply::data(r#"{"v":1}"#);
            if s.path == "/v1/quote/forecast-eps" {
                reply.after(Duration::from_secs(3_600))
            } else {
                reply
            }
        })
        .await;
        let a = archive(&server.server.url, &Knobs::default(), clock.now());
        let started = Instant::now();
        let outcome = capture(&a, &clock).await;
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(outcome.kind, ExitKind::Partial, "{outcome:?}");
        let recorded = rows(&a).await;
        assert_eq!(recorded.len(), 5, "{recorded:?}");
        assert!(recorded.iter().all(|r| r.1 != "forecast_eps"));
    })
}

/// The attempt timeout leaves the run open while the window is; the next
/// start resumes the same run and completes it.
#[test]
fn the_attempt_timeout_leaves_the_run_open_for_the_next_start() {
    hermetic::run(async {
        let clock = FakeClock::at(tue_start());
        clock.advance(600_000);
        let healthy = Arc::new(AtomicUsize::new(0));
        let h = healthy.clone();
        let server = TimedServer::start(clock.clone(), move |s, _| {
            if s.path == "/v1/quote/valuation" && h.load(Ordering::SeqCst) == 0 {
                Reply::new(503, "<html>busy</html>")
            } else {
                Reply::data(r#"{"v":1}"#)
            }
        })
        .await;
        let knobs = Knobs {
            attempt_cap: 10,
            attempt_timeout_minutes: 5,
            ..Knobs::default()
        };
        let a = archive(&server.server.url, &knobs, clock.now());
        let first = capture(&a, &clock).await;
        assert_eq!(first.kind, ExitKind::Resumable, "{first:?}");
        assert_eq!(first.code(), 0);
        assert!(run_end(&a, 1).await.is_none());
        // At 0 and, after the 1 min backoff, at 1 min; the next (4 min
        // later) would start at the 5-minute deadline.
        let valuation = server
            .arrivals()
            .iter()
            .filter(|r| r.path == "/v1/quote/valuation")
            .count();
        assert_eq!(valuation, 2);

        healthy.store(1, Ordering::SeqCst);
        clock.advance(30 * 60_000);
        let second = capture(&a, &clock).await;
        assert_eq!(second.kind, ExitKind::Complete, "{second:?}");
        let store = a.store().await;
        let attempts = store
            .attempts(RunId(1), &key("AAPL.US"), endpoint("valuation"))
            .await
            .unwrap();
        assert_eq!(attempts.len(), 3);
        // Only valuation was requested again, once its stored backoff had
        // passed.
        assert_eq!(server.arrivals().len(), 6 + 1 + 1);
    })
}

/// Membership retries until the carry deadline: an attempt timeout before
/// it leaves the run unfrozen with no per-key fetch, and a start after it
/// freezes by carrying the previous day's membership.
#[test]
fn membership_waits_for_the_carry_deadline_then_carries() {
    hermetic::run(async {
        let clock = FakeClock::at(t("2026-03-02T22:31:00Z"));
        let broken = Arc::new(AtomicUsize::new(0));
        let b = broken.clone();
        let server = TimedServer::start(clock.clone(), move |s, _| {
            if s.path == "/v1/quote/index-constituents" && b.load(Ordering::SeqCst) == 1 {
                Reply::new(503, "<html>busy</html>")
            } else {
                ok_reply(s, &["AAPL.US", "MSFT.US"])
            }
        })
        .await;
        let knobs = Knobs {
            research: vec![],
            indexes: vec![".SPX.US"],
            attempt_cap: 100,
            ..Knobs::default()
        };
        let a = archive(&server.server.url, &knobs, clock.now());
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::Complete);

        // Tuesday: constituents fail; the 25-minute attempt ends before the
        // 60-minute carry deadline.
        broken.store(1, Ordering::SeqCst);
        clock.set(UnixMs(tue_start().0 + 60_000));
        a.calendar(clock.now(), "2026-02-26", "2026-03-12", DAYS);
        let before = server.arrivals().len();
        let first = capture(&a, &clock).await;
        assert_eq!(first.kind, ExitKind::Resumable, "{first:?}");
        let tuesday: Vec<_> = server.arrivals()[before..].to_vec();
        assert!(tuesday.len() > 1);
        assert!(
            tuesday
                .iter()
                .all(|r| r.path == "/v1/quote/index-constituents")
        );
        let plan = a.store().await.plan(RunId(2)).await.unwrap();
        assert!(!plan.frozen);

        // After the carry deadline: the freeze comes first, carried, then the
        // members are fetched. The constituents pair is still a planned pair,
        // so it is retried (recorded, changing nothing) until the deadline.
        clock.set(UnixMs(tue_start().0 + 61 * 60_000));
        let before = server.arrivals().len();
        let second = capture(&a, &clock).await;
        assert_eq!(second.kind, ExitKind::Resumable, "{second:?}");
        let after: Vec<_> = server.arrivals()[before..].to_vec();
        let securities = after
            .iter()
            .filter(|r| r.path != "/v1/quote/index-constituents")
            .count();
        assert_eq!(securities, 12);
        let coverage = a.store().await.coverage(RunId(2)).await.unwrap();
        assert_eq!(coverage.iter().filter(|p| p.has_ok).count(), 12);
        let mut conn = raw(&a.data_dir.join("archive.sqlite3"), false).await;
        let (mode, source): (String, Option<i64>) =
            sqlx::query_as("SELECT mode, source_run_id FROM run_membership WHERE run_id = 2")
                .fetch_one(&mut conn)
                .await
                .unwrap();
        assert_eq!((mode.as_str(), source), ("carried", Some(1)));
    })
}

/// Gap days: a start outside any window marks `missed` exactly the trading
/// days after the first run whose window has closed and that have no run.
#[test]
fn missed_days_are_marked_after_the_first_run_once_closed() {
    hermetic::run(async {
        // Monday 10:00 EST, before any run: nothing is marked.
        let clock = FakeClock::at(t("2026-03-02T15:00:00Z"));
        let server = TimedServer::start(clock.clone(), |s, _| ok_reply(s, &[])).await;
        let a = archive(&server.server.url, &Knobs::default(), clock.now());
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::NoWindow);
        let mut conn = raw(&a.data_dir.join("archive.sqlite3"), false).await;
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM run")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(runs, 0);

        // Monday's run.
        clock.set(t("2026-03-02T23:00:00Z"));
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::Complete);
        // Friday 10:00 EST: Tuesday-Thursday closed with no run; Friday's
        // window has not opened.
        clock.set(t("2026-03-06T15:00:00Z"));
        a.calendar(clock.now(), "2026-02-26", "2026-03-12", DAYS);
        let requests = server.arrivals().len();
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::NoWindow, "{outcome:?}");
        assert_eq!(server.arrivals().len(), requests);
        let marked: Vec<(String, String)> = sqlx::query_as(
            "SELECT r.as_of, e.outcome FROM run r JOIN run_end e ON e.run_id = r.run_id
         ORDER BY r.as_of",
        )
        .fetch_all(&mut conn)
        .await
        .unwrap();
        let marked: Vec<_> = marked
            .iter()
            .map(|(d, o)| (d.as_str(), o.as_str()))
            .collect();
        assert_eq!(
            marked,
            [
                ("2026-03-02", "complete"),
                ("2026-03-03", "missed"),
                ("2026-03-04", "missed"),
                ("2026-03-05", "missed"),
            ]
        );
        // Marking is idempotent.
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::NoWindow);
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM run")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(runs, 4);
    })
}

/// #2577 item 2: concurrent responses are stamped when recorded, so a slow
/// response recorded after faster ones is not refused as `ClockWentBack`
/// and attempt order is time order. Every arrival moves the fake clock one
/// second, and the first request answers last.
#[test]
fn concurrent_responses_are_recorded_in_time_order() {
    hermetic::run(async {
        let clock = FakeClock::at(tue_start());
        clock.advance(60_000);
        let first = Arc::new(AtomicUsize::new(0));
        let f = first.clone();
        let server = TimedServer::start(clock.clone(), move |_, clock| {
            clock.advance(1_000);
            let reply = Reply::data(r#"{"v":1}"#);
            if f.fetch_add(1, Ordering::SeqCst) == 0 {
                reply.after(Duration::from_millis(400))
            } else {
                reply
            }
        })
        .await;
        let knobs = Knobs {
            research: vec!["AAPL.US", "MSFT.US"],
            ..Knobs::default()
        };
        let a = archive(&server.server.url, &knobs, clock.now());
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::Complete, "{outcome:?}");
        let recorded = rows(&a).await;
        assert_eq!(recorded.len(), 12);
        // The slow first request was stamped when it was recorded, after later
        // requests had arrived, not when it was sent.
        let arrivals = server.arrivals();
        let first = &arrivals[0];
        let endpoint_id = research_archive::Catalog::compiled()
            .endpoints()
            .iter()
            .find(|e| e.path == first.path)
            .unwrap()
            .id;
        let stamped = recorded
            .iter()
            .find(|r| r.0 == first.symbol && r.1 == endpoint_id.as_str())
            .unwrap()
            .3;
        assert!(stamped > arrivals[1].at.0, "{stamped} vs {arrivals:?}");
        assert!(recorded.iter().filter(|r| r.3 < stamped).count() >= 1);
    })
}

/// A run left open whose window has since closed is ended `partial` at the
/// next start, which then exits `PARTIAL` even though no window is open, so
/// the closed window alerts.
#[test]
fn a_dangling_run_is_ended_partial_and_alerts() {
    hermetic::run(async {
        let clock = FakeClock::at(tue_start());
        clock.advance(60_000);
        let server = TimedServer::start(clock.clone(), |s, _| match s.path.as_str() {
            "/v1/quote/valuation" => Reply::new(503, "<html>busy</html>"),
            _ => Reply::data(r#"{"v":1}"#),
        })
        .await;
        let knobs = Knobs {
            attempt_cap: 100,
            attempt_timeout_minutes: 1,
            ..Knobs::default()
        };
        let a = archive(&server.server.url, &knobs, clock.now());
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::Resumable);
        clock.set(UnixMs(tue_end().0 + 60_000));
        a.calendar(clock.now(), "2026-02-26", "2026-03-12", DAYS);
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::Partial, "{outcome:?}");
        assert_eq!(outcome.as_of.unwrap().to_string(), "2026-03-03");
        let (end, reason, _) = run_end(&a, 1).await.unwrap();
        assert_eq!(
            (end.as_str(), reason.as_deref()),
            ("partial", Some("dangling"))
        );
        // The next start has nothing left to report.
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::NoWindow);
    })
}

/// A run that ends `complete` with fewer than half of its planned pairs
/// having an `ok` (here, a token the server rejects inside the envelope on
/// four of six endpoints) exits `DEGRADED`, naming the dominant failure.
/// Exactly half is ordinary loss: `complete_with_failures`, exit 0.
#[test]
fn fewer_than_half_ok_is_degraded_and_half_is_not() {
    hermetic::run(async {
        for (failing, kind, code, failure) in [
            (
                &[
                    "/v1/quote/financial-consensus-detail",
                    "/v1/quote/forecast-eps",
                    "/v1/quote/institution-rating-latest",
                    "/v1/quote/institution-ratings",
                ][..],
                ExitKind::Degraded,
                research_archive::exit::DEGRADED,
                "envelope:401004",
            ),
            (
                &[
                    "/v1/quote/financial-consensus-detail",
                    "/v1/quote/forecast-eps",
                    "/v1/quote/institution-rating-latest",
                ][..],
                ExitKind::CompleteWithFailures,
                0,
                "envelope:401004",
            ),
        ] {
            let clock = FakeClock::at(tue_start());
            clock.advance(60_000);
            let failing: Vec<String> = failing.iter().map(|s| (*s).to_owned()).collect();
            let server = TimedServer::start(clock.clone(), move |s, _| {
                if failing.contains(&s.path) {
                    Reply::new(200, r#"{"code":401004,"message":"token revoked"}"#)
                } else {
                    Reply::data(r#"{"v":1}"#)
                }
            })
            .await;
            let knobs = Knobs {
                attempt_cap: 1,
                ..Knobs::default()
            };
            let a = archive(&server.server.url, &knobs, clock.now());
            let outcome = capture(&a, &clock).await;
            assert_eq!(outcome.kind, kind, "{outcome:?}");
            assert_eq!(outcome.code(), code);
            assert_eq!(outcome.failure.as_deref(), Some(failure));
            assert_eq!(a.last_exit()["kind"], kind.as_str());
            assert_eq!(a.last_exit()["code"], code);
            assert_eq!(run_end(&a, 1).await.unwrap().0, "complete");
        }
    })
}

/// The first `auth_unavailable` stops the run at once: the three sibling
/// requests in flight (slow) are dropped instead of awaited, no further
/// attempt starts, the run stays open, and the start exits 75.
#[test]
fn the_first_auth_failure_stops_every_worker_at_once() {
    hermetic::run(async {
        let clock = FakeClock::at(tue_start());
        clock.advance(60_000);
        let server = TimedServer::start(clock.clone(), |s, _| {
            if s.path == "/v1/quote/financial-consensus-detail" {
                Reply::new(401, "<html>who</html>").after(Duration::from_millis(300))
            } else {
                Reply::data(r#"{"v":1}"#).after(Duration::from_secs(20))
            }
        })
        .await;
        let knobs = Knobs {
            research: vec!["AAPL.US", "MSFT.US"],
            ..Knobs::default()
        };
        let a = archive(&server.server.url, &knobs, clock.now());
        let started = Instant::now();
        let outcome = capture(&a, &clock).await;
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(outcome.kind, ExitKind::AuthUnavailable, "{outcome:?}");
        assert_eq!(outcome.code(), research_archive::exit::AUTH_UNAVAILABLE);
        // The first request and the three started before its answer.
        assert_eq!(server.arrivals().len(), 4, "{:?}", server.arrivals());
        let recorded = rows(&a).await;
        assert_eq!(recorded.len(), 1, "{recorded:?}");
        assert_eq!(recorded[0].4, "auth_unavailable");
        assert!(run_end(&a, 1).await.is_none());
        assert_eq!(a.last_exit()["kind"], "auth_unavailable");
    })
}
