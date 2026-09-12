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
}
impl WaitPlan {
    /// `wait_ms == None` selects the mode's default budget:
    /// [`CHANGE_WAIT_MS_DEFAULT`] for change, [`SIGNAL_WAIT_MS_DEFAULT`] for
    /// signal, 0 for elapsed. `signal_events == None` selects
    /// [`DEFAULT_SIGNAL_EVENTS`] in signal mode.
    pub fn new(
        wait_for: Option<WaitFor>,
        wait_ms: Option<u64>,
        settle_ms: Option<u64>,
        signal_events: Option<Vec<String>>,
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
            settle_ms.is_none() || mode == WaitFor::Change,
            "settle_ms requires wait_for=change"
        );
        ensure!(
            signal_events.is_none() || mode == WaitFor::Signal,
            "signal_events requires wait_for=signal"
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
        })
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.budget_ms <= WAIT_MS_MAX && self.settle_ms <= SETTLE_MS_MAX,
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
            "waited_ms":u64::try_from(self.waited.as_millis()).unwrap_or(u64::MAX),"settled":self.settled,
            "baseline_revision":self.baseline.to_string(),
            "baseline_signal_seq":self.signal_baseline});
        if self.mode == WaitFor::Signal {
            report["signal"] = self
                .signal
                .as_ref()
                .map(Signal::to_json)
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
    if plan.mode == WaitFor::Signal {
        let ring = &client.entry.signals;
        let signals = ring.subscribe();
        let find = || ring.first_matching(signal_baseline, &plan.signal_events);
        let (signal, exited) = wait_for_signal(signals, events, stopped, find, deadline).await;
        let outcome = match (&signal, exited) {
            (Some(_), _) => WaitOutcome::Signal,
            (None, true) => WaitOutcome::Exited,
            (None, false) => WaitOutcome::Unchanged,
        };
        return report(outcome, started.elapsed(), false, signal);
    }
    let settle = Duration::from_millis(plan.settle_ms);
    let revisions = match client.entry.handle.model_view.lock() {
        Ok(view) => view.subscribe(),
        Err(_) => {
            return report(WaitOutcome::Unchanged, started.elapsed(), false, None);
        }
    };
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

/// The signal-mode loop (#1620), separated from the client so its timing can
/// be tested under a paused clock. `signals` is the ring's seq channel,
/// `events` the client's protocol channel, `find` the ring lookup for a
/// matching signal above the baseline. The ring is inspected before every
/// select and again on timeout, after the seq channel version has been
/// marked seen, so a signal that lands between the lookup and the select is
/// never missed and one that lands as the budget expires is still reported.
async fn wait_for_signal(
    mut signals: watch::Receiver<u64>,
    mut events: watch::Receiver<u64>,
    stopped: impl Fn() -> bool,
    find: impl Fn() -> Option<Signal>,
    deadline: Instant,
) -> (Option<Signal>, bool) {
    loop {
        signals.borrow_and_update();
        events.borrow_and_update();
        if let Some(signal) = find() {
            return (Some(signal), false);
        }
        if stopped() {
            return (None, true);
        }
        if Instant::now() >= deadline {
            return (None, false);
        }
        tokio::select! {
            result = signals.changed() => {
                if result.is_err() {
                    return (find(), false);
                }
            }
            result = events.changed() => {
                if result.is_err() {
                    return (find(), stopped());
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                return (find(), false);
            }
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
        WaitPlan::new(wait_for, wait_ms, None, None).unwrap()
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
        };
        assert_eq!(report.to_json()["baseline_revision"], "7");
        assert_eq!(report.to_json()["outcome"], "changed");
        assert!(
            report.to_json().get("signal").is_none(),
            "signal only in signal mode"
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
        };
        assert_eq!(report.to_json()["outcome"], "signal");
        assert_eq!(report.to_json()["signal"], signal.to_json());
        assert_eq!(report.to_json()["baseline_signal_seq"], 3);
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
        assert!(WaitPlan::new(Some(WaitFor::Change), Some(WAIT_MS_MAX + 1), None, None).is_err());
        assert!(WaitPlan::new(None, None, Some(10), None).is_err());
    }

    /// #1620 signal mode: its own default budget, the default event set, the
    /// same 20 s ceiling, no settle window and a validated event vocabulary.
    #[test]
    fn signal_mode_defaults_and_validation() {
        let signal = plan(Some(WaitFor::Signal), None);
        assert_eq!(signal.budget_ms, SIGNAL_WAIT_MS_DEFAULT);
        assert_eq!(SIGNAL_WAIT_MS_DEFAULT, 15_000);
        assert_eq!(
            signal.signal_events,
            vec!["stop", "notification", "permission_request", "session_end"]
        );
        assert!(plan(Some(WaitFor::Change), None).signal_events.is_empty());
        assert!(WaitPlan::new(Some(WaitFor::Signal), Some(WAIT_MS_MAX + 1), None, None).is_err());
        assert!(WaitPlan::new(Some(WaitFor::Signal), None, Some(150), None).is_err());
        assert!(
            WaitPlan::new(Some(WaitFor::Change), None, None, Some(vec!["stop".into()])).is_err()
        );
        assert!(WaitPlan::new(Some(WaitFor::Signal), None, None, Some(vec![])).is_err());
        assert!(
            WaitPlan::new(Some(WaitFor::Signal), None, None, Some(vec!["Stop".into()])).is_err()
        );
        let only = WaitPlan::new(
            Some(WaitFor::Signal),
            Some(0),
            None,
            Some(vec!["session_end".into()]),
        )
        .unwrap();
        assert_eq!(only.signal_events, vec!["session_end"]);
        assert_eq!(only.budget_ms, 0);
    }

    /// Signal loop: a matching signal above the baseline ends the wait
    /// whether it was already in the ring, arrives during the wait, or lands
    /// exactly as the budget expires; protocol events only re-check stopped.
    #[tokio::test(start_paused = true)]
    async fn signal_loop_inspects_the_ring_before_select_and_on_timeout() {
        use crate::terminal_renderer::{IncomingSignal, SignalRing};
        let incoming = |event: &str| IncomingSignal {
            event: event.into(),
            notification_type: None,
            message: String::new(),
            claude_session_id: None,
        };
        let stop = vec!["stop".to_string()];
        // Already present above the baseline: returns without sleeping.
        let ring = Arc::new(SignalRing::new());
        ring.push("a", incoming("user_prompt_submit"), 0);
        ring.push("b", incoming("stop"), 0);
        let (events, events_rx) = watch::channel(0u64);
        let found = ring.clone();
        let (signal, exited) = wait_for_signal(
            ring.subscribe(),
            events_rx,
            || false,
            move || found.first_matching(0, &stop),
            Instant::now() + Duration::from_millis(5_000),
        )
        .await;
        assert_eq!(signal.map(|s| s.seq), Some(2));
        assert!(!exited);
        // Present but at or below the baseline: ignored; a later one wakes.
        let ring2 = ring.clone();
        let stop = vec!["stop".to_string()];
        let started = Instant::now();
        let events_rx = events.subscribe();
        let task = tokio::spawn(async move {
            wait_for_signal(
                ring2.subscribe(),
                events_rx,
                || false,
                move || ring2.first_matching(2, &stop),
                started + Duration::from_millis(5_000),
            )
            .await
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(100)).await;
        bump(&events);
        tokio::task::yield_now().await;
        assert!(
            !task.is_finished(),
            "protocol events do not end a signal wait"
        );
        ring.push("c", incoming("notification"), 0);
        tokio::task::yield_now().await;
        assert!(!task.is_finished(), "a non-matching event keeps waiting");
        ring.push("d", incoming("stop"), 0);
        tokio::task::yield_now().await;
        let (signal, exited) = task.await.unwrap();
        assert_eq!(signal.map(|s| s.seq), Some(4));
        assert!(!exited);
        // Budget without a match reports no signal; stopped reports exited.
        let ring3 = ring.clone();
        let stop = vec!["stop".to_string()];
        let (signal, exited) = wait_for_signal(
            ring.subscribe(),
            watch::channel(0u64).1,
            || false,
            move || ring3.first_matching(4, &stop),
            Instant::now() + Duration::from_millis(300),
        )
        .await;
        assert!(signal.is_none() && !exited);
        let (signal, exited) = wait_for_signal(
            ring.subscribe(),
            watch::channel(0u64).1,
            || true,
            || None,
            Instant::now() + Duration::from_millis(300),
        )
        .await;
        assert!(signal.is_none() && exited);
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
