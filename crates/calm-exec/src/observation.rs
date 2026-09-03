//! Kernel→agent observation delivery.

use async_trait::async_trait;
use calm_types::error::CoreError;
use calm_types::observation::Observation;
use calm_types::worker::WorkerSessionId;

/// Kernel→agent push: deliver one observation to a session.
///
/// * **At-least-once, idempotent on `envelope_id`** — the dispatcher dedups
///   its planner push on the envelope id; redelivery of the same id must not
///   double-enqueue.
/// * **`envelope_id` is the cursor** — `Some(events.id)` for observations
///   born from persisted events (the watermark the harness snapshot
///   persists), `None` for synthetic injections (user input, boot replay
///   of a snapshot's pending queue).
/// * **Backpressure is an error, not a drop** — a saturated queue surfaces
///   as `CoreError::ServiceUnavailable` so the caller can retry or park,
///   matching the `/planner/input` 503 contract.
#[async_trait]
pub trait ObservationSink: Send + Sync {
    async fn deliver(
        &self,
        session: &WorkerSessionId,
        observation: Observation,
        envelope_id: Option<i64>,
    ) -> Result<(), CoreError>;
}
