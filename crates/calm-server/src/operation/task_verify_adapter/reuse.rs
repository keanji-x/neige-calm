//! Reuse of a passing worker-requested run at the first gate of an attempt (#2464 §4). Inside the
//! `#g1` `prepare_tx`, after the target check froze an unrefused candidate, the verdict is the
//! attempt's last run when all of these hold:
//!
//! 1. the gate attempt is 1 (no earlier verdict, no regate);
//! 2. the highest-numbered run passed, with `verified` evidence and no reasons;
//! 3. its commit is the candidate's commit (one id fixes tree, parent and message);
//! 4. its refs digest equals the digest taken now, by the target sample's deadline;
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
    AttemptGateOps, GateOpKind, attempt_gate_ops, gate_op_outcome_tx, gate_ops_of_attempt_tx,
};
use crate::operation::task_gate_run::finalize::terminal_result;
use crate::operation::task_gate_run::{gate_run_log_path, refs_digest};

/// The gate being prepared: its attempt row, its gate attempt, the log the gate would write, the
/// directory the run logs are in, and the deadline of the prepare's one sampling bound.
pub(super) struct Gate<'a> {
    pub task_id: &'a str,
    pub attempt: i64,
    pub log_path: &'a Path,
    pub gate_logs_dir: &'a Path,
    pub deadline: tokio::time::Instant,
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
    // (5)
    let preparing = gate_attempt_key(gate.task_id, gate.attempt);
    if !matches!(
        attempt_gate_ops(tx, gate.task_id, Some(&preparing)).await?,
        AttemptGateOps::Clear
    ) {
        return Ok(());
    }
    // (2)
    let ops = gate_ops_of_attempt_tx(tx, gate.task_id).await?;
    let Some(last) = ops.iter().rfind(|op| op.kind == GateOpKind::Run) else {
        return Ok(());
    };
    let Some(outcome) = gate_op_outcome_tx(tx, last).await? else {
        return Ok(());
    };
    let run_log = gate_run_log_path(gate.gate_logs_dir, gate.task_id, last.number);
    let run = terminal_result(last.number, &run_log, outcome);
    let verified = matches!(
        &run.evidence,
        Some(VerifyTargetEvidence::Verified { reasons, .. }) if reasons.is_empty()
    );
    if !run.verdict.passed || !verified {
        return Ok(());
    }
    // (3)
    if run.commit.as_deref() != Some(commit_sha.as_str()) {
        return Ok(());
    }
    // (4)
    let Some(refs) = run.refs.as_deref() else {
        return Ok(());
    };
    if refs_digest(Path::new(cwd), gate.deadline).await.as_deref() != Some(refs) {
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
