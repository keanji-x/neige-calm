//! What a turn's issuance asks of the conversation, and how it says no: shared by the run loop,
//! which reads the card and applies a refusal, and the backend, which resolves the selection.

use serde_json::Value;

use crate::planner_model::FailureKind;

/// A refusal to issue, and what waiting will do about it.
#[derive(Debug, Clone)]
pub(crate) struct IssuanceRefusal {
    pub(crate) kind: FailureKind,
    /// For the log. No advice, no audience.
    pub(crate) log: String,
    /// For the reader. Used by every kind except [`FailureKind::Retryable`], which supplies its
    /// own text via `transient_notice`.
    pub(crate) reader: String,
}

impl IssuanceRefusal {
    pub(crate) fn retryable(log: String) -> Self {
        Self {
            kind: FailureKind::Retryable,
            log,
            reader: String::new(),
        }
    }

    /// The provider refused; `reader` is its own account of why.
    pub(crate) fn rejected(log: String, reader: String) -> Self {
        Self {
            kind: FailureKind::Rejected,
            log,
            reader,
        }
    }

    pub(crate) fn needs_a_choice(log: String, reader: String) -> Self {
        Self {
            kind: FailureKind::NeedsAChoice,
            log,
            reader,
        }
    }
}

/// The provider-neutral reads a model resolution may need. The backend decides which it needs
/// and when; the run loop answers them.
pub(crate) trait SelectionSource: Sync {
    fn check_current(&self) -> crate::error::Result<()>;
    /// Current admission after the native connection gate, before each new RPC.
    /// Required for every source; canonical callers have no default approval.
    fn admit_native_read(&self) -> futures_util::future::BoxFuture<'_, crate::error::Result<()>>;
    /// The card's payload as it stands NOW. A vanished card is an `Err`.
    async fn card_payload(&self) -> std::result::Result<Value, IssuanceRefusal>;

    /// The workspace whose config layers apply to this thread.
    async fn installation_cwd(&self) -> std::result::Result<String, IssuanceRefusal>;
}

impl SelectionSource for crate::conversation_inputs::SelectionReadAdmission {
    fn check_current(&self) -> crate::error::Result<()> {
        self.check_current()
    }

    fn admit_native_read(&self) -> futures_util::future::BoxFuture<'_, crate::error::Result<()>> {
        Box::pin(self.revalidate())
    }

    async fn card_payload(&self) -> std::result::Result<Value, IssuanceRefusal> {
        self.current_payload()
            .await
            .map_err(|error| IssuanceRefusal::retryable(error.to_string()))
    }

    async fn installation_cwd(&self) -> std::result::Result<String, IssuanceRefusal> {
        // The admission owner captured this actual target and rechecks it at
        // each native effect; callers cannot supply a separately-read cwd.
        Ok(self.cwd().to_owned())
    }
}
