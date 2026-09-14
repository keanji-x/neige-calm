//! Change, signal and text waiting for observations: wake on renderer
//! revisions, hook signals and client protocol events, never on a sleep-poll
//! loop. Waiting is presentation; it never touches a receipt or the physical
//! action. The argument contract lives in `wait_plan.rs`, the text loop in
//! `text_wait.rs` (#1666), the repaint phase of a signal wait in
//! `repaint.rs` (#1677 r16); this file dispatches and runs the change and
//! signal loops.
use super::client::Client;
use super::repaint::{Repaint, settle_after_signal};
pub use super::repaint::{RepaintOutcome, RepaintPlan, RepaintReport};
use super::text_conditions::{ConditionState, TextConditions};
use super::text_wait::{self, TextMatch, TextWait};
pub use super::wait_plan::{WaitFor, WaitPlan};
use crate::terminal_renderer::{SharedModelView, Signal};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::watch;
// tokio's Instant equals std's on a live runtime and follows the paused
// clock in tests, so the loop and its timers share one time base.
use tokio::time::Instant;

/// What the wait did, reported verbatim on the observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    Changed,
    Unchanged,
    Exited,
    Elapsed,
    Signal,
    /// Text mode (#1666): a pattern is on the live viewport.
    Matched,
    /// Text mode: no pattern was on the viewport when the budget ended.
    Unmatched,
}
pub struct WaitReport {
    pub mode: WaitFor,
    pub outcome: WaitOutcome,
    pub waited: Duration,
    pub settled: bool,
    /// The revision the wait compared against (reported in elapsed mode too,
    /// where it is the same baseline a change wait would have used).
    pub baseline: u64,
    /// The signal seq the wait compared against (every mode reports it, so a
    /// caller can see which signals a later signal wait would consider new).
    pub signal_baseline: u64,
    /// Signal mode: the signal that ended the wait.
    pub signal: Option<Signal>,
    /// Signal mode: elapsed time when the signal arrived.
    pub signal_at: Option<Duration>,
    /// Signal mode: what happened on the screen after the signal (#1628).
    pub repaint: Option<RepaintReport>,
    /// Text mode (#1666): the match the wait ended on, if any.
    pub text: Option<TextMatch>,
    /// Text and signal modes (#1677 r16): which text conditions held on the
    /// screen the wait (or the repaint phase) ended on.
    pub conditions: Option<ConditionState>,
}
pub(super) fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
impl WaitReport {
    pub fn to_json(&self) -> Value {
        let outcome = match self.outcome {
            WaitOutcome::Changed => "changed",
            WaitOutcome::Unchanged => "unchanged",
            WaitOutcome::Exited => "exited",
            WaitOutcome::Elapsed => "elapsed",
            WaitOutcome::Signal => "signal",
            WaitOutcome::Matched => "matched",
            WaitOutcome::Unmatched => "unmatched",
        };
        let mut report = json!({"mode":self.mode.name(),"outcome":outcome,
            "waited_ms":millis(self.waited),"settled":self.settled,
            "baseline_revision":self.baseline.to_string(),
            "baseline_signal_seq":self.signal_baseline});
        if self.mode == WaitFor::Signal {
            report["signal"] = self
                .signal
                .as_ref()
                .map(Signal::to_json)
                .unwrap_or(Value::Null);
            report["signal_at_ms"] = self.signal_at.map(millis).map_or(Value::Null, Value::from);
            report["repaint"] = self
                .repaint
                .map(RepaintReport::to_json)
                .unwrap_or(Value::Null);
        }
        if self.mode == WaitFor::Text {
            report["text"] = self
                .text
                .as_ref()
                .map(TextMatch::to_json)
                .unwrap_or(Value::Null);
        }
        if matches!(self.mode, WaitFor::Text | WaitFor::Signal) {
            report["conditions"] = self.conditions.unwrap_or_default().to_json();
        }
        report
    }
}

/// Wait according to `plan` against `baseline` (the revision whose change the
/// caller cares about) and `signal_baseline` (the signal seq a signal wait
/// counts from). Change mode returns once the projection revision differs
/// from the baseline and stayed quiet for `settle_ms`, or at the budget, or
/// when the process exited / the client went away. Signal mode returns once a
/// signal with `seq > signal_baseline` and an event in `plan.signal_events`
/// exists, or at the budget, or on exit / disconnect. Text mode (#1666)
/// returns once a `plan.wait_text` pattern is on a live viewport row and the
/// screen then stayed quiet for `settle_ms`, or at the budget, or on exit.
pub async fn wait(
    client: &Client,
    plan: &WaitPlan,
    baseline: u64,
    signal_baseline: u64,
) -> WaitReport {
    let started = Instant::now();
    let budget = Duration::from_millis(plan.budget_ms);
    let report = |outcome, waited, settled, signal| WaitReport {
        mode: plan.mode,
        outcome,
        waited,
        settled,
        baseline,
        signal_baseline,
        signal,
        signal_at: None,
        repaint: None,
        text: None,
        conditions: None,
    };
    if plan.mode == WaitFor::Elapsed {
        if plan.budget_ms > 0 {
            tokio::time::sleep(budget).await;
        }
        return report(WaitOutcome::Elapsed, budget, false, None);
    }
    let deadline = started + budget;
    let events = client.changed();
    let stopped = || {
        client
            .screen
            .lock()
            .map(|state| !state.available || state.exited)
            .unwrap_or(true)
            || client
                .entry
                .exit
                .lock()
                .map(|exit| exit.is_some())
                .unwrap_or(true)
            || projection_unavailable(&client.entry.handle.model_view)
    };
    // Both modes subscribe to the projection: a revision is what a change
    // wait is for, and an invalidation (`ModelView::invalidate`, e.g. the
    // output source disconnecting) must end a signal wait too instead of
    // leaving it — and the connection's input serial — parked to the budget.
    let revisions = match client.entry.handle.model_view.lock() {
        Ok(view) => view.subscribe(),
        Err(_) => {
            return report(WaitOutcome::Unchanged, started.elapsed(), false, None);
        }
    };
    // One capture per revision wake, the view lock dropped before the rows
    // are tested; the frame's own revision names the tested screen (text
    // mode, and the signal repaint phase under text conditions).
    let capture = || {
        client
            .entry
            .handle
            .model_view
            .lock()
            .ok()
            .and_then(|view| view.capture(0).ok())
            .map(|(frame, revision)| (frame.text, revision))
    };
    let conditions = plan.conditions();
    if plan.mode == WaitFor::Signal {
        let ring = &client.entry.signals;
        let signals = ring.subscribe();
        let find = || ring.first_matching(signal_baseline, &plan.signal_events);
        let repaint = RepaintPlan {
            repaint: Duration::from_millis(plan.repaint_ms),
            settle: Duration::from_millis(plan.settle_ms),
        };
        let SignalWait {
            signal,
            exited,
            signal_at,
            repaint,
            conditions,
        } = wait_for_signal(
            signals,
            revisions,
            events,
            stopped,
            capture,
            &conditions,
            find,
            baseline,
            started,
            deadline,
            repaint,
        )
        .await;
        let outcome = match (&signal, exited) {
            (Some(_), _) => WaitOutcome::Signal,
            (None, true) => WaitOutcome::Exited,
            (None, false) => WaitOutcome::Unchanged,
        };
        let settled = repaint.is_some_and(|repaint| {
            matches!(
                repaint.outcome,
                RepaintOutcome::Already | RepaintOutcome::Settled
            )
        });
        let mut report = report(outcome, started.elapsed(), settled, signal);
        report.signal_at = signal_at;
        report.repaint = repaint;
        report.conditions = Some(conditions);
        return report;
    }
    let settle = Duration::from_millis(plan.settle_ms);
    if plan.mode == WaitFor::Text {
        let TextWait {
            matched,
            holds,
            conditions,
            settled,
            exited,
        } = text_wait::wait_for_text(
            revisions,
            events,
            stopped,
            capture,
            &conditions,
            started,
            deadline,
            settle,
        )
        .await;
        let outcome = if exited {
            WaitOutcome::Exited
        } else if holds {
            WaitOutcome::Matched
        } else {
            WaitOutcome::Unmatched
        };
        let mut report = report(outcome, started.elapsed(), settled && !exited, None);
        report.text = matched;
        report.conditions = Some(conditions);
        return report;
    }
    let Progress {
        changed,
        settled,
        exited,
    } = wait_for_change(revisions, events, stopped, baseline, deadline, settle).await;
    let outcome = if exited {
        WaitOutcome::Exited
    } else if changed {
        WaitOutcome::Changed
    } else {
        WaitOutcome::Unchanged
    };
    report(outcome, started.elapsed(), settled && !exited, None)
}

/// `ModelView::invalidate` wakes revision subscribers without a new
/// revision; a change wait must stop there (the capture after it fails
/// explicitly) instead of idling to its budget.
fn projection_unavailable(model_view: &SharedModelView) -> bool {
    model_view
        .lock()
        .map(|view| view.capture(0).is_err())
        .unwrap_or(true)
}

/// The signal loop's verdict.
#[derive(Debug)]
pub struct SignalWait {
    pub signal: Option<Signal>,
    /// Exit / disconnect / invalidation ended the wait without a signal.
    pub exited: bool,
    /// Elapsed since the wait started when the signal was found.
    pub signal_at: Option<Duration>,
    /// Present exactly when a signal was found.
    pub repaint: Option<RepaintReport>,
    /// Which text conditions held on the last screen the repaint phase
    /// tested (#1677 r16; null sides when there were none).
    pub conditions: ConditionState,
}
/// The signal-mode loop (#1620), separated from the client so its timing can
/// be tested under a paused clock. `signals` is the ring's seq channel,
/// `revisions` the projection revision channel (a revision itself never ends
/// a signal wait; its wake re-evaluates `stopped`, which covers projection
/// invalidation, and is recorded for the repaint phase), `events` the
/// client's protocol channel, `find` the ring lookup for a matching signal
/// above the baseline. The ring is inspected before every select and again
/// on timeout, after the seq channel version has been marked seen, so a
/// signal that lands between the lookup and the select is never missed and
/// one that lands as the budget expires is still reported; the timeout
/// branch re-reads `stopped` as well, so an exit that coincides with the
/// deadline is reported as exited, never as unchanged. Once the signal is
/// found the wait continues in [`settle_after_signal`] (#1628) unless
/// `repaint.repaint` is zero; `capture` and `conditions` (#1677 r16) are
/// the text conditions that phase tests, one capture per revision.
#[allow(clippy::too_many_arguments)]
async fn wait_for_signal(
    mut signals: watch::Receiver<u64>,
    mut revisions: watch::Receiver<u64>,
    mut events: watch::Receiver<u64>,
    stopped: impl Fn() -> bool,
    capture: impl Fn() -> Option<(Vec<String>, u64)>,
    conditions: &TextConditions,
    find: impl Fn() -> Option<Signal>,
    baseline: u64,
    started: Instant,
    deadline: Instant,
    repaint: RepaintPlan,
) -> SignalWait {
    let mut screen = Repaint::new(baseline, *revisions.borrow(), started);
    let none = |exited: bool| SignalWait {
        signal: None,
        exited,
        signal_at: None,
        repaint: None,
        conditions: ConditionState::default(),
    };
    // A matching signal wins over every other verdict; otherwise `stopped`
    // is read at the moment the wait ends.
    let signal = loop {
        signals.borrow_and_update();
        screen.observe(*revisions.borrow_and_update(), Instant::now());
        events.borrow_and_update();
        if let Some(signal) = find() {
            break signal;
        }
        if stopped() {
            return none(true);
        }
        if Instant::now() >= deadline {
            return none(false);
        }
        let ended = tokio::select! {
            result = signals.changed() => result.is_err(),
            result = revisions.changed() => result.is_err(),
            result = events.changed() => result.is_err(),
            _ = tokio::time::sleep_until(deadline) => true,
        };
        if ended {
            match find() {
                Some(signal) => break signal,
                None => return none(stopped()),
            }
        }
    };
    let signal_at = Instant::now();
    let (report, conditions) = settle_after_signal(
        revisions,
        events,
        stopped,
        capture,
        conditions,
        &mut screen,
        signal_at,
        deadline,
        repaint,
    )
    .await;
    SignalWait {
        signal: Some(signal),
        exited: false,
        signal_at: Some(signal_at.saturating_duration_since(started)),
        repaint: Some(report),
        conditions,
    }
}

struct Progress {
    changed: bool,
    settled: bool,
    exited: bool,
}

/// The change-mode loop, separated from the client so its timing can be
/// tested under a paused clock. `revisions` is the projection revision
/// channel, `events` the client's protocol channel (ownership, acks, exit,
/// disconnect). Only a new revision starts or extends the quiet window; a
/// protocol event re-evaluates `stopped` and otherwise leaves the window as
/// it was. When the quiet timer completes, the revision and stopped state are
/// read again before `settled` is reported: with several branches ready,
/// `select!` may pick the timer although a newer revision is already waiting.
async fn wait_for_change(
    mut revisions: watch::Receiver<u64>,
    mut events: watch::Receiver<u64>,
    stopped: impl Fn() -> bool,
    baseline: u64,
    deadline: Instant,
    settle: Duration,
) -> Progress {
    let mut progress = Progress {
        changed: false,
        settled: false,
        exited: false,
    };
    let mut seen = baseline;
    let mut quiet_until = deadline;
    loop {
        events.borrow_and_update();
        let current = *revisions.borrow_and_update();
        let now = Instant::now();
        if current != seen {
            seen = current;
            progress.changed = true;
            quiet_until = (now + settle).min(deadline);
        }
        if stopped() {
            progress.exited = true;
            break;
        }
        if now >= deadline {
            break;
        }
        tokio::select! {
            result = revisions.changed() => {
                if result.is_err() {
                    break;
                }
            }
            result = events.changed() => {
                if result.is_err() {
                    break;
                }
            }
            _ = tokio::time::sleep_until(quiet_until) => {
                if *revisions.borrow_and_update() != seen {
                    // A revision landed while the timer was completing: the
                    // window restarts at the top of the loop.
                    continue;
                }
                if stopped() {
                    progress.exited = true;
                    break;
                }
                if progress.changed && quiet_until < deadline {
                    progress.settled = true;
                }
                break;
            }
        }
    }
    progress
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::watch;

    /// `baseline_revision` is the string form of the revision the wait
    /// compared against, in both modes, next to the existing fields.
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
            report.to_json().get("repaint").is_none()
                && report.to_json().get("signal_at_ms").is_none(),
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
        // #1677 r16: the conditions block in signal mode names both sides.
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
            outcome: WaitOutcome::Unchanged,
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
        // #1666 text mode: `text` is the match or null, next to the same
        // baseline fields; the signal fields stay absent.
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
    /// Spawn a signal wait for `stop` above `baseline` with every sender kept
    /// alive by the fixture (a dropped sender ends the loop at once, which
    /// would make a budget test vacuous).
    /// The loop's verdict and the paused-clock time it took.
    type SignalTask = tokio::task::JoinHandle<(SignalWait, Duration)>;
    fn start_signal(
        ring: Arc<crate::terminal_renderer::SignalRing>,
        baseline: u64,
        budget_ms: u64,
    ) -> (SignalFixture, SignalTask) {
        start_signal_repaint(ring, baseline, budget_ms, RepaintPlan::skip())
    }
    /// #1628: the same, with a repaint plan; the revision channel starts at
    /// 0 and the wait's revision baseline is 0.
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

    /// Signal loop: a matching signal above the baseline ends the wait
    /// whether it was already in the ring or arrives during the wait;
    /// protocol events and revisions only re-check stopped.
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

    /// The budget: with every sender alive the loop runs to the deadline and
    /// reports no signal after exactly the budget; a signal pushed at the
    /// deadline itself is still reported (the ring is re-read on timeout).
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

        // A matching signal that becomes visible to the lookup without any
        // channel wake (only the deadline timer wakes the loop) is still
        // reported: the timeout branch re-reads the ring instead of
        // returning unchanged.
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

    /// An exit that coincides with the deadline is reported as exited: the
    /// timeout branch re-reads `stopped` instead of returning unchanged.
    #[tokio::test(start_paused = true)]
    async fn signal_loop_timeout_rechecks_the_stopped_state() {
        use crate::terminal_renderer::SignalRing;
        let ring = Arc::new(SignalRing::new());
        let (fixture, task) = start_signal(ring, 0, 300);
        tokio::task::yield_now().await;
        // The process exits without any wake of the loop (no protocol event
        // reaches it before the timer), so only the timer branch can see it.
        fixture.stopped.store(true, Ordering::SeqCst);
        tokio::time::advance(Duration::from_millis(300)).await;
        let (verdict, waited) = task.await.unwrap();
        assert!(verdict.signal.is_none());
        assert!(verdict.exited, "exit at the deadline reported as unchanged");
        assert_eq!(waited, Duration::from_millis(300));
    }

    /// An invalidated projection wakes a signal wait through the revision
    /// subscription and, composed with `projection_unavailable`, ends it as
    /// exited before the budget (same treatment as change mode).
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

    /// #1628 `already`: a revision since the baseline that has been quiet
    /// for `settle` when the signal arrives ends the wait at the signal.
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

    /// #1628 `settled`: no revision at the signal; the repaint lands 300 ms
    /// later and the wait ends once it has been quiet for `settle`.
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

    /// #1628 `none`: no revision within `repaint` of the signal. A screen
    /// that has been quiet since the start (longer than `settle`) without
    /// any revision is not `already`; the loop waits for the first one.
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

    /// #1628 `settled` from a change that preceded the signal: the revision
    /// lands at t=0, the signal 50 ms later (not yet quiet, so not
    /// `already`), nothing else follows. The quiet window is measured from
    /// the revision, so the wait ends 100 ms after the signal — it neither
    /// idles `repaint` nor reports `none`.
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

    /// #1628: the repaint window and the quiet window are both bounded by
    /// the budget: `none` at the budget when nothing landed, `unsettled` when
    /// revisions were still landing.
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

    /// #1628: a revision that lands as the quiet timer completes restarts
    /// the window (the timer wake re-reads the revision), and an exit during
    /// the repaint phase ends it with the verdict reached so far while the
    /// signal is kept.
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
            // The pending revision is observed at the timer wake (150 ms),
            // so the quiet window restarts there, as in change mode.
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

    /// The quiet timer and a revision notification become ready in the same
    /// poll: a helper task's earlier sleep fires in the same driver pass as
    /// the waiter's quiet timer and bumps the revision before the waiter is
    /// polled. The loop must not report a settled screen that has already
    /// moved on. Repeated because `select!` picks among ready branches at
    /// random.
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

    /// Every revision restarts the window; a sustained burst settles only
    /// after it ends, and a budget that ends mid-burst reports unsettled.
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

    /// An invalidated projection wakes the loop without a revision; with the
    /// `projection_unavailable` predicate that production `wait()` composes
    /// into `stopped`, the loop ends at once as exited rather than idling to
    /// the budget. The production composition itself is covered by the MCP
    /// test `change_wait_stops_when_the_output_source_disconnects`.
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
}
