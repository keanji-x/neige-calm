//! Durable execution identity for a continuing, Track + key scoped task.
//!
//! These types grant no authority. The service admits recovery against the
//! report declaration and caller; storage fences allocation and replay.

use crate::event::TaskContextRef;
use crate::ids::ActorId;
use crate::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Persisted route values; names describe their meaning without changing bytes.
pub const TASK_IN_TRACK_ROUTE: &str = "in-wave";
pub const TASK_CHILD_TRACK_ROUTE: &str = "sub-wave";

/// Released task-root hash field partition. Reused by initial claim freezing
/// and recovery checks; changing this would reinterpret historical evidence.
pub const TASK_ROOT_HASH_FIELDS: &[&str] = &[
    "kind",
    "goal",
    "command",
    "acceptance",
    "gate",
    "no_gate_reason",
    "depends_on",
    "refs",
    "cwd",
    "context",
];

/// Canonical preimage of the released root hash (including terminal goal alias).
/// The caller hashes these bytes with SHA-256; keeping the projection IO-free
/// lets declaration projection and the server claim fence share one definition.
pub fn task_root_hash_preimage(payload: &serde_json::Value) -> String {
    let mut projected = serde_json::Map::new();
    if let Some(object) = payload.as_object() {
        let terminal = object.get("kind").and_then(serde_json::Value::as_str) == Some("terminal");
        for key in TASK_ROOT_HASH_FIELDS {
            if *key == "command" {
                continue;
            }
            let value = if terminal && *key == "goal" {
                object.get("command").or_else(|| object.get("goal"))
            } else {
                object.get(*key)
            };
            if let Some(value) = value.filter(|value| !value.is_null()) {
                projected.insert((*key).into(), value.clone());
            }
        }
    }
    crate::report_blocks::canonical_json(&serde_json::Value::Object(projected))
}

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
        if spawn != TASK_IN_TRACK_ROUTE
            || !matches!(declared_by.as_str(), PLANNER_DECLARATION_AUTHOR | "user")
        {
            return Err(
                "recovery requires execution within the parent Track and known declaration author"
                    .into(),
            );
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn task_recovery_keeps_released_terminal_root_preimage() {
        let old = json!({"kind":"terminal","goal":"printf ok","refs":[],"priority":8});
        let new = json!({"kind":"terminal","command":"printf ok","refs":[],"priority":2});
        assert_eq!(
            task_root_hash_preimage(&old),
            r#"{
  "goal": "printf ok",
  "kind": "terminal",
  "refs": []
}"#
        );
        assert_eq!(task_root_hash_preimage(&old), task_root_hash_preimage(&new));
        let missing = json!({"kind":"terminal","command":"printf ok"});
        assert_ne!(
            task_root_hash_preimage(&new),
            task_root_hash_preimage(&missing),
            "historical absent versus explicit empty fields must not be renormalized"
        );
    }

    #[test]
    fn task_recovery_origin_requires_typed_constraint_and_rejects_future_versions() {
        assert_eq!(
            serde_json::from_value::<TaskAttemptOrigin>(json!({"kind":"initial"})).unwrap(),
            TaskAttemptOrigin::Initial
        );
        let missing = json!({"kind":"recovery","previous_attempt_id":"old","idempotency_key":"request",
            "request_fingerprint":"fingerprint","reason":"retry","actor":{"kind":"Kernel"}});
        assert!(serde_json::from_value::<TaskAttemptOrigin>(missing).is_err());
        assert!(
            serde_json::from_value::<TaskRecoveryConstraint>(json!({"version":"v99","refs":[]}))
                .is_err()
        );
        let empty = TaskRecoveryConstraint::V1 {
            refs: vec![],
            spawn: TASK_IN_TRACK_ROUTE.into(),
            declared_by: PLANNER_DECLARATION_AUTHOR.into(),
        };
        assert!(empty.validate("w").is_err());
    }
}
