use thiserror::Error;

#[derive(Debug, Error)]
pub enum TruthError {
    #[error(transparent)]
    Core(calm_types::error::CoreError),

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("internal: {0}")]
    Internal(String),

    /// A keyed write's `Idempotency-Key` is already bound to a different request; the same key can
    /// never be answered for this one. Final.
    #[error("idempotency key reused: {0}")]
    IdempotencyKeyReused(String),

    /// A keyed binding's UNIQUE wall refused this insert: another request under the same key
    /// committed first, and this transaction rolled back. Retrying the same request under the same
    /// key is answered by that one.
    #[error("idempotency key concurrent: {0}")]
    IdempotencyKeyConcurrent(String),

    /// A keyed write's `Idempotency-Key` names something that can no longer be produced under it
    /// (its result was deleted); only a new key goes anywhere. Final for the key.
    #[error("idempotency key exhausted: {0}")]
    IdempotencyKeyExhausted(String),

    /// A send that replaces a turn was refused inside the transaction that would have removed it,
    /// which rolled back; nothing changed. Carried here so the refusal keeps its kind through the
    /// storage write path.
    #[error("{0}")]
    PlannerTurnNotReplaceable(String),
}

/// What a keyed binding insert raises when its UNIQUE wall refuses it: the typed retryable
/// [`TruthError::IdempotencyKeyConcurrent`], never the raw SQL text. Every other error passes through.
pub(crate) fn idempotency_binding_insert_error(error: sqlx::Error) -> TruthError {
    match error {
        // Only a key collision reaches this arm. A binding's other UNIQUE column (area's
        // `area_id`) holds an id minted in the same transaction, so it cannot collide.
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            TruthError::IdempotencyKeyConcurrent(
                "another request under this Idempotency-Key was accepted at the same time; send \
                 this request again under the same key to receive that request's answer"
                    .into(),
            )
        }
        other => TruthError::Db(other),
    }
}

impl TruthError {
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            TruthError::Core(calm_types::error::CoreError::NotFound(_))
        )
    }
}

#[allow(non_snake_case)]
impl TruthError {
    pub fn NotFound(message: impl Into<String>) -> Self {
        calm_types::error::CoreError::NotFound(message.into()).into()
    }

    pub fn Conflict(message: impl Into<String>) -> Self {
        calm_types::error::CoreError::Conflict(message.into()).into()
    }

    pub fn IdempotencyCollision(message: impl Into<String>) -> Self {
        calm_types::error::CoreError::IdempotencyCollision(message.into()).into()
    }

    #[allow(non_upper_case_globals)]
    pub const Unauthorized: Self = Self::Core(calm_types::error::CoreError::Unauthorized);

    pub fn BadRequest(message: impl Into<String>) -> Self {
        calm_types::error::CoreError::BadRequest(message.into()).into()
    }

    pub fn ServiceUnavailable(message: impl Into<String>) -> Self {
        calm_types::error::CoreError::ServiceUnavailable(message.into()).into()
    }
}

impl From<calm_types::error::CoreError> for TruthError {
    fn from(err: calm_types::error::CoreError) -> Self {
        use calm_types::error::CoreError as Core;
        match err {
            Core::Forbidden(m) => TruthError::Forbidden(m),
            Core::Io(e) => TruthError::Io(e),
            Core::Serde(e) => TruthError::Serde(e),
            Core::Internal(m) => TruthError::Internal(m),
            other => TruthError::Core(other),
        }
    }
}

/// Migration-readability alias; shadows calm-server's `CalmError` enum.
pub type CalmError = TruthError;
pub type Result<T, E = TruthError> = std::result::Result<T, E>;
