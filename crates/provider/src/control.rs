//! Neutral data for the existing managed control operations. Diagnostics are opaque to provider.
use crate::{InputItem, TurnModelSelection};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorrelationKey(pub String);
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnIdentity {
    Native(String),
    Local(CorrelationKey),
}
impl TurnIdentity {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Native(id) => id,
            Self::Local(key) => &key.0,
        }
    }
    pub fn into_string(self) -> String {
        match self {
            Self::Native(id) => id,
            Self::Local(key) => key.0,
        }
    }
}
#[derive(Clone, Debug)]
pub struct InputRequest {
    pub items: Vec<InputItem>,
    pub correlation: CorrelationKey,
}
/// Controls the session selected by the receiving facade, never a caller override.
#[derive(Clone, Debug)]
pub struct SteerRequest {
    pub turn: TurnIdentity,
    pub input: InputRequest,
}
/// Compact the receiving facade's session; selection is the existing host preflight result.
#[derive(Clone, Debug)]
pub struct CompactRequest {
    pub selection: TurnModelSelection,
}
/// Interrupt a turn in the receiving facade's session. Host logical-thread context stays separate.
#[derive(Clone, Debug)]
pub struct InterruptRequest {
    pub turn: TurnIdentity,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmissionEvidence {
    NotWritten,
    WriteMayHaveReachedPeer,
    Unobserved,
}
#[derive(Debug)]
pub enum SteerOutcome<E> {
    Taken {
        turn: TurnIdentity,
    },
    NotTaken {
        error: E,
    },
    Unknown {
        error: E,
        evidence: SubmissionEvidence,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmissionBasis {
    NativeAck,
    LocalWrite,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactNotSubmitted {
    NativeRefusal,
    PreparationFailed,
}
#[derive(Debug)]
pub enum CompactSubmission<E> {
    Submitted {
        basis: SubmissionBasis,
    },
    NotSubmitted {
        error: E,
        reason: CompactNotSubmitted,
    },
    Unconfirmed {
        error: E,
        evidence: SubmissionEvidence,
    },
}
/// Native interrupt evidence, which cannot represent delivery of a local signal.
#[derive(Debug)]
pub enum NativeInterruptOutcome<E> {
    NativeAck,
    NoTarget,
    Unconfirmed { error: E },
}

impl<E> From<NativeInterruptOutcome<E>> for InterruptOutcome<E> {
    fn from(outcome: NativeInterruptOutcome<E>) -> Self {
        match outcome {
            NativeInterruptOutcome::NativeAck => Self::NativeAck,
            NativeInterruptOutcome::NoTarget => Self::NoTarget,
            NativeInterruptOutcome::Unconfirmed { error } => Self::Unconfirmed {
                error,
                evidence: SubmissionEvidence::Unobserved,
            },
        }
    }
}

#[derive(Debug)]
pub enum InterruptOutcome<E> {
    NativeAck,
    LocalSignalDelivered,
    NoTarget,
    Unconfirmed {
        error: E,
        evidence: SubmissionEvidence,
    },
}
impl<E> InterruptOutcome<E> {
    /// Preserve the diagnostic while moving across an integration boundary.
    pub fn map_error<F>(self, map: impl FnOnce(E) -> F) -> InterruptOutcome<F> {
        match self {
            Self::NativeAck => InterruptOutcome::NativeAck,
            Self::LocalSignalDelivered => InterruptOutcome::LocalSignalDelivered,
            Self::NoTarget => InterruptOutcome::NoTarget,
            Self::Unconfirmed { error, evidence } => InterruptOutcome::Unconfirmed {
                error: map(error),
                evidence,
            },
        }
    }
}

/// Start input for the session owned by the receiving facade.
#[derive(Clone, Debug)]
pub struct TurnRequest {
    pub input: InputRequest,
    pub selection: TurnModelSelection,
    pub permission: calm_types::harness::PlannerPermissionMode,
    pub edit: TurnEdit,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnEdit {
    NoEdit,
    ApplyNativePlan(NativeEditPlan),
}
/// Native anchors resolved by the host, never product transcript rows or session overrides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeEditTarget {
    BeforeTurn {
        before_turn_id: String,
    },
    TranscriptCut {
        resume_at: uuid::Uuid,
        drops_turn: uuid::Uuid,
    },
}
/// A target whose preparation succeeded. This is ephemeral, not a storage codec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeEditPlan {
    pub target: NativeEditTarget,
}
/// Error payloads and refusal/recovery diagnostics can have distinct required fields.
#[derive(Debug)]
pub enum TurnStartOutcome<E, R = E> {
    Started {
        turn: TurnIdentity,
    },
    Failed {
        error: E,
    },
    Rejected {
        error: R,
    },
    /// The current producer is local authentication preflight, not a remote reply.
    RecoveryRequired {
        error: R,
    },
}
#[derive(Debug)]
pub enum EditPreparation<E> {
    Prepared { plan: NativeEditPlan },
    Failed { error: E },
}
