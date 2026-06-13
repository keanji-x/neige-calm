//! The observation sink — kernel→agent delivery (issue #679 §"Acyclicity
//! hinges").
//!
//! Issue context: "Both push directions are exec traits: the reaction seam
//! covers agent→truth decision writes; the **observation sink** covers
//! kernel→agent observation delivery (today dispatcher holds
//! `HarnessRegistry` + concrete adapters — that edge becomes a trait or the
//! kernel→provider firewall is fiction)."
//!
//! PR1 defines the trait; the kernel-side dispatcher/reaper rewrite adopts
//! it in PR7/PR8, with the harness registry (real) and FakeProvider's
//! in-memory queue (PR5) as implementations.

use async_trait::async_trait;
use calm_types::error::CoreError;
use calm_types::observation::Observation;
use calm_types::worker::WorkerSessionId;

/// Kernel→agent push: deliver one observation to a session.
///
/// Semantics the implementations must honor (lifted from today's
/// dispatcher→harness contract):
///
/// * **At-least-once, idempotent on `envelope_id`** — the dispatcher dedups
///   its spec push on the envelope id; redelivery of the same id must not
///   double-enqueue.
/// * **`envelope_id` is the cursor** — `Some(events.id)` for observations
///   born from persisted events (the watermark the harness snapshot
///   persists), `None` for synthetic injections (user input, boot replay
///   of a snapshot's pending queue).
/// * **Backpressure is an error, not a drop** — a saturated queue surfaces
///   as `CoreError::ServiceUnavailable` so the caller can retry or park,
///   matching the `/spec/input` 503 contract.
#[async_trait]
pub trait ObservationSink: Send + Sync {
    /// Deliver `observation` to `session`.
    async fn deliver(
        &self,
        session: &WorkerSessionId,
        observation: Observation,
        envelope_id: Option<i64>,
    ) -> Result<(), CoreError>;
}
