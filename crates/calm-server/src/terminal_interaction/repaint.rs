//! The repaint phase of a signal wait (#1628), moved out of `wait.rs` with
//! #1677 r16, which adds the text conditions: Claude's `Stop` hook fires
//! while the busy spinner (`esc to interrupt`) is still painted, so the
//! phase settles only once the screen is quiet AND the conditions
//! (`wait_text` present, `wait_text_absent` gone) hold on the current
//! screen, re-tested once per revision like text mode. Without conditions
//! the phase is exactly the #1628 one. Pure timing over channels, tested
//! under a paused clock.
use super::text_conditions::{ConditionState, TextConditions};
use super::wait::millis;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;

/// Signal mode (#1628): the screen's behaviour after the signal arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepaintOutcome {
    /// A revision had already landed since the baseline, the screen had
    /// been quiet for `settle_ms` when the signal arrived and the text
    /// conditions held on it.
    Already,
    /// A revision landed after the signal and the screen then stayed quiet
    /// for `settle_ms` with the text conditions holding.
    Settled,
    /// No revision landed within `repaint_ms` of the signal (or the budget).
    None,
    /// A revision landed but the budget ended before the screen was quiet
    /// for `settle_ms` with the text conditions holding.
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
    pub(super) fn to_json(self) -> Value {
        json!({"outcome":self.outcome.name(),"waited_ms":millis(self.waited)})
    }
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
/// Revision bookkeeping across both phases of a signal wait: whether any
/// revision landed since the wait's baseline and when the last one did. A
/// revision already above the baseline when the wait starts counts as a
/// change at the start (its real time is unknown, so the quiet window is
/// measured from the start, never earlier).
pub(super) struct Repaint {
    seen: u64,
    pub(super) changed: bool,
    last_change: Instant,
}
impl Repaint {
    pub(super) fn new(baseline: u64, current: u64, started: Instant) -> Self {
        Self {
            seen: current,
            changed: current != baseline,
            last_change: started,
        }
    }
    /// Record `current`; true when it is a new revision.
    pub(super) fn observe(&mut self, current: u64, now: Instant) -> bool {
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
/// The text conditions as tested on the screens of the repaint phase: one
/// capture per revision (never on a timer or protocol wake alone), vacuous
/// when there are no conditions.
struct Tested {
    at: Option<u64>,
    state: ConditionState,
    holds: bool,
}
impl Tested {
    fn new(conditions: &TextConditions) -> Self {
        Self {
            at: None,
            state: ConditionState::default(),
            holds: conditions.is_empty(),
        }
    }
    fn test(
        &mut self,
        conditions: &TextConditions,
        revision: u64,
        capture: &impl Fn() -> Option<(Vec<String>, u64)>,
    ) {
        if conditions.is_empty() || self.at == Some(revision) {
            return;
        }
        self.at = Some(revision);
        match capture() {
            Some((rows, _)) => {
                self.state = conditions.test(&rows).1;
                self.holds = self.state.holds();
            }
            // An unavailable projection holds nothing; `stopped` reports it.
            None => self.holds = false,
        }
    }
}

/// The repaint phase of a signal wait (#1628). At the signal: a revision
/// since the baseline that has been quiet for `settle`, on a screen where
/// the conditions hold, is `Already`. Else the loop keys on
/// `screen.changed` (any revision since the wait's baseline, before or
/// after the signal): while nothing has changed it waits for the first
/// revision until `repaint` after the signal (or the budget) → `None`; once
/// a change exists it waits until the screen has been quiet for `settle`
/// with the conditions holding → `Settled`, or the budget → `Unsettled`
/// (#1677 r16: while the conditions do not hold the phase keeps waiting for
/// further revisions, each re-tested, until the budget). So a revision 50 ms
/// before the signal settles 100 ms after it (settle 150) rather than idling
/// `repaint`. Exit, disconnect and projection invalidation (`stopped`,
/// re-read on every wake) end the phase with the verdict the screen had
/// reached. As in change mode, a timer wake re-reads the revision before
/// settling. Returns the verdict and the conditions' state on the last
/// tested screen.
#[allow(clippy::too_many_arguments)]
pub(super) async fn settle_after_signal(
    mut revisions: watch::Receiver<u64>,
    mut events: watch::Receiver<u64>,
    stopped: impl Fn() -> bool,
    capture: impl Fn() -> Option<(Vec<String>, u64)>,
    conditions: &TextConditions,
    screen: &mut Repaint,
    signal_at: Instant,
    deadline: Instant,
    plan: RepaintPlan,
) -> (RepaintReport, ConditionState) {
    let report = |outcome| RepaintReport {
        outcome,
        waited: Instant::now().saturating_duration_since(signal_at),
    };
    let mut tested = Tested::new(conditions);
    if plan.repaint.is_zero() {
        return (report(RepaintOutcome::Skipped), tested.state);
    }
    let current = *revisions.borrow_and_update();
    screen.observe(current, signal_at);
    tested.test(conditions, current, &capture);
    if screen.changed && screen.quiet_for(signal_at) >= plan.settle && tested.holds {
        return (report(RepaintOutcome::Already), tested.state);
    }
    let repaint_deadline = (signal_at + plan.repaint).min(deadline);
    // A change exists (since the baseline) but never went quiet with the
    // conditions holding before the phase ended → `Unsettled`; no change at
    // all → `None`.
    let verdict = |changed: bool| {
        if changed {
            RepaintOutcome::Unsettled
        } else {
            RepaintOutcome::None
        }
    };
    loop {
        events.borrow_and_update();
        let now = Instant::now();
        let current = *revisions.borrow_and_update();
        if screen.observe(current, now) {
            tested.test(conditions, current, &capture);
        }
        if screen.changed && screen.quiet_for(now) >= plan.settle && tested.holds {
            return (report(RepaintOutcome::Settled), tested.state);
        }
        if stopped() {
            return (report(verdict(screen.changed)), tested.state);
        }
        let timer = if !screen.changed {
            repaint_deadline
        } else if tested.holds {
            (screen.last_change + plan.settle).min(deadline)
        } else {
            // The conditions do not hold: only a further revision can end
            // the phase before the budget.
            deadline
        };
        if now >= timer {
            return (report(verdict(screen.changed)), tested.state);
        }
        let ended = tokio::select! {
            result = revisions.changed() => result.is_err(),
            result = events.changed() => result.is_err(),
            _ = tokio::time::sleep_until(timer) => false,
        };
        if ended {
            return (report(verdict(screen.changed)), tested.state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    const REPAINT: RepaintPlan = RepaintPlan {
        repaint: Duration::from_millis(1_500),
        settle: Duration::from_millis(150),
    };
    struct Fixture {
        revisions: watch::Sender<u64>,
        events: watch::Sender<u64>,
        stopped: Arc<AtomicBool>,
        rows: Arc<Mutex<Vec<String>>>,
        revision: Arc<AtomicU64>,
        captures: Arc<AtomicUsize>,
    }
    impl Fixture {
        /// Paint `rows` as a new revision.
        fn paint(&self, rows: &[&str]) {
            *self.rows.lock().unwrap() = rows.iter().map(|s| (*s).to_owned()).collect();
            let revision = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
            self.revisions.send_replace(revision);
        }
    }
    type Task = tokio::task::JoinHandle<((RepaintReport, ConditionState), Duration)>;
    /// Drive the phase directly, as if the signal had just arrived: the
    /// screen's `rows` are the current revision (`painted_ms_ago` before the
    /// signal, counted as a change since the baseline when nonzero).
    fn start(
        present: &[&str],
        absent: &[&str],
        rows: &[&str],
        painted_ms_ago: Option<u64>,
        budget_ms: u64,
    ) -> (Fixture, Task) {
        let initial = if painted_ms_ago.is_some() { 1 } else { 0 };
        let (revisions, revisions_rx) = watch::channel(initial);
        let (events, events_rx) = watch::channel(0u64);
        let stopped = Arc::new(AtomicBool::new(false));
        let rows = Arc::new(Mutex::new(
            rows.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        ));
        let revision = Arc::new(AtomicU64::new(initial));
        let captures = Arc::new(AtomicUsize::new(0));
        let conditions = TextConditions {
            present: present.iter().map(|s| (*s).to_owned()).collect(),
            absent: absent.iter().map(|s| (*s).to_owned()).collect(),
        };
        let (flag, screen, current, count) = (
            stopped.clone(),
            rows.clone(),
            revision.clone(),
            captures.clone(),
        );
        let signal_at = Instant::now();
        let started = signal_at - Duration::from_millis(painted_ms_ago.unwrap_or(0));
        let task = tokio::spawn(async move {
            let mut repaint = Repaint::new(0, initial, started);
            let result = settle_after_signal(
                revisions_rx,
                events_rx,
                move || flag.load(Ordering::SeqCst),
                move || {
                    count.fetch_add(1, Ordering::SeqCst);
                    Some((
                        screen.lock().unwrap().clone(),
                        current.load(Ordering::SeqCst),
                    ))
                },
                &conditions,
                &mut repaint,
                signal_at,
                signal_at + Duration::from_millis(budget_ms),
                REPAINT,
            )
            .await;
            (result, signal_at.elapsed())
        });
        (
            Fixture {
                revisions,
                events,
                stopped,
                rows,
                revision,
                captures,
            },
            task,
        )
    }
    fn state(present: Option<bool>, absent: Option<bool>) -> ConditionState {
        ConditionState { present, absent }
    }

    /// Without conditions the phase is #1628's: quiet at the signal is
    /// `already`, a later revision settles after the quiet window, nothing
    /// within the repaint window is `none`; no capture is ever taken.
    #[tokio::test(start_paused = true)]
    async fn without_conditions_the_phase_is_unchanged_and_never_captures() {
        let (fixture, task) = start(&[], &[], &["busy"], Some(200), 5_000);
        let ((report, conditions), waited) = task.await.unwrap();
        assert_eq!(report.outcome, RepaintOutcome::Already);
        assert_eq!(conditions, state(None, None));
        assert_eq!(waited, Duration::ZERO);
        assert_eq!(fixture.captures.load(Ordering::SeqCst), 0);
        let (fixture, task) = start(&[], &[], &["idle"], None, 5_000);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(300)).await;
        fixture.paint(&["busy"]);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(150)).await;
        let ((report, _), waited) = task.await.unwrap();
        assert_eq!(report.outcome, RepaintOutcome::Settled);
        assert_eq!(waited, Duration::from_millis(450));
        assert_eq!(fixture.captures.load(Ordering::SeqCst), 0);
        let (_fixture, task) = start(&[], &[], &["idle"], None, 5_000);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(1_500)).await;
        let ((report, _), waited) = task.await.unwrap();
        assert_eq!(report.outcome, RepaintOutcome::None);
        assert_eq!(waited, Duration::from_millis(1_500));
    }

    /// The Claude case: the signal arrives with the busy hint painted and
    /// quiet (would be `already`), the absent condition does not hold, so
    /// the phase keeps waiting; the answer paints 400 ms later without the
    /// hint and the phase settles after the quiet window; `already` is not
    /// used since the screen that settled came after the signal.
    #[tokio::test(start_paused = true)]
    async fn absent_pattern_at_the_signal_defers_the_settle_until_a_revision_removes_it() {
        let (fixture, task) = start(
            &[],
            &["esc to interrupt"],
            &["· Noodling… (esc to interrupt)"],
            Some(200),
            5_000,
        );
        tokio::task::yield_now().await;
        assert!(!task.is_finished(), "already with the hint still there");
        tokio::time::advance(Duration::from_millis(400)).await;
        tokio::task::yield_now().await;
        assert!(!task.is_finished(), "settled while the hint was shown");
        assert_eq!(fixture.captures.load(Ordering::SeqCst), 1);
        fixture.paint(&["❯ the answer"]);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(149)).await;
        assert!(!task.is_finished(), "settled before the quiet window");
        tokio::time::advance(Duration::from_millis(1)).await;
        let ((report, conditions), waited) = task.await.unwrap();
        assert_eq!(report.outcome, RepaintOutcome::Settled);
        assert_eq!(report.waited, Duration::from_millis(550));
        assert_eq!(conditions, state(None, Some(true)));
        assert_eq!(waited, Duration::from_millis(550));
        assert_eq!(
            fixture.captures.load(Ordering::SeqCst),
            2,
            "one per revision"
        );
        // A screen that already holds at the signal is `already`.
        let (_fixture, task) = start(&[], &["esc to interrupt"], &["❯ done"], Some(200), 5_000);
        let ((report, conditions), waited) = task.await.unwrap();
        assert_eq!(report.outcome, RepaintOutcome::Already);
        assert_eq!(conditions, state(None, Some(true)));
        assert_eq!(waited, Duration::ZERO);
    }

    /// The conditions are re-tested on every revision, not only at the
    /// signal: two repaints keep the hint, the third removes it, and only
    /// then does the quiet window count; a repaint that brings the hint
    /// back returns the phase to waiting.
    #[tokio::test(start_paused = true)]
    async fn conditions_are_retested_on_every_revision() {
        let (fixture, task) = start(&["❯"], &["esc to interrupt"], &["thinking"], None, 5_000);
        tokio::task::yield_now().await;
        for frame in [
            "· Noodling… (esc to interrupt)",
            "· Pondering… (esc to interrupt)",
        ] {
            tokio::time::advance(Duration::from_millis(100)).await;
            fixture.paint(&[frame]);
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(200)).await;
            tokio::task::yield_now().await;
            assert!(!task.is_finished(), "{frame}: settled with the hint shown");
        }
        fixture.paint(&["❯ answer", "· esc to interrupt"]);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(200)).await;
        tokio::task::yield_now().await;
        assert!(!task.is_finished(), "present held, absent did not");
        fixture.paint(&["❯ answer", ""]);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(150)).await;
        let ((report, conditions), waited) = task.await.unwrap();
        assert_eq!(report.outcome, RepaintOutcome::Settled);
        assert_eq!(conditions, state(Some(true), Some(true)));
        assert_eq!(waited, Duration::from_millis(950));
        assert_eq!(fixture.captures.load(Ordering::SeqCst), 5);
    }

    /// The budget ends with the hint still shown: `unsettled` (a revision
    /// existed), the state says which side failed; with no revision at all
    /// the verdict stays `none` after the repaint window, conditions or not.
    #[tokio::test(start_paused = true)]
    async fn budget_with_the_pattern_present_is_unsettled_and_no_revision_is_none() {
        let (fixture, task) = start(&[], &["busy"], &["busy"], Some(200), 800);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(799)).await;
        tokio::task::yield_now().await;
        assert!(!task.is_finished(), "ended before the budget");
        tokio::time::advance(Duration::from_millis(1)).await;
        let ((report, conditions), waited) = task.await.unwrap();
        assert_eq!(report.outcome, RepaintOutcome::Unsettled);
        assert_eq!(conditions, state(None, Some(false)));
        assert_eq!(waited, Duration::from_millis(800));
        assert_eq!(fixture.captures.load(Ordering::SeqCst), 1);
        let (fixture, task) = start(&[], &["busy"], &["busy"], None, 5_000);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(1_500)).await;
        let ((report, conditions), waited) = task.await.unwrap();
        assert_eq!(report.outcome, RepaintOutcome::None);
        assert_eq!(conditions, state(None, Some(false)), "tested at the signal");
        assert_eq!(waited, Duration::from_millis(1_500));
        assert_eq!(fixture.captures.load(Ordering::SeqCst), 1);
        // An exit while the hint is shown ends the phase with the verdict
        // reached so far and the state of the last tested screen.
        let (fixture, task) = start(&[], &["busy"], &["busy"], Some(200), 5_000);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(50)).await;
        fixture.stopped.store(true, Ordering::SeqCst);
        fixture.events.send_modify(|value| *value += 1);
        tokio::task::yield_now().await;
        let ((report, conditions), waited) = task.await.unwrap();
        assert_eq!(report.outcome, RepaintOutcome::Unsettled);
        assert_eq!(conditions, state(None, Some(false)));
        assert_eq!(waited, Duration::from_millis(50));
    }
}
