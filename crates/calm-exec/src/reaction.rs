//! Agent→truth decision contracts.

use async_trait::async_trait;
use calm_types::error::CoreError;
use calm_types::event::ArtifactRef;
use calm_types::ids::TrackId;
use calm_types::model::TrackLifecycle;
use calm_types::observation::Observation;
use calm_types::worker::Principal;
use serde_json::Value;

/// Typed decision command committed as a principal through the gated entrance.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionIntent {
    /// Edge legality remains with `track_lifecycle::validate_transition`.
    LifecycleTransition {
        track_id: TrackId,
        to: TrackLifecycle,
        agent_message: Option<String>,
    },
    /// `None` leaves that half unchanged.
    ReportWrite {
        track_id: TrackId,
        summary: Option<String>,
        body: Option<String>,
        agent_message: Option<String>,
    },
    DispatchCodexWorker {
        idempotency_key: String,
        goal: String,
        context: Value,
        acceptance_criteria: Option<String>,
        agent_message: Option<String>,
    },
    DispatchTerminalWorker {
        idempotency_key: String,
        cmd: String,
        cwd: Option<String>,
        agent_message: Option<String>,
    },
    CompleteTask {
        idempotency_key: String,
        result: Value,
        artifacts: Vec<ArtifactRef>,
        agent_message: Option<String>,
    },
    FailTask {
        idempotency_key: String,
        reason: String,
        agent_message: Option<String>,
    },
}

/// Agent-side decision logic.
#[async_trait]
pub trait AgentReactor: Send + Sync {
    fn principal(&self) -> Principal;

    async fn react(&self, observation: &Observation) -> Result<Vec<DecisionIntent>, CoreError>;
}

/// Truth-side commit point for decision intents.
///
/// State, event, and authorization must commit in one transaction.
#[async_trait]
pub trait DecisionSink: Send + Sync {
    async fn commit(&self, principal: &Principal, intent: DecisionIntent) -> Result<(), CoreError>;
}
