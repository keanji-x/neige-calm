//! Kernel→agent observation delivery.

use async_trait::async_trait;
use calm_types::error::CoreError;
use calm_types::observation::Observation;
use calm_types::worker::WorkerSessionId;

/// Kernel→agent push: deliver one observation to a session.
///
/// At-least-once, idempotent on `envelope_id`; a saturated queue is `CoreError::ServiceUnavailable`, never a drop.
#[async_trait]
pub trait ObservationSink: Send + Sync {
    async fn deliver(
        &self,
        session: &WorkerSessionId,
        observation: Observation,
        envelope_id: Option<i64>,
    ) -> Result<(), CoreError>;
}
