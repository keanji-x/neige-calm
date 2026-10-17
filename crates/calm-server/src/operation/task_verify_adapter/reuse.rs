//! Reuse of a passing worker-requested run at the first gate of an attempt (#2464 §4). Inside the
//! short prepare transaction, after revalidating externally observed target/run facts, the verdict is the
//! attempt's last run when all of these hold:
//!
//! 1. the gate attempt is 1 (no earlier verdict, no regate);
//! 2. the highest-numbered run passed, with `verified` evidence and no reasons;
//! 3. its commit is the candidate's commit (one id fixes tree, parent and message);
//! 4. its refs digest equals the digest collected outside the writer, by the target sample's deadline;
//! 5. every other gate op of the attempt is finished and proven stopped (J1: the `#g1` op being
//!    prepared is still `pending` and is left out).
//!
//! The run's log keeps the gate's address (G1): its file is hard-linked as `<attempt>-g1.log`. A
//! link that fails, or any condition that does not hold, leaves the gate to run as before.

use std::path::Path;

use calm_types::verify_target::{VerifyTarget, VerifyTargetEvidence};

use super::gate_attempt_key;
use super::target::{FrozenTarget, PreparedTarget, TaskGateResult};
use crate::error::Result;
use crate::operation::Tx;
use crate::operation::gate_lifecycle::GateVerdict;
use crate::operation::gate_ops::{
    AttemptGateOps, GateOp, GateOpKind, classify_gate_ops, gate_op_outcome_tx,
    gate_ops_of_attempt_tx,
};
use crate::operation::task_gate_run::finalize::terminal_result;
use crate::operation::task_gate_run::gate_run_log_path;

/// The gate being prepared: its attempt row, its gate attempt, the log the gate would write, the
/// directory the run logs are in. Sampling has already completed outside the writer.
pub(super) struct Gate<'a> {
    pub task_id: &'a str,
    pub attempt: i64,
    pub log_path: &'a Path,
    pub gate_logs_dir: &'a Path,
}

/// At gate attempt 1, unlink a `<attempt>-g1.log` left by a prepare that did not commit, before
/// any refusal line or link is written. No reader reaches it before the attempt bump commits, so
/// nothing is lost; left there, a new link would fail on `EEXIST`, and a refusal line or a gate's
/// log would be written through it into the run's log.
pub(super) async fn unlink_leftover_log(gate: &Gate<'_>) -> Result<()> {
    if gate.attempt != 1 {
        return Ok(());
    }
    match tokio::fs::remove_file(gate.log_path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// §4: when the attempt's last run stands for this gate, freeze `Reused` and its verdict in
/// `prepared`; otherwise leave `prepared` as the target check froze it.
pub(super) async fn reuse_passing_run_tx(
    tx: &mut Tx<'_>,
    gate: &Gate<'_>,
    prepared: &mut PreparedTarget,
    observed_refs: Option<&str>,
) -> Result<()> {
    // (1)
    if gate.attempt != 1 {
        return Ok(());
    }
    let FrozenTarget::Candidate {
        candidate_id,
        commit_sha,
        lease_id,
        cwd,
        before,
        refused: false,
        ..
    } = &prepared.target
    else {
        return Ok(());
    };
    let facts = Facts::read(tx, gate.task_id).await?;
    let Some((last, run, run_log)) = eligible_run(
        &facts,
        gate.task_id,
        gate.attempt,
        commit_sha,
        gate.gate_logs_dir,
    ) else {
        return Ok(());
    };
    // (4): the external digest belongs to the exact revalidated run and candidate.
    if observed_refs != run.refs.as_deref() {
        return Ok(());
    }
    // G1: the gate's log address names the run's log.
    if let Err(error) = tokio::fs::hard_link(&run_log, gate.log_path).await {
        tracing::warn!(
            run = %last.key,
            %error,
            "gate reuse: the run's log could not be linked as the gate's; the gate runs"
        );
        return Ok(());
    }
    let verdict = TaskGateResult {
        verdict: GateVerdict {
            log_path: gate.log_path.display().to_string(),
            attempt: gate.attempt,
            ..run.verdict
        },
        cwd: Some(cwd.clone()),
        target: VerifyTarget::Candidate {
            candidate_id: candidate_id.clone(),
            commit_sha: commit_sha.clone(),
            lease_id: lease_id.clone(),
            evidence: VerifyTargetEvidence::Reused {
                run: last.key.clone(),
                cwd: cwd.clone(),
                before: before.clone(),
            },
        },
    };
    prepared.target = FrozenTarget::Reused {
        candidate_id: candidate_id.clone(),
        commit_sha: commit_sha.clone(),
        lease_id: lease_id.clone(),
        cwd: cwd.clone(),
        run: last.key.clone(),
        before: before.clone(),
    };
    prepared.verdict = Some(verdict);
    Ok(())
}

/// Exact SQL selection observed before Git. Commit re-reads this snapshot and the reuse
/// consumer still rechecks live stopped-process proofs; a previous Clear is not authority.
#[derive(PartialEq)]
pub(super) struct Facts {
    ops: Vec<crate::operation::gate_ops::GateOp>,
    last_outcome: Option<crate::operation::OperationOutcome>,
}
impl Facts {
    pub(super) async fn read(tx: &mut Tx<'_>, task_id: &str) -> Result<Self> {
        let ops = gate_ops_of_attempt_tx(tx, task_id).await?;
        let last_outcome = match ops.iter().rfind(|op| op.kind == GateOpKind::Run) {
            Some(last) => gate_op_outcome_tx(tx, last).await?,
            None => None,
        };
        Ok(Self { ops, last_outcome })
    }
}

/// The one reuse eligibility decision, shared before expensive refs observation and at commit.
/// Each invocation repeats current stopped-process proof, rather than trusting an old Clear.
fn eligible_run(
    facts: &Facts,
    task_id: &str,
    attempt: i64,
    commit_sha: &str,
    logs: &Path,
) -> Option<(
    GateOp,
    crate::operation::task_gate_run::finalize::GateRunResult,
    std::path::PathBuf,
)> {
    if attempt != 1 {
        return None;
    }
    let preparing = gate_attempt_key(task_id, attempt);
    let others: Vec<_> = facts
        .ops
        .iter()
        .filter(|op| op.key != preparing)
        .cloned()
        .collect();
    if !matches!(classify_gate_ops(&others), AttemptGateOps::Clear) {
        return None;
    }
    let last = facts.ops.iter().rfind(|op| op.kind == GateOpKind::Run)?;
    let outcome = facts.last_outcome.clone()?;
    let run_log = gate_run_log_path(logs, task_id, last.number);
    let run = terminal_result(last.number, &run_log, outcome);
    if !run.verdict.passed
        || !matches!(&run.evidence, Some(VerifyTargetEvidence::Verified { reasons, .. }) if reasons.is_empty())
        || run.commit.as_deref() != Some(commit_sha)
        || run.refs.is_none()
    {
        return None;
    }
    Some((last.clone(), run, run_log))
}

pub(super) fn needs_refs(
    facts: &Facts,
    task_id: &str,
    attempt: i64,
    identity: &super::target::VerifyIdentity,
    sampled: Option<&std::result::Result<super::target::Sampled, super::target::SampleFailure>>,
    logs: &Path,
) -> Result<bool> {
    let super::target::VerifyIdentity::Bound {
        lease,
        candidate: Some(candidate),
        ..
    } = identity
    else {
        return Ok(false);
    };
    let Some(Ok(sampled)) = sampled else {
        return Ok(false);
    };
    let expected = super::target::Expected::for_candidate(lease, candidate)?;
    Ok(super::target::reasons(sampled, &expected).is_empty()
        && eligible_run(facts, task_id, attempt, &candidate.commit_sha, logs).is_some())
}
