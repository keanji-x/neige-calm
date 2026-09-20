//! Text waiting: re-test the live viewport rows against the text conditions on every revision
//! and return once they have held for the settle window. "Until the screen shows X", not
//! "until X appears anew": a screen that already matches settles from the wait's start.
use super::text_conditions::{ConditionState, TextConditions};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;

/// A pattern found on a rendered row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextMatch {
    /// The matching pattern: first in argument order among the matches.
    pub pattern: String,
    /// The matching row for that pattern: first top-down.
    pub row: usize,
    /// The projection revision of the capture that confirmed the match.
    pub revision: u64,
    /// The match was present on the first capture and never vanished since.
    pub already: bool,
}
impl TextMatch {
    pub fn to_json(&self) -> Value {
        json!({"pattern":self.pattern,"row":self.row,"revision":self.revision.to_string(),"already":self.already})
    }
}

/// The text loop's verdict.
#[derive(Debug, PartialEq, Eq)]
pub struct TextWait {
    /// The present match on the screen the wait ended on, if any.
    pub matched: Option<TextMatch>,
    /// Every condition held on the screen the wait ended on.
    pub holds: bool,
    /// Which conditions held on that screen.
    pub conditions: ConditionState,
    /// The conditions held and the screen was quiet for the settle window.
    pub settled: bool,
    /// Exit / disconnect / invalidation ended the wait.
    pub exited: bool,
}

/// The text-mode loop, separated from the client so its timing can be tested under a paused
/// clock. Rows are captured once per revision wake — never on a protocol event or a timer wake
/// alone; only a revision starts or extends the quiet window. A timer wake re-reads the
/// revision before settling.
#[allow(clippy::too_many_arguments)]
pub async fn wait_for_text(
    mut revisions: watch::Receiver<u64>,
    mut events: watch::Receiver<u64>,
    stopped: impl Fn() -> bool,
    capture: impl Fn() -> Option<(Vec<String>, u64)>,
    conditions: &TextConditions,
    started: Instant,
    deadline: Instant,
    settle: Duration,
) -> TextWait {
    let mut seen: Option<u64> = None;
    let mut matched: Option<TextMatch> = None;
    let mut state = ConditionState::default();
    let mut holds = false;
    // Present since the first capture without interruption: `already`.
    let mut continuous = true;
    let mut last_change = started;
    let verdict =
        |matched: Option<TextMatch>, state: ConditionState, settled: bool, exited: bool| TextWait {
            matched,
            holds: state.holds(),
            conditions: state,
            settled,
            exited,
        };
    loop {
        events.borrow_and_update();
        let current = *revisions.borrow_and_update();
        let now = Instant::now();
        if seen != Some(current) {
            let first = seen.is_none();
            seen = Some(current);
            if !first {
                last_change = now;
            }
            // The capture's own revision names the screen that was tested; the channel value only says
            // a wake was due. An unavailable capture holds nothing.
            let tested = capture().map(|(rows, revision)| (conditions.test(&rows), revision));
            let found = tested
                .as_ref()
                .and_then(|((found, _), revision)| found.clone().map(|(p, r)| (p, r, *revision)));
            state = tested
                .as_ref()
                .map(|((_, state), _)| *state)
                .unwrap_or_default();
            holds = tested.is_some() && state.holds();
            continuous = continuous && found.is_some();
            matched = found.map(|(pattern, row, revision)| TextMatch {
                pattern,
                row,
                revision,
                already: continuous,
            });
        }
        let quiet_until = last_change + settle;
        // Exit, disconnect and invalidation win over a settled match: never `matched`/`settled`.
        if stopped() {
            return verdict(matched, state, false, true);
        }
        if holds && now >= quiet_until && quiet_until < deadline {
            return verdict(matched, state, true, false);
        }
        if now >= deadline {
            return verdict(matched, state, false, false);
        }
        let timer = if holds {
            quiet_until.min(deadline)
        } else {
            deadline
        };
        tokio::select! {
            result = revisions.changed() => {
                if result.is_err() {
                    return verdict(matched, state, false, stopped());
                }
            }
            result = events.changed() => {
                if result.is_err() {
                    return verdict(matched, state, false, stopped());
                }
            }
            _ = tokio::time::sleep_until(timer) => {
                if Some(*revisions.borrow_and_update()) != seen {
                    // A revision landed while the timer was completing: re-test at the top of the loop.
                    continue;
                }
                if stopped() {
                    return verdict(matched, state, false, true);
                }
                if holds && timer < deadline {
                    return verdict(matched, state, true, false);
                }
                return verdict(matched, state, false, false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    #[test]
    fn text_match_json_shape() {
        assert_eq!(
            TextMatch {
                pattern: "❯".into(),
                row: 3,
                revision: 42,
                already: true
            }
            .to_json(),
            json!({"pattern":"❯","row":3,"revision":"42","already":true})
        );
    }

    struct Fixture {
        revisions: watch::Sender<u64>,
        events: watch::Sender<u64>,
        stopped: Arc<AtomicBool>,
        rows: Arc<Mutex<Vec<String>>>,
        revision: Arc<AtomicU64>,
        captures: Arc<AtomicUsize>,
    }
    impl Fixture {
        /// Paint `rows` as a new revision (the capture then reports it).
        fn paint(&self, rows: &[&str]) {
            *self.rows.lock().unwrap() = rows.iter().map(|s| (*s).to_owned()).collect();
            let revision = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
            self.revisions.send_replace(revision);
        }
        fn event(&self) {
            self.events.send_modify(|value| *value += 1);
        }
    }
    type Task = tokio::task::JoinHandle<(TextWait, Duration)>;
    /// Every sender is kept alive by the fixture (a dropped sender ends the loop at once).
    fn start(patterns: &[&str], rows: &[&str], settle_ms: u64, budget_ms: u64) -> (Fixture, Task) {
        start_with(patterns, &[], rows, settle_ms, budget_ms)
    }
    /// The same with both condition lists.
    fn start_with(
        present: &[&str],
        absent: &[&str],
        rows: &[&str],
        settle_ms: u64,
        budget_ms: u64,
    ) -> (Fixture, Task) {
        let (revisions, revisions_rx) = watch::channel(0u64);
        let (events, events_rx) = watch::channel(0u64);
        let stopped = Arc::new(AtomicBool::new(false));
        let rows = Arc::new(Mutex::new(
            rows.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        ));
        let revision = Arc::new(AtomicU64::new(0));
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
        let started = Instant::now();
        let task = tokio::spawn(async move {
            let result = wait_for_text(
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
                started,
                started + Duration::from_millis(budget_ms),
                Duration::from_millis(settle_ms),
            )
            .await;
            (result, started.elapsed())
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
    fn found(pattern: &str, row: usize, revision: u64, already: bool) -> Option<TextMatch> {
        Some(TextMatch {
            pattern: pattern.into(),
            row,
            revision,
            already,
        })
    }

    #[tokio::test(start_paused = true)]
    async fn match_on_a_later_revision_settles_after_the_quiet_window() {
        let (fixture, task) = start(&["trust the files", "❯"], &["$ "], 150, 5_000);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(100)).await;
        assert!(!task.is_finished(), "nothing matched yet");
        fixture.paint(&["Do you trust the files in this folder?", "", "❯ Yes"]);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(149)).await;
        assert!(!task.is_finished(), "settled before the quiet window");
        tokio::time::advance(Duration::from_millis(1)).await;
        let (verdict, waited) = task.await.unwrap();
        assert_eq!(verdict.matched, found("trust the files", 0, 1, false));
        assert!(verdict.settled && !verdict.exited);
        assert_eq!(waited, Duration::from_millis(250));
        assert_eq!(fixture.captures.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn already_matching_screen_settles_from_the_start() {
        let (fixture, task) = start(&["READY"], &["hello", "READY $ "], 150, 5_000);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(149)).await;
        assert!(!task.is_finished(), "a quiet window is still required");
        tokio::time::advance(Duration::from_millis(1)).await;
        let (verdict, waited) = task.await.unwrap();
        assert_eq!(verdict.matched, found("READY", 1, 0, true));
        assert!(verdict.settled);
        assert_eq!(waited, Duration::from_millis(150));
        assert_eq!(fixture.captures.load(Ordering::SeqCst), 1);
        let (_fixture, task) = start(&["READY"], &["READY"], 0, 5_000);
        let (verdict, waited) = task.await.unwrap();
        assert_eq!(verdict.matched, found("READY", 0, 0, true));
        assert!(verdict.settled);
        assert_eq!(waited, Duration::ZERO);
    }

    /// A later reappearance is not `already` and settles from its own revision.
    #[tokio::test(start_paused = true)]
    async fn match_that_vanishes_before_settling_does_not_end_the_wait() {
        let (fixture, task) = start(&["READY"], &["booting"], 150, 5_000);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(100)).await;
        fixture.paint(&["READY"]);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(100)).await;
        fixture.paint(&["loading"]);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(300)).await;
        // Without the yield the task may simply not have been polled yet.
        tokio::task::yield_now().await;
        assert!(!task.is_finished(), "a vanished match must not settle");
        assert_eq!(fixture.captures.load(Ordering::SeqCst), 3);
        fixture.paint(&["READY again"]);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(150)).await;
        let (verdict, waited) = task.await.unwrap();
        assert_eq!(verdict.matched, found("READY", 0, 3, false));
        assert!(verdict.settled);
        assert_eq!(waited, Duration::from_millis(650));
        // Present at the start, vanished, back: no longer `already`.
        let (fixture, task) = start(&["READY"], &["READY"], 150, 5_000);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(50)).await;
        fixture.paint(&["gone"]);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(50)).await;
        fixture.paint(&["READY"]);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(150)).await;
        let (verdict, waited) = task.await.unwrap();
        assert_eq!(verdict.matched, found("READY", 0, 2, false));
        assert_eq!(waited, Duration::from_millis(250));
    }

    #[tokio::test(start_paused = true)]
    async fn budget_reports_unmatched_or_an_unsettled_match() {
        let (fixture, task) = start(&["READY"], &["$ "], 150, 300);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(100)).await;
        fixture.paint(&["still not it"]);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(199)).await;
        assert!(!task.is_finished(), "ended before the budget");
        tokio::time::advance(Duration::from_millis(1)).await;
        let (verdict, waited) = task.await.unwrap();
        assert_eq!(verdict.matched, None);
        assert!(!verdict.settled && !verdict.exited);
        assert_eq!(waited, Duration::from_millis(300));
        assert_eq!(fixture.captures.load(Ordering::SeqCst), 2);

        let (fixture, task) = start(&["READY"], &["$ "], 150, 300);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(250)).await;
        fixture.paint(&["READY"]);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(50)).await;
        let (verdict, waited) = task.await.unwrap();
        assert_eq!(verdict.matched, found("READY", 0, 1, false));
        assert!(!verdict.settled && !verdict.exited);
        assert_eq!(waited, Duration::from_millis(300));
    }

    /// The quiet timer and a revision notification become ready in the same poll; repeated
    /// because `select!` picks among ready branches at random.
    #[tokio::test(start_paused = true)]
    async fn timer_completion_rechecks_the_revision_before_settling() {
        for _ in 0..64 {
            let (fixture, task) = start(&["READY"], &["$ "], 150, 5_000);
            tokio::task::yield_now().await;
            fixture.paint(&["READY"]);
            tokio::task::yield_now().await;
            let revisions = fixture.revisions.clone();
            let helper = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                revisions.send_modify(|value| *value += 1);
            });
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(150)).await;
            helper.await.unwrap();
            tokio::task::yield_now().await;
            assert!(
                !task.is_finished(),
                "settled although a revision was pending"
            );
            tokio::time::advance(Duration::from_millis(150)).await;
            let (verdict, waited) = task.await.unwrap();
            assert!(verdict.settled && verdict.matched.is_some());
            assert_eq!(waited, Duration::from_millis(300));
        }
    }

    /// Repeated because the `select!` pick is random; the helper's earlier sleep fires first.
    #[tokio::test(start_paused = true)]
    async fn exit_wins_over_a_settled_match() {
        let (fixture, task) = start(&["READY"], &["READY"], 0, 5_000);
        fixture.stopped.store(true, Ordering::SeqCst);
        let (verdict, waited) = task.await.unwrap();
        assert!(verdict.exited && !verdict.settled, "{verdict:?}");
        assert_eq!(verdict.matched, found("READY", 0, 0, true));
        assert_eq!(waited, Duration::ZERO);
        for _ in 0..64 {
            let (fixture, task) = start(&["READY"], &["READY"], 150, 5_000);
            tokio::task::yield_now().await;
            let (stopped, events) = (fixture.stopped.clone(), fixture.events.clone());
            let helper = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                stopped.store(true, Ordering::SeqCst);
                events.send_modify(|value| *value += 1);
            });
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(200)).await;
            helper.await.unwrap();
            let (verdict, waited) = task.await.unwrap();
            assert!(verdict.exited && !verdict.settled, "{verdict:?}");
            assert_eq!(verdict.matched, found("READY", 0, 0, true));
            assert_eq!(waited, Duration::from_millis(200));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn protocol_events_do_not_capture_and_an_exit_ends_the_wait() {
        let (fixture, task) = start(&["READY"], &["READY"], 150, 5_000);
        tokio::task::yield_now().await;
        for _ in 0..4 {
            tokio::time::advance(Duration::from_millis(30)).await;
            fixture.event();
            tokio::task::yield_now().await;
        }
        assert!(!task.is_finished());
        tokio::time::advance(Duration::from_millis(30)).await;
        let (verdict, waited) = task.await.unwrap();
        assert!(verdict.settled, "events must not restart the quiet window");
        assert_eq!(waited, Duration::from_millis(150));
        assert_eq!(fixture.captures.load(Ordering::SeqCst), 1);

        let (fixture, task) = start(&["READY"], &["$ "], 150, 5_000);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(100)).await;
        fixture.paint(&["READY"]);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(50)).await;
        fixture.stopped.store(true, Ordering::SeqCst);
        fixture.event();
        tokio::task::yield_now().await;
        let (verdict, waited) = task.await.unwrap();
        assert_eq!(verdict.matched, found("READY", 0, 1, false));
        assert!(verdict.exited && !verdict.settled);
        assert_eq!(waited, Duration::from_millis(150));

        let (fixture, task) = start(&["READY"], &["$ "], 150, 300);
        tokio::task::yield_now().await;
        fixture.stopped.store(true, Ordering::SeqCst);
        tokio::time::advance(Duration::from_millis(300)).await;
        let (verdict, _) = task.await.unwrap();
        assert!(verdict.exited, "exit at the deadline reported as unmatched");
    }

    /// Absent-only: `matched` stays null, `conditions` names the side that held.
    #[tokio::test(start_paused = true)]
    async fn absent_condition_alone_ends_the_wait_when_the_pattern_is_gone() {
        let (fixture, task) = start_with(
            &[],
            &["esc to interrupt"],
            &["· Noodling… (esc to interrupt)"],
            150,
            5_000,
        );
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(300)).await;
        tokio::task::yield_now().await;
        assert!(!task.is_finished(), "the busy hint is still on the screen");
        fixture.paint(&["❯ the answer"]);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(149)).await;
        assert!(!task.is_finished(), "settled before the quiet window");
        tokio::time::advance(Duration::from_millis(1)).await;
        let (verdict, waited) = task.await.unwrap();
        assert_eq!(verdict.matched, None, "no present pattern was asked");
        assert!(verdict.holds && verdict.settled && !verdict.exited);
        assert_eq!(
            verdict.conditions,
            ConditionState {
                present: None,
                absent: Some(true)
            }
        );
        assert_eq!(waited, Duration::from_millis(450));
        assert_eq!(fixture.captures.load(Ordering::SeqCst), 2);
        // Already absent at the start: settles from the wait's start.
        let (_fixture, task) = start_with(&[], &["busy"], &["❯ done"], 150, 5_000);
        let (verdict, waited) = task.await.unwrap();
        assert!(verdict.holds && verdict.settled);
        assert_eq!(waited, Duration::from_millis(150));
        // Still present at the budget: not held, unsettled.
        let (_fixture, task) = start_with(&[], &["busy"], &["busy"], 150, 300);
        let (verdict, waited) = task.await.unwrap();
        assert!(!verdict.holds && !verdict.settled && !verdict.exited);
        assert_eq!(verdict.conditions.absent, Some(false));
        assert_eq!(waited, Duration::from_millis(300));
    }

    /// Present AND absent: the present match is reported (`already`) but does not hold until a
    /// revision removes the hint.
    #[tokio::test(start_paused = true)]
    async fn present_match_with_the_absent_pattern_still_on_screen_keeps_waiting() {
        let (fixture, task) = start_with(
            &["❯"],
            &["esc to interrupt"],
            &["❯ ", "· Noodling… (esc to interrupt)"],
            150,
            5_000,
        );
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(400)).await;
        tokio::task::yield_now().await;
        assert!(
            !task.is_finished(),
            "a present match must not settle while an absent pattern is shown"
        );
        fixture.paint(&["❯ the answer", ""]);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(150)).await;
        let (verdict, waited) = task.await.unwrap();
        assert_eq!(verdict.matched, found("❯", 0, 1, true), "{verdict:?}");
        assert!(verdict.holds && verdict.settled);
        assert_eq!(
            verdict.conditions,
            ConditionState {
                present: Some(true),
                absent: Some(true)
            }
        );
        assert_eq!(waited, Duration::from_millis(550));
        // The other order: the hint is gone but the prompt not yet there.
        let (fixture, task) = start_with(&["❯"], &["esc to interrupt"], &["thinking"], 150, 5_000);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(200)).await;
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        fixture.paint(&["❯ "]);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(150)).await;
        let (verdict, waited) = task.await.unwrap();
        assert_eq!(verdict.matched, found("❯", 0, 1, false));
        assert!(verdict.holds && verdict.settled);
        assert_eq!(waited, Duration::from_millis(350));
        // Budget with the present match shown but the hint still there:
        // matched is reported, held is false, unsettled.
        let (_fixture, task) = start_with(&["❯"], &["busy"], &["❯ busy"], 150, 300);
        let (verdict, waited) = task.await.unwrap();
        assert_eq!(verdict.matched, found("❯", 0, 0, true));
        assert!(!verdict.holds && !verdict.settled);
        assert_eq!(
            verdict.conditions,
            ConditionState {
                present: Some(true),
                absent: Some(false)
            }
        );
        assert_eq!(waited, Duration::from_millis(300));
    }
}
