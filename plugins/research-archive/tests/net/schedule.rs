//! The retry schedule and the membership freeze rule, on a fake clock:
//! minute-scale backoff carries pairs through an outage, membership waits
//! for an `ok` or the carry deadline (scenarios A, B and C), and no request
//! starts after the deadline even when the rate-limit slot comes late.

use crate::archive::security_count;
use std::sync::Arc;
use std::time::Duration;

use research_archive::clock::Clock;
use research_archive::state::ExitKind;
use research_archive::store::PlanRequest;
use research_archive::types::{AsOf, Key, RunId, Source, UnixMs, Window};

use crate::archive::{Knobs, members, t};
use crate::fake::Reply;
use crate::fake_clock::{FakeClock, TimedServer};
use crate::hermetic;
use crate::runner::{DAYS, archive, capture, rows, run_end, tue_end, tue_start, wait_for_rows};
use crate::support::raw;

const CONSTITUENTS: &str = "/v1/quote/index-constituents";

/// `(index_sym, mode, frozen_at)` of the run's membership rows.
pub async fn membership(a: &crate::archive::Archive, run: i64) -> Vec<(String, String, i64)> {
    let mut conn = raw(&a.data_dir.join("archive.sqlite3"), false).await;
    sqlx::query_as("SELECT index_sym, mode, frozen_at FROM run_membership WHERE run_id = ?")
        .bind(run)
        .fetch_all(&mut conn)
        .await
        .unwrap()
}

pub fn index_knobs(cap: u32) -> Knobs {
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
        assert_eq!(coverage.len(), 2 * security_count() + 7);
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
/// - When this start's deadline comes before the carry deadline, the
///   constituents are retried every 2 minutes whatever the cap, the run is
///   left unfrozen with no per-key fetch, and the next start at or after
///   the carry deadline freezes at once and completes.
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
        // At 5, 7, … 29 minutes: 13 attempts, cap 2 notwithstanding.
        let arrivals = server.arrivals();
        assert_eq!(arrivals.len(), 13, "{arrivals:?}");
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
/// slots (1 per second), and none of them is sent.
#[test]
fn no_request_starts_after_the_window_end_however_late_the_slot() {
    hermetic::run(async {
        let clock = FakeClock::at(UnixMs(tue_end().0 - 60_000));
        let server = TimedServer::start(clock.clone(), |_, _| Reply::data(r#"{"v":1}"#)).await;
        let knobs = Knobs {
            rps: 1,
            ..Knobs::default()
        };
        let a = archive(&server.server.url, &knobs, clock.now());
        // Once the first response is recorded, the clock passes the window
        // end; the other requests are still waiting for their slots.
        let (outcome, ()) = tokio::join!(capture(&a, &clock), async {
            wait_for_rows(&a, 1).await;
            clock.set(UnixMs(tue_end().0 + 1_000));
        });
        assert_eq!(outcome.kind, ExitKind::Partial, "{outcome:?}");
        let arrivals = server.arrivals();
        assert_eq!(arrivals.len(), 1, "{arrivals:?}");
        assert!(arrivals[0].at < tue_end());
        assert_eq!(rows(&a).await.len(), 1);
    })
}

/// Channel A: an outage of the constituents of 22, 25, 40 or 55 minutes
/// from the window start. Retried every 2 minutes (the cap of 3 does not
/// apply before the carry deadline), they succeed before the 60-minute
/// carry deadline and the membership is `observed`.
#[test]
fn membership_outages_short_of_the_carry_deadline_are_observed() {
    hermetic::run(async {
        for minutes in [22, 25, 40, 55] {
            let start = UnixMs(tue_start().0 + 60_000);
            let clock = FakeClock::at(start);
            let ends = start.0 + minutes * 60_000;
            let server = TimedServer::start(clock.clone(), move |s, clock| {
                if s.path == CONSTITUENTS && clock.now().0 < ends {
                    Reply::new(503, "<html>down</html>")
                } else if s.path == CONSTITUENTS {
                    Reply::data(&members(&["AAPL.US", "MSFT.US"]))
                } else {
                    Reply::data(r#"{"v":1}"#)
                }
            })
            .await;
            let knobs = Knobs {
                attempt_timeout_minutes: 120,
                ..index_knobs(3)
            };
            let a = archive(&server.server.url, &knobs, clock.now());
            let outcome = capture(&a, &clock).await;
            assert_eq!(
                outcome.kind,
                ExitKind::Complete,
                "{minutes} min: {outcome:?}"
            );
            let frozen = membership(&a, 1).await;
            assert_eq!(frozen[0].1, "observed", "{minutes} min");
            assert!(
                frozen[0].2 >= ends && frozen[0].2 < ends + 2 * 60_000,
                "{minutes} min"
            );
        }
    })
}

/// A late start (after the carry deadline) freezes at once from the stored
/// rows, here carrying Monday's membership; its throttled first
/// constituents attempt and the `ok` after it are coverage work recorded
/// after the freeze and change nothing.
#[test]
fn a_late_start_freezes_from_stored_rows_before_any_attempt() {
    hermetic::run(async {
        let clock = FakeClock::at(t("2026-03-02T23:00:00Z"));
        let throttled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let th = throttled.clone();
        let server = TimedServer::start(clock.clone(), move |s, _| {
            if s.path == CONSTITUENTS && th.swap(false, std::sync::atomic::Ordering::SeqCst) {
                Reply::new(
                    200,
                    r#"{"code":429002,"message":"please retry after: 0.2s"}"#,
                )
            } else if s.path == CONSTITUENTS {
                Reply::data(&members(&["AAPL.US", "MSFT.US"]))
            } else {
                Reply::data(r#"{"v":1}"#)
            }
        })
        .await;
        let a = archive(&server.server.url, &index_knobs(6), clock.now());
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::Complete);

        throttled.store(true, std::sync::atomic::Ordering::SeqCst);
        let late = UnixMs(tue_start().0 + 70 * 60_000);
        clock.set(late);
        a.calendar(late, "2026-02-26", "2026-03-12", DAYS);
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::Complete, "{outcome:?}");
        let frozen = membership(&a, 2).await;
        assert_eq!((frozen[0].1.as_str(), frozen[0].2), ("carried", late.0));
        let constituents: Vec<_> = rows(&a)
            .await
            .into_iter()
            .filter(|r| r.1 == "index_constituents" && r.3 >= late.0)
            .map(|r| r.4)
            .collect();
        assert_eq!(constituents, ["throttled", "ok"]);
    })
}

/// Codex: a run planned before the carry deadline with no constituents
/// attempt resumes after it. The freeze comes at once from the stored rows
/// (`none`); the constituents attempt, which hangs until the test opens
/// it, is coverage work and does not hold up the security pairs.
#[test]
fn a_resume_after_the_carry_deadline_freezes_at_once_despite_a_hanging_attempt() {
    hermetic::run(async {
        let clock = FakeClock::at(UnixMs(tue_start().0 + 65 * 60_000));
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let g = gate.clone();
        let server = TimedServer::start(clock.clone(), move |s, _| {
            if s.path == CONSTITUENTS {
                Reply::data(&members(&["AAPL.US", "MSFT.US"])).gated(&g)
            } else {
                Reply::data(r#"{"v":1}"#)
            }
        })
        .await;
        let a = archive(&server.server.url, &index_knobs(6), clock.now());
        let store = a.store().await;
        store
            .plan_run(&PlanRequest {
                source: Source::Longbridge,
                as_of: AsOf::parse("2026-03-03").unwrap(),
                window: Window::new(tue_start(), tue_end()).unwrap(),
                started_at: UnixMs(tue_start().0 + 5 * 60_000),
                config: vec![Key::new("AAPL.US").unwrap()],
                indexes: vec![Key::new(".SPX.US").unwrap()],
            })
            .await
            .unwrap();
        store.close().await;
        let (outcome, ()) = tokio::join!(capture(&a, &clock), async {
            // The security pairs land while the constituents hang.
            wait_for_rows(&a, security_count() as i64).await;
            gate.add_permits(1);
        });
        assert_eq!(outcome.kind, ExitKind::Complete, "{outcome:?}");
        let frozen = membership(&a, 1).await;
        assert_eq!(
            (frozen[0].1.as_str(), frozen[0].2),
            ("none", tue_start().0 + 65 * 60_000)
        );
        // The security rows were recorded before the constituents
        // answered (the barrier above); then the constituents row.
        let recorded = rows(&a).await;
        assert_eq!(recorded.len(), security_count() + 7);
        let constituents: Vec<_> = recorded
            .iter()
            .filter(|r| r.1 == "index_constituents")
            .map(|r| r.4.as_str())
            .collect();
        assert_eq!(constituents, ["ok"]);
    })
}

/// Codex: the first auth failure stops its siblings before its own row is
/// written. Every request answers 401, and the first one only once the test
/// holds the archive's write lock, so that row's write waits. The siblings,
/// queued on their rate-limit slots (1 per second), are dropped at the
/// latch: no second request arrives while the write waits (bounded at
/// 2.5 s), and only the auth row is recorded.
#[test]
fn the_auth_latch_comes_before_a_slow_auth_row() {
    hermetic::run(async {
        let clock = FakeClock::at(UnixMs(tue_start().0 + 60_000));
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let g = gate.clone();
        let server = TimedServer::start(clock.clone(), move |_, _| {
            Reply::new(401, "<html>who</html>").gated(&g)
        })
        .await;
        let knobs = Knobs {
            rps: 1,
            ..Knobs::default()
        };
        let a = archive(&server.server.url, &knobs, clock.now());
        let (outcome, ()) = tokio::join!(capture(&a, &clock), async {
            while server.arrivals().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let mut lock = raw(&a.data_dir.join("archive.sqlite3"), false).await;
            sqlx::query("BEGIN IMMEDIATE")
                .execute(&mut lock)
                .await
                .unwrap();
            gate.add_permits(1);
            // Bounded: with the latch after the write, the next slot (1 s)
            // would send a sibling here.
            let waited = tokio::time::timeout(Duration::from_millis(2_500), async {
                while server.arrivals().len() < 2 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await;
            sqlx::query("ROLLBACK").execute(&mut lock).await.unwrap();
            assert!(
                waited.is_err(),
                "a sibling was sent: {:?}",
                server.arrivals()
            );
        });
        assert_eq!(outcome.kind, ExitKind::AuthUnavailable, "{outcome:?}");
        assert_eq!(server.arrivals().len(), 1);
        let recorded = rows(&a).await;
        assert_eq!(recorded.len(), 1, "{recorded:?}");
        assert_eq!(recorded[0].4, "auth_unavailable");
    })
}

/// Codex: the wait a throttle named is not stored, so a start that resumes a
/// pair whose last row is `throttled` waits the longest wait a server may
/// name (60 s) from that row, not the 1 s fallback. The first start ends at
/// its one-minute deadline right after a 60-second throttle; the next start,
/// 3 s later, does not retry before the 60 s are up.
#[test]
fn a_throttle_wait_survives_a_restart() {
    hermetic::run(async {
        let start = UnixMs(tue_start().0 + 60_000);
        let clock = FakeClock::at(start);
        let throttled = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let th = throttled.clone();
        let server = TimedServer::start(clock.clone(), move |s, _| {
            if s.path == "/v1/quote/financial-consensus-detail"
                && !s
                    .query
                    .as_deref()
                    .unwrap_or_default()
                    .contains("period_type=af")
                && th.swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                Reply::new(
                    200,
                    r#"{"code":429002,"message":"please retry after: 60s"}"#,
                )
            } else {
                Reply::data(r#"{"v":1}"#)
            }
        })
        .await;
        let knobs = Knobs {
            attempt_timeout_minutes: 1,
            ..Knobs::default()
        };
        let a = archive(&server.server.url, &knobs, clock.now());
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::Resumable);

        clock.set(UnixMs(start.0 + 3_000));
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::Complete);
        let consensus: Vec<_> = server
            .arrivals()
            .into_iter()
            .filter(|r| {
                r.path == "/v1/quote/financial-consensus-detail"
                    && !r
                        .query
                        .as_deref()
                        .unwrap_or_default()
                        .contains("period_type=af")
            })
            .map(|r| r.at.0 - start.0)
            .collect();
        assert_eq!(consensus.len(), 2);
        assert!(consensus[1] >= 60_000, "{consensus:?}");
    })
}

/// Two auth failures answered at the same moment: one gate releases both
/// 401s. The first latches the stop and its row is recorded; the second is
/// dropped like any sibling.
#[test]
fn of_two_simultaneous_auth_failures_only_the_winner_is_recorded() {
    hermetic::run(async {
        let clock = FakeClock::at(UnixMs(tue_start().0 + 60_000));
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let g = gate.clone();
        let server = TimedServer::start(clock.clone(), move |_, _| {
            Reply::new(401, "<html>who</html>").gated(&g)
        })
        .await;
        let a = archive(&server.server.url, &Knobs::default(), clock.now());
        let (outcome, ()) = tokio::join!(capture(&a, &clock), async {
            while server.arrivals().len() < 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            gate.add_permits(2);
        });
        assert_eq!(outcome.kind, ExitKind::AuthUnavailable, "{outcome:?}");
        let recorded = rows(&a).await;
        assert_eq!(recorded.len(), 1, "{recorded:?}");
        assert_eq!(recorded[0].4, "auth_unavailable");
    })
}

/// The auth row that wins the latch cannot be written (the archive's write
/// lock is held past the Store's 5 s busy timeout): the start is `FATAL`,
/// not `AUTH_UNAVAILABLE`.
#[test]
fn an_auth_row_that_cannot_be_written_is_fatal() {
    hermetic::run(async {
        let clock = FakeClock::at(UnixMs(tue_start().0 + 60_000));
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let g = gate.clone();
        let server = TimedServer::start(clock.clone(), move |_, _| {
            Reply::new(401, "<html>who</html>").gated(&g)
        })
        .await;
        let a = archive(&server.server.url, &Knobs::default(), clock.now());
        let (outcome, ()) = tokio::join!(capture(&a, &clock), async {
            while server.arrivals().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let mut lock = raw(&a.data_dir.join("archive.sqlite3"), false).await;
            sqlx::query("BEGIN IMMEDIATE")
                .execute(&mut lock)
                .await
                .unwrap();
            gate.add_permits(2);
            tokio::time::sleep(Duration::from_secs(6)).await;
            sqlx::query("ROLLBACK").execute(&mut lock).await.unwrap();
        });
        assert_eq!(outcome.kind, ExitKind::Fatal, "{outcome:?}");
        assert!(rows(&a).await.is_empty());
        assert_eq!(a.last_exit()["kind"], "fatal");
    })
}
