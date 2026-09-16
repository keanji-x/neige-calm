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
    /// Change mode: the screen still equals the baseline at the budget.
    Unchanged,
    Exited,
    Elapsed,
    Signal,
    /// Signal mode (#1692): the budget ended without a matching signal — a
    /// fact about the ring, not about the screen (which may have moved).
    NoSignal,
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
            WaitOutcome::NoSignal => "no_signal",
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
/// exists, or on exit / disconnect, or at the budget — then on a frame
/// boundary, at most `settle_ms` later (#1692). Text mode (#1666)
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
            settled,
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
            (None, false) => WaitOutcome::NoSignal,
        };
        // With a signal the repaint phase's verdict is the settle fact;
        // without one it is the frame-boundary phase's (#1692).
        let settled = match repaint {
            Some(repaint) => matches!(
                repaint.outcome,
                RepaintOutcome::Already | RepaintOutcome::Settled
            ),
            None => settled,
        };
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
    /// No signal (#1692): whether the screen was quiet for `settle` when the
    /// frame-boundary phase ended. Meaningless with a signal (the repaint
    /// verdict says it) and false on exit.
    pub settled: bool,
}
/// One Ink frame is written in a burst of PTY chunks a few ms apart; a gap
/// of 30 ms separates frames, also during a spinner that repaints every
/// ~100 ms (#1692).
const FRAME_GAP: Duration = Duration::from_millis(30);
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
/// deadline is reported as exited, never as no signal. Once the signal is
/// found the wait continues in [`settle_after_signal`] (#1628) unless
/// `repaint.repaint` is zero; `capture` and `conditions` (#1677 r16) are
/// the text conditions that phase tests, one capture per revision. A budget
/// that ends without a signal continues in [`frame_boundary`] (#1692) so
/// the capture that follows lands on a frame boundary whenever one occurs
/// within the grace; a signal that lands during that grace is not looked
/// for — the budget is over.
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
    let none = |exited: bool, settled: bool| SignalWait {
        signal: None,
        exited,
        signal_at: None,
        repaint: None,
        conditions: ConditionState::default(),
        settled,
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
            return none(true, false);
        }
        let ended = if Instant::now() >= deadline {
            true
        } else {
            tokio::select! {
                result = signals.changed() => result.is_err(),
                result = revisions.changed() => result.is_err(),
                result = events.changed() => result.is_err(),
                _ = tokio::time::sleep_until(deadline) => true,
            }
        };
        if ended {
            if let Some(signal) = find() {
                break signal;
            }
            if stopped() {
                return none(true, false);
            }
            let (exited, settled) = frame_boundary(
                &mut revisions,
                &mut events,
                &stopped,
                &mut screen,
                deadline,
                repaint.settle,
            )
            .await;
            return none(exited, settled);
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
        settled: false,
    }
}

/// The frame-boundary phase of a signal wait whose budget ended without a
/// signal (#1692). Round 19 captured a torn Ink frame because the timeout
/// branch returned at the deadline instant, wherever the PTY chunk stream
/// happened to be; the signal branch and the change and text modes all
/// settle first, only this branch did not. It returns once the projection
/// has been quiet for `min(settle, FRAME_GAP)` — a frame boundary, reached
/// within a few tens of ms even under a spinner — or at `deadline + settle`
/// at the latest (a streaming log never pauses). `settled` is true only
/// when the screen was quiet for the full `settle` (an idle screen returns
/// at the deadline itself). The ring is not inspected here: the budget is
/// over, a signal landing now is the next wait's. Exit, disconnect,
/// projection invalidation and a closed channel end the phase at once.
/// Returns `(exited, settled)`.
async fn frame_boundary(
    revisions: &mut watch::Receiver<u64>,
    events: &mut watch::Receiver<u64>,
    stopped: &impl Fn() -> bool,
    screen: &mut Repaint,
    deadline: Instant,
    settle: Duration,
) -> (bool, bool) {
    let grace_end = deadline + settle;
    let frame_gap = settle.min(FRAME_GAP);
    loop {
        events.borrow_and_update();
        let now = Instant::now();
        screen.observe(*revisions.borrow_and_update(), now);
        if stopped() {
            return (true, false);
        }
        let quiet = screen.quiet_for(now);
        if quiet >= settle {
            return (false, true);
        }
        if quiet >= frame_gap || now >= grace_end {
            return (false, false);
        }
        // `frame_gap <= settle`, so the frame timer is never later than the
        // settle timer; the grace end bounds both.
        let timer = (now + (frame_gap - quiet)).min(grace_end);
        let ended = tokio::select! {
            result = revisions.changed() => result.is_err(),
            result = events.changed() => result.is_err(),
            _ = tokio::time::sleep_until(timer) => false,
        };
        if ended {
            return (stopped(), false);
        }
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
mod tests;
