//! Neutral data for the existing managed control operations. Diagnostics are opaque to provider.
use crate::{InputItem, TurnModelSelection};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorrelationKey(pub String);
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeSessionRef(pub String);
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionTarget {
    CurrentBinding,
    Native(NativeSessionRef),
}
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnTarget {
    pub session: SessionTarget,
    pub turn: TurnIdentity,
}
#[derive(Clone, Debug)]
pub struct InputRequest {
    pub items: Vec<InputItem>,
    pub correlation: CorrelationKey,
}
#[derive(Clone, Debug)]
pub struct SteerRequest {
    pub target: TurnTarget,
    pub input: InputRequest,
}
#[derive(Clone, Debug)]
pub struct CompactRequest {
    pub session: SessionTarget,
    pub selection: TurnModelSelection,
}
#[derive(Clone, Debug)]
pub struct InterruptRequest {
    pub target: TurnTarget,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmissionEvidence {
    NotWritten,
    LocalWrite,
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
    NoRunningTarget {
        target: TurnTarget,
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
    InvalidRequest,
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
