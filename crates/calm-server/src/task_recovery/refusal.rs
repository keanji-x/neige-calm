//! Typed recovery refusals. The site that refuses decides everything the
//! read side may say about it: its [`RefusalSite`], its wire code, its
//! write-path kind, the one continuation the kernel supports and a complete
//! reason sentence. Nothing downstream re-derives a continuation or a
//! sentence from the code, the task shape or the message text.

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

/// Every production site that constructs a refusal, one variant each, in
/// source order. [`RefusalSite::ALL`] lists them all;
/// `task_recovery/tests.rs` drives each through the real function and
/// asserts the `(site, code, kind, continuation)` it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RefusalSite {
    // ---- admission::authorize_tx ----
    /// The actor is neither a User nor a Planner.
    ActorNotUserOrPlanner,
    /// The Planner session cannot be resolved to a role.
    PlannerSessionUnresolved,
    // ---- admission::recovery_policy ----
    /// The Track lifecycle does not schedule work.
    TrackNotReady,
    /// Child-task routes are not recoverable.
    ChildTaskRoute,
    /// A Planner asked for a task outside auto-declare (user-owned or
    /// declare-and-wait).
    PlannerOutsideAutoDeclare,
    /// The bounded Planner retry for this key was consumed.
    PlannerRetryLimit,
    // ---- admission::admit_contract_and_predecessor_tx ----
    /// The frozen constraint fails `validate` for this Track.
    ConstraintShapeInvalid,
    /// The frozen file-delivery input contract cannot be honoured.
    FileDeliveryInputUnhonoured,
    // ---- admission::claim_constraint_tx ----
    /// The frozen context closure was truncated at claim.
    FrozenContextTruncated,
    /// No frozen context was recorded for the failed execution.
    FrozenContextMissing,
    /// The frozen context is valid JSON but not a reference list.
    FrozenContextMalformed,
    // ---- admission::declaration_tx ----
    /// No declaration in the report carries the key.
    DeclarationMissing,
    /// The declaration is duplicated, tombstoned or not ready.
    DeclarationNotCurrent,
    /// The declaration block carries validation diagnostics.
    DeclarationInvalid,
    /// The declare-and-wait release was withdrawn.
    ReleaseWithdrawn,
    // ---- admission::check_constraint_tx ----
    /// The constraint fails `validate` at the inner check.
    InnerConstraintShapeInvalid,
    /// The declaration's route or author differs from the frozen ones.
    RouteOrAuthorChanged,
    /// A frozen context reference names a missing Track.
    FrozenTrackMissing,
    /// A frozen context reference left the authorized area.
    ContextMovedOutsideArea,
    /// A frozen context reference names a Track without a report.
    FrozenReportMissing,
    /// A frozen context reference names a missing block.
    FrozenBlockMissing,
    /// The root reference no longer names the declaration block.
    RootIdentityChanged,
    /// A frozen reference's content hash changed.
    RootHashChanged,
    // ---- admission::require_recoverable_predecessor_tx ----
    /// A prepared isolated execution shares its key with other operations or
    /// verification effects.
    IsolatedAmbiguousOperations,
    /// An ordinary worker card was prepared for the key.
    OrdinaryWorkerPrepared,
    /// Verification effects exist for the key without a worker card.
    VerificationEffectsWithoutWorker,
    /// The failure is not a spawn failure and nothing proves a stop.
    NotSpawnFailedWithoutStopProof,
    /// A keyed operation has uncertain external effects.
    OperationUncertainExternalEffects,
    // ---- isolated_codex::recovery::require_stopped_tx ----
    /// The execution is not a failed isolated-route execution.
    IsolatedRouteMismatch,
    /// The named operation is not this execution's isolated operation.
    IsolatedOperationNotThisExecution,
    /// Compensation state or spawn artifacts were recorded.
    IsolatedCompensationRecorded,
    /// The isolated operation has not reached a terminal phase.
    IsolatedStopPending,
    /// The isolated operation ended without a failed outcome.
    IsolatedOperationTerminalWithoutFailure,
    // ---- isolated_codex::recovery::confirmed_record_tx ----
    /// The operation's journal record cannot be read as a prepared run.
    IsolatedRecordUnreadable,
    /// The journal holds no quiescence proof for the terminal operation.
    IsolatedStopUnconfirmed,
    /// The recorded run's admission is not closed, or its identity chain
    /// does not match this execution.
    IsolatedStopIdentityMismatch,
    // ---- admission::check_recovery_attempt_tx ----
    /// The attempt's allocation row is missing.
    AllocationMissing,
    /// The accepted recovery's predecessor row is missing.
    PredecessorRowMissing,
    /// The accepted recovery's admitting actor is unsupported.
    ProvenanceUnsupported,
    /// The Track no longer schedules the accepted recovery.
    TrackNoLongerSchedules,
}

impl RefusalSite {
    /// Every variant, exactly once, for the test inventory:
    /// `task_recovery/tests.rs` proves it exhaustive by a wildcard-free
    /// match (a new variant does not compile until it is listed here) and
    /// drives every listed site through the real function.
    #[cfg(test)]
    pub(crate) const ALL: &[RefusalSite] = &[
        Self::ActorNotUserOrPlanner,
        Self::PlannerSessionUnresolved,
        Self::TrackNotReady,
        Self::ChildTaskRoute,
        Self::PlannerOutsideAutoDeclare,
        Self::PlannerRetryLimit,
        Self::ConstraintShapeInvalid,
        Self::FileDeliveryInputUnhonoured,
        Self::FrozenContextTruncated,
        Self::FrozenContextMissing,
        Self::FrozenContextMalformed,
        Self::DeclarationMissing,
        Self::DeclarationNotCurrent,
        Self::DeclarationInvalid,
        Self::ReleaseWithdrawn,
        Self::InnerConstraintShapeInvalid,
        Self::RouteOrAuthorChanged,
        Self::FrozenTrackMissing,
        Self::ContextMovedOutsideArea,
        Self::FrozenReportMissing,
        Self::FrozenBlockMissing,
        Self::RootIdentityChanged,
        Self::RootHashChanged,
        Self::IsolatedAmbiguousOperations,
        Self::OrdinaryWorkerPrepared,
        Self::VerificationEffectsWithoutWorker,
        Self::NotSpawnFailedWithoutStopProof,
        Self::OperationUncertainExternalEffects,
        Self::IsolatedRouteMismatch,
        Self::IsolatedOperationNotThisExecution,
        Self::IsolatedCompensationRecorded,
        Self::IsolatedStopPending,
        Self::IsolatedOperationTerminalWithoutFailure,
        Self::IsolatedRecordUnreadable,
        Self::IsolatedStopUnconfirmed,
        Self::IsolatedStopIdentityMismatch,
        Self::AllocationMissing,
        Self::PredecessorRowMissing,
        Self::ProvenanceUnsupported,
        Self::TrackNoLongerSchedules,
    ];
}

/// The single continuation the kernel supports for a refused recovery,
/// decided by the refusing site. Surfaced by `calm.plan.list` as
/// `recovery.guidance.supported_continuation`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SupportedContinuation {
    /// Declare a new task (new key); never retry the same key.
    NewTask,
    /// Only an explicit User recovery can allocate another execution.
    UserRecovery,
    /// The isolated predecessor is still stopping; its settlement briefing
    /// re-opens the decision.
    WaitForSettlement,
    /// No kernel-supported continuation for this refusal.
    None,
}

impl SupportedContinuation {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NewTask => "new_task",
            Self::UserRecovery => "user_recovery",
            Self::WaitForSettlement => "wait_for_settlement",
            Self::None => "none",
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

/// A refusal as the site decided it. `reason` is a complete, true
/// sentence: it is the wire `reason` and, unchanged, the
/// `guidance.blocking_condition`.
#[derive(Clone, Debug)]
pub(crate) struct RecoveryRefusal {
    pub site: RefusalSite,
    pub code: RecoveryRefusalCode,
    pub kind: RefusalKind,
    pub continuation: SupportedContinuation,
    pub reason: String,
}

impl RecoveryRefusal {
    pub(crate) fn conflict(
        site: RefusalSite,
        code: RecoveryRefusalCode,
        continuation: SupportedContinuation,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            site,
            code,
            kind: RefusalKind::Conflict,
            continuation,
            reason: reason.into(),
        }
    }

    pub(crate) fn forbidden(
        site: RefusalSite,
        code: RecoveryRefusalCode,
        continuation: SupportedContinuation,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            site,
            code,
            kind: RefusalKind::Forbidden,
            continuation,
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
