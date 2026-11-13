//! The attach reader's completion owner survives its transport task. Retries consume only
//! its original explicit exit and captured output, never a projection row or a new replay.
use super::{OutputWake, output_capture::TerminalOutputSnapshot, wake_projector};
use crate::{
    db::RouteRepo, scheduler::TerminalTaskHook, session_projection_repo::WorkerSessionState,
};
use std::sync::Arc;
use tokio::sync::{Mutex, watch};

pub(super) struct LegacyExit {
    pub code: Option<i32>,
    pub signalled: bool,
    pub output: TerminalOutputSnapshot,
}

pub(super) struct LegacyCompletion {
    repo: Option<Arc<dyn RouteRepo>>,
    hook: Option<Arc<TerminalTaskHook>>,
    terminal: String,
    wake: OutputWake,
    persisted: watch::Sender<bool>,
    observed: Mutex<Option<LegacyExit>>,
}
impl LegacyCompletion {
    pub fn new(
        repo: Option<Arc<dyn RouteRepo>>,
        hook: Option<Arc<TerminalTaskHook>>,
        terminal: String,
        wake: OutputWake,
        persisted: watch::Sender<bool>,
    ) -> Arc<Self> {
        Arc::new(Self {
            repo,
            hook,
            terminal,
            wake,
            persisted,
            observed: Mutex::new(None),
        })
    }

    pub async fn observe(&self, exit: LegacyExit) -> anyhow::Result<bool> {
        let mut observed = self.observed.lock().await;
        anyhow::ensure!(observed.is_none(), "legacy exit already observed");
        *observed = Some(exit);
        self.complete(observed.as_ref()).await
    }

    /// Serialize with the reader and other retries. No writer lease spans this call.
    pub async fn retry(&self) -> anyhow::Result<bool> {
        let observed = self.observed.lock().await;
        self.complete(observed.as_ref()).await
    }

    async fn complete(&self, observed: Option<&LegacyExit>) -> anyhow::Result<bool> {
        if *self.persisted.borrow() {
            return Ok(true);
        }
        let Some(exit) = observed else {
            return Ok(false);
        };
        // Typed Store remains the sole exit writer even if classification changes.
        if let Some(repo) = &self.repo
            && let Some(term) = repo.terminal_get(&self.terminal).await?
        {
            anyhow::ensure!(
                !term.is_typed(repo.as_ref()).await?,
                "typed terminal requires Store completion"
            );
        }
        // Preserve all three existing idempotent attempts after a failure. Only complete
        // success acknowledges the observation; the same immutable output is used on retry.
        let mut failure = None;
        if let Some(repo) = &self.repo {
            if let Err(error) = repo
                .terminal_set_exit_with_output(
                    &self.terminal,
                    exit.code,
                    exit.signalled,
                    &exit.output.text,
                    exit.output.truncated,
                )
                .await
            {
                failure = Some(anyhow::Error::from(error));
            }
            let state = if exit.signalled {
                WorkerSessionState::Failed
            } else {
                WorkerSessionState::Exited
            };
            if let Err(error) =
                crate::terminal_sweeper::complete_ephemeral_session_from_terminal_exit(
                    repo.as_ref(),
                    &self.terminal,
                    state,
                )
                .await
            {
                failure = Some(anyhow::Error::from(error));
            }
        }
        if let Some(hook) = &self.hook
            && let Err(error) = hook
                .on_terminal_exit(
                    &self.terminal,
                    exit.code,
                    exit.signalled,
                    &exit.output.text,
                    exit.output.truncated,
                )
                .await
        {
            failure = Some(anyhow::Error::from(error));
        }
        wake_projector(&self.wake, &self.terminal);
        if let Some(error) = failure {
            return Err(error);
        }
        self.persisted.send_replace(true);
        Ok(true)
    }
}
