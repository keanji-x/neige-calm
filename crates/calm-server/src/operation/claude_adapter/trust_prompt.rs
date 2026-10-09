//! #1755: Claude Code asks "Is this a project you created or one you trust?" before a session
//! starts in a folder it holds no trust record for, with "No, exit" selected. A Track worker's
//! checkout is such a folder the first time, and no human is there to answer, so the kernel
//! answers for a scheduler-spawned worker: Down, then Enter once Claude itself shows the cursor
//! resting on "Yes, I trust this folder". Claude persists the trust in its own config; neige never
//! touches it. Owner Claude cards are never answered for: a human is there to decide.
//!
//! The dialog only exists before the session starts, so the watch ends, sending nothing more, at
//! this worker session's `SessionStart` hook: text that merely looks like the dialog afterwards
//! (a grep hit in the worker's output) is never answered.
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::broadcast::{self, error::TryRecvError};

use crate::db::write_with_actor_events_typed;
use crate::event::{BroadcastEnvelope, Event, EventBus};
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
/// The declining option's row while the cursor is on it (Claude's default).
const DECLINE_CURSOR: &str = "❯ No, exit";
/// Claude's hook event once a session has started; its `session_id` is the worker's `--session-id`.
const SESSION_START_HOOK: &str = "SessionStart";
/// How long the screen must be unchanged before a key is sent, and the cursor row steady before
/// Enter: Claude paints the dialog before its input handler is live, and a key sent then is lost
/// or undone when the dialog settles.
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
    /// No dialog was answered: the session started first, or the window ended without one.
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
    /// The `--session-id` the worker's Claude was started with.
    pub claude_session_id: String,
}

/// This worker session's `SessionStart` hook on the kernel event bus, subscribed before the
/// spawn so it can never be missed.
struct SessionStart {
    events: broadcast::Receiver<BroadcastEnvelope>,
    card_id: String,
    claude_session_id: String,
    started: bool,
}

impl SessionStart {
    fn is_start(&self, envelope: &BroadcastEnvelope) -> bool {
        let Event::ClaudeHook {
            card_id, payload, ..
        } = &envelope.event
        else {
            return false;
        };
        card_id.as_str() == self.card_id
            && payload.get("hook_event_name").and_then(Value::as_str) == Some(SESSION_START_HOOK)
            && payload.get("session_id").and_then(Value::as_str)
                == Some(self.claude_session_id.as_str())
    }

    /// Whether the session has started, from what the bus delivered so far. A lagged or closed
    /// bus may have dropped the hook, so it counts as started: the kernel then stays silent.
    fn seen(&mut self) -> bool {
        while !self.started {
            match self.events.try_recv() {
                Ok(envelope) => self.started = self.is_start(&envelope),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Lagged(_) | TryRecvError::Closed) => self.started = true,
            }
        }
        self.started
    }

    async fn wait(&mut self) {
        while !self.started {
            match self.events.recv().await {
                Ok(envelope) => self.started = self.is_start(&envelope),
                Err(_) => self.started = true,
            }
        }
    }
}

/// A watch subscribed before the spawn and started once the spawn succeeded.
pub(crate) struct PendingTrustWatch {
    target: TrustTarget,
    session: SessionStart,
    watch: TrustPromptWatch,
}

impl PendingTrustWatch {
    pub(crate) fn before_spawn(
        ctx: &SpawnCtx,
        target: TrustTarget,
        watch: &TrustPromptWatch,
    ) -> Self {
        let session = SessionStart {
            events: ctx.events.subscribe(),
            card_id: target.card_id.clone(),
            claude_session_id: target.claude_session_id.clone(),
            started: false,
        };
        Self {
            target,
            session,
            watch: watch.clone(),
        }
    }

    /// Watch the just-spawned worker's terminal in the background and answer the dialog when it
    /// appears. A dialog the kernel could not accept fails the worker's task through the
    /// scheduler's startup-blocked failure.
    pub(crate) fn start(self, ctx: &SpawnCtx, write: WriteContext) {
        let renderer = ctx.terminal_renderer.clone();
        let repo = ctx.repo.clone();
        let events = ctx.events.clone();
        let Self {
            target,
            mut session,
            watch,
        } = self;
        tokio::spawn(async move {
            let outcome = answer(&renderer, &target, &watch, &mut session).await;
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
}

/// `wait` raced against the session start: `None` once the session has started.
async fn until_started(
    session: &mut SessionStart,
    wait: impl Future<Output = ScreenWait>,
) -> Option<ScreenWait> {
    if session.seen() {
        return None;
    }
    let waited = tokio::select! {
        biased;
        () = session.wait() => return None,
        waited = wait => waited,
    };
    (!session.seen()).then_some(waited)
}

async fn answer(
    renderer: &TerminalRendererRegistry,
    target: &TrustTarget,
    watch: &TrustPromptWatch,
    session: &mut SessionStart,
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
    match until_started(
        session,
        terminal.wait_until(&dialog, watch.appear, TRUST_SETTLE),
    )
    .await
    {
        Some(ScreenWait::Held) => {}
        Some(ScreenWait::Stopped) => return TrustOutcome::Stopped,
        Some(ScreenWait::TimedOut) | None => return TrustOutcome::NotShown,
    }
    match terminal.claim_if_unowned().await {
        Ok(KernelClaim::Granted) => {}
        Ok(KernelClaim::HeldByAnother) => return TrustOutcome::HumanOwned,
        Ok(KernelClaim::Unavailable(reason)) => {
            return not_accepted(&format!("the terminal could not be claimed: {reason}"));
        }
        Err(error) => return not_accepted(&format!("the terminal could not be claimed: {error}")),
    }
    let outcome = select_yes(&terminal, watch, session).await;
    terminal.release().await;
    outcome
}

/// Down until Claude's cursor rests on the accepting option (one retry when it stayed on "No,
/// exit"), then Enter only while it is still there: Enter is never sent on "No, exit".
async fn select_yes(
    terminal: &KernelTerminal,
    watch: &TrustPromptWatch,
    session: &mut SessionStart,
) -> TrustOutcome {
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
        if session.seen() {
            return TrustOutcome::NotShown;
        }
        if let Some(failed) = pressed("Down", terminal.press("Down").await) {
            return failed;
        }
        let rested = terminal.wait_until(&at_rest, watch.step, TRUST_SETTLE);
        match until_started(session, rested).await {
            None => return TrustOutcome::NotShown,
            Some(ScreenWait::Stopped) => return TrustOutcome::Stopped,
            Some(ScreenWait::Held) if terminal.shows(&on_yes) => {
                rests_on_yes = true;
                break;
            }
            Some(ScreenWait::Held) if terminal.shows(&on_no) => {}
            Some(_) => break,
        }
    }
    // Re-read right before Enter: the steady row must still be the accepting one.
    if !rests_on_yes || !terminal.shows(&on_yes) {
        return not_accepted(&format!("the cursor never rested on \"{TRUST_OPTION}\""));
    }
    if session.seen() {
        return TrustOutcome::NotShown;
    }
    if let Some(failed) = pressed("Enter", terminal.press("Enter").await) {
        return failed;
    }
    dialog_left(terminal, watch, session).await
}

/// After Enter: the session starting, or the question gone and still gone a settle period later.
async fn dialog_left(
    terminal: &KernelTerminal,
    watch: &TrustPromptWatch,
    session: &mut SessionStart,
) -> TrustOutcome {
    let gone = TextConditions {
        present: vec![],
        absent: vec![TRUST_QUESTION.into()],
    };
    let deadline = tokio::time::Instant::now() + watch.step;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match until_started(session, terminal.wait_until(&gone, left, Duration::ZERO)).await {
            None => return TrustOutcome::Accepted,
            Some(ScreenWait::Stopped) => return TrustOutcome::Stopped,
            Some(ScreenWait::TimedOut) => {
                return not_accepted("the dialog was still shown after Enter");
            }
            Some(ScreenWait::Held) => {}
        }
        tokio::select! {
            biased;
            () = session.wait() => return TrustOutcome::Accepted,
            () = tokio::time::sleep(TRUST_SETTLE) => {}
        }
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
