//! `calm.plan.list.candidate.verification` (#1727 S4 D8): the twelve-value state of one
//! attempt's gate, derived from the tasks row, its persisted verdict and whether a task-verify
//! Operation exists for it. Pure over its inputs; every D8 row has one arm.

use calm_types::verify_target::VerifyTarget;
use serde::Serialize;

use crate::model::TaskStatus;
use crate::operation::task_verify_adapter::TaskGateResult;
use crate::operation::task_verify_adapter::target::GATE_TARGET_MISMATCH;

/// `integrity.mismatches` entry of a gated `done` row without a verdict.
pub(crate) const MISMATCH_GATE_RESULT_MISSING: &str = "gate_result_missing";

/// `verification.state` — D8's twelve values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum VerificationState {
    /// No gate declared; the `candidate` settlement is all there is to accept.
    Ungated,
    /// The worker has not reported; a gate can only be admitted after the report.
    NotStarted,
    /// `verifying` with no task-verify Operation: the delivery has not settled as a candidate
    /// (or the sweep has not reached it) — see `delivery.state`.
    NotAdmitted,
    /// A task-verify Operation exists (any phase, a terminal one not yet reconciled included).
    Running,
    Passed,
    Red,
    Timeout,
    Infra,
    TargetMismatch,
    /// The verdict checked nothing against a candidate (legacy lease, pre-upgrade freeze or verdict).
    Unbound,
    /// The attempt ended before a gate ran (worker timeout, spawn failure, abandoned delivery).
    NotReached {
        #[serde(skip_serializing_if = "Option::is_none")]
        status_detail: Option<String>,
    },
    /// A gated row is `done` without a verdict: only the gate flip writes `done`, so this is damage.
    Inconsistent {
        mismatches: Vec<&'static str>,
    },
}

/// The `verification` object: the state, the attempt counter, and — for a recorded verdict —
/// its target, log path and the runs-view path of the gate log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct VerificationView {
    #[serde(flatten)]
    pub state: VerificationState,
    pub gate_attempt: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<VerifyTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_log: Option<String>,
}

/// D8's `verification` table, first match wins. `gate_result` is the row's `gate_result_json`
/// decoded as [`TaskGateResult`] (`None` when the column is empty — an unreadable value is
/// treated the same, and is unreachable: every producer since the first gate wrote the four
/// non-defaulted keys). `gate_op_present` says whether a task-verify Operation exists for an
/// attempt at or above the row's `gate_attempt`.
pub(crate) fn verification_state(
    task_id: &str,
    gate_json: Option<&str>,
    status: TaskStatus,
    status_detail: Option<&str>,
    gate_attempt: i64,
    gate_result: Option<&TaskGateResult>,
    gate_op_present: bool,
) -> VerificationView {
    let bare = |state: VerificationState| VerificationView {
        state,
        gate_attempt,
        target: None,
        log_path: None,
        gate_log: None,
    };
    if gate_json.is_none() {
        return bare(VerificationState::Ungated);
    }
    match status {
        TaskStatus::Pending
        | TaskStatus::Canceled
        | TaskStatus::Dispatched
        | TaskStatus::Running => bare(VerificationState::NotStarted),
        TaskStatus::Verifying if gate_op_present => bare(VerificationState::Running),
        TaskStatus::Verifying => bare(VerificationState::NotAdmitted),
        TaskStatus::Done | TaskStatus::Failed => match gate_result {
            Some(result) => {
                let state = if matches!(result.target, VerifyTarget::Unbound { .. }) {
                    VerificationState::Unbound
                } else {
                    match result.verdict.status_detail.as_deref() {
                        None => VerificationState::Passed,
                        Some("gate-red") => VerificationState::Red,
                        Some("gate-timeout") => VerificationState::Timeout,
                        Some(GATE_TARGET_MISMATCH) => VerificationState::TargetMismatch,
                        // `gate-infra`, and any class this build does not know: the code was not judged.
                        Some(_) => VerificationState::Infra,
                    }
                };
                VerificationView {
                    state,
                    gate_attempt,
                    target: Some(result.target.clone()),
                    log_path: Some(result.verdict.log_path.clone()),
                    gate_log: Some(format!(
                        "runs/{task_id}/gates/{}.log",
                        result.verdict.attempt
                    )),
                }
            }
            None if status == TaskStatus::Failed => bare(VerificationState::NotReached {
                status_detail: status_detail.map(str::to_string),
            }),
            None => bare(VerificationState::Inconsistent {
                mismatches: vec![MISMATCH_GATE_RESULT_MISSING],
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::task_verify_adapter::GateVerdict;
    use calm_types::verify_target::{
        NoCandidateReason, ProvenanceSample, Sample, UnboundReason, VerifyTargetEvidence,
    };
    use serde_json::json;

    fn result(status_detail: Option<&str>, target: VerifyTarget) -> TaskGateResult {
        TaskGateResult {
            verdict: GateVerdict {
                passed: status_detail.is_none(),
                status_detail: status_detail.map(str::to_string),
                failing_step: None,
                exit_code: None,
                log_tail: String::new(),
                log_path: "/logs/t-g2.log".into(),
                attempt: 2,
            },
            cwd: None,
            target,
        }
    }

    fn candidate(evidence: VerifyTargetEvidence) -> VerifyTarget {
        VerifyTarget::Candidate {
            candidate_id: "c-1".into(),
            commit_sha: "a".repeat(40),
            lease_id: "l-1".into(),
            evidence,
        }
    }

    fn sample() -> Sample {
        Sample {
            head: "a".repeat(40),
            dirty: vec![],
            provenance: ProvenanceSample {
                realpath: "/w".into(),
                common_dir: "/r/.git".into(),
                registered: true,
            },
        }
    }

    /// D8's thirteen rows, one assertion each; the twelve `state` labels all appear.
    #[test]
    fn verification_state_covers_every_row() {
        let gate = Some(r#"{"steps":[{"name":"t","cmd":"true"}]}"#);
        let state = |view: &VerificationView| serde_json::to_value(view).unwrap()["state"].clone();
        let verified = candidate(VerifyTargetEvidence::Verified {
            cwd: "/w".into(),
            before: sample(),
            after: sample(),
            reasons: vec![],
        });

        // 1. no gate.
        let v = verification_state("t", None, TaskStatus::Done, None, 0, None, false);
        assert_eq!(v.state, VerificationState::Ungated);
        assert_eq!(state(&v), json!("ungated"));
        // 2. before the report.
        for status in [TaskStatus::Dispatched, TaskStatus::Running] {
            let v = verification_state("t", gate, status, None, 0, None, false);
            assert_eq!(v.state, VerificationState::NotStarted, "{status:?}");
        }
        assert_eq!(
            verification_state("t", gate, TaskStatus::Pending, None, 0, None, false).state,
            VerificationState::NotStarted,
            "total over pending/canceled too"
        );
        // 3. verifying, no op.
        let v = verification_state("t", gate, TaskStatus::Verifying, None, 0, None, false);
        assert_eq!(v.state, VerificationState::NotAdmitted);
        assert_eq!(
            serde_json::to_value(&v).unwrap(),
            json!({"state": "not_admitted", "gate_attempt": 0})
        );
        // 4. verifying, an op exists.
        let v = verification_state("t", gate, TaskStatus::Verifying, None, 1, None, true);
        assert_eq!(v.state, VerificationState::Running);
        assert_eq!(v.gate_attempt, 1);
        // 5. done / failed with a verdict: the five classes.
        let passed = result(None, verified.clone());
        let v = verification_state("t", gate, TaskStatus::Done, None, 2, Some(&passed), false);
        assert_eq!(v.state, VerificationState::Passed);
        assert_eq!(v.target, Some(verified.clone()));
        assert_eq!(v.log_path.as_deref(), Some("/logs/t-g2.log"));
        assert_eq!(v.gate_log.as_deref(), Some("runs/t/gates/2.log"));
        let wire = serde_json::to_value(&v).unwrap();
        assert_eq!(wire["state"], json!("passed"));
        assert_eq!(wire["target"]["kind"], json!("candidate"));
        for (detail, expected, label) in [
            ("gate-red", VerificationState::Red, "red"),
            ("gate-timeout", VerificationState::Timeout, "timeout"),
            ("gate-infra", VerificationState::Infra, "infra"),
            (
                "gate-target-mismatch",
                VerificationState::TargetMismatch,
                "target_mismatch",
            ),
        ] {
            let r = result(Some(detail), verified.clone());
            let v = verification_state(
                "t",
                gate,
                TaskStatus::Failed,
                Some(detail),
                2,
                Some(&r),
                false,
            );
            assert_eq!(v.state, expected, "{detail}");
            assert_eq!(state(&v), json!(label));
            assert!(v.target.is_some());
        }
        // `no_candidate` is `infra` with its target intact.
        let r = result(
            Some("gate-infra"),
            VerifyTarget::NoCandidate {
                reason: NoCandidateReason::NoDeliveryRow,
            },
        );
        let v = verification_state(
            "t",
            gate,
            TaskStatus::Failed,
            Some("gate-infra"),
            1,
            Some(&r),
            false,
        );
        assert_eq!(v.state, VerificationState::Infra);
        assert_eq!(
            serde_json::to_value(&v).unwrap()["target"],
            json!({"kind": "no_candidate", "reason": {"kind": "no_delivery_row"}})
        );
        // an unbound target reads `unbound` whatever the class (D12 (b)(c)).
        for (detail, reason) in [
            (None, UnboundReason::LegacyVerdict),
            (Some("gate-red"), UnboundReason::LegacyFrozen),
            (Some("gate-infra"), UnboundReason::LegacyLease),
            (None, UnboundReason::Terminal),
        ] {
            let r = result(detail, VerifyTarget::Unbound { reason });
            let status = if detail.is_none() {
                TaskStatus::Done
            } else {
                TaskStatus::Failed
            };
            let v = verification_state("t", gate, status, detail, 1, Some(&r), false);
            assert_eq!(v.state, VerificationState::Unbound, "{reason:?}");
            assert_eq!(state(&v), json!("unbound"));
        }
        // 6. failed without a verdict.
        let v = verification_state(
            "t",
            gate,
            TaskStatus::Failed,
            Some("worker-timeout"),
            0,
            None,
            false,
        );
        assert_eq!(
            v.state,
            VerificationState::NotReached {
                status_detail: Some("worker-timeout".into())
            }
        );
        assert_eq!(
            serde_json::to_value(&v).unwrap(),
            json!({"state": "not_reached", "status_detail": "worker-timeout", "gate_attempt": 0})
        );
        // 7. done, gated, no verdict.
        let v = verification_state("t", gate, TaskStatus::Done, None, 1, None, false);
        assert_eq!(
            v.state,
            VerificationState::Inconsistent {
                mismatches: vec![MISMATCH_GATE_RESULT_MISSING]
            }
        );
        assert_eq!(
            serde_json::to_value(&v).unwrap(),
            json!({"state": "inconsistent", "mismatches": ["gate_result_missing"], "gate_attempt": 1})
        );
        // An unknown class is not judged code.
        let r = result(Some("gate-something-new"), verified);
        let v = verification_state("t", gate, TaskStatus::Failed, None, 1, Some(&r), false);
        assert_eq!(v.state, VerificationState::Infra);
    }
}
