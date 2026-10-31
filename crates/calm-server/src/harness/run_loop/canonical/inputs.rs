//! Transient issuer references and the last committed canonical read projection.
use super::CommittedFrame;
use crate::conversation_inputs::InputSource;
use crate::harness::registry::CanonicalSourceIndex;
use calm_types::conversation::CanonicalPendingIdentity;
use futures::FutureExt;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

pub(in crate::harness::run_loop) struct CanonicalInputs {
    // This is an identity index, not an ordered queue or an admission cache.
    // Every claim/crossing still revalidates its actual issuer and current scope.
    pub(super) sources: Arc<CanonicalSourceIndex>,
    projection: Mutex<CommittedFrame>,
    progress: Arc<Notify>,
    execution: crate::harness::registry::MaintenanceExecutionCell,
}

impl CanonicalInputs {
    pub fn consumed(&self, inputs: &[CanonicalPendingIdentity]) {
        self.sources.consumed(inputs);
    }
    pub fn new(
        frame: CommittedFrame,
        sources: Arc<CanonicalSourceIndex>,
        execution: crate::harness::registry::MaintenanceExecutionCell,
    ) -> Self {
        let progress = sources.change_signal();
        Self {
            sources,
            projection: Mutex::new(frame),
            progress,
            execution,
        }
    }

    pub(in crate::harness::run_loop) fn maintenance_execution(
        &self,
    ) -> Option<crate::auth::LegacyPrivateExecution> {
        self.execution.read().expect("maintenance issuer").clone()
    }

    pub async fn observe_source(&self, input: CanonicalPendingIdentity, source: InputSource) {
        self.sources.observe(input, source);
    }

    #[cfg(test)]
    pub async fn source(&self, input: &CanonicalPendingIdentity) -> Option<InputSource> {
        self.sources.source(input)
    }

    pub(in crate::harness::run_loop) async fn progress_requested(&self) {
        self.sources.changed().await;
    }

    /// A timer may win while a hint is pending. Consume that old hint before
    /// reading current facts; hints arriving during the pass remain pending.
    pub(super) fn begin_progress(&self) {
        let _ = self.progress.notified().now_or_never();
    }

    /// A coalescing scheduling hint, never an input or execution grant.
    pub(in crate::harness::run_loop) fn request_progress(&self) {
        self.progress.notify_one();
    }

    /// One readiness interpretation for pending selection and restored claims.
    pub(super) fn execution_source(
        &self,
        input: &CanonicalPendingIdentity,
    ) -> crate::error::Result<Option<InputSource>> {
        use crate::harness::registry::SourceReadiness;
        match self.sources.readiness(input) {
            SourceReadiness::Ready(source) => {
                source.check_current()?;
                Ok(Some(source))
            }
            SourceReadiness::AwaitingHandoff => Ok(None),
            SourceReadiness::NeedsSource => Err(crate::error::CalmError::Unauthorized),
        }
    }

    pub async fn committed(&self) -> CommittedFrame {
        self.projection.lock().await.clone()
    }

    pub async fn publish_committed(&self, frame: CommittedFrame) {
        *self.projection.lock().await = frame;
    }
}
