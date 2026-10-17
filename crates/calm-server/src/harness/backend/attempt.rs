//! Provider observations for one immutable input attempt, independent of retry policy.
use calm_types::conversation::InputAttempt;
use provider::codex::{RequestCrossing, RpcRemoteError};
use serde::Serialize;

use super::{BackendRewind, TurnStartFailure};
use crate::session_projection_repo::AgentProvider;
use crate::shared_codex_appserver::TurnId;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AttemptCorrelation {
    attempt: InputAttempt,
    provider: AgentProvider,
    thread_id: String,
    client_id: String,
    native_turn_id: Option<String>,
    native_session_id: Option<String>,
    local_turn_id: Option<String>,
    rpc_request_id: Option<u64>,
}

impl AttemptCorrelation {
    pub(super) fn new(attempt: &InputAttempt, provider: AgentProvider, thread: &str) -> Self {
        Self {
            attempt: attempt.clone(),
            provider,
            thread_id: thread.into(),
            client_id: attempt.submission_id.as_str().into(),
            native_turn_id: None,
            native_session_id: None,
            local_turn_id: None,
            rpc_request_id: None,
        }
    }

    pub(super) fn with_turn(mut self, turn: String) -> Self {
        self.native_turn_id = Some(turn);
        self
    }

    pub(super) fn with_local_turn(mut self, turn: String) -> Self {
        self.local_turn_id = Some(turn);
        self
    }
    pub(super) fn with_session(mut self, session: Option<String>) -> Self {
        self.native_session_id = session;
        self
    }

    pub(super) fn with_request(mut self, request: Option<u64>) -> Self {
        self.rpc_request_id = request;
        self
    }

    pub(crate) fn attempt(&self) -> &InputAttempt {
        &self.attempt
    }
    pub(crate) fn provider(&self) -> &AgentProvider {
        &self.provider
    }
    pub(crate) fn thread_id(&self) -> &str {
        &self.thread_id
    }
    pub(crate) fn client_id(&self) -> &str {
        &self.client_id
    }
    pub(crate) fn native_turn_id(&self) -> Option<&str> {
        self.native_turn_id.as_deref()
    }
    pub(crate) fn local_turn_id(&self) -> Option<&str> {
        self.local_turn_id.as_deref()
    }
    pub(crate) fn native_session_id(&self) -> Option<&str> {
        self.native_session_id.as_deref()
    }
    pub(crate) fn rpc_request_id(&self) -> Option<u64> {
        self.rpc_request_id
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NonDispatchBasis {
    LocalPreflight,
    TransportBeforeSend,
    Unsupported,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct NonDispatchEvidence {
    correlation: AttemptCorrelation,
    basis: NonDispatchBasis,
}

impl NonDispatchEvidence {
    pub(super) fn new(correlation: AttemptCorrelation, basis: NonDispatchBasis) -> Self {
        Self { correlation, basis }
    }
    pub(crate) fn correlation(&self) -> &AttemptCorrelation {
        &self.correlation
    }
    pub(crate) fn basis(&self) -> NonDispatchBasis {
        self.basis
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativeAcceptanceBasis {
    CodexTurnStart,
    CodexTurnSteer,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct NativeAcceptanceReceipt {
    correlation: AttemptCorrelation,
    basis: NativeAcceptanceBasis,
}

impl NativeAcceptanceReceipt {
    /// Only a validated native response reaches this constructor.
    pub(super) fn codex(
        correlation: AttemptCorrelation,
        turn: String,
        basis: NativeAcceptanceBasis,
    ) -> Self {
        assert!(!turn.is_empty());
        Self {
            correlation: correlation.with_turn(turn),
            basis,
        }
    }
    pub(crate) fn correlation(&self) -> &AttemptCorrelation {
        &self.correlation
    }
    pub(crate) fn basis(&self) -> NativeAcceptanceBasis {
        self.basis
    }
    pub(crate) fn native_turn_id(&self) -> &str {
        self.correlation
            .native_turn_id
            .as_deref()
            .expect("native receipt carries a turn")
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "certainty", content = "evidence", rename_all = "snake_case")]
pub(crate) enum InputDelivery {
    NotDispatched(NonDispatchEvidence),
    Accepted(NativeAcceptanceReceipt),
    Unknown(AttemptCorrelation),
}

impl InputDelivery {
    pub(crate) fn correlation(&self) -> &AttemptCorrelation {
        match self {
            Self::NotDispatched(e) => e.correlation(),
            Self::Accepted(e) => e.correlation(),
            Self::Unknown(c) => c,
        }
    }
}

/// This observation never follows from the new prompt's delivery status.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "certainty", content = "plan", rename_all = "snake_case")]
pub(crate) enum RewindObservation {
    NotRequested,
    Unknown(BackendRewind),
    Applied(BackendRewind),
}

/// In-memory diagnostics only: remote error data may include prompt text.
#[derive(Debug)]
pub(crate) struct NativeRpcFailure {
    pub(super) method: String,
    pub(super) request_id: Option<u64>,
    pub(super) crossing: RequestCrossing,
    pub(super) remote_error: Option<RpcRemoteError>,
}

impl NativeRpcFailure {
    pub(super) fn from_rpc(error: &provider::codex::RpcAttemptFailure) -> Self {
        Self {
            method: error.method().into(),
            request_id: error.request_id(),
            crossing: error.crossing(),
            remote_error: error.remote_error().cloned(),
        }
    }
    pub(crate) fn method(&self) -> &str {
        &self.method
    }
    pub(crate) fn request_id(&self) -> Option<u64> {
        self.request_id
    }
    pub(crate) fn crossing(&self) -> RequestCrossing {
        self.crossing
    }
    pub(crate) fn remote_error(&self) -> Option<&RpcRemoteError> {
        self.remote_error.as_ref()
    }
}

pub(crate) struct InputAttemptReport {
    pub(super) delivery: InputDelivery,
    pub(super) rewind: RewindObservation,
    pub(super) outcome: Result<TurnId, TurnStartFailure>,
    pub(super) rpc_failure: Option<NativeRpcFailure>,
}

impl InputAttemptReport {
    pub(crate) fn attempt(&self) -> &InputAttempt {
        self.delivery.correlation().attempt()
    }
    pub(crate) fn delivery(&self) -> &InputDelivery {
        &self.delivery
    }
    pub(crate) fn rewind(&self) -> &RewindObservation {
        &self.rewind
    }
    pub(crate) fn outcome(&self) -> &Result<TurnId, TurnStartFailure> {
        &self.outcome
    }
    pub(crate) fn rpc_failure(&self) -> Option<&NativeRpcFailure> {
        self.rpc_failure.as_ref()
    }
    pub(crate) fn into_outcome(self) -> Result<TurnId, TurnStartFailure> {
        self.outcome
    }
}

pub(crate) struct RewindReport {
    pub(super) thread_id: String,
    pub(super) observation: RewindObservation,
    pub(super) outcome: Result<(), TurnStartFailure>,
}

impl RewindReport {
    pub(crate) fn thread_id(&self) -> &str {
        &self.thread_id
    }
    pub(crate) fn observation(&self) -> &RewindObservation {
        &self.observation
    }
    pub(crate) fn into_outcome(self) -> Result<(), TurnStartFailure> {
        self.outcome
    }
}
