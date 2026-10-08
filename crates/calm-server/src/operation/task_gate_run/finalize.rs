//! The run's result (#2464 §5): one function, [`finalize_run`], turns the live observer's inputs
//! into exactly one result by precedence, and [`terminal_result`] reads one for a run that ended
//! without a live observer (P11–P14).

use std::path::Path;

use calm_types::verify_target::{
    ProvenanceSample, Sample, SamplePhase, VerifyTargetEvidence, render_reasons,
};
use serde::{Deserialize, Serialize};

use super::FrozenRun;
use super::checkpoint::{CHECKPOINT_STEP, read_run_ref};
use crate::db::sqlite::begin_immediate_tx;
use crate::error::Result;
use crate::git_candidate::delivery::{PROVENANCE_MISMATCH_EXIT_CODES, failure_sentence};
use crate::operation::gate_lifecycle::GateVerdict;
use crate::operation::gate_process::{GateObservation, GateWait, read_log_tail};
use crate::operation::task_verify_adapter::target::{
    Expected, GATE_INFRA, GATE_TARGET_MISMATCH, GATE_TIMEOUT, append_log_line, reasons, sample,
};
use crate::operation::{
    OperationOutcome, ParkedCompletion, ParkedOutcome, SpawnCtx, complete_parked_tx,
};

/// One run's result, the op's result. `commit` is the run's commit (its ref) when the checkpoint
/// finished; `refs` the digest taken at spawn; `evidence` the after-sample against the commit.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GateRunResult {
    pub run: i64,
    pub commit: Option<String>,
    pub refs: Option<String>,
    pub verdict: GateVerdict,
    pub evidence: Option<VerifyTargetEvidence>,
}

fn failed(status_detail: &str, frozen: &FrozenRun, log_path: &Path, line: String) -> GateVerdict {
    GateVerdict {
        passed: false,
        status_detail: Some(status_detail.to_string()),
        failing_step: None,
        exit_code: None,
        log_tail: line,
        log_path: log_path.display().to_string(),
        attempt: frozen.run,
    }
}

/// Append the kernel's `line` to the run's log; the tail is the log's as the steps left it, then the
/// line (the append is not read back: it may still be in flight).
async fn tail_with(log_path: &Path, line: &str) -> String {
    let mut tail = read_log_tail(log_path);
    append_log_line(log_path, line).await;
    if !tail.is_empty() && !tail.ends_with('\n') {
        tail.push('\n');
    }
    tail.push_str(line);
    tail
}

/// An infra verdict whose line is appended to the run's log.
async fn infra(frozen: &FrozenRun, log_path: &Path, line: String) -> GateVerdict {
    let tail = tail_with(log_path, &line).await;
    failed(GATE_INFRA, frozen, log_path, tail)
}

/// The checkpoint's own checks stand in for a before-sample (D1): it proved the provenance and
/// left the checkout clean at the run's commit.
fn checkpoint_sample(frozen: &FrozenRun, commit: &str) -> Sample {
    Sample {
        head: commit.to_string(),
        dirty: Vec::new(),
        provenance: ProvenanceSample {
            realpath: frozen.canonical_path.clone(),
            common_dir: frozen.git_common_dir.clone(),
            registered: true,
        },
    }
}

/// §5 P1–P9, in precedence order; the first row that matches produces the result:
/// P1 the group is not proven stopped → infra; P2 the checkpoint exited 10/11/12/15 → target
/// mismatch; P3 it exited otherwise → infra; P4 the run ref is missing or unreadable, or the
/// after-sample fails → infra; P5 the after-sample differs from the ref or is dirty → target
/// mismatch; then the observation as classified: P6 passed, P7 red, P8 timeout, P9 infra.
pub(crate) async fn finalize_run(
    frozen: &FrozenRun,
    observation: &GateObservation,
    stopped: Result<()>,
    refs: Option<String>,
) -> GateRunResult {
    let log_path = Path::new(&observation.verdict.log_path).to_path_buf();
    let result = |verdict: GateVerdict, commit: Option<String>, evidence| GateRunResult {
        run: frozen.run,
        commit,
        refs: refs.clone(),
        verdict,
        evidence,
    };
    // P1
    if let Err(error) = stopped {
        let line = format!("gate-infra: the run's processes could not be proven stopped: {error}");
        return result(infra(frozen, &log_path, line).await, None, None);
    }
    // P2, P3: step 1 is the checkpoint; no declared step ran.
    if observation.started_step == Some(1)
        && let GateWait::Exited(code) = observation.wait
        && code != 0
    {
        if PROVENANCE_MISMATCH_EXIT_CODES.contains(&code) {
            let line = format!(
                "gate run REFUSED before any step ran: {}",
                failure_sentence(&code.to_string(), None)
            );
            let tail = tail_with(&log_path, &line).await;
            let mut verdict = failed(GATE_TARGET_MISMATCH, frozen, &log_path, tail);
            verdict.failing_step = Some(CHECKPOINT_STEP.into());
            verdict.exit_code = Some(code);
            return result(verdict, None, None);
        }
        let sentence = match code {
            13 | 14 => failure_sentence(&code.to_string(), None),
            _ => failure_sentence("git", Some(code)),
        };
        let mut verdict = infra(
            frozen,
            &log_path,
            format!("gate-infra: the run's checkpoint failed: {sentence}"),
        )
        .await;
        verdict.failing_step = Some(CHECKPOINT_STEP.into());
        verdict.exit_code = Some(code);
        return result(verdict, None, None);
    }
    // P4
    let cwd = Path::new(&frozen.cwd);
    let commit = match read_run_ref(cwd, &frozen.ref_name).await {
        Ok(Some(commit)) => commit,
        Ok(None) => {
            let line = "gate-infra: the run's checkpoint pinned no commit".to_string();
            return result(infra(frozen, &log_path, line).await, None, None);
        }
        Err(error) => {
            let line = format!("gate-infra: the run's commit could not be read: {error}");
            return result(infra(frozen, &log_path, line).await, None, None);
        }
    };
    let expected = Expected {
        canonical_path: frozen.canonical_path.clone(),
        git_common_dir: frozen.git_common_dir.clone(),
        commit_sha: commit.clone(),
    };
    let after = match sample(cwd, &expected).await {
        Ok(after) => after,
        Err(failure) => {
            let line = format!(
                "gate-infra: the checkout was not sampled after the run: {}",
                failure.reason
            );
            let evidence = VerifyTargetEvidence::Unsampled {
                phase: SamplePhase::Finalize {
                    cwd: frozen.cwd.clone(),
                    reason: failure.reason,
                },
            };
            return result(
                infra(frozen, &log_path, line).await,
                Some(commit),
                Some(evidence),
            );
        }
    };
    // P5
    let reasons = reasons(&after, &expected);
    let evidence = VerifyTargetEvidence::Verified {
        cwd: frozen.cwd.clone(),
        before: checkpoint_sample(frozen, &commit),
        after: after.sample.clone(),
        reasons: reasons.clone(),
    };
    if !reasons.is_empty() {
        let line = format!(
            "gate run RESULT DISCARDED: the checkout changed during the run ({}): HEAD {commit}→{}",
            render_reasons(&reasons),
            after.sample.head
        );
        let tail = tail_with(&log_path, &line).await;
        let verdict = failed(GATE_TARGET_MISMATCH, frozen, &log_path, tail);
        return result(verdict, Some(commit), Some(evidence));
    }
    // P6–P9: the shared classification of the observation.
    result(observation.verdict.clone(), Some(commit), Some(evidence))
}

/// Commit the live observer's result. A run already resolved by recovery keeps that resolution.
pub(crate) async fn complete_run_op(
    ctx: &SpawnCtx,
    op_id: &str,
    result: &GateRunResult,
) -> Result<()> {
    let outcome = ParkedOutcome::Succeeded {
        result: serde_json::to_value(result)?,
    };
    let pool = ctx.operation_repo.sqlite_pool();
    let mut tx = begin_immediate_tx(&pool).await?;
    match complete_parked_tx(&mut tx, &op_id.to_string(), &outcome).await? {
        ParkedCompletion::Completed(completed) => {
            tx.commit().await?;
            ctx.completion.complete(completed);
        }
        ParkedCompletion::AlreadyResolved { phase } => {
            tx.rollback().await?;
            tracing::debug!(op_id, phase = ?phase, "gate run observer: op already resolved; result discarded");
        }
    }
    Ok(())
}

/// The result of a finished run op: its own result when the observer completed it, otherwise the
/// infra or timeout reading of its failure (P11–P14). A reason that names `gate-infra` is infra
/// whatever class the driver filed it under (P11 before P13); `parked_deadline` is a timeout.
pub(crate) fn terminal_result(
    frozen_run: i64,
    log_path: &Path,
    outcome: OperationOutcome,
) -> GateRunResult {
    let (status_detail, reason) = match outcome {
        OperationOutcome::Succeeded { result }
        | OperationOutcome::SucceededViaCollision { result, .. } => {
            match serde_json::from_value::<GateRunResult>(result) {
                Ok(result) => return result,
                Err(error) => (GATE_INFRA, format!("gate run result unparseable: {error}")),
            }
        }
        OperationOutcome::Failed {
            last_error,
            last_error_class,
            ..
        } => {
            let status = if !last_error.starts_with(GATE_INFRA)
                && last_error_class.as_deref() == Some("parked_deadline")
            {
                GATE_TIMEOUT
            } else {
                GATE_INFRA
            };
            (status, last_error)
        }
        OperationOutcome::Stuck { reason, .. } => (GATE_INFRA, reason),
    };
    GateRunResult {
        run: frozen_run,
        commit: None,
        refs: None,
        verdict: GateVerdict {
            passed: false,
            status_detail: Some(status_detail.to_string()),
            failing_step: None,
            exit_code: None,
            log_tail: reason,
            log_path: log_path.display().to_string(),
            attempt: frozen_run,
        },
        evidence: None,
    }
}
