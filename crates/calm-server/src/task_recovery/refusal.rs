//! Typed recovery refusals. Every admission refusal names its code at the
//! site that refuses; the read side never derives a code from message text.

use crate::error::CalmError;

/// Refusal codes on the wire as `TaskRecoveryCapability.code`. The string
/// spellings are a published vocabulary; add a variant rather than reusing
/// one whose meaning does not fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryRefusalCode {
    /// The caller may not request recovery at all (service boundary).
    NotAuthorized,
    /// A Planner asked; only an explicit User recovery is admissible.
    UserAuthorizationRequired,
    /// The bounded Planner retry for this key was consumed.
    RecoveryLimitReached,
    /// The Track lifecycle does not schedule work.
    TrackNotReady,
    /// Child-task routes are not recoverable.
    UnsupportedSpawn,
    /// The failed predecessor has no accepted proof that its writes stopped.
    PredecessorNotQuiescent,
    /// The declaration was withdrawn, is not ready, or is invalid.
    DeclarationWithdrawn,
    /// The failed execution carries no complete, well-formed frozen contract.
    MissingFrozenContract,
    /// The frozen contract no longer matches the current report.
    ContractChanged,
    /// An accepted recovery lost its allocation or predecessor row.
    RecoveryLineageMissing,
}

impl RecoveryRefusalCode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NotAuthorized => "not_authorized",
            Self::UserAuthorizationRequired => "user_authorization_required",
            Self::RecoveryLimitReached => "recovery_limit_reached",
            Self::TrackNotReady => "track_not_ready",
            Self::UnsupportedSpawn => "unsupported_spawn",
            Self::PredecessorNotQuiescent => "predecessor_not_quiescent",
            Self::DeclarationWithdrawn => "declaration_withdrawn",
            Self::MissingFrozenContract => "missing_frozen_contract",
            Self::ContractChanged => "contract_changed",
            Self::RecoveryLineageMissing => "recovery_lineage_missing",
        }
    }
}

/// Which `CalmError` (and therefore HTTP/RPC status) a refusal maps to on
/// the write path. The mapping is fixed per site, never derived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefusalKind {
    Conflict,
    Forbidden,
}

#[derive(Clone, Debug)]
pub(crate) struct RecoveryRefusal {
    pub code: RecoveryRefusalCode,
    pub kind: RefusalKind,
    pub reason: String,
}

impl RecoveryRefusal {
    pub(crate) fn conflict(code: RecoveryRefusalCode, reason: impl Into<String>) -> Self {
        Self {
            code,
            kind: RefusalKind::Conflict,
            reason: reason.into(),
        }
    }

    pub(crate) fn forbidden(code: RecoveryRefusalCode, reason: impl Into<String>) -> Self {
        Self {
            code,
            kind: RefusalKind::Forbidden,
            reason: reason.into(),
        }
    }
}

impl From<RecoveryRefusal> for CalmError {
    fn from(refusal: RecoveryRefusal) -> Self {
        match refusal.kind {
            RefusalKind::Conflict => CalmError::Conflict(refusal.reason),
            RefusalKind::Forbidden => CalmError::Forbidden(refusal.reason),
        }
    }
}

/// Admission outcome: a typed refusal the read side can name, or any other
/// failure (storage, serialization, missing rows) that is not a refusal.
#[derive(Debug)]
pub(crate) enum AdmissionError {
    Refused(RecoveryRefusal),
    Other(CalmError),
}

impl From<RecoveryRefusal> for AdmissionError {
    fn from(refusal: RecoveryRefusal) -> Self {
        Self::Refused(refusal)
    }
}

impl From<CalmError> for AdmissionError {
    fn from(error: CalmError) -> Self {
        Self::Other(error)
    }
}

impl From<sqlx::Error> for AdmissionError {
    fn from(error: sqlx::Error) -> Self {
        Self::Other(error.into())
    }
}

impl From<serde_json::Error> for AdmissionError {
    fn from(error: serde_json::Error) -> Self {
        Self::Other(error.into())
    }
}

impl From<calm_truth::TruthError> for AdmissionError {
    fn from(error: calm_truth::TruthError) -> Self {
        Self::Other(error.into())
    }
}

/// Write-path callers keep their exact `CalmError` (403/409) behaviour.
impl From<AdmissionError> for CalmError {
    fn from(error: AdmissionError) -> Self {
        match error {
            AdmissionError::Refused(refusal) => refusal.into(),
            AdmissionError::Other(error) => error,
        }
    }
}

impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refusal) => f.write_str(&refusal.reason),
            Self::Other(error) => error.fmt(f),
        }
    }
}

pub(crate) type Admission<T> = std::result::Result<T, AdmissionError>;
