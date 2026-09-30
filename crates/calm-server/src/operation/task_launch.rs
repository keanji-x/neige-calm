//! The last check before a task execution's first provider or process effect.
use crate::db::{RepoEventWrite, write_in_tx_typed};
use crate::error::{CalmError, Result};
use std::future::Future;

#[derive(Clone)]
pub(crate) struct TaskLaunch {
    task_id: String,
    operation: super::Operation,
}

#[derive(Debug)]
pub(crate) struct LaunchFailure {
    pub error: CalmError,
    pub effect_started: bool,
}
impl From<CalmError> for LaunchFailure {
    fn from(error: CalmError) -> Self {
        Self {
            error,
            effect_started: false,
        }
    }
}

impl TaskLaunch {
    pub(crate) fn new(task_id: &str, operation: &super::Operation) -> Self {
        Self {
            task_id: task_id.into(),
            operation: operation.clone(),
        }
    }

    pub(crate) fn operation(&self) -> &super::Operation {
        &self.operation
    }

    pub(crate) async fn run<T, F>(self, repo: &dyn RepoEventWrite, effect: F) -> Result<T>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        self.run_observed(repo, effect)
            .await
            .map_err(|failure| failure.error)
    }

    pub(crate) fn task_id(&self) -> &str {
        &self.task_id
    }

    /// Refuse the effect when the execution is no longer the current, live attempt; otherwise run it.
    pub(crate) async fn run_observed<T, F>(
        self,
        repo: &dyn RepoEventWrite,
        effect: F,
    ) -> std::result::Result<T, LaunchFailure>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        let task_id = self.task_id.clone();
        write_in_tx_typed(repo, move |tx| {
            Box::pin(async move {
                crate::task_recovery::require_attempt_startable_tx(tx, &task_id).await
            })
        })
        .await?;
        effect.await.map_err(|error| LaunchFailure {
            error,
            effect_started: true,
        })
    }
}
