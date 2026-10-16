//! The deadline follows the clock, a clock stepped back sends nothing, and
//! the recorder stamps under its turn, all through the production
//! `capture::run`.

use crate::archive::security_count;
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use std::time::Duration;

use research_archive::capture;
use research_archive::clock::Clock;
use research_archive::state::{ExitKind, Outcome};
use research_archive::types::UnixMs;

use crate::archive::Knobs;
use crate::fake::{FakeServer, Reply};
use crate::fake_clock::{Arrival, FakeClock, TimedServer};
use crate::hermetic;
use crate::runner::{archive, capture, rows, tue_end, tue_start};

/// Codex (round 4): the clock jumps to the window end while a request is
/// in flight, and the server answers a bare 429, which the SDK retries up to
/// five times inside one send (0.1 s, doubling). The deadline is the clock's,
/// not a timeout fixed when the request started: the request, retries
/// included, is dropped within one `clock::WALL_STEP` (250 ms) of the jump.
/// With a fixed timeout all five retries reach the server after the end.
#[test]
fn a_clock_jump_past_the_window_end_stops_the_sdk_retries() {
    hermetic::run(async {
        let end = tue_end();
        let clock = FakeClock::at(UnixMs(end.0 - 10_000));
        let server = TimedServer::start(clock.clone(), move |_, clock| {
            clock.set(end);
            Reply::new(429, "busy")
        })
        .await;
        let a = archive(&server.server.url, &Knobs::default(), clock.now());
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::Partial, "{outcome:?}");
        let arrivals = server.arrivals();
        let after = arrivals.iter().filter(|r| r.at >= end).count();
        // The retries at 0.1 s and, with scheduling slack, 0.3 s; never all
        // five (the last ones at 0.7, 1.5 and 3.1 s).
        assert!(
            after <= 2,
            "{after} requests at or after the end: {arrivals:?}"
        );
        assert!(rows(&a).await.is_empty());
    })
}

/// One start whose first request steps the clock back to `to` and is
/// answered 1.5 s later, after the other three workers' rate-limit slots
/// (0.25 s apart at 4 rps): how it ended, the requests the server saw, and
/// the rows.
async fn step_back_to(to: UnixMs) -> (Outcome, Vec<Arrival>, usize) {
    let start = UnixMs(tue_start().0 + 60_000);
    let clock = FakeClock::at(start);
    let server = TimedServer::start(clock.clone(), move |_, clock| {
        clock.set(to);
        Reply::data(r#"{"v":1}"#).after(Duration::from_millis(1500))
    })
    .await;
    let knobs = Knobs {
        rps: 4,
        ..Knobs::default()
    };
    let a = archive(&server.server.url, &knobs, start);
    let outcome = capture(&a, &clock).await;
    (outcome, server.arrivals(), rows(&a).await.len())
}

/// Codex (round 5): the clock steps back to an hour before the window start
/// while the other workers wait for their slots. The check at the request
/// start bounds the clock from below too (`window_start ≤ now < deadline`):
/// none of them sends. Checking only `now < deadline`, all three reach the
/// server before the window. The first reply is outside the window, so the
/// recorder refuses it as for a closed window: the start ends `resumable`.
#[test]
fn a_clock_stepped_back_before_the_window_start_sends_nothing() {
    hermetic::run(async {
        let (outcome, arrivals, rows) = step_back_to(UnixMs(tue_start().0 - 3_600_000)).await;
        let early: Vec<_> = arrivals.iter().filter(|r| r.at < tue_start()).collect();
        assert!(early.is_empty(), "sent before the window start: {early:?}");
        assert_eq!(arrivals.len(), 1, "{arrivals:?}");
        assert_eq!(outcome.kind, ExitKind::Resumable, "{outcome:?}");
        assert_eq!(rows, 0);
    })
}

/// A step back that stays inside the window still sends, but the first
/// reply is stamped before the run's start: the Store's monotonic run clock
/// refuses it (`ClockWentBack`) and the start ends `FATAL`, with no row.
#[test]
fn a_clock_stepped_back_inside_the_window_is_fatal() {
    hermetic::run(async {
        let (outcome, arrivals, rows) = step_back_to(UnixMs(tue_start().0 + 30_000)).await;
        assert!(arrivals.len() > 1, "{arrivals:?}");
        assert_eq!(outcome.kind, ExitKind::Fatal, "{outcome:?}");
        assert_eq!(rows, 0);
    })
}

/// A clock that moves forward on every read and is slow to return, by an
/// amount that varies between reads: a worker that read it is often
/// overtaken by another that read it later.
struct Jittery(AtomicI64);

impl Clock for Jittery {
    fn now(&self) -> UnixMs {
        let at = self.0.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_micros(u64::try_from(at % 7).unwrap() * 300));
        UnixMs(at)
    }

    async fn sleep_until(&self, at: UnixMs) {
        self.0.fetch_max(at.0, Ordering::SeqCst);
        tokio::task::yield_now().await;
    }
}

/// #2577 item 2, with real parallel workers: on a multi-threaded runtime
/// the fake server holds every reply until four requests are waiting, then
/// releases them together, so four workers record at the same moment on
/// different threads, against a clock that moves on every read and is slow
/// to return. The recorder takes the stamp only once it has its turn, so
/// stamps follow the order of the writes and every run completes. Stamping
/// before taking the turn lets a worker that read the clock earlier write
/// after one that read it later (`ClockWentBack`, a `fatal` start).
#[test]
fn stamps_are_taken_under_the_turn_with_parallel_workers() {
    let _hermetic = hermetic::enter(true);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            for round in 0..4 {
                let gate = Arc::new(tokio::sync::Semaphore::new(0));
                let g = gate.clone();
                let server = FakeServer::start(move |_| Reply::data(r#"{"v":1}"#).gated(&g)).await;
                let start = UnixMs(tue_start().0 + 60_000);
                let clock = Arc::new(Jittery(AtomicI64::new(start.0)));
                let knobs = Knobs {
                    research: vec!["AAPL.US", "MSFT.US", "NVDA.US"],
                    ..Knobs::default()
                };
                let a = archive(&server.url, &knobs, start);
                let config = a.config();
                let pairs = 3 * security_count();
                let (outcome, ()) = tokio::join!(capture::run(&config, clock, None), async {
                    // Release the replies in batches of the requests in
                    // flight, all of a batch at once.
                    let mut released = 0;
                    while released < pairs {
                        let batch = capture::MAX_IN_FLIGHT.min(pairs - released);
                        let started = std::time::Instant::now();
                        while server.seen().len() < released + batch {
                            assert!(started.elapsed() < Duration::from_secs(30));
                            tokio::time::sleep(Duration::from_millis(2)).await;
                        }
                        gate.add_permits(batch);
                        released += batch;
                    }
                });
                assert_eq!(
                    outcome.kind,
                    ExitKind::Complete,
                    "round {round}: {outcome:?}"
                );
                assert_eq!(rows(&a).await.len(), pairs);
            }
        });
}
