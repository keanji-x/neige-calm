//! `calm.task.replace` (#1785 S2): the Planner ends one attached codex/claude attempt (stopping it
//! when it still runs) and the kernel appends the successor declaration `<root>.<n>` in the same
//! report transaction, with an immutable receipt. The receipt is the replay answer and the carry
//! plan: when the successor's lease is prepared, the predecessor's settled candidate is merged
//! onto the upstream of that moment ([`crate::operation::workspace_lease::carry`]).

pub(crate) mod admission;
pub(crate) mod receipt;
mod refusal;
pub(crate) mod route;
pub(crate) mod view;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::error::{CalmError, Result};

/// What the successor starts from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CarryMode {
    /// The predecessor's settled candidate, else the carry the predecessor itself received.
    #[default]
    Candidate,
    /// The upstream alone.
    None,
}

/// The tool's arguments. `goal` and `acceptance` are the complete next-round contract; `context`
/// replaces the predecessor's wholesale and is `{}` when absent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplaceArgs {
    pub key: String,
    pub expected_attempt_id: String,
    pub idempotency_key: String,
    pub reason: String,
    pub goal: String,
    pub acceptance: String,
    #[serde(default)]
    pub context: Option<Value>,
    #[serde(default)]
    pub carry: CarryMode,
}

impl ReplaceArgs {
    pub(crate) fn validate(&self) -> Result<()> {
        let blank = |value: &str| value.trim().is_empty();
        if !calm_types::report_blocks::tasks::key_is_valid(&self.key)
            || blank(&self.expected_attempt_id)
            || blank(&self.idempotency_key)
            || self.idempotency_key.len() > 200
            || blank(&self.reason)
            || self.reason.len() > 4096
            || blank(&self.goal)
            || blank(&self.acceptance)
            || self
                .context
                .as_ref()
                .is_some_and(|context| !context.is_object())
        {
            return Err(CalmError::BadRequest(
                "task_replace: requires a valid key, expected_attempt_id, idempotency_key (at most \
                 200 bytes), reason (at most 4096 bytes), nonblank goal and acceptance, and an \
                 object context when given"
                    .into(),
            ));
        }
        Ok(())
    }

    /// The successor's `context`.
    pub(crate) fn successor_context(&self) -> Value {
        self.context.clone().unwrap_or_else(|| json!({}))
    }

    /// The request identity a replay must match exactly (everything but the request key).
    pub(crate) fn fingerprint(&self) -> String {
        let canonical = json!({
            "key": self.key,
            "expected_attempt_id": self.expected_attempt_id,
            "reason": self.reason,
            "goal": self.goal,
            "acceptance": self.acceptance,
            "context": self.successor_context(),
            "carry": self.carry,
        });
        hex::encode(Sha256::digest(canonical.to_string().as_bytes()))
    }
}

/// A carry that failed while the successor's lease was prepared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CarryFailure {
    /// The candidate conflicts with the upstream (`refused: carry-conflict: <paths>`).
    Conflict,
    /// The carry commit could not be computed (`carry-infra: <why>`).
    Infra,
}

/// `<key>: <facts>. <sentence>` — the spawn-failure reason `spawn-failed: …` carries.
pub(crate) fn carry_failure(failure: CarryFailure, facts: &str) -> String {
    let refusal = match failure {
        CarryFailure::Conflict => refusal::Refusal::CarryConflict,
        CarryFailure::Infra => refusal::Refusal::CarryInfra,
    };
    format!("{}: {facts}. {}", refusal.key(), refusal.sentence())
}

/// A request key reused for a different request.
pub(crate) fn idempotency_conflict() -> CalmError {
    refusal::Refusal::IdempotencyConflict.refuse("")
}

/// The appended successor would not be projected into a runnable attempt.
pub(crate) fn successor_unschedulable(facts: &str) -> CalmError {
    refusal::Refusal::SuccessorUnschedulable.refuse(facts)
}
