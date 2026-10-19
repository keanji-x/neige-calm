//! Caller wall times for track deletion. Stages may nest and must not be added together.
//! SQLite's transaction observer owns admission/BEGIN/body/COMMIT timing; these events
//! describe lifecycle work, not physical lock ownership. No message, path or error payload is logged.
use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use tracing::{Dispatch, Span};

const TARGET: &str = "calm_server::track_deletion";

/// The fixed stages of the deletion workflow, rather than an extensible owner registry.
#[derive(Clone, Copy)]
pub(super) enum Stage {
    Request,
    ManagedGuard,
    OperationLock,
    TrackLock,
    Preflight,
    Reprojection,
    Snapshot,
    OwnedDeletion,
    Teardown,
    DisposalCheck,
    CodexQuiesce,
    ClaudeSweep,
    AcpSweep,
    ExecutionQuiesce,
    TerminalQuiesce,
    HarnessShutdown,
    WorkspaceRecycle,
    Commit,
    PostCommit,
    WorktreeSweep,
    Compensation,
    Recovery,
}

impl Stage {
    fn name(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::ManagedGuard => "managed_guard",
            Self::OperationLock => "operation_lock",
            Self::TrackLock => "track_lock",
            Self::Preflight => "preflight",
            Self::Reprojection => "reprojection",
            Self::Snapshot => "snapshot",
            Self::OwnedDeletion => "owned_deletion",
            Self::Teardown => "teardown",
            Self::DisposalCheck => "disposal_check",
            Self::CodexQuiesce => "codex_quiesce",
            Self::ClaudeSweep => "claude_sweep",
            Self::AcpSweep => "acp_sweep",
            Self::ExecutionQuiesce => "execution_quiesce",
            Self::TerminalQuiesce => "terminal_quiesce",
            Self::HarnessShutdown => "harness_shutdown",
            Self::WorkspaceRecycle => "workspace_recycle",
            Self::Commit => "commit",
            Self::PostCommit => "post_commit",
            Self::WorktreeSweep => "worktree_sweep",
            Self::Compensation => "compensation",
            Self::Recovery => "recovery",
        }
    }
}

struct Context {
    track_id: String,
    deletion_id: String,
    started: Instant,
    dispatch: Dispatch,
    span: Span,
}

#[derive(Clone)]
pub(super) struct DeletionObservation(Arc<Context>);

impl DeletionObservation {
    pub(super) fn new(track_id: &str) -> Self {
        let deletion_id = crate::model::new_id();
        Self(Arc::new(Context {
            track_id: track_id.to_owned(),
            span: tracing::info_span!("track_deletion", track_id, %deletion_id),
            deletion_id,
            started: Instant::now(),
            dispatch: tracing::dispatcher::get_default(Clone::clone),
        }))
    }

    pub(super) fn dispatch(&self) -> Dispatch {
        self.0.dispatch.clone()
    }

    pub(super) fn span(&self) -> Span {
        self.0.span.clone()
    }

    pub(super) fn stage(&self, stage: Stage) -> StageObservation {
        let observation = StageObservation {
            context: self.clone(),
            stage,
            started: Instant::now(),
            finished: false,
        };
        observation.emit("begin");
        observation
    }

    pub(super) async fn observe<T, E>(
        &self,
        stage: Stage,
        future: impl Future<Output = Result<T, E>>,
    ) -> Result<T, E> {
        let mut observation = self.stage(stage);
        let result = future.await;
        observation.finish(if result.is_ok() { "ok" } else { "error" });
        result
    }

    pub(super) fn observe_sync<T, E>(
        &self,
        stage: Stage,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let mut observation = self.stage(stage);
        let result = operation();
        observation.finish(if result.is_ok() { "ok" } else { "error" });
        result
    }
}

pub(super) struct StageObservation {
    context: DeletionObservation,
    stage: Stage,
    started: Instant,
    finished: bool,
}

impl StageObservation {
    pub(super) fn finish(&mut self, outcome: &'static str) {
        self.finished = true;
        self.emit(outcome);
    }

    fn emit(&self, outcome: &'static str) {
        let context = &self.context.0;
        if context.dispatch.is::<tracing::subscriber::NoSubscriber>() {
            return;
        }
        // A cancelled task may drop outside its polling subscriber, just as a
        // cancelled SQLx transaction does. Retain the originating dispatch.
        tracing::dispatcher::with_default(&context.dispatch, || {
            tracing::info!(
                target: TARGET,
                parent: &context.span,
                track_id = %context.track_id,
                deletion_id = %context.deletion_id,
                stage = self.stage.name(),
                outcome,
                elapsed_ms = self.started.elapsed().as_secs_f64() * 1000.0,
                deletion_elapsed_ms = context.started.elapsed().as_secs_f64() * 1000.0,
                "track deletion stage"
            );
        });
    }
}

impl Drop for StageObservation {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(if std::thread::panicking() {
                "panicked"
            } else {
                "interrupted"
            });
        }
    }
}
