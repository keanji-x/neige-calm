//! #1755: Claude Code asks "Is this a project you created or one you trust?" before a session
//! starts in a folder it holds no trust record for, with "No, exit" selected. A Track worker's
//! checkout is such a folder the first time, and no human is there to answer, so the kernel
//! answers for a scheduler-spawned worker: Down, then Enter once Claude itself shows the cursor
//! resting on "Yes, I trust this folder". Claude persists the trust in its own config; neige never
//! touches it. Owner Claude cards are never answered for: a human is there to decide.
//!
//! The kernel decides once, on the first screen that settles. Claude paints nothing before either
//! the dialog (untrusted folder) or its session banner (trusted), so a first settled screen that
//! is not the dialog means the session has started: the watch ends there and never sends a key,
//! whatever the worker prints later (a grep hit that quotes the dialog included).
use std::sync::Arc;
use std::time::Duration;

use crate::db::write_with_actor_events_typed;
use crate::event::EventBus;
use crate::operation::SpawnCtx;
use crate::state::WriteContext;
use crate::terminal_interaction::{
    AllPresent, InputOutcome, KernelClaim, KernelTerminal, ScreenWait, SettledScreen,
    TextConditions,
};
use crate::terminal_renderer::TerminalRendererRegistry;

/// The dialog's question, as Claude Code 2.1.280 paints it.
pub(crate) const TRUST_QUESTION: &str = "Is this a project you created or one you trust?";
/// The accepting option's label.
pub(crate) const TRUST_OPTION: &str = "Yes, I trust this folder";
/// The accepting option's row while Claude's selection cursor is on it.
pub(crate) const TRUST_CURSOR: &str = "❯ Yes, I trust this folder";
/// The declining option's row while the cursor is on it (Claude's default).
const DECLINE_CURSOR: &str = "❯ No, exit";
/// How long a screen must be unchanged to count as settled: the first settled screen decides,
/// and the cursor row must rest this long before Enter. Claude paints the dialog before its input
/// handler is live, and a key sent then is lost or undone when the dialog settles.
pub const TRUST_SETTLE: Duration = Duration::from_secs(1);
/// Down presses per answer: one, plus one retry when Claude dropped the first.
const DOWN_ATTEMPTS: usize = 2;

/// How long the kernel watches a new worker's screen for the dialog, and how long each step of
/// answering it may take.
#[derive(Clone, Debug)]
pub struct TrustPromptWatch {
    /// From the spawn until the dialog must be on screen; past it the screen is left alone.
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

/// How one watch ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustOutcome {
    /// The kernel selected "Yes, I trust this folder" and the dialog went away.
    Accepted,
    /// No key was sent: the first settled screen was not the dialog, no screen settled within the
    /// window, or the dialog left before the kernel acted.
    NotShown,
    /// The terminal stopped (exit, no live renderer) before an answer: no failure is reported.
    Stopped,
    /// A human held the terminal when the dialog appeared: theirs to answer, no input was sent.
    HumanOwned,
    /// The dialog was shown and the kernel could not accept it: the worker task fails with this
    /// reason.
    NotAccepted(String),
}

/// The worker whose terminal is watched.
pub(crate) struct TrustTarget {
    pub card_id: String,
    pub track_id: String,
    pub terminal_id: String,
    pub worker_session_id: String,
}

/// Watch the just-spawned worker's terminal in the background and answer the dialog when it is
/// the first screen to settle. A dialog the kernel could not accept fails the worker's task
/// through the scheduler's startup-blocked failure.
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

fn dialog() -> AllPresent {
    AllPresent(vec![TRUST_QUESTION.into(), TRUST_OPTION.into()])
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
    // Decided once: a first settled screen that is not the dialog ends the watch for good.
    match terminal.settled_screen(watch.appear, TRUST_SETTLE).await {
        SettledScreen::Rows(rows) if dialog().holds(&rows) => {}
        SettledScreen::Rows(_) | SettledScreen::TimedOut => return TrustOutcome::NotShown,
        SettledScreen::Stopped => return TrustOutcome::Stopped,
    }
    match terminal.claim_if_unowned().await {
        Ok(KernelClaim::Granted) => {}
        Ok(KernelClaim::HeldByAnother) => return TrustOutcome::HumanOwned,
        Ok(KernelClaim::Unavailable(reason)) => {
            return not_accepted(&format!("the terminal could not be claimed: {reason}"));
        }
        Err(error) => return not_accepted(&format!("the terminal could not be claimed: {error}")),
    }
    let outcome = select_yes(&terminal, watch).await;
    terminal.release().await;
    outcome
}

/// Down until Claude's cursor rests on the accepting option (one retry when it stayed on "No,
/// exit"), then Enter only while it is still there: Enter is never sent on "No, exit", and no key
/// is sent once the dialog has left the screen.
async fn select_yes(terminal: &KernelTerminal, watch: &TrustPromptWatch) -> TrustOutcome {
    let cursor = |row: &str| TextConditions {
        present: vec![row.into()],
        absent: vec![],
    };
    let (on_yes, on_no) = (cursor(TRUST_CURSOR), cursor(DECLINE_CURSOR));
    let at_rest = TextConditions {
        present: vec![TRUST_CURSOR.into(), DECLINE_CURSOR.into()],
        absent: vec![],
    };
    let mut rests_on_yes = false;
    for _ in 0..DOWN_ATTEMPTS {
        if !terminal.shows(&dialog()) {
            return TrustOutcome::NotShown;
        }
        if let Some(failed) = pressed("Down", terminal.press("Down").await) {
            return failed;
        }
        match terminal
            .wait_until(&at_rest, watch.step, TRUST_SETTLE)
            .await
        {
            ScreenWait::Stopped => return TrustOutcome::Stopped,
            ScreenWait::Held if terminal.shows(&on_yes) => {
                rests_on_yes = true;
                break;
            }
            ScreenWait::Held if terminal.shows(&on_no) => {}
            _ => break,
        }
    }
    // Re-read right before Enter: still the dialog, and the steady row still the accepting one.
    if !terminal.shows(&dialog()) {
        return TrustOutcome::NotShown;
    }
    if !rests_on_yes || !terminal.shows(&on_yes) {
        return not_accepted(&format!("the cursor never rested on \"{TRUST_OPTION}\""));
    }
    if let Some(failed) = pressed("Enter", terminal.press("Enter").await) {
        return failed;
    }
    dialog_left(terminal, watch).await
}

/// After Enter: the question gone, and still gone a settle period later.
async fn dialog_left(terminal: &KernelTerminal, watch: &TrustPromptWatch) -> TrustOutcome {
    let gone = TextConditions {
        present: vec![],
        absent: vec![TRUST_QUESTION.into()],
    };
    let deadline = tokio::time::Instant::now() + watch.step;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match terminal.wait_until(&gone, left, Duration::ZERO).await {
            ScreenWait::Held => {}
            ScreenWait::Stopped => return TrustOutcome::Stopped,
            ScreenWait::TimedOut => return not_accepted("the dialog was still shown after Enter"),
        }
        tokio::time::sleep(TRUST_SETTLE).await;
        if terminal.shows(&gone) {
            return TrustOutcome::Accepted;
        }
    }
}

/// `None` when the key was written; otherwise the outcome it ends the answer with.
fn pressed(key: &str, written: anyhow::Result<InputOutcome>) -> Option<TrustOutcome> {
    match written {
        Ok(InputOutcome::Written) => None,
        Ok(InputOutcome::Refused) => Some(not_accepted(&format!(
            "{key} was refused: the terminal was taken over"
        ))),
        Ok(InputOutcome::Unknown) => Some(not_accepted(&format!("{key} was not acknowledged"))),
        Err(error) => Some(not_accepted(&format!("{key} could not be sent: {error}"))),
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
