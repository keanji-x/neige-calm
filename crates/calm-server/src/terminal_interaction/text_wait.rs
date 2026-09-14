//! Text waiting (#1666): wake on renderer revisions and client protocol
//! events, re-test the live viewport rows against literal patterns on every
//! revision, and return once a match has stayed quiet for the settle window.
//! "Until the screen shows X", not "until X appears anew": a screen that
//! already matches settles from the wait's start.
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
    /// The projection revision of the capture that confirmed the match (the
    /// observation captured after the wait can be later).
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
    /// The match present when the wait ended, if any.
    pub matched: Option<TextMatch>,
    /// A match was present and the screen quiet for the settle window.
    pub settled: bool,
    /// Exit / disconnect / invalidation ended the wait.
    pub exited: bool,
}

/// First pattern in argument order, then first row top-down; a pattern is a
/// substring of a row as rendered (rows are already trailing-trimmed).
pub fn find_match(patterns: &[String], rows: &[String]) -> Option<(String, usize)> {
    patterns.iter().find_map(|pattern| {
        rows.iter()
            .position(|row| row.contains(pattern.as_str()))
            .map(|row| (pattern.clone(), row))
    })
}

/// The text-mode loop, separated from the client so its timing can be
/// tested under a paused clock. `revisions` is the projection revision
/// channel, `events` the client's protocol channel (both re-evaluate
/// `stopped`), `capture` one read of the live viewport (its trimmed rows and
/// revision; `None` when the projection is unavailable, which `stopped`
/// then reports). The rows are captured once per revision wake — never on a
/// protocol event or a timer wake alone — and re-tested against `patterns`;
/// a match that disappears returns the wait to "no match". Only a revision
/// starts or extends the quiet window; it ends the wait while a match is
/// present. As in change mode, a timer wake re-reads the revision before
/// settling, and a window that would end at or after the deadline reports
/// the budget verdict instead.
#[allow(clippy::too_many_arguments)]
pub async fn wait_for_text(
    mut revisions: watch::Receiver<u64>,
    mut events: watch::Receiver<u64>,
    stopped: impl Fn() -> bool,
    capture: impl Fn() -> Option<(Vec<String>, u64)>,
    patterns: &[String],
    started: Instant,
    deadline: Instant,
    settle: Duration,
) -> TextWait {
    let mut seen: Option<u64> = None;
    let mut matched: Option<TextMatch> = None;
    // Present since the first capture without interruption: `already`.
    let mut continuous = true;
    let mut last_change = started;
    let verdict = |matched: Option<TextMatch>, settled: bool, exited: bool| TextWait {
        matched,
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
            // The capture's own revision names the screen that was tested; the
            // channel value only says a wake was due.
            let found = capture().and_then(|(rows, revision)| {
                find_match(patterns, &rows).map(|(pattern, row)| (pattern, row, revision))
            });
            continuous = continuous && found.is_some();
            matched = found.map(|(pattern, row, revision)| TextMatch {
                pattern,
                row,
                revision,
                already: continuous,
            });
        }
        let quiet_until = last_change + settle;
        if matched.is_some() && now >= quiet_until && quiet_until < deadline {
            return verdict(matched, true, false);
        }
        if stopped() {
            return verdict(matched, false, true);
        }
        if now >= deadline {
            return verdict(matched, false, false);
        }
        let timer = if matched.is_some() {
            quiet_until.min(deadline)
        } else {
            deadline
        };
        tokio::select! {
            result = revisions.changed() => {
                if result.is_err() {
                    return verdict(matched, false, stopped());
                }
            }
            result = events.changed() => {
                if result.is_err() {
                    return verdict(matched, false, stopped());
                }
            }
            _ = tokio::time::sleep_until(timer) => {
                if Some(*revisions.borrow_and_update()) != seen {
                    // A revision landed while the timer was completing: the
                    // rows are re-tested at the top of the loop.
                    continue;
                }
                if stopped() {
                    return verdict(matched, false, true);
                }
                if matched.is_some() && timer < deadline {
                    return verdict(matched, true, false);
                }
                return verdict(matched, false, false);
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
    fn find_match_prefers_the_first_pattern_then_the_first_row() {
        let patterns = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let rows = patterns(&["a x", "b y", "b z", "", "a"]);
        assert_eq!(
            find_match(&patterns(&["b", "a"]), &rows),
            Some(("b".into(), 1))
        );
        assert_eq!(
            find_match(&patterns(&["a", "b"]), &rows),
            Some(("a".into(), 0))
        );
        assert_eq!(
            find_match(&patterns(&["z", "b z"]), &rows),
            Some(("z".into(), 2))
        );
        assert_eq!(find_match(&patterns(&["q"]), &rows), None);
        assert_eq!(find_match(&patterns(&["a"]), &[]), None);
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
    /// Every sender is kept alive by the fixture (a dropped sender ends the
    /// loop at once, which would make a budget test vacuous).
    fn start(patterns: &[&str], rows: &[&str], settle_ms: u64, budget_ms: u64) -> (Fixture, Task) {
        let (revisions, revisions_rx) = watch::channel(0u64);
        let (events, events_rx) = watch::channel(0u64);
        let stopped = Arc::new(AtomicBool::new(false));
        let rows = Arc::new(Mutex::new(
            rows.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        ));
        let revision = Arc::new(AtomicU64::new(0));
        let captures = Arc::new(AtomicUsize::new(0));
        let patterns: Vec<String> = patterns.iter().map(|s| (*s).to_owned()).collect();
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
                &patterns,
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

    /// The target appears on a later revision and the wait ends once that
    /// screen has been quiet for `settle`; the report names the pattern,
    /// the row and the confirming revision.
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

    /// A screen that already shows the target returns after `settle` from
    /// the wait's start with `already: true`; `settle: 0` returns at once.
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

    /// A match that disappears before the quiet window ends returns the wait
    /// to "no match"; a later reappearance is not `already` and settles from
    /// its own revision.
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
        // Let the waiter observe the elapsed timer before asking: without
        // the yield the task may simply not have been polled yet.
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

    /// The budget: no match at the deadline reports none; a match present
    /// but not yet quiet at the deadline is reported unsettled.
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

    /// The quiet timer and a revision notification become ready in the same
    /// poll (a helper's earlier sleep bumps the revision before the waiter
    /// is polled): the timer wake re-reads the revision and restarts the
    /// window instead of settling on a screen that already moved. Repeated
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

    /// Protocol events wake the loop but neither capture nor extend the
    /// window; an exit (or a stopped state read at the timer) ends the wait
    /// as exited with whatever match stood.
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
}
