//! The head sentence of a gate-result wake names the candidate a standing verdict covers (#2459).

use super::Observation;
use crate::verify_target::{
    ProvenanceSample, Sample, UnboundReason, VerifyTarget, VerifyTargetEvidence,
};

fn sample(head: &str) -> Sample {
    Sample {
        head: head.into(),
        dirty: Vec::new(),
        provenance: ProvenanceSample {
            realpath: "/leases/lease-1".into(),
            common_dir: "/repo/.git".into(),
            registered: true,
        },
    }
}

fn verified_candidate() -> VerifyTarget {
    VerifyTarget::Candidate {
        candidate_id: "cand-1".into(),
        commit_sha: "a".repeat(40),
        lease_id: "lease-1".into(),
        evidence: VerifyTargetEvidence::Verified {
            cwd: "/leases/lease-1".into(),
            before: sample(&"a".repeat(40)),
            after: sample(&"a".repeat(40)),
            reasons: Vec::new(),
        },
    }
}

fn gate_head(passed: bool, target: Option<VerifyTarget>) -> String {
    let (failing_step, exit_code) = if passed {
        (None, Some(0))
    } else {
        (Some("test".to_string()), Some(101))
    };
    let text = Observation::TaskGateResult {
        idempotency_key: "w:k".into(),
        key: "k".into(),
        passed,
        failing_step,
        exit_code,
        log_tail: String::new(),
        attempt: 2,
        status_detail: None,
        target: target.map(Box::new),
    }
    .to_turn_text();
    text.split_once(" Log tail:")
        .or_else(|| text.split_once(" Read the full log at "))
        .expect("the head sentence precedes the log tail or, on a pass, the log path")
        .0
        .to_string()
}

#[test]
fn a_passed_gate_on_a_verified_candidate_names_the_candidate() {
    assert_eq!(
        gate_head(true, Some(verified_candidate())),
        format!(
            "Task k gate passed on candidate cand-1 ({}) (gate run 2).",
            "a".repeat(40)
        )
    );
}

#[test]
fn a_failed_gate_on_a_verified_candidate_names_the_candidate() {
    assert_eq!(
        gate_head(false, Some(verified_candidate())),
        format!(
            "Task k gate FAILED at step test (exit 101) on candidate cand-1 ({}) (gate run 2). \
             Earlier steps passed; later steps did not run.",
            "a".repeat(40)
        )
    );
}

#[test]
fn an_unbound_or_absent_target_keeps_the_old_sentence() {
    for target in [
        Some(VerifyTarget::Unbound {
            reason: UnboundReason::LegacyLease,
        }),
        None,
    ] {
        assert_eq!(
            gate_head(true, target.clone()),
            "Task k gate passed (gate run 2).",
            "{target:?}"
        );
        assert_eq!(
            gate_head(false, target.clone()),
            "Task k gate FAILED at step test (exit 101) (gate run 2). \
             Earlier steps passed; later steps did not run.",
            "{target:?}"
        );
    }
}

#[test]
fn a_reused_run_names_the_candidate_and_the_run() {
    let target = VerifyTarget::Candidate {
        candidate_id: "cand-1".into(),
        commit_sha: "a".repeat(40),
        lease_id: "lease-1".into(),
        evidence: VerifyTargetEvidence::Reused {
            run: "w:k#r2".into(),
            cwd: "/leases/lease-1".into(),
            before: sample(&"a".repeat(40)),
        },
    };
    assert_eq!(
        gate_head(true, Some(target)),
        format!(
            "Task k gate passed on candidate cand-1 ({}), reusing the worker's passing run w:k#r2 \
             (gate run 2).",
            "a".repeat(40)
        )
    );
}
