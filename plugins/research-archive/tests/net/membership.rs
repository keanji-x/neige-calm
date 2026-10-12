//! Membership at the shipped cadence and around the freeze: what an outage
//! of the constituents does when a start runs every 30 minutes for 25, that
//! the freeze never waits for an attempt in flight, and that only failures
//! after the freeze count toward the constituents pair's cap.

use std::sync::Arc;

use research_archive::clock::Clock;
use research_archive::state::ExitKind;
use research_archive::types::UnixMs;

use crate::archive::members;
use crate::fake::Reply;
use crate::fake_clock::{FakeClock, TimedServer};
use crate::hermetic;
use crate::runner::{archive, capture, rows, tue_start, wait_for_rows};
use crate::schedule::{index_knobs, membership};

const CONSTITUENTS: &str = "/v1/quote/index-constituents";
const MIN: i64 = 60_000;

/// A server whose constituents fail until `ends`.
async fn outage_until(clock: &Arc<FakeClock>, ends: i64) -> TimedServer {
    TimedServer::start(Arc::clone(clock), move |s, clock| {
        if s.path == CONSTITUENTS && clock.now().0 < ends {
            Reply::new(503, "<html>down</html>")
        } else if s.path == CONSTITUENTS {
            Reply::data(&members(&["AAPL.US", "MSFT.US"]))
        } else {
            Reply::data(r#"{"v":1}"#)
        }
    })
    .await
}

/// Channel A, at the shipped cadence: a start at the window start, at +30
/// and at +60 minutes, each with the 25-minute attempt timeout and the
/// 60-minute carry deadline. Constituents retried every 2 minutes recover
/// from outages of 22, 25 and 40 minutes (`observed`). An outage of 55
/// minutes outlasts the second start's last attempt (+54); the third start
/// begins after the carry deadline and freezes at once, `none`. No retry
/// happens between +55 and +60.
#[test]
fn membership_outages_at_the_shipped_cadence() {
    hermetic::run(async {
        for (minutes, mode) in [
            (22, "observed"),
            (25, "observed"),
            (40, "observed"),
            (55, "none"),
        ] {
            let start = tue_start();
            let clock = FakeClock::at(start);
            let server = outage_until(&clock, start.0 + minutes * MIN).await;
            let a = archive(&server.server.url, &index_knobs(3), clock.now());
            let mut outcome = None;
            for offset in [0, 30, 60] {
                clock.set(UnixMs(start.0 + offset * MIN));
                let o = capture(&a, &clock).await;
                if o.kind != ExitKind::Resumable {
                    outcome = Some(o);
                    break;
                }
            }
            let outcome = outcome.expect("a start ends the run");
            assert_eq!(
                outcome.kind,
                ExitKind::Complete,
                "{minutes} min: {outcome:?}"
            );
            let frozen = membership(&a, 1).await;
            assert_eq!(frozen[0].1, mode, "{minutes} min");
            let last_try = server
                .arrivals()
                .iter()
                .filter(|r| r.path == CONSTITUENTS && r.at.0 < start.0 + 60 * MIN)
                .map(|r| r.at.0 - start.0)
                .max()
                .unwrap();
            assert!(
                last_try <= 54 * MIN,
                "{minutes} min: last try at {last_try}"
            );
        }
    })
}

/// Channel A: the carry deadline falls inside the start, and the clamped
/// retry at carry - 15 s hangs past it. The freeze comes exactly at the
/// carry deadline without it; the security pairs run while it hangs; its
/// late `ok` is recorded after the freeze and changes nothing (`none`).
#[test]
fn the_freeze_never_waits_for_an_attempt_in_flight() {
    hermetic::run(async {
        let carry = UnixMs(tue_start().0 + 60 * MIN);
        let clock = FakeClock::at(UnixMs(carry.0 - 20_000));
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let g = gate.clone();
        let failed_once = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let f = failed_once.clone();
        let server = TimedServer::start(Arc::clone(&clock), move |s, _| {
            if s.path != CONSTITUENTS {
                Reply::data(r#"{"v":1}"#)
            } else if !f.swap(true, std::sync::atomic::Ordering::SeqCst) {
                Reply::new(503, "<html>down</html>")
            } else {
                Reply::data(&members(&["AAPL.US", "MSFT.US"])).gated(&g)
            }
        })
        .await;
        let a = archive(&server.server.url, &index_knobs(6), clock.now());
        let (outcome, ()) = tokio::join!(capture(&a, &clock), async {
            // The failed constituents row and the six security rows.
            wait_for_rows(&a, 7).await;
            gate.add_permits(1);
        });
        assert_eq!(outcome.kind, ExitKind::Complete, "{outcome:?}");
        let frozen = membership(&a, 1).await;
        assert_eq!((frozen[0].1.as_str(), frozen[0].2), ("none", carry.0));
        let constituents: Vec<_> = rows(&a)
            .await
            .into_iter()
            .filter(|r| r.1 == "index_constituents")
            .map(|r| (r.3, r.4))
            .collect();
        assert_eq!(constituents.len(), 2, "{constituents:?}");
        assert_eq!(constituents[0], (carry.0 - 20_000, "transient".to_owned()));
        assert!(
            constituents[1].0 >= carry.0 && constituents[1].1 == "ok",
            "{constituents:?}"
        );
    })
}

/// Channel A: the constituents failed 13 times before the freeze (retried
/// every 2 minutes, cap 2). Those were the membership rule's failures: after
/// the freeze the pair has spent none of its cap, so the recovered upstream
/// is tried and the run completes with every pair `ok`.
#[test]
fn only_failures_after_the_freeze_count_toward_the_constituents_cap() {
    hermetic::run(async {
        let start = UnixMs(tue_start().0 + 5 * MIN);
        let clock = FakeClock::at(start);
        let server = outage_until(&clock, start.0 + 30 * MIN).await;
        let a = archive(&server.server.url, &index_knobs(2), clock.now());
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::Resumable);
        assert_eq!(rows(&a).await.len(), 13);

        clock.set(UnixMs(tue_start().0 + 61 * MIN));
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::Complete, "{outcome:?}");
        let frozen = membership(&a, 1).await;
        assert_eq!(frozen[0].1, "none");
        let late_ok = rows(&a)
            .await
            .into_iter()
            .any(|r| r.1 == "index_constituents" && r.4 == "ok" && r.3 >= frozen[0].2);
        assert!(late_ok);
    })
}
