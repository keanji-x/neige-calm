use super::*;
use tokio::sync::watch;

#[test]
fn wait_report_names_its_baseline_revision() {
    let report = WaitReport {
        mode: WaitFor::Elapsed,
        outcome: WaitOutcome::Elapsed,
        waited: Duration::from_millis(0),
        settled: false,
        baseline: 42,
        signal_baseline: 3,
        signal: None,
        signal_at: None,
        repaint: None,
        text: None,
        conditions: None,
    };
    assert_eq!(
        report.to_json(),
        json!({"mode":"elapsed","outcome":"elapsed","waited_ms":0,"settled":false,"baseline_revision":"42","baseline_signal_seq":3})
    );
    let report = WaitReport {
        mode: WaitFor::Change,
        outcome: WaitOutcome::Changed,
        waited: Duration::from_millis(812),
        settled: true,
        baseline: 7,
        signal_baseline: 0,
        signal: None,
        signal_at: None,
        repaint: None,
        text: None,
        conditions: None,
    };
    assert_eq!(report.to_json()["baseline_revision"], "7");
    assert_eq!(report.to_json()["outcome"], "changed");
    assert!(
        report.to_json().get("signal").is_none(),
        "signal only in signal mode"
    );
    assert!(
        report.to_json().get("repaint").is_none() && report.to_json().get("signal_at_ms").is_none(),
        "repaint fields only in signal mode"
    );
    let signal = Signal {
        seq: 4,
        event: "stop".into(),
        notification_type: None,
        message: String::new(),
        claude_session_id: Some("s".into()),
        received_at_ms: 1,
    };
    let report = WaitReport {
        mode: WaitFor::Signal,
        outcome: WaitOutcome::Signal,
        waited: Duration::from_millis(5),
        settled: false,
        baseline: 7,
        signal_baseline: 3,
        signal: Some(signal.clone()),
        signal_at: Some(Duration::from_millis(3)),
        repaint: Some(RepaintReport {
            outcome: RepaintOutcome::Settled,
            waited: Duration::from_millis(2),
        }),
        text: None,
        conditions: Some(ConditionState {
            present: None,
            absent: Some(true),
        }),
    };
    assert_eq!(report.to_json()["outcome"], "signal");
    assert_eq!(
        report.to_json()["conditions"],
        json!({"present":null,"absent":true})
    );
    assert_eq!(report.to_json()["signal"], signal.to_json());
    assert_eq!(report.to_json()["baseline_signal_seq"], 3);
    assert_eq!(report.to_json()["signal_at_ms"], 3);
    assert_eq!(
        report.to_json()["repaint"],
        json!({"outcome":"settled","waited_ms":2})
    );
    // No signal: both fields present and null, like `signal`.
    let report = WaitReport {
        mode: WaitFor::Signal,
        outcome: WaitOutcome::NoSignal,
        waited: Duration::from_millis(5),
        settled: false,
        baseline: 7,
        signal_baseline: 3,
        signal: None,
        signal_at: None,
        repaint: None,
        text: None,
        conditions: None,
    };
    assert_eq!(report.to_json()["outcome"], "no_signal");
    assert_eq!(report.to_json()["signal_at_ms"], Value::Null);
    assert_eq!(report.to_json()["repaint"], Value::Null);
    assert_eq!(
        report.to_json()["conditions"],
        json!({"present":null,"absent":null}),
        "no conditions: both sides null"
    );
    for (outcome, name) in [
        (RepaintOutcome::Already, "already"),
        (RepaintOutcome::Settled, "settled"),
        (RepaintOutcome::None, "none"),
        (RepaintOutcome::Unsettled, "unsettled"),
        (RepaintOutcome::Skipped, "skipped"),
    ] {
        assert_eq!(outcome.name(), name);
    }
    assert!(
        report.to_json().get("text").is_none(),
        "text only in text mode"
    );
    // Text mode: `text` is the match or null; the signal fields stay absent.
    let matched = TextMatch {
        pattern: "❯".into(),
        row: 5,
        revision: 9,
        already: false,
    };
    let report = WaitReport {
        mode: WaitFor::Text,
        outcome: WaitOutcome::Matched,
        waited: Duration::from_millis(400),
        settled: true,
        baseline: 7,
        signal_baseline: 3,
        signal: None,
        signal_at: None,
        repaint: None,
        text: Some(matched.clone()),
        conditions: Some(ConditionState {
            present: Some(true),
            absent: None,
        }),
    };
    assert_eq!(
        report.to_json(),
        json!({"mode":"text","outcome":"matched","waited_ms":400,"settled":true,"baseline_revision":"7","baseline_signal_seq":3,
            "text":{"pattern":"❯","row":5,"revision":"9","already":false},
            "conditions":{"present":true,"absent":null}})
    );
    let report = WaitReport {
        outcome: WaitOutcome::Unmatched,
        text: None,
        ..report
    };
    assert_eq!(report.to_json()["outcome"], "unmatched");
    assert_eq!(report.to_json()["text"], Value::Null);
}

fn incoming(event: &str) -> crate::terminal_renderer::IncomingSignal {
    crate::terminal_renderer::IncomingSignal {
        event: event.into(),
        notification_type: None,
        message: String::new(),
        claude_session_id: None,
    }
}
struct SignalFixture {
    revisions: watch::Sender<u64>,
    events: watch::Sender<u64>,
    stopped: Arc<AtomicBool>,
}
/// Every sender is kept alive by the fixture (a dropped sender ends the loop at once, which
/// would make a budget test vacuous).
type SignalTask = tokio::task::JoinHandle<(SignalWait, Duration)>;
fn start_signal(
    ring: Arc<crate::terminal_renderer::SignalRing>,
    baseline: u64,
    budget_ms: u64,
) -> (SignalFixture, SignalTask) {
    start_signal_repaint(ring, baseline, budget_ms, RepaintPlan::skip())
}
/// The same, with a repaint plan; the revision channel and baseline start at 0.
fn start_signal_repaint(
    ring: Arc<crate::terminal_renderer::SignalRing>,
    baseline: u64,
    budget_ms: u64,
    repaint: RepaintPlan,
) -> (SignalFixture, SignalTask) {
    let (revisions, revisions_rx) = watch::channel(0u64);
    let (events, events_rx) = watch::channel(0u64);
    let stopped = Arc::new(AtomicBool::new(false));
    let flag = stopped.clone();
    let started = Instant::now();
    let waiter = ring.clone();
    let task = tokio::spawn(async move {
        let stop = vec!["stop".to_string()];
        let result = wait_for_signal(
            waiter.subscribe(),
            revisions_rx,
            events_rx,
            move || flag.load(Ordering::SeqCst),
            || panic!("no conditions: the repaint phase never captures"),
            &TextConditions::default(),
            move || waiter.first_matching(baseline, &stop),
            0,
            started,
            started + Duration::from_millis(budget_ms),
            repaint,
        )
        .await;
        (result, started.elapsed())
    });
    (
        SignalFixture {
            revisions,
            events,
            stopped,
        },
        task,
    )
}

#[tokio::test(start_paused = true)]
async fn signal_loop_inspects_the_ring_before_select() {
    use crate::terminal_renderer::SignalRing;
    // Already present above the baseline: returns without sleeping.
    let ring = Arc::new(SignalRing::new());
    ring.push("a", incoming("user_prompt_submit"), 0);
    ring.push("b", incoming("stop"), 0);
    let (fixture, task) = start_signal(ring.clone(), 0, 5_000);
    let (verdict, waited) = task.await.unwrap();
    assert_eq!(verdict.signal.map(|s| s.seq), Some(2));
    assert!(!verdict.exited);
    assert_eq!(waited, Duration::ZERO);
    assert_eq!(verdict.signal_at, Some(Duration::ZERO));
    assert_eq!(
        verdict.repaint,
        Some(RepaintReport {
            outcome: RepaintOutcome::Skipped,
            waited: Duration::ZERO
        }),
        "repaint 0 returns at the signal"
    );
    drop(fixture);
    // Present but at or below the baseline: ignored; a later one wakes.
    let (fixture, task) = start_signal(ring.clone(), 2, 5_000);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(100)).await;
    bump(&fixture.events);
    bump(&fixture.revisions);
    tokio::task::yield_now().await;
    assert!(
        !task.is_finished(),
        "protocol events and revisions do not end a signal wait"
    );
    ring.push("c", incoming("notification"), 0);
    tokio::task::yield_now().await;
    assert!(!task.is_finished(), "a non-matching event keeps waiting");
    ring.push("d", incoming("stop"), 0);
    tokio::task::yield_now().await;
    let (verdict, _) = task.await.unwrap();
    assert_eq!(verdict.signal.map(|s| s.seq), Some(4));
    assert!(!verdict.exited);
    // Stopped before the wait starts reports exited at once.
    let (fixture, task) = start_signal(ring.clone(), 4, 300);
    fixture.stopped.store(true, Ordering::SeqCst);
    let (verdict, _) = task.await.unwrap();
    assert!(verdict.signal.is_none() && verdict.exited);
    assert!(verdict.signal_at.is_none() && verdict.repaint.is_none());
}

/// A signal pushed at the deadline itself is still reported (the ring is re-read on timeout).
#[tokio::test(start_paused = true)]
async fn signal_loop_runs_to_the_budget_and_rereads_the_ring_on_timeout() {
    use crate::terminal_renderer::SignalRing;
    let ring = Arc::new(SignalRing::new());
    let (fixture, task) = start_signal(ring.clone(), 0, 300);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(299)).await;
    assert!(!task.is_finished(), "ended before the budget");
    tokio::time::advance(Duration::from_millis(1)).await;
    let (verdict, waited) = task.await.unwrap();
    assert!(verdict.signal.is_none() && !verdict.exited);
    assert_eq!(waited, Duration::from_millis(300));
    drop(fixture);

    // A matching signal visible to the lookup without any channel wake is still reported: the
    // timeout branch re-reads the ring.
    let (signals_tx, signals_rx) = watch::channel(0u64);
    let (revisions_tx, revisions_rx) = watch::channel(0u64);
    let (events_tx, events_rx) = watch::channel(0u64);
    let visible = Arc::new(AtomicBool::new(false));
    let flag = visible.clone();
    let started = Instant::now();
    let task = tokio::spawn(async move {
        let result = wait_for_signal(
            signals_rx,
            revisions_rx,
            events_rx,
            || false,
            || None,
            &TextConditions::default(),
            move || {
                flag.load(Ordering::SeqCst).then(|| Signal {
                    seq: 1,
                    event: "stop".into(),
                    notification_type: None,
                    message: String::new(),
                    claude_session_id: None,
                    received_at_ms: 0,
                })
            },
            0,
            started,
            started + Duration::from_millis(300),
            RepaintPlan::skip(),
        )
        .await;
        (result, started.elapsed())
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(299)).await;
    assert!(!task.is_finished());
    visible.store(true, Ordering::SeqCst);
    tokio::time::advance(Duration::from_millis(1)).await;
    let (verdict, waited) = task.await.unwrap();
    assert_eq!(
        verdict.signal.map(|s| s.seq),
        Some(1),
        "signal at the deadline"
    );
    assert!(!verdict.exited);
    assert_eq!(waited, Duration::from_millis(300));
    drop((signals_tx, revisions_tx, events_tx));
}

#[tokio::test(start_paused = true)]
async fn signal_loop_timeout_rechecks_the_stopped_state() {
    use crate::terminal_renderer::SignalRing;
    let ring = Arc::new(SignalRing::new());
    let (fixture, task) = start_signal(ring, 0, 300);
    tokio::task::yield_now().await;
    // No protocol event reaches the loop before the timer, so only the timer branch can see the exit.
    fixture.stopped.store(true, Ordering::SeqCst);
    tokio::time::advance(Duration::from_millis(300)).await;
    let (verdict, waited) = task.await.unwrap();
    assert!(verdict.signal.is_none());
    assert!(verdict.exited, "exit at the deadline reported as no_signal");
    assert_eq!(waited, Duration::from_millis(300));
}

#[tokio::test(start_paused = true)]
async fn invalidated_projection_stops_a_signal_wait_before_the_budget() {
    use crate::terminal_renderer::{ModelView, SignalRing};
    let view = ModelView::new(80, 24, (220, 220, 220), (15, 20, 24));
    let revisions = view.lock().unwrap().subscribe();
    let (events, events_rx) = watch::channel(0u64);
    let ring = Arc::new(SignalRing::new());
    let started = Instant::now();
    let stopped_view = view.clone();
    let waiter = ring.clone();
    let task = tokio::spawn(async move {
        let stop = vec!["stop".to_string()];
        let result = wait_for_signal(
            waiter.subscribe(),
            revisions,
            events_rx,
            move || projection_unavailable(&stopped_view),
            || None,
            &TextConditions::default(),
            move || waiter.first_matching(0, &stop),
            0,
            started,
            started + Duration::from_millis(5_000),
            RepaintPlan::skip(),
        )
        .await;
        (result, started.elapsed())
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(100)).await;
    assert!(!task.is_finished(), "a live projection keeps waiting");
    view.lock().unwrap().invalidate("simulated source gap");
    tokio::task::yield_now().await;
    let (verdict, waited) = task.await.unwrap();
    assert!(verdict.signal.is_none() && verdict.exited);
    assert_eq!(waited, Duration::from_millis(100));
    drop(events);
}

const REPAINT: RepaintPlan = RepaintPlan {
    repaint: Duration::from_millis(1_500),
    settle: Duration::from_millis(150),
};
fn repaint(outcome: RepaintOutcome, waited_ms: u64) -> Option<RepaintReport> {
    Some(RepaintReport {
        outcome,
        waited: Duration::from_millis(waited_ms),
    })
}
fn stop_ring() -> Arc<crate::terminal_renderer::SignalRing> {
    Arc::new(crate::terminal_renderer::SignalRing::new())
}

#[tokio::test(start_paused = true)]
async fn repaint_already_when_a_quiet_revision_preceded_the_signal() {
    let ring = stop_ring();
    let (fixture, task) = start_signal_repaint(ring.clone(), 0, 5_000, REPAINT);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(100)).await;
    bump(&fixture.revisions);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(200)).await;
    ring.push("a", incoming("stop"), 0);
    tokio::task::yield_now().await;
    let (verdict, waited) = task.await.unwrap();
    assert_eq!(verdict.signal.as_ref().map(|s| s.seq), Some(1));
    assert_eq!(verdict.signal_at, Some(Duration::from_millis(300)));
    assert_eq!(verdict.repaint, repaint(RepaintOutcome::Already, 0));
    assert_eq!(waited, Duration::from_millis(300));
}

#[tokio::test(start_paused = true)]
async fn repaint_settles_after_a_revision_that_follows_the_signal() {
    let ring = stop_ring();
    let (fixture, task) = start_signal_repaint(ring.clone(), 0, 5_000, REPAINT);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(100)).await;
    ring.push("a", incoming("stop"), 0);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(300)).await;
    assert!(!task.is_finished(), "returned at the signal");
    bump(&fixture.revisions);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(149)).await;
    assert!(!task.is_finished(), "settled before the quiet window");
    tokio::time::advance(Duration::from_millis(1)).await;
    let (verdict, waited) = task.await.unwrap();
    assert_eq!(verdict.signal_at, Some(Duration::from_millis(100)));
    assert_eq!(verdict.repaint, repaint(RepaintOutcome::Settled, 450));
    assert_eq!(waited, Duration::from_millis(550));
}

/// A screen quiet since the start without any revision is not `already`.
#[tokio::test(start_paused = true)]
async fn repaint_none_when_nothing_lands_within_the_repaint_window() {
    let ring = stop_ring();
    let (_fixture, task) = start_signal_repaint(ring.clone(), 0, 5_000, REPAINT);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(400)).await;
    ring.push("a", incoming("stop"), 0);
    tokio::task::yield_now().await;
    assert!(!task.is_finished(), "quiet without a change is not already");
    tokio::time::advance(Duration::from_millis(1_499)).await;
    assert!(!task.is_finished(), "gave up before repaint_ms");
    tokio::time::advance(Duration::from_millis(1)).await;
    let (verdict, waited) = task.await.unwrap();
    assert_eq!(verdict.signal_at, Some(Duration::from_millis(400)));
    assert_eq!(verdict.repaint, repaint(RepaintOutcome::None, 1_500));
    assert_eq!(waited, Duration::from_millis(1_900));
}

/// The quiet window is measured from the revision that preceded the signal, so the wait ends
/// 100 ms after the signal — it neither idles `repaint` nor reports `none`.
#[tokio::test(start_paused = true)]
async fn repaint_settles_from_a_revision_that_preceded_the_signal() {
    let ring = stop_ring();
    let (fixture, task) = start_signal_repaint(ring.clone(), 0, 5_000, REPAINT);
    tokio::task::yield_now().await;
    bump(&fixture.revisions);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(50)).await;
    ring.push("a", incoming("stop"), 0);
    tokio::task::yield_now().await;
    assert!(!task.is_finished(), "a revision 50 ms old is not already");
    tokio::time::advance(Duration::from_millis(99)).await;
    assert!(!task.is_finished(), "settled before the quiet window");
    tokio::time::advance(Duration::from_millis(1)).await;
    let (verdict, waited) = task.await.unwrap();
    assert_eq!(verdict.signal_at, Some(Duration::from_millis(50)));
    assert_eq!(verdict.repaint, repaint(RepaintOutcome::Settled, 100));
    assert_eq!(waited, Duration::from_millis(150));
}

#[tokio::test(start_paused = true)]
async fn repaint_windows_are_bounded_by_the_budget() {
    let ring = stop_ring();
    let (fixture, task) = start_signal_repaint(ring.clone(), 0, 800, REPAINT);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(100)).await;
    ring.push("a", incoming("stop"), 0);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(700)).await;
    let (verdict, waited) = task.await.unwrap();
    assert_eq!(verdict.repaint, repaint(RepaintOutcome::None, 700));
    assert_eq!(waited, Duration::from_millis(800));
    drop(fixture);

    let (fixture, task) = start_signal_repaint(ring.clone(), 1, 1_000, REPAINT);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(100)).await;
    ring.push("b", incoming("stop"), 0);
    for _ in 0..30 {
        tokio::task::yield_now().await;
        bump(&fixture.revisions);
        tokio::time::advance(Duration::from_millis(30)).await;
    }
    let (verdict, waited) = task.await.unwrap();
    assert_eq!(verdict.signal.as_ref().map(|s| s.seq), Some(2));
    assert_eq!(verdict.repaint, repaint(RepaintOutcome::Unsettled, 900));
    assert_eq!(waited, Duration::from_millis(1_000));
}

#[tokio::test(start_paused = true)]
async fn repaint_timer_rereads_the_revision_and_an_exit_ends_the_phase() {
    for _ in 0..32 {
        let ring = stop_ring();
        let (fixture, task) = start_signal_repaint(ring.clone(), 0, 5_000, REPAINT);
        tokio::task::yield_now().await;
        ring.push("a", incoming("stop"), 0);
        tokio::task::yield_now().await;
        bump(&fixture.revisions);
        tokio::task::yield_now().await;
        let revisions = fixture.revisions.clone();
        let helper = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            bump(&revisions);
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(150)).await;
        helper.await.unwrap();
        tokio::task::yield_now().await;
        assert!(
            !task.is_finished(),
            "settled although a revision was pending"
        );
        // The pending revision is observed at the timer wake (150 ms), so the quiet window restarts there.
        tokio::time::advance(Duration::from_millis(150)).await;
        let (verdict, waited) = task.await.unwrap();
        assert_eq!(verdict.repaint, repaint(RepaintOutcome::Settled, 300));
        assert_eq!(waited, Duration::from_millis(300));
    }

    let ring = stop_ring();
    let (fixture, task) = start_signal_repaint(ring.clone(), 0, 5_000, REPAINT);
    tokio::task::yield_now().await;
    ring.push("a", incoming("stop"), 0);
    tokio::task::yield_now().await;
    bump(&fixture.revisions);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(50)).await;
    fixture.stopped.store(true, Ordering::SeqCst);
    bump(&fixture.events);
    tokio::task::yield_now().await;
    let (verdict, waited) = task.await.unwrap();
    assert!(
        verdict.signal.is_some() && !verdict.exited,
        "the signal wins"
    );
    assert_eq!(verdict.repaint, repaint(RepaintOutcome::Unsettled, 50));
    assert_eq!(waited, Duration::from_millis(50));
}

/// Quiet for 1 ms less than `settle` is a frame boundary (past the 30 ms gap): returned at
/// once but not settled.
#[tokio::test(start_paused = true)]
async fn no_signal_on_a_quiet_screen_returns_at_the_deadline() {
    let (_fixture, task) = start_signal_repaint(stop_ring(), 0, 300, REPAINT);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(299)).await;
    assert!(!task.is_finished(), "ended before the budget");
    tokio::time::advance(Duration::from_millis(1)).await;
    let (verdict, waited) = task.await.unwrap();
    assert!(verdict.signal.is_none() && !verdict.exited);
    assert!(verdict.settled, "idle for the whole budget: settled");
    assert_eq!(waited, Duration::from_millis(300), "idle: at the deadline");
    for (quiet_ms, settled) in [(150, true), (149, false)] {
        let (fixture, task) = start_signal_repaint(stop_ring(), 0, 300, REPAINT);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(300 - quiet_ms)).await;
        bump(&fixture.revisions);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(quiet_ms)).await;
        let (verdict, waited) = task.await.unwrap();
        assert_eq!(verdict.settled, settled, "quiet for {quiet_ms} ms");
        assert_eq!(
            waited,
            Duration::from_millis(300),
            "quiet for {quiet_ms} ms"
        );
    }
}

/// `settle_ms: 0`: quiet for 0 ms holds trivially, so the budget end is `settled` even under streaming output.
#[tokio::test(start_paused = true)]
async fn no_signal_with_settle_zero_returns_at_the_deadline_settled() {
    let plan = RepaintPlan {
        repaint: Duration::from_millis(1_500),
        settle: Duration::ZERO,
    };
    for streaming in [false, true] {
        let (fixture, task) = start_signal_repaint(stop_ring(), 0, 300, plan);
        for _ in 0..30 {
            tokio::task::yield_now().await;
            if streaming {
                bump(&fixture.revisions);
                tokio::task::yield_now().await;
            }
            tokio::time::advance(Duration::from_millis(10)).await;
        }
        let (verdict, waited) = task.await.unwrap();
        assert!(verdict.signal.is_none() && !verdict.exited);
        assert!(verdict.settled, "settle 0 is quiet (streaming {streaming})");
        assert_eq!(
            waited,
            Duration::from_millis(300),
            "no grace with settle 0 (streaming {streaming})"
        );
    }
}

/// Spinner: the last revision 10 ms before the deadline; the loop waits for the 30 ms frame gap.
#[tokio::test(start_paused = true)]
async fn no_signal_under_a_spinner_returns_on_the_next_frame_boundary() {
    let (fixture, task) = start_signal_repaint(stop_ring(), 0, 300, REPAINT);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(90)).await;
    bump(&fixture.revisions);
    tokio::task::yield_now().await;
    for _ in 0..2 {
        tokio::time::advance(Duration::from_millis(100)).await;
        bump(&fixture.revisions);
        tokio::task::yield_now().await;
    }
    // Now 290, the third repaint; the deadline (300) passes 10 ms later.
    tokio::time::advance(Duration::from_millis(10)).await;
    assert!(!task.is_finished(), "returned at the deadline mid-frame");
    tokio::time::advance(Duration::from_millis(19)).await;
    assert!(!task.is_finished(), "returned before the frame gap");
    tokio::time::advance(Duration::from_millis(1)).await;
    let (verdict, waited) = task.await.unwrap();
    assert!(verdict.signal.is_none() && !verdict.exited);
    assert!(!verdict.settled, "20 ms past the deadline is not settled");
    assert_eq!(
        waited,
        Duration::from_millis(320),
        "the frame boundary, not the deadline"
    );
}

/// Streaming: a revision every 10 ms never leaves a frame gap, so the loop gives up at `deadline + settle`.
#[tokio::test(start_paused = true)]
async fn no_signal_under_streaming_output_returns_at_the_grace_end() {
    let ring = stop_ring();
    let (fixture, task) = start_signal_repaint(ring.clone(), 0, 300, REPAINT);
    for step in 0..45 {
        tokio::task::yield_now().await;
        bump(&fixture.revisions);
        tokio::task::yield_now().await;
        if step == 35 {
            ring.push("a", incoming("stop"), 0);
        }
        tokio::time::advance(Duration::from_millis(10)).await;
    }
    tokio::task::yield_now().await;
    let (verdict, waited) = task.await.unwrap();
    assert!(
        verdict.signal.is_none(),
        "a signal during the grace is not looked for"
    );
    assert!(!verdict.exited);
    assert!(!verdict.settled, "still streaming at the grace end");
    assert_eq!(waited, Duration::from_millis(450), "deadline + settle");
}

/// An exit during the grace ends it at once as exited.
#[tokio::test(start_paused = true)]
async fn an_exit_during_the_frame_boundary_grace_is_exited() {
    let (fixture, task) = start_signal_repaint(stop_ring(), 0, 300, REPAINT);
    for _ in 0..33 {
        tokio::task::yield_now().await;
        bump(&fixture.revisions);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(10)).await;
    }
    assert!(!task.is_finished(), "streaming: still in the grace");
    fixture.stopped.store(true, Ordering::SeqCst);
    bump(&fixture.events);
    tokio::task::yield_now().await;
    let (verdict, waited) = task.await.unwrap();
    assert!(verdict.signal.is_none() && verdict.exited);
    assert!(!verdict.settled);
    assert_eq!(waited, Duration::from_millis(330));
}

struct Fixture {
    revisions: watch::Sender<u64>,
    events: watch::Sender<u64>,
    stopped: Arc<AtomicBool>,
}
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
fn start(
    settle_ms: u64,
    budget_ms: u64,
) -> (Fixture, tokio::task::JoinHandle<(Progress, Duration)>) {
    let (revisions, revisions_rx) = watch::channel(0u64);
    let (events, events_rx) = watch::channel(0u64);
    let stopped = Arc::new(AtomicBool::new(false));
    let flag = stopped.clone();
    let started = Instant::now();
    let task = tokio::spawn(async move {
        let progress = wait_for_change(
            revisions_rx,
            events_rx,
            move || flag.load(Ordering::SeqCst),
            0,
            started + Duration::from_millis(budget_ms),
            Duration::from_millis(settle_ms),
        )
        .await;
        (progress, started.elapsed())
    });
    (
        Fixture {
            revisions,
            events,
            stopped,
        },
        task,
    )
}
fn bump(sender: &watch::Sender<u64>) {
    sender.send_modify(|value| *value += 1);
}

/// A helper task's earlier sleep fires in the same driver pass as the quiet timer and bumps the
/// revision before the waiter is polled. Repeated because `select!` picks among ready branches at random.
#[tokio::test(start_paused = true)]
async fn timer_completion_rechecks_the_revision_before_settling() {
    for _ in 0..64 {
        let (fixture, task) = start(150, 5_000);
        tokio::task::yield_now().await;
        bump(&fixture.revisions);
        tokio::task::yield_now().await;
        let revisions = fixture.revisions.clone();
        let helper = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            bump(&revisions);
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(200)).await;
        helper.await.unwrap();
        tokio::task::yield_now().await;
        assert!(
            !task.is_finished(),
            "settled although a revision was pending"
        );
        tokio::time::advance(Duration::from_millis(150)).await;
        let (progress, waited) = task.await.unwrap();
        assert!(progress.changed && progress.settled && !progress.exited);
        assert_eq!(waited, Duration::from_millis(350));
    }
}

#[tokio::test(start_paused = true)]
async fn timer_completion_rechecks_the_stopped_state() {
    let (fixture, task) = start(150, 5_000);
    tokio::task::yield_now().await;
    bump(&fixture.revisions);
    tokio::task::yield_now().await;
    fixture.stopped.store(true, Ordering::SeqCst);
    tokio::time::advance(Duration::from_millis(150)).await;
    let (progress, _) = task.await.unwrap();
    assert!(progress.exited && !progress.settled);
}

/// Protocol events (acks, ownership) must not extend the quiet window.
#[tokio::test(start_paused = true)]
async fn protocol_events_do_not_restart_the_quiet_window() {
    let (fixture, task) = start(150, 5_000);
    tokio::task::yield_now().await;
    bump(&fixture.revisions);
    tokio::task::yield_now().await;
    for _ in 0..4 {
        tokio::time::advance(Duration::from_millis(40)).await;
        bump(&fixture.events);
        tokio::task::yield_now().await;
    }
    let (progress, waited) = task.await.unwrap();
    assert!(progress.changed && progress.settled);
    assert_eq!(waited, Duration::from_millis(160));
}

#[tokio::test(start_paused = true)]
async fn revisions_extend_the_quiet_window_until_the_burst_ends() {
    let (fixture, task) = start(150, 5_000);
    for _ in 0..20 {
        tokio::task::yield_now().await;
        bump(&fixture.revisions);
        tokio::time::advance(Duration::from_millis(30)).await;
    }
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    tokio::time::advance(Duration::from_millis(150)).await;
    let (progress, waited) = task.await.unwrap();
    assert!(progress.changed && progress.settled);
    assert_eq!(waited, Duration::from_millis(750));

    let (fixture, task) = start(150, 300);
    for _ in 0..20 {
        tokio::task::yield_now().await;
        bump(&fixture.revisions);
        tokio::time::advance(Duration::from_millis(30)).await;
    }
    let (progress, waited) = task.await.unwrap();
    assert!(progress.changed && !progress.settled && !progress.exited);
    assert_eq!(waited, Duration::from_millis(300));
}

/// The production composition itself is covered by the MCP test
/// `change_wait_stops_when_the_output_source_disconnects`.
#[tokio::test(start_paused = true)]
async fn invalidated_projection_stops_the_wait_before_the_budget() {
    use crate::terminal_renderer::ModelView;
    let view = ModelView::new(80, 24, (220, 220, 220), (15, 20, 24));
    let revisions = view.lock().unwrap().subscribe();
    let (events, events_rx) = watch::channel(0u64);
    let started = Instant::now();
    let stopped_view = view.clone();
    let task = tokio::spawn(async move {
        let progress = wait_for_change(
            revisions,
            events_rx,
            move || projection_unavailable(&stopped_view),
            0,
            started + Duration::from_millis(5_000),
            Duration::from_millis(150),
        )
        .await;
        (progress, started.elapsed())
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(100)).await;
    assert!(!task.is_finished(), "a live projection keeps waiting");
    view.lock().unwrap().invalidate("simulated source gap");
    tokio::task::yield_now().await;
    let (progress, waited) = task.await.unwrap();
    assert!(progress.exited && !progress.changed && !progress.settled);
    assert_eq!(waited, Duration::from_millis(100));
    drop(events);
}

#[tokio::test(start_paused = true)]
async fn no_change_reports_unchanged_at_the_budget() {
    let (fixture, task) = start(150, 300);
    tokio::time::advance(Duration::from_millis(300)).await;
    let (progress, waited) = task.await.unwrap();
    assert!(!progress.changed && !progress.settled && !progress.exited);
    assert_eq!(waited, Duration::from_millis(300));
    drop(fixture);
}
