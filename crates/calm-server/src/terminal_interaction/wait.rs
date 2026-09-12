//! Change and signal waiting for observations: wake on renderer revisions,
//! hook signals and client protocol events, never on a sleep-poll loop.
//! Waiting is presentation; it never touches a receipt or the physical action.
use super::client::Client;
use crate::terminal_hooks::{DEFAULT_SIGNAL_EVENTS, TERMINAL_SIGNAL_EVENTS};
use crate::terminal_renderer::{SharedModelView, Signal};
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::watch;
// tokio's Instant equals std's on a live runtime and follows the paused
// clock in tests, so the loop and its timers share one time base.
use tokio::time::Instant;

pub const WAIT_MS_MAX: u64 = 20_000;
pub const SETTLE_MS_MAX: u64 = 2_000;
pub const SETTLE_MS_DEFAULT: u64 = 150;
/// Budget when `wait_ms` is omitted in change mode. Elapsed mode keeps 0 so an
/// observation without waiting arguments stays an immediate read.
pub const CHANGE_WAIT_MS_DEFAULT: u64 = 2_000;
/// Budget when `wait_ms` is omitted in signal mode (#1620): a model answer
/// takes seconds, and the wait ends early on the signal anyway.
pub const SIGNAL_WAIT_MS_DEFAULT: u64 = 15_000;
/// Signal mode (#1628): how long after the signal to wait for the first
/// repaint. Claude's `Stop` hook fires before the TUI paints the answer, so
/// a signal readback that returned at once would still show the spinner.
pub const REPAINT_MS_MAX: u64 = 5_000;
pub const REPAINT_MS_DEFAULT: u64 = 1_500;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WaitFor {
    #[default]
    Elapsed,
    Change,
    Signal,
}
impl WaitFor {
    fn name(self) -> &'static str {
        match self {
            Self::Elapsed => "elapsed",
            Self::Change => "change",
            Self::Signal => "signal",
        }
    }
}

/// Validated waiting arguments shared by observe and action readbacks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WaitPlan {
    pub mode: WaitFor,
    pub budget_ms: u64,
    pub settle_ms: u64,
    /// Signal mode only: snake_case hook events that end the wait.
    pub signal_events: Vec<String>,
    /// Signal mode only: how long after the signal to wait for a repaint
    /// (0 returns at the signal as before #1628). 0 in the other modes.
    pub repaint_ms: u64,
}
impl WaitPlan {
    /// `wait_ms == None` selects the mode's default budget:
    /// [`CHANGE_WAIT_MS_DEFAULT`] for change, [`SIGNAL_WAIT_MS_DEFAULT`] for
    /// signal, 0 for elapsed. `signal_events == None` selects
    /// [`DEFAULT_SIGNAL_EVENTS`] in signal mode; `repaint_ms == None` selects
    /// [`REPAINT_MS_DEFAULT`] there.
    pub fn new(
        wait_for: Option<WaitFor>,
        wait_ms: Option<u64>,
        settle_ms: Option<u64>,
        signal_events: Option<Vec<String>>,
        repaint_ms: Option<u64>,
    ) -> Result<Self> {
        let mode = wait_for.unwrap_or_default();
        let wait_ms = wait_ms.unwrap_or(match mode {
            WaitFor::Change => CHANGE_WAIT_MS_DEFAULT,
            WaitFor::Signal => SIGNAL_WAIT_MS_DEFAULT,
            WaitFor::Elapsed => 0,
        });
        ensure!(wait_ms <= WAIT_MS_MAX, "wait_ms must be 0..{WAIT_MS_MAX}");
        ensure!(
            settle_ms.is_none_or(|settle| settle <= SETTLE_MS_MAX),
            "settle_ms must be 0..{SETTLE_MS_MAX}"
        );
        ensure!(
            settle_ms.is_none() || matches!(mode, WaitFor::Change | WaitFor::Signal),
            "settle_ms requires wait_for=change or wait_for=signal"
        );
        ensure!(
            signal_events.is_none() || mode == WaitFor::Signal,
            "signal_events requires wait_for=signal"
        );
        ensure!(
            repaint_ms.is_none_or(|repaint| repaint <= REPAINT_MS_MAX),
            "repaint_ms must be 0..{REPAINT_MS_MAX}"
        );
        ensure!(
            repaint_ms.is_none() || mode == WaitFor::Signal,
            "repaint_ms requires wait_for=signal"
        );
        let signal_events = match mode {
            WaitFor::Signal => {
                let events = signal_events.unwrap_or_else(|| {
                    DEFAULT_SIGNAL_EVENTS
                        .iter()
                        .map(|e| e.to_string())
                        .collect()
                });
                ensure!(
                    !events.is_empty(),
                    "signal_events must name at least one event"
                );
                for event in &events {
                    ensure!(
                        TERMINAL_SIGNAL_EVENTS.contains(&event.as_str()),
                        "unknown signal event {event:?}; expected one of {TERMINAL_SIGNAL_EVENTS:?}"
                    );
                }
                events
            }
            _ => Vec::new(),
        };
        Ok(Self {
            mode,
            budget_ms: wait_ms,
            settle_ms: settle_ms.unwrap_or(SETTLE_MS_DEFAULT),
            signal_events,
            repaint_ms: match mode {
                WaitFor::Signal => repaint_ms.unwrap_or(REPAINT_MS_DEFAULT),
                _ => 0,
            },
        })
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.budget_ms <= WAIT_MS_MAX
                && self.settle_ms <= SETTLE_MS_MAX
                && self.repaint_ms <= REPAINT_MS_MAX,
            "observation wait exceeds limits"
        );
        ensure!(
            self.mode != WaitFor::Signal || !self.signal_events.is_empty(),
            "signal wait without events"
        );
        Ok(())
    }
}

/// What the wait did, reported verbatim on the observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    Changed,
    Unchanged,
    Exited,
    Elapsed,
    Signal,
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
}
/// Signal mode (#1628): the screen's behaviour after the signal arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepaintOutcome {
    /// A revision had already landed since the baseline and the screen had
    /// been quiet for `settle_ms` when the signal arrived.
    Already,
    /// A revision landed after the signal and the screen then stayed quiet
    /// for `settle_ms`.
    Settled,
    /// No revision landed within `repaint_ms` of the signal (or the budget).
    None,
    /// A revision landed after the signal but the budget ended before the
    /// screen was quiet for `settle_ms`.
    Unsettled,
    /// `repaint_ms: 0`: returned at the signal without looking at the screen.
    Skipped,
}
impl RepaintOutcome {
    pub fn name(self) -> &'static str {
        match self {
            Self::Already => "already",
            Self::Settled => "settled",
            Self::None => "none",
            Self::Unsettled => "unsettled",
            Self::Skipped => "skipped",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepaintReport {
    pub outcome: RepaintOutcome,
    /// Time spent after the signal.
    pub waited: Duration,
}
impl RepaintReport {
    fn to_json(self) -> Value {
        json!({"outcome":self.outcome.name(),"waited_ms":millis(self.waited)})
    }
}
fn millis(duration: Duration) -> u64 {
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
        report
    }
}

/// Wait according to `plan` against `baseline` (the revision whose change the
/// caller cares about) and `signal_baseline` (the signal seq a signal wait
/// counts from). Change mode returns once the projection revision differs
/// from the baseline and stayed quiet for `settle_ms`, or at the budget, or
/// when the process exited / the client went away. Signal mode returns once a
/// signal with `seq > signal_baseline` and an event in `plan.signal_events`
/// exists, or at the budget, or on exit / disconnect.
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
        } = wait_for_signal(
            signals, revisions, events, stopped, find, baseline, started, deadline, repaint,
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
        return report;
    }
    let settle = Duration::from_millis(plan.settle_ms);
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

/// Signal mode (#1628): the repaint window after the signal and the quiet
/// window that ends it. `repaint == 0` skips the phase.
#[derive(Clone, Copy, Debug)]
pub struct RepaintPlan {
    pub repaint: Duration,
    pub settle: Duration,
}
impl RepaintPlan {
    #[cfg(test)]
    pub fn skip() -> Self {
        Self {
            repaint: Duration::ZERO,
            settle: Duration::ZERO,
        }
    }
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
}
/// Revision bookkeeping across both phases of a signal wait: whether any
/// revision landed since the wait's baseline and when the last one did. A
/// revision already above the baseline when the wait starts counts as a
/// change at the start (its real time is unknown, so the quiet window is
/// measured from the start, never earlier).
struct Repaint {
    seen: u64,
    changed: bool,
    last_change: Instant,
}
impl Repaint {
    fn new(baseline: u64, current: u64, started: Instant) -> Self {
        Self {
            seen: current,
            changed: current != baseline,
            last_change: started,
        }
    }
    /// Record `current`; true when it is a new revision.
    fn observe(&mut self, current: u64, now: Instant) -> bool {
        if current == self.seen {
            return false;
        }
        self.seen = current;
        self.changed = true;
        self.last_change = now;
        true
    }
    fn quiet_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last_change)
    }
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
/// `repaint.repaint` is zero.
#[allow(clippy::too_many_arguments)]
async fn wait_for_signal(
    mut signals: watch::Receiver<u64>,
    mut revisions: watch::Receiver<u64>,
    mut events: watch::Receiver<u64>,
    stopped: impl Fn() -> bool,
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
    let report = settle_after_signal(
        revisions,
        events,
        stopped,
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
    }
}

/// The repaint phase of a signal wait (#1628). At the signal: a revision
/// since the baseline that has been quiet for `settle` is `Already`. Else
/// the loop waits for the next revision until `repaint` after the signal
/// (or the budget) → `None`, and once one lands, until the screen has been
/// quiet for `settle` → `Settled`, or the budget → `Unsettled`. Exit,
/// disconnect and projection invalidation (`stopped`, re-read on every wake)
/// end the phase with the verdict the screen had reached. As in change
/// mode, a timer wake re-reads the revision before settling.
async fn settle_after_signal(
    mut revisions: watch::Receiver<u64>,
    mut events: watch::Receiver<u64>,
    stopped: impl Fn() -> bool,
    screen: &mut Repaint,
    signal_at: Instant,
    deadline: Instant,
    plan: RepaintPlan,
) -> RepaintReport {
    let report = |outcome| RepaintReport {
        outcome,
        waited: Instant::now().saturating_duration_since(signal_at),
    };
    if plan.repaint.is_zero() {
        return report(RepaintOutcome::Skipped);
    }
    screen.observe(*revisions.borrow_and_update(), signal_at);
    if screen.changed && screen.quiet_for(signal_at) >= plan.settle {
        return report(RepaintOutcome::Already);
    }
    let repaint_deadline = (signal_at + plan.repaint).min(deadline);
    let mut changed_after_signal = false;
    let verdict = |changed_after_signal: bool| {
        if changed_after_signal {
            RepaintOutcome::Unsettled
        } else {
            RepaintOutcome::None
        }
    };
    loop {
        events.borrow_and_update();
        let now = Instant::now();
        if screen.observe(*revisions.borrow_and_update(), now) {
            changed_after_signal = true;
        }
        if changed_after_signal && screen.quiet_for(now) >= plan.settle {
            return report(RepaintOutcome::Settled);
        }
        if stopped() {
            return report(verdict(changed_after_signal));
        }
        let timer = if changed_after_signal {
            (screen.last_change + plan.settle).min(deadline)
        } else {
            repaint_deadline
        };
        if now >= timer {
            return report(verdict(changed_after_signal));
        }
        let ended = tokio::select! {
            result = revisions.changed() => result.is_err(),
            result = events.changed() => result.is_err(),
            _ = tokio::time::sleep_until(timer) => false,
        };
        if ended {
            return report(verdict(changed_after_signal));
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
mod tests {
    use super::*;
    use tokio::sync::watch;

    fn plan(wait_for: Option<WaitFor>, wait_ms: Option<u64>) -> WaitPlan {
        WaitPlan::new(wait_for, wait_ms, None, None, None).unwrap()
    }

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
        };
        assert_eq!(report.to_json()["outcome"], "signal");
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
        };
        assert_eq!(report.to_json()["signal_at_ms"], Value::Null);
        assert_eq!(report.to_json()["repaint"], Value::Null);
        for (outcome, name) in [
            (RepaintOutcome::Already, "already"),
            (RepaintOutcome::Settled, "settled"),
            (RepaintOutcome::None, "none"),
            (RepaintOutcome::Unsettled, "unsettled"),
            (RepaintOutcome::Skipped, "skipped"),
        ] {
            assert_eq!(outcome.name(), name);
        }
    }

    #[test]
    fn omitted_wait_ms_defaults_per_mode() {
        assert_eq!(plan(None, None).budget_ms, 0);
        assert_eq!(plan(Some(WaitFor::Elapsed), None).budget_ms, 0);
        assert_eq!(
            plan(Some(WaitFor::Change), None).budget_ms,
            CHANGE_WAIT_MS_DEFAULT
        );
        assert_eq!(CHANGE_WAIT_MS_DEFAULT, 2_000);
        assert_eq!(plan(Some(WaitFor::Change), Some(0)).budget_ms, 0);
        assert_eq!(plan(Some(WaitFor::Change), Some(15_000)).budget_ms, 15_000);
        assert!(
            WaitPlan::new(
                Some(WaitFor::Change),
                Some(WAIT_MS_MAX + 1),
                None,
                None,
                None
            )
            .is_err()
        );
        assert!(WaitPlan::new(None, None, Some(10), None, None).is_err());
        assert!(
            WaitPlan::new(Some(WaitFor::Change), None, None, None, Some(0)).is_err(),
            "repaint_ms outside signal mode"
        );
        assert!(WaitPlan::new(None, None, None, None, Some(0)).is_err());
        assert_eq!(plan(Some(WaitFor::Change), None).repaint_ms, 0);
    }

    /// #1620 signal mode: its own default budget, the default event set, the
    /// same 20 s ceiling, a validated event vocabulary and (#1628) a settle
    /// window plus a bounded repaint window.
    #[test]
    fn signal_mode_defaults_and_validation() {
        let signal = plan(Some(WaitFor::Signal), None);
        assert_eq!(signal.budget_ms, SIGNAL_WAIT_MS_DEFAULT);
        assert_eq!(SIGNAL_WAIT_MS_DEFAULT, 15_000);
        assert_eq!(signal.repaint_ms, REPAINT_MS_DEFAULT);
        assert_eq!(REPAINT_MS_DEFAULT, 1_500);
        assert_eq!(signal.settle_ms, SETTLE_MS_DEFAULT);
        let tuned = WaitPlan::new(Some(WaitFor::Signal), None, Some(300), None, Some(0)).unwrap();
        assert_eq!((tuned.settle_ms, tuned.repaint_ms), (300, 0));
        assert!(
            WaitPlan::new(
                Some(WaitFor::Signal),
                None,
                None,
                None,
                Some(REPAINT_MS_MAX + 1)
            )
            .is_err()
        );
        assert!(
            WaitPlan::new(
                Some(WaitFor::Signal),
                None,
                Some(SETTLE_MS_MAX + 1),
                None,
                None
            )
            .is_err()
        );
        assert!(
            WaitPlan {
                repaint_ms: REPAINT_MS_MAX + 1,
                ..plan(Some(WaitFor::Signal), None)
            }
            .validate()
            .is_err()
        );
        assert_eq!(
            signal.signal_events,
            vec!["stop", "notification", "permission_request", "session_end"]
        );
        assert!(plan(Some(WaitFor::Change), None).signal_events.is_empty());
        assert!(
            WaitPlan::new(
                Some(WaitFor::Signal),
                Some(WAIT_MS_MAX + 1),
                None,
                None,
                None
            )
            .is_err()
        );
        assert!(
            WaitPlan::new(
                Some(WaitFor::Change),
                None,
                None,
                Some(vec!["stop".into()]),
                None
            )
            .is_err()
        );
        assert!(WaitPlan::new(Some(WaitFor::Signal), None, None, Some(vec![]), None).is_err());
        assert!(
            WaitPlan::new(
                Some(WaitFor::Signal),
                None,
                None,
                Some(vec!["Stop".into()]),
                None
            )
            .is_err()
        );
        let only = WaitPlan::new(
            Some(WaitFor::Signal),
            Some(0),
            None,
            Some(vec!["session_end".into()]),
            None,
        )
        .unwrap();
        assert_eq!(only.signal_events, vec!["session_end"]);
        assert_eq!(only.budget_ms, 0);
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

    /// #1628 `none`: no revision within `repaint` of the signal. A revision
    /// that preceded the signal but was not yet quiet does not count as
    /// `already`; the loop still waits for the next one.
    #[tokio::test(start_paused = true)]
    async fn repaint_none_when_nothing_lands_within_the_repaint_window() {
        let ring = stop_ring();
        let (fixture, task) = start_signal_repaint(ring.clone(), 0, 5_000, REPAINT);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(100)).await;
        ring.push("a", incoming("stop"), 0);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(1_499)).await;
        assert!(!task.is_finished(), "gave up before repaint_ms");
        tokio::time::advance(Duration::from_millis(1)).await;
        let (verdict, waited) = task.await.unwrap();
        assert_eq!(verdict.repaint, repaint(RepaintOutcome::None, 1_500));
        assert_eq!(waited, Duration::from_millis(1_600));
        drop(fixture);

        let (fixture, task) = start_signal_repaint(ring.clone(), 1, 5_000, REPAINT);
        tokio::task::yield_now().await;
        bump(&fixture.revisions);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(50)).await;
        ring.push("b", incoming("stop"), 0);
        tokio::task::yield_now().await;
        assert!(!task.is_finished(), "a revision 50 ms old is not quiet");
        tokio::time::advance(Duration::from_millis(1_500)).await;
        let (verdict, _) = task.await.unwrap();
        assert_eq!(verdict.repaint, repaint(RepaintOutcome::None, 1_500));
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
