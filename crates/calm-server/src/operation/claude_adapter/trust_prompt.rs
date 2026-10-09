//! #1755: Claude Code asks "Is this a project you created or one you trust?" before a session
//! starts in a folder it holds no trust record for, with "No, exit" selected. A Track worker's
//! checkout is such a folder the first time, and no human is there to answer, so the kernel
//! answers for a scheduler-spawned worker: Down, then Enter once Claude itself shows the cursor on
//! "Yes, I trust this folder". Claude persists the trust in its own config; neige never touches it.
//! Owner Claude cards are never answered for: a human is there to decide.
use std::sync::Arc;
use std::time::Duration;

use crate::db::write_with_actor_events_typed;
use crate::event::EventBus;
use crate::operation::SpawnCtx;
use crate::state::WriteContext;
use crate::terminal_interaction::{
    AllPresent, InputOutcome, KernelClaim, KernelTerminal, ScreenWait, TextConditions,
};
use crate::terminal_renderer::TerminalRendererRegistry;

/// The dialog's question, as Claude Code 2.1.280 paints it.
pub(crate) const TRUST_QUESTION: &str = "Is this a project you created or one you trust?";
/// The accepting option's label.
pub(crate) const TRUST_OPTION: &str = "Yes, I trust this folder";
/// The accepting option's row while Claude's selection cursor is on it.
pub(crate) const TRUST_CURSOR: &str = "❯ Yes, I trust this folder";

/// How long the kernel watches a new worker's screen for the dialog, and how long each step of
/// answering it may take.
#[derive(Clone, Debug)]
pub struct TrustPromptWatch {
    /// From the spawn until the dialog must be on screen; past it the screen is left alone.
    pub appear: Duration,
    /// For Claude to move its cursor after Down, and to leave the dialog after Enter.
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
    /// The dialog was not on screen within [`TrustPromptWatch::appear`]: no input was sent.
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

/// Watch the just-spawned worker's terminal in the background and answer the dialog when it
/// appears. A dialog the kernel could not accept fails the worker's task through the scheduler's
/// startup-blocked failure.
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
    let dialog = AllPresent(vec![TRUST_QUESTION.into(), TRUST_OPTION.into()]);
    match terminal.wait_until(&dialog, watch.appear).await {
        ScreenWait::Held => {}
        ScreenWait::Stopped => return TrustOutcome::Stopped,
        ScreenWait::TimedOut => return TrustOutcome::NotShown,
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

/// Down, then Enter only once Claude shows its cursor on the accepting option: an option order
/// that changed must never turn the Enter into "No, exit".
async fn select_yes(terminal: &KernelTerminal, watch: &TrustPromptWatch) -> TrustOutcome {
    if let Some(failed) = pressed("Down", terminal.press("Down").await) {
        return failed;
    }
    let cursor = TextConditions {
        present: vec![TRUST_CURSOR.into()],
        absent: vec![],
    };
    match terminal.wait_until(&cursor, watch.step).await {
        ScreenWait::Held => {}
        ScreenWait::Stopped => return TrustOutcome::Stopped,
        ScreenWait::TimedOut => {
            return not_accepted(&format!("the cursor never reached \"{TRUST_OPTION}\""));
        }
    }
    if let Some(failed) = pressed("Enter", terminal.press("Enter").await) {
        return failed;
    }
    let gone = TextConditions {
        present: vec![],
        absent: vec![TRUST_QUESTION.into()],
    };
    match terminal.wait_until(&gone, watch.step).await {
        ScreenWait::Held => TrustOutcome::Accepted,
        ScreenWait::Stopped => TrustOutcome::Stopped,
        ScreenWait::TimedOut => not_accepted("the dialog was still shown after Enter"),
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
