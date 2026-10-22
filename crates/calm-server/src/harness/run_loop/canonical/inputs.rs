//! Transient issuer references and the last committed canonical read projection.
use super::CommittedFrame;
use crate::conversation_inputs::InputSource;
use crate::harness::registry::CanonicalSourceIndex;
use calm_types::conversation::CanonicalPendingIdentity;
use std::sync::Arc;
use tokio::sync::Mutex;

pub(in crate::harness::run_loop) struct CanonicalInputs {
    // This is an identity index, not an ordered queue or an admission cache.
    // Every claim/crossing still revalidates its actual issuer and current scope.
    pub(super) sources: Arc<CanonicalSourceIndex>,
    projection: Mutex<CommittedFrame>,
    execution: crate::harness::registry::MaintenanceExecutionCell,
}

impl CanonicalInputs {
    pub fn new(
        frame: CommittedFrame,
        sources: Arc<CanonicalSourceIndex>,
        execution: crate::harness::registry::MaintenanceExecutionCell,
    ) -> Self {
        Self {
            sources,
            projection: Mutex::new(frame),
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

    pub async fn source(&self, input: &CanonicalPendingIdentity) -> Option<InputSource> {
        self.sources.source(input)
    }

    pub async fn committed(&self) -> CommittedFrame {
        self.projection.lock().await.clone()
    }

    pub async fn publish_committed(&self, frame: CommittedFrame) {
        *self.projection.lock().await = frame;
    }
}
