//! A kernel-originated client of one terminal (#1755): the renderer connection, text wait,
//! claim-if-unowned, acknowledged input and release the Planner's terminal tools use, bound to no
//! Planner identity. An adapter drives a startup screen of its own provider with it; the rules of
//! that screen stay with the adapter.
use super::Binding;
use super::client::Client;
use super::input_control::{self, ClaimStep, ReleaseStep};
pub use super::operations::InputOutcome;
use super::operations::{reserve_input, send_reserved};
pub use super::text_conditions::{AllPresent, TextConditions};
use super::text_conditions::{ConditionState, RowTest};
use crate::terminal_renderer::{
    CONTROL_HELD_BY_ANOTHER_CLIENT, ClientInputScope, TerminalRendererRegistry, WriteShape,
};
use anyhow::{Result, ensure};
use futures::future::BoxFuture;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How a kernel wait for a screen ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenWait {
    /// The test held on the live viewport.
    Held,
    /// The process exited, the connection ended or the projection was invalidated.
    Stopped,
    /// The budget ended first.
    TimedOut,
}

/// How a wait for the first settled screen ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettledScreen {
    /// The rows of the first non-blank screen that stayed unchanged for the settle window.
    Rows(Vec<String>),
    /// The process exited, the connection ended or the projection was invalidated.
    Stopped,
    /// No non-blank screen settled within the budget.
    TimedOut,
}

/// Holds on any non-blank screen and keeps the rows it was last tested on: the wait re-tests on
/// every revision, so once it settles they are the settled screen's rows.
struct NonBlank(Mutex<Vec<String>>);
impl RowTest for NonBlank {
    fn test(&self, rows: &[String]) -> (Option<(String, usize)>, ConditionState) {
        if let Ok(mut last) = self.0.lock() {
            *last = rows.to_vec();
        }
        let non_blank = rows.iter().any(|row| !row.trim().is_empty());
        let state = ConditionState {
            present: Some(non_blank),
            absent: None,
        };
        (None, state)
    }
}

/// The verdict of a kernel claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KernelClaim {
    /// This connection holds control.
    Granted,
    /// Another client (a human) already held control; nothing changed.
    HeldByAnother,
    /// Not granted for another reason: refused, unconfirmed, or granted then taken over.
    Unavailable(String),
}

pub struct KernelTerminal {
    client: Client,
}

impl KernelTerminal {
    /// Attach an observer connection to the live renderer of `terminal_id`. It never starts a
    /// process; a terminal without a live renderer is an error.
    pub async fn attach(
        renderer: &TerminalRendererRegistry,
        terminal_id: &str,
        card_id: &str,
        worker_session_id: &str,
    ) -> Result<Self> {
        let entry = renderer
            .get(terminal_id)
            .ok_or_else(|| anyhow::anyhow!("terminal {terminal_id} has no live renderer"))?;
        // The kernel's own connection, dropped when its caller is done: no Planner binding to recheck.
        let kernel: Arc<dyn Fn() -> BoxFuture<'static, bool> + Send + Sync> =
            Arc::new(|| Box::pin(async { true }));
        let scope = ClientInputScope::Bound {
            observe: kernel.clone(),
            control: kernel,
        };
        let binding = Binding {
            terminal_id: terminal_id.to_owned(),
            card_id: card_id.to_owned(),
            worker_session_id: worker_session_id.to_owned(),
            task: None,
        };
        Ok(Self {
            client: Client::attach(entry, scope, binding).await?,
        })
    }

    /// Wait until `test` holds on the live viewport, re-tested on every revision, and the screen
    /// has then been unchanged for `settle` (zero: as soon as it holds).
    pub async fn wait_until(
        &self,
        test: &impl RowTest,
        budget: Duration,
        settle: Duration,
    ) -> ScreenWait {
        match super::wait::wait_for_rows(&self.client, test, budget, settle).await {
            None => ScreenWait::Stopped,
            Some(waited) if waited.exited => ScreenWait::Stopped,
            Some(waited) if waited.holds && (settle.is_zero() || waited.settled) => {
                ScreenWait::Held
            }
            Some(_) => ScreenWait::TimedOut,
        }
    }

    /// The first non-blank screen that stays unchanged for `settle`, as its rows.
    pub async fn settled_screen(&self, budget: Duration, settle: Duration) -> SettledScreen {
        let screen = NonBlank(Mutex::new(Vec::new()));
        match self.wait_until(&screen, budget, settle).await {
            ScreenWait::Held => SettledScreen::Rows(screen.0.into_inner().unwrap_or_default()),
            ScreenWait::Stopped => SettledScreen::Stopped,
            ScreenWait::TimedOut => SettledScreen::TimedOut,
        }
    }

    /// Whether `test` holds on the live viewport right now.
    pub fn shows(&self, test: &impl RowTest) -> bool {
        super::wait::live_rows(&self.client).is_some_and(|(rows, _)| test.test(&rows).1.holds())
    }

    /// Claim control only if no other client holds it (decided under the owner-registry lock).
    pub async fn claim_if_unowned(&self) -> Result<KernelClaim> {
        let (control, grants_before) = {
            let state = self
                .client
                .screen
                .lock()
                .map_err(|_| anyhow::anyhow!("terminal state poisoned"))?;
            (state.control, state.grants)
        };
        if control.is_some() {
            return Ok(KernelClaim::Granted);
        }
        Ok(
            match input_control::claim_unowned(&self.client, grants_before).await? {
                ClaimStep::Held | ClaimStep::Claimed(_) => KernelClaim::Granted,
                ClaimStep::Unavailable { reason, .. }
                    if reason == CONTROL_HELD_BY_ANOTHER_CLIENT =>
                {
                    KernelClaim::HeldByAnother
                }
                ClaimStep::Unavailable { reason, .. } => KernelClaim::Unavailable(reason),
            },
        )
    }

    /// Press `key` (encoded for the live input modes) as one acknowledged write.
    pub async fn press(&self, key: &str) -> Result<InputOutcome> {
        {
            let state = self
                .client
                .screen
                .lock()
                .map_err(|_| anyhow::anyhow!("terminal state poisoned"))?;
            ensure!(
                state.available && !state.exited && state.pending.is_none(),
                "terminal unavailable or prior input outcome unknown"
            );
        }
        let modes = {
            let view = self
                .client
                .entry
                .handle
                .model_view
                .lock()
                .map_err(|_| anyhow::anyhow!("terminal view poisoned"))?;
            view.capture(0)?.0.input_surface().modes
        };
        let bytes = calm_terminal_view::key_bytes(key, modes)?;
        let sequence = reserve_input(&self.client)?;
        Ok(send_reserved(&self.client, bytes, sequence, WriteShape::Verbatim).await)
    }

    /// Release control if this connection still holds it; true once it no longer does.
    pub async fn release(&self) -> bool {
        matches!(
            input_control::release(&self.client).await,
            ReleaseStep::Released | ReleaseStep::NotHeld
        )
    }
}
