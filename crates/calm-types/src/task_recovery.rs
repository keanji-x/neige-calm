//! Durable execution identity for a continuing, Track + key scoped task.
//!
//! These types grant no authority. The service admits recovery against the
//! report declaration and caller; storage fences allocation and replay.

use crate::event::TaskContextRef;
use crate::ids::ActorId;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskRecoveryRequest {
    pub expected_attempt_id: String,
    pub idempotency_key: String,
    pub reason: String,
}

/// Stable acknowledgement, including when the response to the first call was lost.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskRecoveryReceipt {
    pub key: String,
    pub previous_attempt_id: String,
    pub attempt_id: String,
    pub generation: i64,
}

/// Evidence carried into a recovery. Never synthesize this from today's report
/// when the failed execution has no claim freeze. Historical hash semantics are
/// unchanged; route and attribution are explicit because the root hash omits them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "version", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskRecoveryConstraint {
    V1 {
        refs: Vec<TaskContextRef>,
        spawn: String,
        declared_by: String,
    },
}

impl TaskRecoveryConstraint {
    pub fn refs(&self) -> &[TaskContextRef] {
        match self {
            Self::V1 { refs, .. } => refs,
        }
    }

    /// Validate evidence shape, not caller permission or current report equality.
    pub fn validate(&self, track_id: &str) -> Result<(), String> {
        let Self::V1 {
            refs,
            spawn,
            declared_by,
        } = self;
        if spawn != "in-wave" || !matches!(declared_by.as_str(), "spec" | "user") {
            return Err("recovery requires an in-wave route and known declaration author".into());
        }
        if refs.iter().filter(|reference| reference.is_root).count() != 1 {
            return Err("recovery requires exactly one frozen root reference".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for reference in refs {
            if reference.track_id.as_str().is_empty()
                || reference.block_id.is_empty()
                || reference.rev < 0
                || reference.hash.len() != 64
                || !reference.hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                || (reference.is_root && reference.track_id.as_str() != track_id)
                || !seen.insert((reference.track_id.as_str(), reference.block_id.as_str()))
            {
                return Err("recovery frozen reference is missing, malformed or duplicated".into());
            }
        }
        Ok(())
    }
}

/// Initial allocations have no recovery metadata. Every recovery field is
/// required for the recovery variant, including its frozen constraint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskAttemptOrigin {
    Initial,
    Recovery {
        previous_attempt_id: String,
        idempotency_key: String,
        request_fingerprint: String,
        reason: String,
        actor: ActorId,
        constraint: TaskRecoveryConstraint,
    },
}

/// Allocation remains present when the scheduler's pending projection is absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAttemptAllocation {
    pub attempt_id: String,
    pub track_id: String,
    pub key: String,
    pub generation: i64,
    pub origin: TaskAttemptOrigin,
    pub created_at_ms: i64,
}

impl TaskAttemptAllocation {
    pub fn recovery_receipt(&self) -> Option<TaskRecoveryReceipt> {
        let TaskAttemptOrigin::Recovery {
            previous_attempt_id,
            ..
        } = &self.origin
        else {
            return None;
        };
        Some(TaskRecoveryReceipt {
            key: self.key.clone(),
            previous_attempt_id: previous_attempt_id.clone(),
            attempt_id: self.attempt_id.clone(),
            generation: self.generation,
        })
    }
}
