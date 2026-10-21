//! Concurrent recorder ordering through the production capture entry point.
use crate::runner::{archive, capture, rows, tue_start, wait_for_rows};
use crate::{
    archive::{Knobs, security_count},
    fake::Reply,
    fake_clock::{FakeClock, TimedServer},
    hermetic,
};
use research_archive::{clock::Clock, state::ExitKind};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// #2577 item 2: concurrent responses are stamped when recorded, so a slow
/// response recorded after faster ones is not refused as `ClockWentBack`
/// and attempt order is time order. Every arrival moves the fake clock one
/// second, and the first request is answered only once three later ones
/// are recorded.
#[test]
fn concurrent_responses_are_recorded_in_time_order() {
    hermetic::run(async {
        let clock = FakeClock::at(tue_start());
        clock.advance(60_000);
        let first = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let (f, g) = (first.clone(), gate.clone());
        let server = TimedServer::start(clock.clone(), move |_, clock| {
            clock.advance(1_000);
            let reply = Reply::data(r#"{"v":1}"#);
            if f.fetch_add(1, Ordering::SeqCst) == 0 {
                reply.gated(&g)
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
        let (outcome, ()) = tokio::join!(capture(&a, &clock), async {
            wait_for_rows(&a, 3).await;
            gate.add_permits(1);
        });
        assert_eq!(outcome.kind, ExitKind::Complete, "{outcome:?}");
        let recorded = rows(&a).await;
        assert_eq!(recorded.len(), 2 * security_count() + 6);
        // The slow first request was stamped when it was recorded, after later
        // requests had arrived, not when it was sent.
        let arrivals = server.arrivals();
        let first = &arrivals[0];
        let endpoint_id = research_archive::Catalog::compiled()
            .endpoints()
            .iter()
            .find(|e| {
                e.path == first.path
                    && (e.id.as_str() == "consensus_annual")
                        == first
                            .query
                            .as_deref()
                            .unwrap_or_default()
                            .contains("period_type=af")
            })
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
