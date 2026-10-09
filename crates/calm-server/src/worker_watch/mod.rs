//! The worker watcher (#2492): a plain Assistant conversation per Track, created on demand under a
//! fixed key, that the quiet-worker detector (`worker_quiet`) hands each quiet episode to. The
//! watcher reads the worker's screen with the terminal tools, may accept a workspace-trust prompt,
//! and reports one closed outcome with `neige_worker_report`; the kernel turns that report into one
//! templated `track.wake_requested` line for the Planner, the only agent that asks the owner.

mod report;
mod watcher;

#[cfg(feature = "fixtures")]
pub use report::WORKER_REPORT_AUTHORIZED;
pub use report::report;
pub use watcher::{WORKER_WATCHER_CONVERSATION_KEY, WorkerWatcher, watcher_card_id};

use serde::Deserialize;

use crate::error::Result;
use crate::prompts::render_named;

/// The message one quiet episode sends the watcher: the task, the attempt, the silence and the
/// watcher's rules.
const WATCH_TEMPLATE: &str = include_str!("../../prompts/terminal/worker-quiet-watch.md");

/// The one line a report wakes the Planner with.
const REPORT_TEMPLATE: &str = include_str!("../../prompts/terminal/worker-watch-report.md");

/// The longest `note`, in characters.
pub const MAX_NOTE_CHARS: usize = 300;

/// The watcher message for one quiet episode of `attempt_id`'s worker, silent for `quiet_secs`.
pub fn watch_text(task_key: &str, attempt_id: &str, quiet_secs: i64) -> Result<String> {
    Ok(render_named(
        WATCH_TEMPLATE.trim_end(),
        &[
            ("task_key", task_key),
            ("attempt_id", attempt_id),
            ("quiet_secs", &quiet_secs.to_string()),
        ],
    )?)
}

/// What the watcher found on a quiet worker's screen: the closed set `neige_worker_report` takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// It accepted the worker CLI's prompt to trust the task's own workspace.
    TrustAccepted,
    /// The worker waits at its input prompt: it finished a turn.
    IdleAtPrompt,
    /// The owner must act on the screen; the report carries a note saying what.
    NeedsOwner,
    /// The screen is none of the above, so the watcher typed nothing.
    Unclear,
}

impl Outcome {
    pub const ALL: [Outcome; 4] = [
        Self::TrustAccepted,
        Self::IdleAtPrompt,
        Self::NeedsOwner,
        Self::Unclear,
    ];

    /// The wire spelling, as the tool schema's enum lists it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TrustAccepted => "trust_accepted",
            Self::IdleAtPrompt => "idle_at_prompt",
            Self::NeedsOwner => "needs_owner",
            Self::Unclear => "unclear",
        }
    }

    /// What the outcome asks of the Planner, in the wake line.
    fn meaning(self) -> &'static str {
        match self {
            Self::TrustAccepted => {
                "it accepted the worker's prompt to trust the task's workspace, so the worker can begin"
            }
            Self::IdleAtPrompt => {
                "the worker is idle at its input prompt; handle it like a finished turn"
            }
            Self::NeedsOwner => "the owner must act on the worker's screen; ask them",
            Self::Unclear => {
                "the worker's screen is none the watcher may handle and nothing was typed; ask the owner"
            }
        }
    }
}

/// One sentence for the Planner: trimmed, one line, 1..=[`MAX_NOTE_CHARS`] characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note(String);

impl Note {
    pub fn parse(text: &str) -> std::result::Result<Self, String> {
        let text = text.trim();
        if text.is_empty() || text.chars().count() > MAX_NOTE_CHARS {
            return Err(format!("note is 1..{MAX_NOTE_CHARS} characters"));
        }
        if text
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
        {
            return Err("note is one line with no control characters".into());
        }
        Ok(Self(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A report's outcome with its note: `needs_owner` cannot exist without one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    TrustAccepted(Option<Note>),
    IdleAtPrompt(Option<Note>),
    NeedsOwner(Note),
    Unclear(Option<Note>),
}

impl Verdict {
    /// `None` only for `needs_owner` without a note.
    pub fn new(outcome: Outcome, note: Option<Note>) -> Option<Self> {
        Some(match outcome {
            Outcome::TrustAccepted => Self::TrustAccepted(note),
            Outcome::IdleAtPrompt => Self::IdleAtPrompt(note),
            Outcome::NeedsOwner => Self::NeedsOwner(note?),
            Outcome::Unclear => Self::Unclear(note),
        })
    }

    pub fn outcome(&self) -> Outcome {
        match self {
            Self::TrustAccepted(_) => Outcome::TrustAccepted,
            Self::IdleAtPrompt(_) => Outcome::IdleAtPrompt,
            Self::NeedsOwner(_) => Outcome::NeedsOwner,
            Self::Unclear(_) => Outcome::Unclear,
        }
    }

    pub fn note(&self) -> Option<&Note> {
        match self {
            Self::NeedsOwner(note) => Some(note),
            Self::TrustAccepted(note) | Self::IdleAtPrompt(note) | Self::Unclear(note) => {
                note.as_ref()
            }
        }
    }
}

/// The one line the Planner is woken with for `verdict` on `attempt_id` of task `task_key`.
pub fn report_line(task_key: &str, attempt_id: &str, verdict: &Verdict) -> Result<String> {
    let note = verdict
        .note()
        .map(|note| format!(" Note: {}", note.as_str()))
        .unwrap_or_default();
    Ok(render_named(
        REPORT_TEMPLATE.trim_end(),
        &[
            ("task_key", task_key),
            ("attempt_id", attempt_id),
            ("outcome", verdict.outcome().as_str()),
            ("meaning", verdict.outcome().meaning()),
            ("note", &note),
        ],
    )?)
}

#[cfg(test)]
mod tests;
