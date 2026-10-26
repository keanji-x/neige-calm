//! Caller-owned policy and intent construction share the admission writer transaction.
use super::*;

#[async_trait]
pub trait Admission: Send + Sync {
    async fn authorize_tx(&self, tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<()>;
    async fn intent_tx(
        self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ) -> Result<ExecutionIntent>;
}

/// Planner preparation is process-free and already owns its ephemeral payload.
/// It imposes no additional admission policy.
pub struct PreparedIntent(pub ExecutionIntent);
#[async_trait]
impl Admission for PreparedIntent {
    async fn authorize_tx(&self, _: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<()> {
        Ok(())
    }
    async fn intent_tx(
        self,
        _: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ) -> Result<ExecutionIntent> {
        Ok(self.0)
    }
}

pub(crate) use crate::harness::run_loop::issuance::TrackAdmissionFence;

/// A take-once transfer from the run loop to the occupancy writer.
pub(crate) struct TrackFenceHandoff(std::sync::Mutex<Option<TrackAdmissionFence>>);
impl TrackFenceHandoff {
    pub(crate) fn new(fence: TrackAdmissionFence) -> Self {
        Self(std::sync::Mutex::new(Some(fence)))
    }
    pub(crate) fn release(&self) {
        self.0.lock().expect("Track fence handoff").take();
    }
}

pub(crate) enum OccupancyFence<'a> {
    ReleaseOnCommit(&'a TrackFenceHandoff),
    Held(&'a TrackAdmissionFence),
}
/// Admission consumes a handoff once inside the writer transaction. Keeping the
/// guard local also covers cancellation and every error after it is consumed.
pub(super) enum AdmittedFence<'a> {
    Owned(TrackAdmissionFence),
    Borrowed(&'a TrackAdmissionFence),
}
impl<'a> OccupancyFence<'a> {
    pub(super) fn claim(&self) -> Result<AdmittedFence<'a>> {
        match self {
            Self::Held(fence) => Ok(AdmittedFence::Borrowed(fence)),
            Self::ReleaseOnCommit(handoff) => handoff
                .0
                .lock()
                .expect("Track fence handoff")
                .take()
                .map(AdmittedFence::Owned)
                .ok_or_else(|| conflict("Planner admission Track fence is empty")),
        }
    }
}
impl AdmittedFence<'_> {
    pub(super) fn check(&self, track: &workspace_runtime::execution::TrackId) -> Result<()> {
        let fence = match self {
            Self::Owned(fence) => fence,
            Self::Borrowed(fence) => fence,
        };
        if fence.track().as_str() == track.0 {
            Ok(())
        } else {
            Err(conflict("Planner admission Track fence changed"))
        }
    }
}
impl Drop for OccupancyFence<'_> {
    fn drop(&mut self) {
        if let Self::ReleaseOnCommit(handoff) = self {
            handoff.release();
        }
    }
}

#[cfg(test)]
mod tests;
