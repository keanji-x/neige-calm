//! Change waiting for observations: wake on renderer revisions and client
//! protocol events, never on a sleep-poll loop. Waiting is presentation; it
//! never touches a receipt or the physical action.
use super::client::Client;
use crate::terminal_renderer::SharedModelView;
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

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WaitFor {
    #[default]
    Elapsed,
    Change,
}
impl WaitFor {
    fn name(self) -> &'static str {
        match self {
            Self::Elapsed => "elapsed",
            Self::Change => "change",
        }
    }
}

/// Validated waiting arguments shared by observe and action readbacks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WaitPlan {
    pub mode: WaitFor,
    pub budget_ms: u64,
    pub settle_ms: u64,
}
impl WaitPlan {
    /// `wait_ms == None` selects the mode's default budget:
    /// [`CHANGE_WAIT_MS_DEFAULT`] for change, 0 for elapsed.
    pub fn new(
        wait_for: Option<WaitFor>,
        wait_ms: Option<u64>,
        settle_ms: Option<u64>,
    ) -> Result<Self> {
        let mode = wait_for.unwrap_or_default();
        let wait_ms = wait_ms.unwrap_or(match mode {
            WaitFor::Change => CHANGE_WAIT_MS_DEFAULT,
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
        Ok(Self {
            mode,
            budget_ms: wait_ms,
            settle_ms: settle_ms.unwrap_or(SETTLE_MS_DEFAULT),
        })
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.budget_ms <= WAIT_MS_MAX && self.settle_ms <= SETTLE_MS_MAX,
            "observation wait exceeds limits"
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
}
pub struct WaitReport {
    pub mode: WaitFor,
    pub outcome: WaitOutcome,
    pub waited: Duration,
    pub settled: bool,
    /// The revision the wait compared against (reported in elapsed mode too,
    /// where it is the same baseline a change wait would have used).
    pub baseline: u64,
}
impl WaitReport {
    pub fn to_json(&self) -> Value {
        let outcome = match self.outcome {
            WaitOutcome::Changed => "changed",
            WaitOutcome::Unchanged => "unchanged",
            WaitOutcome::Exited => "exited",
            WaitOutcome::Elapsed => "elapsed",
        };
        json!({"mode":self.mode.name(),"outcome":outcome,
            "waited_ms":u64::try_from(self.waited.as_millis()).unwrap_or(u64::MAX),"settled":self.settled,
            "baseline_revision":self.baseline.to_string()})
    }
}

/// Wait according to `plan` against `baseline` (the revision whose change the
/// caller cares about). Change mode returns once the projection revision
/// differs from the baseline and stayed quiet for `settle_ms`, or at the
/// budget, or when the process exited / the client went away.
pub async fn wait(client: &Client, plan: WaitPlan, baseline: u64) -> WaitReport {
    let started = Instant::now();
    let budget = Duration::from_millis(plan.budget_ms);
    if plan.mode == WaitFor::Elapsed {
        if plan.budget_ms > 0 {
            tokio::time::sleep(budget).await;
        }
        return WaitReport {
            mode: plan.mode,
            outcome: WaitOutcome::Elapsed,
            waited: budget,
            settled: false,
            baseline,
        };
    }
    let deadline = started + budget;
    let settle = Duration::from_millis(plan.settle_ms);
    let revisions = match client.entry.handle.model_view.lock() {
        Ok(view) => view.subscribe(),
        Err(_) => {
            return WaitReport {
                mode: plan.mode,
                outcome: WaitOutcome::Unchanged,
                waited: started.elapsed(),
                settled: false,
                baseline,
            };
        }
    };
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
    WaitReport {
        mode: plan.mode,
        outcome,
        waited: started.elapsed(),
        settled: settled && !exited,
        baseline,
    }
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
        WaitPlan::new(wait_for, wait_ms, None).unwrap()
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
        };
        assert_eq!(
            report.to_json(),
            json!({"mode":"elapsed","outcome":"elapsed","waited_ms":0,"settled":false,"baseline_revision":"42"})
        );
        let report = WaitReport {
            mode: WaitFor::Change,
            outcome: WaitOutcome::Changed,
            waited: Duration::from_millis(812),
            settled: true,
            baseline: 7,
        };
        assert_eq!(report.to_json()["baseline_revision"], "7");
        assert_eq!(report.to_json()["outcome"], "changed");
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
        assert!(WaitPlan::new(Some(WaitFor::Change), Some(WAIT_MS_MAX + 1), None).is_err());
        assert!(WaitPlan::new(None, None, Some(10)).is_err());
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
