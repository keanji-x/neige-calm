//! Git candidate delivery vocabulary (#1727 S4): the shape one `task.git_delivery_settled` carries.
//!
//! `DeliverySettlement` is the single definition of a settlement outcome; the settlement row,
//! the event and the Planner read surface all copy it. `DeliveryWakeReason` is the wake
//! disposition the settlement transaction decides exactly once from the tasks row: the row
//! changes afterwards, the event does not, so the push predicate reads only the event.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// How one Git delivery settled: a pinned candidate, or why none was minted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum DeliverySettlement {
    /// The delivery script pinned a candidate ref. `commit_sha == base_sha` is a delivery that
    /// changed nothing (no new commit); `base_is_ancestor` is the script's observation, not a verdict.
    Candidate {
        candidate_id: String,
        commit_sha: String,
        base_sha: String,
        base_is_ancestor: bool,
    },
    /// No candidate exists for this delivery. `code` is the kernel's classification, `reason` the
    /// evidence line written on the delivery row, `retry_allowed` whether a retry is admissible.
    Failed {
        code: DeliveryFailureCode,
        reason: String,
        retry_allowed: bool,
    },
}

/// Why a Git delivery produced no candidate. Every value has exactly one producer (D2 code table).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum DeliveryFailureCode {
    /// The lease directory no longer exists; never retryable.
    WorkspaceMissing,
    /// The worktree is not the registered lease worktree, HEAD left the slice branch, or an
    /// operation is in progress; retryable once the Planner repairs the worktree.
    ProvenanceMismatch,
    /// The script exited before pinning a ref (hook, lock, disk, unreadable base); retryable.
    CommitFailed,
    /// The kernel cannot prove what happened (infra class, timeout, probe unknown); retryable.
    Unresolved,
}

impl DeliveryFailureCode {
    /// The wire spelling, for turn text and diagnostics.
    pub fn wire_str(self) -> &'static str {
        match self {
            DeliveryFailureCode::WorkspaceMissing => "workspace_missing",
            DeliveryFailureCode::ProvenanceMismatch => "provenance_mismatch",
            DeliveryFailureCode::CommitFailed => "commit_failed",
            DeliveryFailureCode::Unresolved => "unresolved",
        }
    }
}

/// The wake disposition decided once in the settlement transaction and copied into the event.
/// Only `DeferredToGate` is silent: `task.gate_result` carries that wake.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum DeliveryWakeReason {
    /// The delivery failed; every failed settlement wakes the Planner once.
    Failed,
    /// A candidate for a task without a gate; nothing else will wake the Planner for it.
    UngatedCandidate,
    /// A candidate for a gated task whose row already left `verifying`; no `task.gate_result` follows.
    GateAlreadyTerminal,
    /// A candidate for a gated task still `verifying`; the gate verdict is the wake.
    DeferredToGate,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn delivery_settlement_round_trips_both_kinds() {
        let candidate = DeliverySettlement::Candidate {
            candidate_id: "d-1".into(),
            commit_sha: "a".repeat(40),
            base_sha: "b".repeat(40),
            base_is_ancestor: true,
        };
        let wire = serde_json::to_value(&candidate).unwrap();
        assert_eq!(
            wire,
            json!({
                "kind": "candidate",
                "candidate_id": "d-1",
                "commit_sha": "a".repeat(40),
                "base_sha": "b".repeat(40),
                "base_is_ancestor": true,
            })
        );
        assert_eq!(
            serde_json::from_value::<DeliverySettlement>(wire).unwrap(),
            candidate
        );

        let failed = DeliverySettlement::Failed {
            code: DeliveryFailureCode::CommitFailed,
            reason: "index.lock exists".into(),
            retry_allowed: true,
        };
        let wire = serde_json::to_value(&failed).unwrap();
        assert_eq!(
            wire,
            json!({
                "kind": "failed",
                "code": "commit_failed",
                "reason": "index.lock exists",
                "retry_allowed": true,
            })
        );
        assert_eq!(
            serde_json::from_value::<DeliverySettlement>(wire).unwrap(),
            failed
        );

        // `deny_unknown_fields` on both variants; a missing field is an error too.
        assert!(
            serde_json::from_value::<DeliverySettlement>(json!({
                "kind": "failed", "code": "unresolved", "reason": "", "retry_allowed": true, "extra": 1
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<DeliverySettlement>(json!({
                "kind": "candidate", "candidate_id": "d", "commit_sha": "c", "base_sha": "b",
                "base_is_ancestor": false, "retry_allowed": true
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<DeliverySettlement>(json!({
                "kind": "candidate", "candidate_id": "d", "commit_sha": "c", "base_sha": "b"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<DeliverySettlement>(json!({ "kind": "pending" })).is_err()
        );
    }

    #[test]
    fn wake_reason_and_failure_code_round_trip_all_values() {
        let reasons = [
            (DeliveryWakeReason::Failed, "failed"),
            (DeliveryWakeReason::UngatedCandidate, "ungated_candidate"),
            (
                DeliveryWakeReason::GateAlreadyTerminal,
                "gate_already_terminal",
            ),
            (DeliveryWakeReason::DeferredToGate, "deferred_to_gate"),
        ];
        for (reason, wire) in reasons {
            assert_eq!(serde_json::to_value(reason).unwrap(), json!(wire));
            assert_eq!(
                serde_json::from_value::<DeliveryWakeReason>(json!(wire)).unwrap(),
                reason
            );
        }
        assert!(serde_json::from_value::<DeliveryWakeReason>(json!("silent")).is_err());
        assert!(serde_json::from_value::<DeliveryWakeReason>(json!("Failed")).is_err());

        let codes = [
            (DeliveryFailureCode::WorkspaceMissing, "workspace_missing"),
            (
                DeliveryFailureCode::ProvenanceMismatch,
                "provenance_mismatch",
            ),
            (DeliveryFailureCode::CommitFailed, "commit_failed"),
            (DeliveryFailureCode::Unresolved, "unresolved"),
        ];
        for (code, wire) in codes {
            assert_eq!(serde_json::to_value(code).unwrap(), json!(wire));
            assert_eq!(code.wire_str(), wire);
            assert_eq!(
                serde_json::from_value::<DeliveryFailureCode>(json!(wire)).unwrap(),
                code
            );
        }
        assert!(serde_json::from_value::<DeliveryFailureCode>(json!("timeout")).is_err());
    }
}
