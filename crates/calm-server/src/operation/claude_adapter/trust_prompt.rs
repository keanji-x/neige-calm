//! #1755: Claude Code asks "Is this a project you created or one you trust?" before a session
//! starts in a folder it holds no trust record for, with "No, exit" selected. A Track worker's
//! checkout is such a folder the first time, and no human is there to answer, so the kernel
//! answers for a scheduler-spawned worker: Down, then Enter once Claude itself shows the cursor
//! resting on "Yes, I trust this folder". Claude persists the trust in its own config; neige never
//! touches it. Owner Claude cards are never answered for: a human is there to decide.
//!
//! The session's own output can quote the dialog, so the kernel never acts on text alone once the
//! session may have started. Claude writes its transcript (`<projects>/<cwd slug>/<session
//! id>.jsonl`) only after trust, and paints its "Claude Code v<version>" banner on top of every
//! session screen and never on the dialog: either one, seen at any point of the watch (the banner
//! anywhere in the buffer, scrollback included), ends the watch for good without a key. Only a
//! complete dialog screen in Claude's own layout (a rule row, then the "Accessing workspace:"
//! header one column in; an echoed goal is indented further) is ever answered.
//!
//! The watch never fails the task: a dialog it cannot get past ends it with a warning, and the
//! task's liveness timeout stays the backstop.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::operation::SpawnCtx;
use crate::terminal_interaction::{
    AllPresent, ConditionState, InputOutcome, KernelClaim, KernelTerminal, RowTest, ScreenWait,
    TextConditions,
};
use crate::terminal_renderer::TerminalRendererRegistry;

/// The session banner Claude Code paints on top of every session screen, never on the dialog.
const SESSION_BANNER: &str = "Claude Code v";
/// The dialog screen's rule row, painted from column 0 right above its header.
const DIALOG_RULE: char = '─';
/// The dialog's header row as Claude Code 2.1.280 paints it, one column in.
const DIALOG_HEADER_ROW: &str = " Accessing workspace:";
/// The dialog's question.
pub(crate) const TRUST_QUESTION: &str = "Is this a project you created or one you trust?";
/// The declining option's label (Claude's default selection).
const DECLINE_OPTION: &str = "No, exit";
/// The accepting option's label.
pub(crate) const TRUST_OPTION: &str = "Yes, I trust this folder";
/// The dialog screen's footer.
const DIALOG_FOOTER: &str = "Enter to confirm";
/// The accepting option's row while Claude's selection cursor is on it.
pub(crate) const TRUST_CURSOR: &str = "❯ Yes, I trust this folder";
/// The declining option's row while the cursor is on it.
const DECLINE_CURSOR: &str = "❯ No, exit";
/// How long the dialog must be unchanged before Down, and the cursor row steady before Enter:
/// Claude paints the dialog before its input handler is live, and a key sent then is lost or
/// undone when the dialog settles.
pub const TRUST_SETTLE: Duration = Duration::from_secs(1);
/// Down presses per answer: one, plus one retry when Claude dropped the first.
const DOWN_ATTEMPTS: usize = 2;

/// How long the kernel watches a new worker's screen for the dialog, and how long each step of
/// answering it may take.
#[derive(Clone, Debug)]
pub struct TrustPromptWatch {
    /// From the spawn until a complete dialog must have settled; past it nothing is sent.
    pub appear: Duration,
    /// For Claude's cursor to settle after Down, and for the session to start after Enter.
    pub step: Duration,
    /// Test observability: every watch's outcome, sent once it is final.
    #[cfg(feature = "fixtures")]
    pub outcomes: Option<tokio::sync::mpsc::UnboundedSender<TrustOutcome>>,
}

impl Default for TrustPromptWatch {
    fn default() -> Self {
        Self {
            appear: Duration::from_secs(60),
            step: Duration::from_secs(10),
            #[cfg(feature = "fixtures")]
            outcomes: None,
        }
    }
}

/// How one watch ended. No outcome fails the task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustOutcome {
    /// The kernel selected "Yes, I trust this folder" and the session started.
    Accepted,
    /// No (further) key was sent: the session had started, no complete dialog settled within the
    /// window, or the dialog left the screen before the kernel acted.
    NotShown,
    /// The terminal stopped (exit, no live renderer), or a key's outcome could not be confirmed.
    Stopped,
    /// Another client (a human, as a rule) held or took the terminal: theirs to answer.
    HumanOwned,
    /// The kernel could not get past the dialog (the cursor never rested on Yes, or no session
    /// started after Enter). Logged; the task's liveness timeout is the backstop.
    Unanswered(String),
    /// No transcript path could be derived for the worker's cwd (its project directory name is
    /// longer than Claude keeps as is), so the session start could be missed: the watch stayed
    /// out without looking at the screen.
    Unwatched,
}

/// The worker whose terminal is watched.
pub(crate) struct TrustTarget {
    pub card_id: String,
    pub terminal_id: String,
    pub worker_session_id: String,
    /// Where this worker's Claude writes its session transcript, once its session has started;
    /// `None` when that path cannot be derived reliably.
    pub transcript: Option<PathBuf>,
}

/// Watch the just-spawned worker's terminal in the background and answer the dialog if it comes
/// before the session.
pub(crate) fn watch_worker_trust_prompt(
    ctx: &SpawnCtx,
    target: TrustTarget,
    watch: &TrustPromptWatch,
) {
    let renderer = ctx.terminal_renderer.clone();
    let watch = watch.clone();
    tokio::spawn(async move {
        let outcome = match &target.transcript {
            Some(transcript) => answer(&renderer, &target, transcript, &watch).await,
            None => {
                tracing::warn!(card_id = %target.card_id, terminal_id = %target.terminal_id, "claude worker trust dialog watch stays out: no reliable transcript path for the worker's cwd");
                TrustOutcome::Unwatched
            }
        };
        match &outcome {
            TrustOutcome::Unanswered(reason) => {
                tracing::warn!(card_id = %target.card_id, terminal_id = %target.terminal_id, %reason, "claude worker trust dialog left unanswered; the task's liveness timeout is the backstop");
            }
            other => {
                tracing::info!(card_id = %target.card_id, terminal_id = %target.terminal_id, outcome = ?other, "claude worker trust dialog watch ended");
            }
        }
        #[cfg(feature = "fixtures")]
        if let Some(outcomes) = &watch.outcomes {
            let _ = outcomes.send(outcome);
        }
    });
}

/// Evidence that the session has started, sticky for the whole watch.
struct SessionStart<'a> {
    seen: AtomicBool,
    transcript: &'a Path,
}
impl SessionStart<'_> {
    /// From one screen: the banner on it, or the transcript on disk.
    fn observe(&self, rows: &[String]) -> bool {
        if rows.iter().any(|row| row.contains(SESSION_BANNER)) || self.transcript.exists() {
            self.seen.store(true, Ordering::SeqCst);
        }
        self.seen.load(Ordering::SeqCst)
    }
    /// Before an action: the banner anywhere in the buffer (scrollback included), or the
    /// transcript. A buffer that cannot be read counts as started.
    fn confirmed(&self, terminal: &KernelTerminal) -> bool {
        if !self.seen.load(Ordering::SeqCst)
            && (terminal.buffer_contains(SESSION_BANNER).unwrap_or(true)
                || self.transcript.exists())
        {
            self.seen.store(true, Ordering::SeqCst);
        }
        self.seen.load(Ordering::SeqCst)
    }
}

fn verdict(holds: bool) -> (Option<(String, usize)>, ConditionState) {
    let state = ConditionState {
        present: Some(holds),
        absent: None,
    };
    (None, state)
}

/// Holds once the session has been seen to start.
struct SessionSeen<'a>(&'a SessionStart<'a>);
impl RowTest for SessionSeen<'_> {
    fn test(&self, rows: &[String]) -> (Option<(String, usize)>, ConditionState) {
        verdict(self.0.observe(rows))
    }
}

/// A complete dialog screen in Claude's layout: a rule row from column 0, the header row right
/// under it, and the question, both options and the footer.
fn dialog_screen(rows: &[String]) -> bool {
    let anchored = rows
        .windows(2)
        .any(|pair| pair[0].starts_with(DIALOG_RULE) && pair[1].starts_with(DIALOG_HEADER_ROW));
    let elements = AllPresent(
        [TRUST_QUESTION, DECLINE_OPTION, TRUST_OPTION, DIALOG_FOOTER]
            .map(String::from)
            .to_vec(),
    );
    anchored && elements.holds(rows)
}

/// Holds on a dialog screen while the session was never seen to start, and on `rows` too when
/// given (a cursor row).
struct Dialog<'a> {
    session: &'a SessionStart<'a>,
    rows: Option<TextConditions>,
}
impl RowTest for Dialog<'_> {
    fn test(&self, rows: &[String]) -> (Option<(String, usize)>, ConditionState) {
        let also = self
            .rows
            .as_ref()
            .is_none_or(|also| also.test(rows).1.holds());
        verdict(!self.session.observe(rows) && dialog_screen(rows) && also)
    }
}

/// How a wait for the dialog ended.
enum Seen {
    Dialog,
    Session,
    Stopped,
    TimedOut,
}

/// `dialog` settled for `settle`, raced against the session starting.
async fn dialog_or_session(
    terminal: &KernelTerminal,
    session: &SessionStart<'_>,
    dialog: &Dialog<'_>,
    budget: Duration,
    settle: Duration,
) -> Seen {
    let started = SessionSeen(session);
    let waited = tokio::select! {
        biased;
        waited = terminal.wait_until(&started, budget, Duration::ZERO) => match waited {
            ScreenWait::Held => return Seen::Session,
            other => other,
        },
        waited = terminal.wait_until(dialog, budget, settle) => waited,
    };
    match waited {
        _ if session.observe(&[]) => Seen::Session,
        ScreenWait::Held => Seen::Dialog,
        ScreenWait::Stopped => Seen::Stopped,
        ScreenWait::TimedOut => Seen::TimedOut,
    }
}

async fn answer(
    renderer: &TerminalRendererRegistry,
    target: &TrustTarget,
    transcript: &Path,
    watch: &TrustPromptWatch,
) -> TrustOutcome {
    let terminal = match KernelTerminal::attach(
        renderer,
        &target.terminal_id,
        &target.card_id,
        &target.worker_session_id,
    )
    .await
    {
        Ok(terminal) => terminal,
        Err(error) => {
            tracing::debug!(terminal_id = %target.terminal_id, %error, "claude worker trust dialog watch could not attach");
            return TrustOutcome::Stopped;
        }
    };
    let session = SessionStart {
        seen: AtomicBool::new(false),
        transcript,
    };
    let dialog = Dialog {
        session: &session,
        rows: None,
    };
    match dialog_or_session(&terminal, &session, &dialog, watch.appear, TRUST_SETTLE).await {
        Seen::Dialog => {}
        Seen::Session | Seen::TimedOut => return TrustOutcome::NotShown,
        Seen::Stopped => return TrustOutcome::Stopped,
    }
    if session.confirmed(&terminal) {
        return TrustOutcome::NotShown;
    }
    match terminal.claim_if_unowned().await {
        Ok(KernelClaim::Granted) => {}
        Ok(KernelClaim::HeldByAnother | KernelClaim::Unavailable(_)) => {
            return TrustOutcome::HumanOwned;
        }
        Err(_) => return TrustOutcome::Stopped,
    }
    let outcome = select_yes(&terminal, &session, watch).await;
    terminal.release().await;
    outcome
}

/// Down until Claude's cursor rests on the accepting option (one retry when it stayed on "No,
/// exit"), then Enter only while the dialog still shows it there. Before every key the session
/// start is checked again: no key is sent once it may have started, and none on "No, exit".
async fn select_yes(
    terminal: &KernelTerminal,
    session: &SessionStart<'_>,
    watch: &TrustPromptWatch,
) -> TrustOutcome {
    let on = |cursors: &[&str]| Dialog {
        session,
        rows: Some(TextConditions {
            present: cursors.iter().map(|row| (*row).to_owned()).collect(),
            absent: vec![],
        }),
    };
    let shown = Dialog {
        session,
        rows: None,
    };
    let (on_yes, on_no, at_rest) = (
        on(&[TRUST_CURSOR]),
        on(&[DECLINE_CURSOR]),
        on(&[TRUST_CURSOR, DECLINE_CURSOR]),
    );
    let mut rests_on_yes = false;
    for _ in 0..DOWN_ATTEMPTS {
        if session.confirmed(terminal) || !terminal.shows(&shown) {
            return TrustOutcome::NotShown;
        }
        if let Some(ended) = pressed(terminal.press("Down").await) {
            return ended;
        }
        match dialog_or_session(terminal, session, &at_rest, watch.step, TRUST_SETTLE).await {
            Seen::Session => return TrustOutcome::NotShown,
            Seen::Stopped => return TrustOutcome::Stopped,
            Seen::Dialog if terminal.shows(&on_yes) => {
                rests_on_yes = true;
                break;
            }
            Seen::Dialog if terminal.shows(&on_no) => {}
            Seen::Dialog | Seen::TimedOut => break,
        }
    }
    // Re-read right before Enter: no session, still the dialog, the cursor still on Yes.
    if session.confirmed(terminal) || !terminal.shows(&shown) {
        return TrustOutcome::NotShown;
    }
    if !rests_on_yes || !terminal.shows(&on_yes) {
        return unanswered(
            terminal,
            &format!("the cursor never rested on \"{TRUST_OPTION}\""),
        );
    }
    if let Some(ended) = pressed(terminal.press("Enter").await) {
        return ended;
    }
    // Accepted once the session starts: its banner or its transcript.
    match terminal
        .wait_until(&SessionSeen(session), watch.step, Duration::ZERO)
        .await
    {
        ScreenWait::Held => TrustOutcome::Accepted,
        ScreenWait::Stopped => TrustOutcome::Stopped,
        ScreenWait::TimedOut if session.confirmed(terminal) => TrustOutcome::Accepted,
        ScreenWait::TimedOut => unanswered(terminal, "no session started after Enter"),
    }
}

/// The kernel could not get past the dialog; when another client holds the terminal by now, it
/// is theirs.
fn unanswered(terminal: &KernelTerminal, detail: &str) -> TrustOutcome {
    if terminal.holds_control() {
        TrustOutcome::Unanswered(format!("Claude's workspace trust dialog: {detail}"))
    } else {
        TrustOutcome::HumanOwned
    }
}

/// `None` when the key was written. A refused key means another client took the terminal: the
/// owner is handling the dialog.
fn pressed(written: anyhow::Result<InputOutcome>) -> Option<TrustOutcome> {
    match written {
        Ok(InputOutcome::Written) => None,
        Ok(InputOutcome::Refused) => Some(TrustOutcome::HumanOwned),
        Ok(InputOutcome::Unknown) | Err(_) => Some(TrustOutcome::Stopped),
    }
}
