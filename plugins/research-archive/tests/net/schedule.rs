//! The retry schedule and the membership freeze rule, on a fake clock:
//! minute-scale backoff carries pairs through an outage, membership waits
//! for an `ok` or the carry deadline (scenarios A, B and C), and no request
//! starts after the deadline even when the rate-limit slot comes late.

use std::sync::Arc;
use std::time::Duration;

use research_archive::clock::Clock;
use research_archive::state::ExitKind;
use research_archive::types::{RunId, UnixMs};

use crate::archive::{Knobs, members};
use crate::fake::Reply;
use crate::fake_clock::{FakeClock, TimedServer};
use crate::hermetic;
use crate::runner::{archive, capture, rows, run_end, tue_end, tue_start};
use crate::support::raw;

const CONSTITUENTS: &str = "/v1/quote/index-constituents";

/// `(index_sym, mode, frozen_at)` of the run's membership rows.
async fn membership(a: &crate::archive::Archive, run: i64) -> Vec<(String, String, i64)> {
    let mut conn = raw(&a.data_dir.join("archive.sqlite3"), false).await;
    sqlx::query_as("SELECT index_sym, mode, frozen_at FROM run_membership WHERE run_id = ?")
        .bind(run)
        .fetch_all(&mut conn)
        .await
        .unwrap()
}

fn index_knobs(cap: u32) -> Knobs {
    Knobs {
        research: vec!["AAPL.US"],
        indexes: vec![".SPX.US"],
        attempt_cap: cap,
        ..Knobs::default()
    }
}

/// Every endpoint fails for the first 90 s of the run, then recovers: with
/// minute-scale backoff each pair is retried after the outage and the run
/// ends `complete` with every pair `ok`.
#[test]
fn a_short_outage_loses_no_pair() {
    hermetic::run(async {
        let start = UnixMs(tue_start().0 + 60_000);
        let clock = FakeClock::at(start);
        let server = TimedServer::start(clock.clone(), move |s, clock| {
            if clock.now().0 < start.0 + 90_000 {
                Reply::new(503, "<html>outage</html>")
            } else if s.path == CONSTITUENTS {
                Reply::data(&members(&["AAPL.US", "MSFT.US"]))
            } else {
                Reply::data(r#"{"v":1}"#)
            }
        })
        .await;
        let a = archive(&server.server.url, &index_knobs(6), clock.now());
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::Complete, "{outcome:?}");
        let coverage = a.store().await.coverage(RunId(1)).await.unwrap();
        assert_eq!(coverage.len(), 13);
        assert!(coverage.iter().all(|p| p.has_ok));
    })
}

/// Scenario A: a 20 s constituents blip right after the window opens. The
/// constituents are retried a minute later and the membership is
/// `observed`, not carried or `none` with most of the carry time left.
#[test]
fn scenario_a_a_constituents_blip_is_retried_and_observed() {
    hermetic::run(async {
        let start = UnixMs(tue_start().0 + 60_000);
        let clock = FakeClock::at(start);
        let server = TimedServer::start(clock.clone(), move |s, clock| {
            if s.path == CONSTITUENTS && clock.now().0 < start.0 + 20_000 {
                Reply::new(503, "<html>blip</html>")
            } else if s.path == CONSTITUENTS {
                Reply::data(&members(&["AAPL.US", "MSFT.US"]))
            } else {
                Reply::data(r#"{"v":1}"#)
            }
        })
        .await;
        let a = archive(&server.server.url, &index_knobs(6), clock.now());
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::Complete, "{outcome:?}");
        let frozen = membership(&a, 1).await;
        assert_eq!(frozen.len(), 1);
        assert_eq!(frozen[0].1, "observed");
        assert!(frozen[0].2 >= start.0 + 60_000, "{frozen:?}");
    })
}

/// Scenario B: constituents keep failing.
/// - When this start's deadline comes before the carry deadline, the run is
///   left unfrozen with no per-key fetch, even with the cap exhausted, and
///   the next start at or after the carry deadline freezes and completes.
/// - When the carry deadline falls inside the start, the start waits for it
///   and freezes then, in the same start.
#[test]
fn scenario_b_membership_waits_for_the_carry_deadline() {
    hermetic::run(async {
        let carry = UnixMs(tue_start().0 + 60 * 60_000);
        let clock = FakeClock::at(UnixMs(tue_start().0 + 5 * 60_000));
        let server = TimedServer::start(clock.clone(), |s, _| {
            if s.path == CONSTITUENTS {
                Reply::new(503, "<html>down</html>")
            } else {
                Reply::data(r#"{"v":1}"#)
            }
        })
        .await;
        let a = archive(&server.server.url, &index_knobs(2), clock.now());
        let first = capture(&a, &clock).await;
        assert_eq!(first.kind, ExitKind::Resumable, "{first:?}");
        assert!(membership(&a, 1).await.is_empty());
        let arrivals = server.arrivals();
        assert_eq!(arrivals.len(), 2, "{arrivals:?}");
        assert!(arrivals.iter().all(|r| r.path == CONSTITUENTS));

        clock.set(UnixMs(carry.0 + 60_000));
        let second = capture(&a, &clock).await;
        assert_eq!(second.kind, ExitKind::CompleteWithFailures, "{second:?}");
        let frozen = membership(&a, 1).await;
        assert_eq!(frozen[0].1, "none");
        assert!(frozen[0].2 >= carry.0, "{frozen:?}");

        // The carry deadline inside the start: wait for it.
        let clock = FakeClock::at(UnixMs(tue_start().0 + 40 * 60_000));
        let server = TimedServer::start(clock.clone(), |s, _| {
            if s.path == CONSTITUENTS {
                Reply::new(503, "<html>down</html>")
            } else {
                Reply::data(r#"{"v":1}"#)
            }
        })
        .await;
        let a = archive(&server.server.url, &index_knobs(2), clock.now());
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::CompleteWithFailures, "{outcome:?}");
        let frozen = membership(&a, 1).await;
        assert!(
            frozen[0].2 >= carry.0 && frozen[0].2 < carry.0 + 60_000,
            "{frozen:?}"
        );
    })
}

/// Scenario C: this start's deadline is the window end and the carry
/// deadline (600 minutes) falls inside it. Constituents keep failing and
/// reach the cap within minutes; the run still waits for the carry
/// deadline, freezes, fetches and completes. It is never `partial` while
/// the window allows the work.
#[test]
fn scenario_c_a_long_start_waits_for_the_carry_deadline_not_the_window_end() {
    hermetic::run(async {
        let carry = UnixMs(tue_start().0 + 600 * 60_000);
        let clock = FakeClock::at(UnixMs(tue_start().0 + 5 * 3_600_000));
        let server = TimedServer::start(clock.clone(), |s, _| {
            if s.path == CONSTITUENTS {
                Reply::new(503, "<html>down</html>")
            } else {
                Reply::data(r#"{"v":1}"#)
            }
        })
        .await;
        let knobs = Knobs {
            attempt_timeout_minutes: 720,
            carry_deadline_minutes: 600,
            ..index_knobs(3)
        };
        let a = archive(&server.server.url, &knobs, clock.now());
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::CompleteWithFailures, "{outcome:?}");
        assert_eq!(run_end(&a, 1).await.unwrap().0, "complete");
        let frozen = membership(&a, 1).await;
        assert!(frozen[0].2 >= carry.0, "{frozen:?}");
        assert!(clock.now() < tue_end());
    })
}

/// The deadline is checked again at the actual request start: the clock
/// passes the window end while three requests wait for their rate-limit
/// slots (2 per second), and none of them is sent.
#[test]
fn no_request_starts_after_the_window_end_however_late_the_slot() {
    hermetic::run(async {
        let clock = FakeClock::at(UnixMs(tue_end().0 - 60_000));
        let server = TimedServer::start(clock.clone(), |_, _| Reply::data(r#"{"v":1}"#)).await;
        let knobs = Knobs {
            rps: 2,
            ..Knobs::default()
        };
        let a = archive(&server.server.url, &knobs, clock.now());
        let mover = Arc::clone(&clock);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            mover.set(UnixMs(tue_end().0 + 1_000));
        });
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::Partial, "{outcome:?}");
        let arrivals = server.arrivals();
        assert_eq!(arrivals.len(), 1, "{arrivals:?}");
        assert!(arrivals[0].at < tue_end());
        assert_eq!(rows(&a).await.len(), 1);
    })
}
