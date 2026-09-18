//! Validated waiting arguments shared by observe and action readbacks (moved
//! out of `wait.rs` with #1666 so the loops stay in one file and the
//! argument contract in another).
use super::text_conditions::TextConditions;
use crate::terminal_hooks::{DEFAULT_SIGNAL_EVENTS, TERMINAL_SIGNAL_EVENTS};
use anyhow::{Result, ensure};
use serde::Deserialize;

pub const WAIT_MS_MAX: u64 = 20_000;
pub const SETTLE_MS_MAX: u64 = 2_000;
pub const SETTLE_MS_DEFAULT: u64 = 150;
/// #1725 — a `submit` is two PTY writes `SUBMIT_CR_GAP` apart. Policy, not
/// a correctness guarantee (a readback starts after the write completed):
/// the gap is kept small relative to the default settle so a submit's
/// readback is not dominated by the gap.
const _: () = assert!(
    crate::terminal_renderer::SUBMIT_CR_GAP.as_millis() * 3 <= SETTLE_MS_DEFAULT as u128,
    "SUBMIT_CR_GAP stays at most a third of the default settle (#1725)"
);
/// Budget when `wait_ms` is omitted in change mode. Elapsed mode keeps 0 so an
/// observation without waiting arguments stays an immediate read.
pub const CHANGE_WAIT_MS_DEFAULT: u64 = 2_000;
/// Budget when `wait_ms` is omitted in signal mode (#1620): a model answer
/// takes seconds, and the wait ends early on the signal anyway.
pub const SIGNAL_WAIT_MS_DEFAULT: u64 = 15_000;
/// Budget when `wait_ms` is omitted in text mode (#1666): a TUI start takes
/// seconds too, and the wait ends early once the target screen shows.
pub const TEXT_WAIT_MS_DEFAULT: u64 = 15_000;
/// Signal mode (#1628): how long after the signal to wait for the first
/// repaint. Claude's `Stop` hook fires before the TUI paints the answer, so
/// a signal readback that returned at once would still show the spinner.
pub const REPAINT_MS_MAX: u64 = 5_000;
pub const REPAINT_MS_DEFAULT: u64 = 1_500;
/// Text mode (#1666): at most this many patterns per wait, each at most
/// this many bytes.
pub const WAIT_TEXT_MAX_PATTERNS: usize = 8;
pub const WAIT_TEXT_MAX_BYTES: usize = 200;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WaitFor {
    #[default]
    Elapsed,
    Change,
    Signal,
    Text,
}
impl WaitFor {
    pub fn name(self) -> &'static str {
        match self {
            Self::Elapsed => "elapsed",
            Self::Change => "change",
            Self::Signal => "signal",
            Self::Text => "text",
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
    /// Text mode (#1666): literal patterns, any of which on any live
    /// viewport row ends the wait once the screen is quiet; signal mode
    /// (#1677 r16): the repaint phase settles only while one is on a row.
    /// Empty elsewhere.
    pub wait_text: Vec<String>,
    /// Text and signal modes (#1677 r16): literal patterns none of which may
    /// be on any live viewport row for the wait to end (text) or the
    /// repaint phase to settle (signal). Empty elsewhere.
    pub wait_text_absent: Vec<String>,
}
impl WaitPlan {
    /// `wait_ms == None` selects the mode's default budget:
    /// [`CHANGE_WAIT_MS_DEFAULT`] for change, [`SIGNAL_WAIT_MS_DEFAULT`] for
    /// signal, [`TEXT_WAIT_MS_DEFAULT`] for text, 0 for elapsed.
    /// `signal_events == None` selects [`DEFAULT_SIGNAL_EVENTS`] in signal
    /// mode; `repaint_ms == None` selects [`REPAINT_MS_DEFAULT`] there.
    /// Text mode requires at least one of `wait_text` / `wait_text_absent`;
    /// signal mode accepts either; both are refused in change and elapsed
    /// mode.
    pub fn new(
        wait_for: Option<WaitFor>,
        wait_ms: Option<u64>,
        settle_ms: Option<u64>,
        signal_events: Option<Vec<String>>,
        repaint_ms: Option<u64>,
        wait_text: Option<Vec<String>>,
        wait_text_absent: Option<Vec<String>>,
    ) -> Result<Self> {
        let mode = wait_for.unwrap_or_default();
        let wait_ms = wait_ms.unwrap_or(match mode {
            WaitFor::Change => CHANGE_WAIT_MS_DEFAULT,
            WaitFor::Signal => SIGNAL_WAIT_MS_DEFAULT,
            WaitFor::Text => TEXT_WAIT_MS_DEFAULT,
            WaitFor::Elapsed => 0,
        });
        ensure!(wait_ms <= WAIT_MS_MAX, "wait_ms must be 0..{WAIT_MS_MAX}");
        ensure!(
            settle_ms.is_none_or(|settle| settle <= SETTLE_MS_MAX),
            "settle_ms must be 0..{SETTLE_MS_MAX}"
        );
        ensure!(
            settle_ms.is_none()
                || matches!(mode, WaitFor::Change | WaitFor::Signal | WaitFor::Text),
            "settle_ms requires wait_for=change, wait_for=signal or wait_for=text"
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
        ensure!(
            wait_text.is_none() || matches!(mode, WaitFor::Text | WaitFor::Signal),
            "wait_text requires wait_for=text or wait_for=signal"
        );
        ensure!(
            wait_text_absent.is_none() || matches!(mode, WaitFor::Text | WaitFor::Signal),
            "wait_text_absent requires wait_for=text or wait_for=signal"
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
        ensure!(
            mode != WaitFor::Text || wait_text.is_some() || wait_text_absent.is_some(),
            "wait_for=text requires wait_text or wait_text_absent"
        );
        // Review r1 E: `repaint_ms: 0` skips the phase that tests them.
        ensure!(
            mode != WaitFor::Signal
                || repaint_ms != Some(0)
                || (wait_text.is_none() && wait_text_absent.is_none()),
            "text conditions need a repaint window; repaint_ms must be > 0"
        );
        let patterns = |list: Option<Vec<String>>| -> Result<Vec<String>> {
            match list {
                Some(patterns) => {
                    validate_wait_text(&patterns)?;
                    Ok(patterns)
                }
                None => Ok(Vec::new()),
            }
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
            wait_text: patterns(wait_text)?,
            wait_text_absent: patterns(wait_text_absent)?,
        })
    }
    /// The text conditions of this wait (#1677 r16): empty outside text and
    /// signal modes.
    pub fn conditions(&self) -> TextConditions {
        TextConditions {
            present: self.wait_text.clone(),
            absent: self.wait_text_absent.clone(),
        }
    }
    /// Whether the wait tests live viewport rows: text mode, or signal mode
    /// with text conditions. Such a wait needs the live viewport
    /// (`scroll_offset` 0).
    pub fn tests_text(&self) -> bool {
        match self.mode {
            WaitFor::Text => true,
            WaitFor::Signal => !self.wait_text.is_empty() || !self.wait_text_absent.is_empty(),
            _ => false,
        }
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
        for list in [&self.wait_text, &self.wait_text_absent] {
            match self.mode {
                WaitFor::Text | WaitFor::Signal => {
                    if !list.is_empty() {
                        validate_wait_text(list)?;
                    }
                }
                _ => ensure!(
                    list.is_empty(),
                    "text conditions outside text or signal mode"
                ),
            }
        }
        ensure!(
            self.mode != WaitFor::Text || !self.conditions().is_empty(),
            "wait_for=text without text conditions"
        );
        ensure!(
            self.mode != WaitFor::Signal || self.repaint_ms > 0 || self.conditions().is_empty(),
            "text conditions need a repaint window; repaint_ms must be > 0"
        );
        Ok(())
    }
}

/// Text mode (#1666): 1..=8 literal patterns, each 1..=200 bytes and free of
/// control characters (a pattern is matched against rendered rows, which
/// never contain any).
fn validate_wait_text(patterns: &[String]) -> Result<()> {
    ensure!(
        (1..=WAIT_TEXT_MAX_PATTERNS).contains(&patterns.len()),
        "wait_text must list 1..{WAIT_TEXT_MAX_PATTERNS} patterns"
    );
    for pattern in patterns {
        ensure!(
            !pattern.is_empty()
                && pattern.len() <= WAIT_TEXT_MAX_BYTES
                && !pattern.chars().any(char::is_control),
            "each wait_text pattern must be 1..{WAIT_TEXT_MAX_BYTES} bytes of printable text"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(wait_for: Option<WaitFor>, wait_ms: Option<u64>) -> WaitPlan {
        WaitPlan::new(wait_for, wait_ms, None, None, None, None, None).unwrap()
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
                None,
                None,
                None
            )
            .is_err()
        );
        assert!(WaitPlan::new(None, None, Some(10), None, None, None, None).is_err());
        assert!(
            WaitPlan::new(Some(WaitFor::Change), None, None, None, Some(0), None, None).is_err(),
            "repaint_ms outside signal mode"
        );
        assert!(WaitPlan::new(None, None, None, None, Some(0), None, None).is_err());
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
        let tuned = WaitPlan::new(
            Some(WaitFor::Signal),
            None,
            Some(300),
            None,
            Some(0),
            None,
            None,
        )
        .unwrap();
        assert_eq!((tuned.settle_ms, tuned.repaint_ms), (300, 0));
        assert!(
            WaitPlan::new(
                Some(WaitFor::Signal),
                None,
                None,
                None,
                Some(REPAINT_MS_MAX + 1),
                None,
                None
            )
            .is_err()
        );
        assert!(
            WaitPlan::new(
                Some(WaitFor::Signal),
                None,
                Some(SETTLE_MS_MAX + 1),
                None,
                None,
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
                None,
                None,
                None
            )
            .is_err()
        );
        assert!(
            WaitPlan::new(
                Some(WaitFor::Signal),
                None,
                None,
                Some(vec![]),
                None,
                None,
                None
            )
            .is_err()
        );
        assert!(
            WaitPlan::new(
                Some(WaitFor::Signal),
                None,
                None,
                Some(vec!["Stop".into()]),
                None,
                None,
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
            None,
            None,
        )
        .unwrap();
        assert_eq!(only.signal_events, vec!["session_end"]);
        assert_eq!(only.budget_ms, 0);
    }

    /// #1666 text mode: its own default budget, settle allowed, patterns
    /// required and bounded (count, bytes, control characters), refused in
    /// every other mode; `validate` re-checks a hand-built plan.
    #[test]
    fn text_mode_defaults_and_validation() {
        let text = |patterns: Vec<&str>| {
            WaitPlan::new(
                Some(WaitFor::Text),
                None,
                None,
                None,
                None,
                Some(patterns.into_iter().map(str::to_owned).collect()),
                None,
            )
        };
        let start = text(vec!["trust the files", "❯"]).unwrap();
        assert_eq!(start.budget_ms, TEXT_WAIT_MS_DEFAULT);
        assert_eq!(TEXT_WAIT_MS_DEFAULT, 15_000);
        assert_eq!(start.settle_ms, SETTLE_MS_DEFAULT);
        assert_eq!(start.repaint_ms, 0);
        assert!(start.signal_events.is_empty());
        assert_eq!(start.wait_text, vec!["trust the files", "❯"]);
        assert_eq!(start.mode.name(), "text");
        start.validate().unwrap();
        let tuned = WaitPlan::new(
            Some(WaitFor::Text),
            Some(0),
            Some(300),
            None,
            None,
            Some(vec!["x".into()]),
            None,
        )
        .unwrap();
        assert_eq!((tuned.budget_ms, tuned.settle_ms), (0, 300));
        // Mode coupling in both directions (#1677 r16: signal mode accepts
        // the text conditions too; change and elapsed refuse them).
        assert!(
            WaitPlan::new(Some(WaitFor::Text), None, None, None, None, None, None).is_err(),
            "wait_for=text without text conditions"
        );
        for mode in [None, Some(WaitFor::Change)] {
            assert!(
                WaitPlan::new(mode, None, None, None, None, Some(vec!["x".into()]), None).is_err(),
                "wait_text outside text/signal mode: {mode:?}"
            );
            assert!(
                WaitPlan::new(mode, None, None, None, None, None, Some(vec!["x".into()])).is_err(),
                "wait_text_absent outside text/signal mode: {mode:?}"
            );
        }
        assert!(
            WaitPlan::new(
                Some(WaitFor::Text),
                None,
                None,
                Some(vec!["stop".into()]),
                None,
                Some(vec!["x".into()]),
                None
            )
            .is_err(),
            "signal_events in text mode"
        );
        assert!(
            WaitPlan::new(
                Some(WaitFor::Text),
                None,
                None,
                None,
                Some(0),
                Some(vec!["x".into()]),
                None
            )
            .is_err(),
            "repaint_ms in text mode"
        );
        // Pattern bounds.
        assert!(text(vec![]).is_err(), "no pattern");
        assert!(text(vec!["x"; 8]).is_ok());
        assert!(text(vec!["x"; 9]).is_err(), "nine patterns");
        assert!(text(vec![""]).is_err(), "empty pattern");
        let long = "y".repeat(200);
        assert!(text(vec![long.as_str()]).is_ok());
        let longer = "y".repeat(201);
        assert!(text(vec![longer.as_str()]).is_err(), "201 bytes");
        assert!(text(vec!["a\nb"]).is_err(), "newline");
        assert!(text(vec!["a\tb"]).is_err(), "tab");
        assert!(text(vec!["a\u{1b}[0m"]).is_err(), "escape");
        assert!(
            WaitPlan {
                wait_text: vec![],
                ..text(vec!["x"]).unwrap()
            }
            .validate()
            .is_err()
        );
        assert!(
            WaitPlan {
                wait_text: vec!["x".into()],
                ..plan(Some(WaitFor::Change), None)
            }
            .validate()
            .is_err()
        );
        assert!(
            WaitPlan::new(
                Some(WaitFor::Text),
                Some(WAIT_MS_MAX + 1),
                None,
                None,
                None,
                Some(vec!["x".into()]),
                None
            )
            .is_err()
        );
    }

    /// #1677 r16 `wait_text_absent`: the same bounds as `wait_text`, alone
    /// or with it in text mode, either in signal mode (where the conditions
    /// gate the repaint settle), refused in change and elapsed mode;
    /// `conditions()` and `tests_text()` follow.
    #[test]
    fn absent_conditions_and_signal_mode_conditions() {
        let absent = WaitPlan::new(
            Some(WaitFor::Text),
            None,
            None,
            None,
            None,
            None,
            Some(vec!["esc to interrupt".into()]),
        )
        .unwrap();
        assert!(absent.wait_text.is_empty());
        assert_eq!(absent.wait_text_absent, vec!["esc to interrupt"]);
        assert_eq!(absent.budget_ms, TEXT_WAIT_MS_DEFAULT);
        assert!(absent.tests_text());
        assert_eq!(
            absent.conditions(),
            TextConditions {
                present: vec![],
                absent: vec!["esc to interrupt".into()]
            }
        );
        absent.validate().unwrap();
        let both = WaitPlan::new(
            Some(WaitFor::Text),
            None,
            None,
            None,
            None,
            Some(vec!["❯".into()]),
            Some(vec!["esc to interrupt".into()]),
        )
        .unwrap();
        assert_eq!(both.conditions().present, vec!["❯"]);
        assert_eq!(both.conditions().absent, vec!["esc to interrupt"]);
        let signal = WaitPlan::new(
            Some(WaitFor::Signal),
            None,
            None,
            None,
            None,
            None,
            Some(vec!["esc to interrupt".into()]),
        )
        .unwrap();
        assert_eq!(signal.budget_ms, SIGNAL_WAIT_MS_DEFAULT);
        assert_eq!(signal.repaint_ms, REPAINT_MS_DEFAULT);
        assert!(signal.tests_text(), "conditions read the live viewport");
        assert_eq!(signal.conditions().absent, vec!["esc to interrupt"]);
        signal.validate().unwrap();
        let plain = plan(Some(WaitFor::Signal), None);
        assert!(!plain.tests_text() && plain.conditions().is_empty());
        assert!(!plan(Some(WaitFor::Change), None).tests_text());
        let with_present = WaitPlan::new(
            Some(WaitFor::Signal),
            None,
            None,
            None,
            None,
            Some(vec!["❯".into()]),
            None,
        )
        .unwrap();
        assert_eq!(with_present.conditions().present, vec!["❯"]);
        // Bounds on the absent list, in both modes.
        for mode in [WaitFor::Text, WaitFor::Signal] {
            let absent = |patterns: Vec<&str>| {
                WaitPlan::new(
                    Some(mode),
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(patterns.into_iter().map(str::to_owned).collect()),
                )
            };
            assert!(absent(vec![]).is_err(), "{mode:?}: no pattern");
            assert!(absent(vec!["x"; 8]).is_ok());
            assert!(absent(vec!["x"; 9]).is_err(), "{mode:?}: nine patterns");
            assert!(absent(vec![""]).is_err(), "{mode:?}: empty pattern");
            let longer = "y".repeat(201);
            assert!(
                absent(vec![longer.as_str()]).is_err(),
                "{mode:?}: 201 bytes"
            );
            assert!(absent(vec!["a\tb"]).is_err(), "{mode:?}: tab");
        }
        // Review r1 E: repaint_ms 0 would skip the phase that tests them.
        for (present, absent) in [
            (Some(vec!["❯".into()]), None),
            (None, Some(vec!["busy".into()])),
        ] {
            let skipped = WaitPlan::new(
                Some(WaitFor::Signal),
                None,
                None,
                None,
                Some(0),
                present,
                absent,
            );
            assert!(
                skipped
                    .unwrap_err()
                    .to_string()
                    .contains("repaint_ms must be > 0")
            );
        }
        assert!(
            WaitPlan::new(Some(WaitFor::Signal), None, None, None, Some(0), None, None).is_ok(),
            "repaint_ms 0 without conditions stays valid"
        );
        assert!(
            WaitPlan::new(
                Some(WaitFor::Signal),
                None,
                None,
                None,
                Some(1),
                None,
                Some(vec!["busy".into()])
            )
            .is_ok()
        );
        assert!(
            WaitPlan {
                repaint_ms: 0,
                ..signal.clone()
            }
            .validate()
            .is_err(),
            "validate refuses the hand-built shape too"
        );
        // Hand-built plans: conditions outside their modes, text without any.
        assert!(
            WaitPlan {
                wait_text_absent: vec!["x".into()],
                ..plan(Some(WaitFor::Change), None)
            }
            .validate()
            .is_err()
        );
        assert!(
            WaitPlan {
                wait_text_absent: vec!["x".into()],
                ..plan(None, None)
            }
            .validate()
            .is_err()
        );
        assert!(
            WaitPlan {
                wait_text: vec![],
                ..absent.clone()
            }
            .validate()
            .is_ok(),
            "absent alone satisfies text mode"
        );
        assert!(
            WaitPlan {
                wait_text_absent: vec![],
                ..absent
            }
            .validate()
            .is_err(),
            "text mode without any condition"
        );
        assert!(
            WaitPlan {
                wait_text_absent: vec!["".into()],
                ..plan(Some(WaitFor::Signal), None)
            }
            .validate()
            .is_err(),
            "an empty pattern fails validate too"
        );
    }
}
