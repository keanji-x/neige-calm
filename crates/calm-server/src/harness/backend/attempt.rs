//! Provider observations for one immutable input attempt, independent of retry policy.
use calm_types::conversation::InputAttempt;
use serde::Serialize;

use super::TurnStartFailure;
use crate::session_projection_repo::AgentProvider;
use crate::shared_codex_appserver::TurnId;

/// Transport observations keep native receipts distinct from a Neige process invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExecutionIdentity {
    Native(TurnId),
    Local(uuid::Uuid),
}
impl ExecutionIdentity {
    pub(crate) fn turn_id(&self) -> String {
        match self {
            Self::Native(turn) => turn.clone(),
            Self::Local(turn) => turn.to_string(),
        }
    }
}

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

    pub(super) fn with_local_turn(mut self, turn: uuid::Uuid) -> Self {
        self.local_turn_id = Some(turn.to_string());
        self
    }

    pub(super) fn with_native_session(mut self, native: String) -> Self {
        self.native_session_id = Some(native);
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

pub(crate) struct InputAttemptReport {
    pub(super) delivery: InputDelivery,
    pub(super) outcome: Result<ExecutionIdentity, TurnStartFailure>,
}

impl InputAttemptReport {
    /// Local evidence can identify execution without proving native acceptance.
    pub(super) fn unknown(
        correlation: AttemptCorrelation,
        outcome: Result<ExecutionIdentity, TurnStartFailure>,
    ) -> Self {
        Self {
            delivery: InputDelivery::Unknown(correlation),
            outcome,
        }
    }

    pub(crate) fn attempt(&self) -> &InputAttempt {
        self.delivery.correlation().attempt()
    }
    pub(crate) fn delivery(&self) -> &InputDelivery {
        &self.delivery
    }
    pub(crate) fn outcome(&self) -> &Result<ExecutionIdentity, TurnStartFailure> {
        &self.outcome
    }
}
