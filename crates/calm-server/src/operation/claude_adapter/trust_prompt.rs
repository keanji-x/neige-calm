//! #1755: Claude Code asks "Is this a project you created or one you trust?" before a session
//! starts in a folder it holds no trust record for, with "No, exit" selected. A Track worker's
//! checkout is such a folder the first time, and no human is there to answer, so the kernel
//! answers for a scheduler-spawned worker: Down, then Enter once Claude itself shows the cursor
//! resting on "Yes, I trust this folder". Claude persists the trust in its own config; neige never
//! touches it. Owner Claude cards are never answered for: a human is there to decide.
//!
//! Text alone cannot tell the dialog from a session whose output quotes it (a goal or a grep hit
//! that quotes the dialog). The session's own banner can: every session screen carries "Claude
//! Code v<version>" on top, and the dialog screen never does. So the first time any screen the
//! watch observes carries the banner, the session has started and the watch ends for good without
//! a key; only a complete dialog screen without the banner is ever answered.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::db::write_with_actor_events_typed;
use crate::event::EventBus;
use crate::operation::SpawnCtx;
use crate::state::WriteContext;
use crate::terminal_interaction::{
    AllPresent, ConditionState, InputOutcome, KernelClaim, KernelTerminal, RowTest, ScreenWait,
    TextConditions,
};
use crate::terminal_renderer::TerminalRendererRegistry;

/// The session banner Claude Code paints on top of every session screen, never on the dialog.
const SESSION_BANNER: &str = "Claude Code v";
/// The dialog screen's header, as Claude Code 2.1.280 paints it.
const WORKSPACE_HEADER: &str = "Accessing workspace:";
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
    /// For Claude's cursor to settle after Down, and to leave the dialog after Enter.
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

/// How one watch ended. Only `NotAccepted` fails the worker's task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustOutcome {
    /// The kernel selected "Yes, I trust this folder" and the session started or the dialog left.
    Accepted,
    /// No further key was sent: the session banner appeared, no complete dialog settled within
    /// the window, or the dialog left the screen before the kernel acted.
    NotShown,
    /// The terminal stopped (exit, no live renderer), or a key's outcome could not be confirmed.
    Stopped,
    /// Another client (a human, as a rule) held or took the terminal: theirs to answer.
    HumanOwned,
    /// The banner-free dialog was still shown after the kernel answered: the worker task fails
    /// with this reason.
    NotAccepted(String),
}

/// The worker whose terminal is watched.
pub(crate) struct TrustTarget {
    pub card_id: String,
    pub track_id: String,
    pub terminal_id: String,
    pub worker_session_id: String,
}

/// Watch the just-spawned worker's terminal in the background and answer the dialog if it comes
/// before the session. A dialog the kernel could not accept fails the worker's task through the
/// scheduler's startup-blocked failure.
pub(crate) fn watch_worker_trust_prompt(
    ctx: &SpawnCtx,
    write: WriteContext,
    target: TrustTarget,
    watch: &TrustPromptWatch,
) {
    let renderer = ctx.terminal_renderer.clone();
    let repo = ctx.repo.clone();
    let events = ctx.events.clone();
    let watch = watch.clone();
    tokio::spawn(async move {
        let outcome = answer(&renderer, &target, &watch).await;
        match &outcome {
            TrustOutcome::NotAccepted(reason) => {
                tracing::warn!(card_id = %target.card_id, terminal_id = %target.terminal_id, %reason, "claude worker trust dialog not accepted; failing its task");
                fail_worker(repo, &events, &write, &target, reason).await;
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

/// Whether the session banner was on any screen tested so far: sticky for the whole watch.
#[derive(Default)]
struct SessionBanner(AtomicBool);
impl SessionBanner {
    fn observe(&self, rows: &[String]) -> bool {
        if rows.iter().any(|row| row.contains(SESSION_BANNER)) {
            self.0.store(true, Ordering::SeqCst);
        }
        self.seen()
    }
    fn seen(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

fn verdict(holds: bool) -> (Option<(String, usize)>, ConditionState) {
    let state = ConditionState {
        present: Some(holds),
        absent: None,
    };
    (None, state)
}

/// Holds once the session banner has been seen.
struct SessionSeen<'a>(&'a SessionBanner);
impl RowTest for SessionSeen<'_> {
    fn test(&self, rows: &[String]) -> (Option<(String, usize)>, ConditionState) {
        verdict(self.0.observe(rows))
    }
}

/// Holds on a complete dialog screen (header, question, both options, footer) while no screen
/// ever carried the banner, and on `rows` too when given (a cursor row).
struct Dialog<'a> {
    banner: &'a SessionBanner,
    rows: Option<TextConditions>,
}
impl RowTest for Dialog<'_> {
    fn test(&self, rows: &[String]) -> (Option<(String, usize)>, ConditionState) {
        let screen = AllPresent(
            [
                WORKSPACE_HEADER,
                TRUST_QUESTION,
                DECLINE_OPTION,
                TRUST_OPTION,
                DIALOG_FOOTER,
            ]
            .map(String::from)
            .to_vec(),
        );
        let also = self
            .rows
            .as_ref()
            .is_none_or(|also| also.test(rows).1.holds());
        verdict(!self.banner.observe(rows) && screen.holds(rows) && also)
    }
}

/// Holds once the dialog has left: the banner seen, or the dialog's header gone.
struct DialogLeft<'a>(&'a SessionBanner);
impl RowTest for DialogLeft<'_> {
    fn test(&self, rows: &[String]) -> (Option<(String, usize)>, ConditionState) {
        let session = self.0.observe(rows);
        verdict(session || !rows.iter().any(|row| row.contains(WORKSPACE_HEADER)))
    }
}

/// How a wait for the dialog ended.
enum Seen {
    Dialog,
    Session,
    Stopped,
    TimedOut,
}

/// `dialog` settled for `settle`, raced against the session banner on any screen.
async fn dialog_or_session(
    terminal: &KernelTerminal,
    banner: &SessionBanner,
    dialog: &Dialog<'_>,
    budget: Duration,
    settle: Duration,
) -> Seen {
    let session = SessionSeen(banner);
    let waited = tokio::select! {
        biased;
        waited = terminal.wait_until(&session, budget, Duration::ZERO) => match waited {
            ScreenWait::Held => return Seen::Session,
            other => other,
        },
        waited = terminal.wait_until(dialog, budget, settle) => waited,
    };
    match waited {
        _ if banner.seen() => Seen::Session,
        ScreenWait::Held => Seen::Dialog,
        ScreenWait::Stopped => Seen::Stopped,
        ScreenWait::TimedOut => Seen::TimedOut,
    }
}

async fn answer(
    renderer: &TerminalRendererRegistry,
    target: &TrustTarget,
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
    let banner = SessionBanner::default();
    let dialog = Dialog {
        banner: &banner,
        rows: None,
    };
    match dialog_or_session(&terminal, &banner, &dialog, watch.appear, TRUST_SETTLE).await {
        Seen::Dialog => {}
        Seen::Session | Seen::TimedOut => return TrustOutcome::NotShown,
        Seen::Stopped => return TrustOutcome::Stopped,
    }
    match terminal.claim_if_unowned().await {
        Ok(KernelClaim::Granted) => {}
        Ok(KernelClaim::HeldByAnother) => return TrustOutcome::HumanOwned,
        Ok(KernelClaim::Unavailable(reason)) => {
            tracing::info!(terminal_id = %target.terminal_id, %reason, "claude worker trust dialog: terminal not claimed");
            return TrustOutcome::HumanOwned;
        }
        Err(_) => return TrustOutcome::Stopped,
    }
    let outcome = select_yes(&terminal, &banner, watch).await;
    terminal.release().await;
    outcome
}

/// Down until Claude's cursor rests on the accepting option (one retry when it stayed on "No,
/// exit"), then Enter only while the banner-free dialog still shows it there: Enter is never sent
/// on "No, exit", and no key is sent once the session started or the dialog left.
async fn select_yes(
    terminal: &KernelTerminal,
    banner: &SessionBanner,
    watch: &TrustPromptWatch,
) -> TrustOutcome {
    let on = |cursors: &[&str]| Dialog {
        banner,
        rows: Some(TextConditions {
            present: cursors.iter().map(|row| (*row).to_owned()).collect(),
            absent: vec![],
        }),
    };
    let shown = Dialog { banner, rows: None };
    let (on_yes, on_no, at_rest) = (
        on(&[TRUST_CURSOR]),
        on(&[DECLINE_CURSOR]),
        on(&[TRUST_CURSOR, DECLINE_CURSOR]),
    );
    let mut rests_on_yes = false;
    for _ in 0..DOWN_ATTEMPTS {
        if !terminal.shows(&shown) {
            return TrustOutcome::NotShown;
        }
        if let Some(ended) = pressed(terminal.press("Down").await) {
            return ended;
        }
        match dialog_or_session(terminal, banner, &at_rest, watch.step, TRUST_SETTLE).await {
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
    // Re-read right before Enter: still the banner-free dialog, the cursor still on Yes.
    if !terminal.shows(&shown) {
        return TrustOutcome::NotShown;
    }
    if !rests_on_yes || !terminal.shows(&on_yes) {
        return not_accepted(&format!("the cursor never rested on \"{TRUST_OPTION}\""));
    }
    if let Some(ended) = pressed(terminal.press("Enter").await) {
        return ended;
    }
    dialog_left(terminal, banner, watch).await
}

/// After Enter: accepted once the banner appears, or once the dialog's header is gone and still
/// gone a settle period later. The task fails only if the banner-free dialog is still shown when
/// the step ends.
async fn dialog_left(
    terminal: &KernelTerminal,
    banner: &SessionBanner,
    watch: &TrustPromptWatch,
) -> TrustOutcome {
    let left = DialogLeft(banner);
    let deadline = tokio::time::Instant::now() + watch.step;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match terminal.wait_until(&left, remaining, Duration::ZERO).await {
            ScreenWait::Held if banner.seen() => return TrustOutcome::Accepted,
            ScreenWait::Held => {}
            ScreenWait::Stopped => return TrustOutcome::Stopped,
            ScreenWait::TimedOut if terminal.shows(&Dialog { banner, rows: None }) => {
                return not_accepted("the dialog was still shown after Enter");
            }
            ScreenWait::TimedOut => return TrustOutcome::NotShown,
        }
        tokio::time::sleep(TRUST_SETTLE).await;
        if terminal.shows(&left) {
            return TrustOutcome::Accepted;
        }
    }
}

/// `None` when the key was written. A refused key means another client took the terminal: the
/// owner is handling the dialog, so nothing fails.
fn pressed(written: anyhow::Result<InputOutcome>) -> Option<TrustOutcome> {
    match written {
        Ok(InputOutcome::Written) => None,
        Ok(InputOutcome::Refused) => Some(TrustOutcome::HumanOwned),
        Ok(InputOutcome::Unknown) | Err(_) => Some(TrustOutcome::Stopped),
    }
}

fn not_accepted(detail: &str) -> TrustOutcome {
    TrustOutcome::NotAccepted(format!(
        "Claude's workspace trust dialog was not accepted: {detail}"
    ))
}

async fn fail_worker(
    repo: Arc<dyn crate::db::RouteRepo>,
    events: &EventBus,
    write: &WriteContext,
    target: &TrustTarget,
    reason: &str,
) {
    let (card_id, track_id, reason) = (
        target.card_id.clone(),
        target.track_id.clone(),
        reason.to_owned(),
    );
    let result =
        write_with_actor_events_typed::<(), _>(repo.as_ref(), None, events, write, move |tx| {
            Box::pin(async move {
                let events = crate::scheduler::fail_worker_startup_blocked_tx(
                    tx, &card_id, &track_id, &reason,
                )
                .await?;
                Ok(((), events))
            })
        })
        .await;
    if let Err(error) = result {
        tracing::warn!(card_id = %target.card_id, %error, "claude worker trust dialog failure could not be recorded");
    }
}
