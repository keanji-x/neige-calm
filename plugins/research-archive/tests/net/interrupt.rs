//! A capture interrupted during membership, in process on a fake clock: the
//! capture future is dropped while its constituents request is in flight,
//! which leaves the archive as a kill would (every write is one
//! transaction). I4: the plan is whole and nothing is frozen; a rerun
//! completes the run. I12: the rerun keeps the planned keys and index set
//! after the config changed.
//!
//! These run in process because the membership phase cannot be reached in
//! real time at every hour (see the suite's `capture_cli`).

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use research_archive::state::ExitKind;
use research_archive::types::{Key, RunId, UnixMs};

use crate::archive::{Archive, Knobs, members};
use crate::fake::Reply;
use crate::fake_clock::{FakeClock, TimedServer};
use crate::hermetic;
use crate::runner::{archive, capture, tue_start};
use crate::support::raw;

const CONSTITUENTS: &str = "/v1/quote/index-constituents";

/// A server whose constituents hang while `hold` is set.
async fn server(clock: &Arc<FakeClock>, hold: Arc<AtomicBool>) -> TimedServer {
    let never = Arc::new(tokio::sync::Semaphore::new(0));
    TimedServer::start(Arc::clone(clock), move |s, _| {
        if s.path != CONSTITUENTS {
            Reply::data(r#"{"v":1}"#)
        } else if hold.load(Ordering::SeqCst) {
            Reply::data(&members(&["AAPL.US", "MSFT.US"])).gated(&never)
        } else {
            Reply::data(&members(&["AAPL.US", "MSFT.US"]))
        }
    })
    .await
}

fn knobs() -> Knobs {
    Knobs {
        research: vec!["AAPL.US"],
        indexes: vec![".SPX.US"],
        ..Knobs::default()
    }
}

/// Run a capture and drop it once its constituents request has arrived.
async fn interrupt_in_membership(a: &Archive, clock: &Arc<FakeClock>, server: &TimedServer) {
    tokio::select! {
        outcome = capture(a, clock) => panic!("the capture ended: {outcome:?}"),
        () = async {
            while !server.arrivals().iter().any(|r| r.path == CONSTITUENTS) {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        } => {}
    }
}

/// `(key, origin)` of every `run_key` row, and `(index, mode)` of every
/// `run_membership` row.
async fn universe(a: &Archive) -> (Vec<(String, String)>, Vec<(String, String)>) {
    let mut conn = raw(&a.data_dir.join("archive.sqlite3"), false).await;
    let keys = sqlx::query_as("SELECT key, origin FROM run_key WHERE origin NOT IN ('benchmark','market') ORDER BY key, origin")
        .fetch_all(&mut conn)
        .await
        .unwrap();
    let frozen = sqlx::query_as("SELECT index_sym, mode FROM run_membership")
        .fetch_all(&mut conn)
        .await
        .unwrap();
    (keys, frozen)
}

fn pairs(rows: &[(&str, &str)]) -> Vec<(String, String)> {
    rows.iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect()
}

#[test]
fn i4_an_interruption_during_membership_leaves_a_whole_plan_and_a_rerun_completes() {
    hermetic::run(async {
        let clock = FakeClock::at(UnixMs(tue_start().0 + 60_000));
        let hold = Arc::new(AtomicBool::new(true));
        let server = server(&clock, hold.clone()).await;
        let a = archive(
            &server.server.url,
            &knobs(),
            research_archive::clock::Clock::now(&*clock),
        );
        interrupt_in_membership(&a, &clock, &server).await;

        let (keys, frozen) = universe(&a).await;
        assert_eq!(
            keys,
            pairs(&[(".SPX.US", "index-symbol"), ("AAPL.US", "config")])
        );
        assert!(frozen.is_empty());
        assert!(server.arrivals().iter().all(|r| r.path == CONSTITUENTS));

        hold.store(false, Ordering::SeqCst);
        clock.advance(60_000);
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::Complete, "{outcome:?}");
        let (keys, frozen) = universe(&a).await;
        assert_eq!(frozen, pairs(&[(".SPX.US", "observed")]));
        assert_eq!(keys.len(), 4, "{keys:?}");
    })
}

#[test]
fn i12_a_resumed_run_keeps_its_planned_keys_and_indexes() {
    hermetic::run(async {
        let clock = FakeClock::at(UnixMs(tue_start().0 + 60_000));
        let hold = Arc::new(AtomicBool::new(true));
        let server = server(&clock, hold.clone()).await;
        let a = archive(
            &server.server.url,
            &knobs(),
            research_archive::clock::Clock::now(&*clock),
        );
        interrupt_in_membership(&a, &clock, &server).await;

        a.write_config(&Knobs {
            research: vec!["NVDA.US"],
            indexes: vec![".DJI.US"],
            ..Knobs::default()
        });
        hold.store(false, Ordering::SeqCst);
        clock.advance(60_000);
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::Complete, "{outcome:?}");
        let plan = a.store().await.plan(RunId(1)).await.unwrap();
        let names =
            |keys: &[Key]| -> Vec<String> { keys.iter().map(ToString::to_string).collect() };
        assert_eq!(names(&plan.config), ["AAPL.US"]);
        assert_eq!(names(&plan.indexes), [".SPX.US"]);
        let (_, frozen) = universe(&a).await;
        assert_eq!(frozen, pairs(&[(".SPX.US", "observed")]));
        for r in server.arrivals() {
            assert!(r.symbol != "NVDA.US" && r.symbol != ".DJI.US", "{r:?}");
        }
    })
}
