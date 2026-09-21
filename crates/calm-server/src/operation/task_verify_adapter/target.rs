//! The verification target of a task-verify gate (#1727 S4 D3): what the verdict was checked
//! against, frozen at `prepare_tx`, sampled again when the gate ends, and carried on every
//! verdict as [`TaskGateResult::target`].
//!
//! Producers (D3's producer × variant table): `prepare_target_tx` freezes the target and refuses
//! in the same transaction (P1–P5), `finalize` is the one exit of the three completion paths
//! (P6, P7), `compensation_target` feeds the compensation step (P8) and `reconcile_result_tx`
//! is the scheduler's reconcile arm (P9, P9b, P10). The wire vocabulary lives in
//! `calm_types::verify_target`; the identity check is `GIT_LEASE_PROVENANCE_SCRIPT`, executed
//! here and never restated (5.1.5).

use std::path::Path;
use std::process::Stdio;

use calm_types::forge_git::GIT_LEASE_PROVENANCE_SCRIPT;
use calm_types::verify_target::{
    MismatchReason, NoCandidateReason, ProvenanceSample, Sample, SamplePhase, UnboundReason,
    VerifyTarget, VerifyTargetEvidence, render_reasons,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{FrozenVerify, GateSpec, GateVerdict, gate_attempt_key};
use crate::error::{CalmError, Result};
use crate::git_candidate::abandonment::abandonment_for_delivery_tx;
use crate::git_candidate::candidate::{CandidateRow, candidate_for_attempt_tx};
use crate::git_candidate::delivery::{
    DeliveryRow, DeliverySettled, delivery_latest_for_attempt_tx, lease_for_delivery_tx,
};
use crate::model::{Task, TaskKind};
use crate::operation::forge_action_adapter::forge_base_env;
use crate::operation::workspace_lease::facts::{LeaseStates, latest_workspace_lease_for_card_tx};
use crate::operation::workspace_lease::{DeliveryPolicy, WorkspaceLease};
use crate::operation::{OperationOutcome, Tx, TxOutput};

/// `status_detail` of a verdict whose target check failed (the fourth value; isolated never
/// produces it).
pub const GATE_TARGET_MISMATCH: &str = "gate-target-mismatch";
const GATE_INFRA: &str = "gate-infra";
const GATE_TIMEOUT: &str = "gate-timeout";

/// The `sh -c` text of one provenance observation: the shared function, then one call with the
/// two positional parameters (`canonical_path`, `git_common_dir`).
const PROVENANCE_SAMPLE_SCRIPT: &str = "neige_lease_provenance \"$1\" \"$2\"";

/// Why a frozen gate checks nothing against a candidate: the three [`UnboundReason`]s a freeze
/// can carry (`LegacyVerdict` belongs to a persisted verdict, never to a freeze).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrozenUnbound {
    LegacyLease,
    LegacyFrozen,
    Terminal,
}

impl FrozenUnbound {
    fn wire(self) -> UnboundReason {
        match self {
            FrozenUnbound::LegacyLease => UnboundReason::LegacyLease,
            FrozenUnbound::LegacyFrozen => UnboundReason::LegacyFrozen,
            FrozenUnbound::Terminal => UnboundReason::Terminal,
        }
    }
}

/// What `prepare_tx` froze about the target (`FrozenVerify.target`). Absent on an op frozen
/// before slice 4, which reads as `Unbound { LegacyFrozen }` (D12 (c)).
///
/// `Candidate` carries the prepare-time sample inline (`clippy::large_enum_variant`): the value
/// is a persisted shape built once per freeze and matched by field on every completion path.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FrozenTarget {
    /// A candidate row existed; `before` is the prepare-time sample and `refused` says whether
    /// it mismatched (the verdict was then written in the prepare transaction and spawn is a
    /// no-op). `canonical_path` / `git_common_dir` are the lease row's expectation, frozen so the
    /// after-sample needs no row.
    Candidate {
        candidate_id: String,
        commit_sha: String,
        lease_id: String,
        cwd: String,
        canonical_path: String,
        git_common_dir: String,
        before: Sample,
        refused: bool,
    },
    /// Candidate-bound but no candidate row: refused in the prepare transaction, spawn no-op.
    NoCandidate {
        reason: NoCandidateReason,
    },
    Unbound {
        reason: FrozenUnbound,
    },
}

impl Default for FrozenTarget {
    fn default() -> Self {
        FrozenTarget::Unbound {
            reason: FrozenUnbound::LegacyFrozen,
        }
    }
}

impl FrozenTarget {
    /// The verdict was written in the prepare transaction: no process, `Ready(NoOp)`.
    pub(crate) fn spawn_is_noop(&self) -> bool {
        matches!(
            self,
            FrozenTarget::Candidate { refused: true, .. } | FrozenTarget::NoCandidate { .. }
        )
    }
}

/// The task-verify verdict as persisted (`tasks.gate_result_json`), returned as the op result and
/// projected into `task.gate_result`: the shared [`GateVerdict`] flattened, the frozen `cwd`, and
/// the target. A verdict persisted before slice 4 has no `target` and reads as
/// `Unbound { LegacyVerdict }` (D12 (b)).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskGateResult {
    #[serde(flatten)]
    pub verdict: GateVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default = "legacy_verdict_target")]
    pub target: VerifyTarget,
}

fn legacy_verdict_target() -> VerifyTarget {
    VerifyTarget::Unbound {
        reason: UnboundReason::LegacyVerdict,
    }
}

/// Whether one attempt is candidate-bound, read from its rows (admission, `prepare_tx` and the
/// reconcile arm P10 all read this one derivation). `Bound` carries the three rows inline
/// (`clippy::large_enum_variant`): built once per read and destructured at once.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub(crate) enum VerifyIdentity {
    Unbound {
        reason: FrozenUnbound,
    },
    /// A kernel-delivery lease: the delivery row names the lease; before any report the card's
    /// latest lease stands in.
    Bound {
        lease: WorkspaceLease,
        delivery: Option<DeliveryRow>,
        candidate: Option<CandidateRow>,
        abandoned: bool,
    },
}

pub(crate) async fn verify_target_identity(tx: &mut Tx<'_>, task: &Task) -> Result<VerifyIdentity> {
    if task.kind == TaskKind::Terminal {
        return Ok(VerifyIdentity::Unbound {
            reason: FrozenUnbound::Terminal,
        });
    }
    let delivery = delivery_latest_for_attempt_tx(tx, &task.id).await?;
    let lease = match (&delivery, task.worker_card_id.as_deref()) {
        (Some(delivery), _) => Some(lease_for_delivery_tx(tx, delivery).await?),
        (None, Some(card_id)) => {
            latest_workspace_lease_for_card_tx(tx, card_id, LeaseStates::Any).await?
        }
        (None, None) => None,
    };
    let Some(lease) = lease else {
        return Ok(VerifyIdentity::Unbound {
            reason: FrozenUnbound::LegacyLease,
        });
    };
    if lease.delivery_policy != Some(DeliveryPolicy::Kernel) {
        return Ok(VerifyIdentity::Unbound {
            reason: FrozenUnbound::LegacyLease,
        });
    }
    let candidate = candidate_for_attempt_tx(tx, &task.id).await?;
    let abandoned = match &delivery {
        Some(delivery) => abandonment_for_delivery_tx(tx, &delivery.delivery_id)
            .await?
            .is_some(),
        None => false,
    };
    Ok(VerifyIdentity::Bound {
        lease,
        delivery,
        candidate,
        abandoned,
    })
}

/// The delivery state a candidate-bound attempt without a candidate row is in. A `candidate`
/// settlement without its row cannot be verified against and is impossible by construction (one
/// transaction writes both); it is read as still pending — nothing is minted for it.
pub(crate) fn no_candidate_reason(
    delivery: Option<&DeliveryRow>,
    abandoned: bool,
) -> NoCandidateReason {
    let Some(delivery) = delivery else {
        return NoCandidateReason::NoDeliveryRow;
    };
    let delivery_id = delivery.delivery_id.clone();
    match &delivery.settlement {
        None | Some(DeliverySettled::Candidate { .. }) => {
            NoCandidateReason::DeliveryPending { delivery_id }
        }
        Some(DeliverySettled::Failed { .. }) if abandoned => {
            NoCandidateReason::DeliveryAbandoned { delivery_id }
        }
        Some(DeliverySettled::Failed { .. }) => NoCandidateReason::DeliveryFailed { delivery_id },
    }
}

/// What a sample is compared to (D3.0): the lease row's identity and the candidate's commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expected {
    pub canonical_path: String,
    pub git_common_dir: String,
    pub commit_sha: String,
}

impl Expected {
    fn for_candidate(lease: &WorkspaceLease, candidate: &CandidateRow) -> Result<Self> {
        let base = lease.base.as_ref().ok_or_else(|| {
            CalmError::Internal(format!(
                "kernel lease {} has no recorded base",
                lease.lease_id
            ))
        })?;
        let utf8 = |path: &Path| {
            path.to_str().map(str::to_owned).ok_or_else(|| {
                CalmError::Internal(format!(
                    "lease {} path {} is not UTF-8",
                    lease.lease_id,
                    path.display()
                ))
            })
        };
        Ok(Expected {
            canonical_path: utf8(&base.canonical_path)?,
            git_common_dir: utf8(&base.git_common_dir)?,
            commit_sha: candidate.commit_sha.clone(),
        })
    }
}

/// One D3.0 sample plus the provenance script's conclusion (exit 0 holds; 10 / 12 do not). The
/// three identity conditions are decided by the script alone (5.1.5), so the conclusion travels
/// beside the observation instead of being recomputed from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sampled {
    pub sample: Sample,
    pub provenance_holds: bool,
}

/// A sampling command failed or its output could not be read: not a mismatch (unable to check
/// is not a failed check).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleFailure {
    pub reason: String,
}

async fn run_sampling_command(
    cwd: &Path,
    argv: &[&str],
    what: &str,
) -> std::result::Result<std::process::Output, SampleFailure> {
    let mut cmd = tokio::process::Command::new(argv[0]);
    cmd.args(&argv[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    forge_base_env(&mut cmd);
    cmd.output().await.map_err(|error| SampleFailure {
        reason: format!("{what} could not be spawned in {}: {error}", cwd.display()),
    })
}

fn stderr_tail(output: &std::process::Output) -> String {
    let text = String::from_utf8_lossy(&output.stderr);
    text.lines().last().unwrap_or_default().trim().to_string()
}

fn exit_label(output: &std::process::Output) -> String {
    match output.status.code() {
        Some(code) => format!("exit {code}"),
        None => "killed by a signal".to_string(),
    }
}

/// Take one D3.0 sample in `cwd`, in order: (i) the provenance script with the expected identity,
/// (ii) `git rev-parse --verify HEAD^{commit}`, (iii) the 5.1.6 status command. Any exit outside
/// the script's conclusion vocabulary, any non-zero git exit, and any unparseable output is a
/// [`SampleFailure`].
pub async fn sample(
    cwd: &Path,
    expected: &Expected,
) -> std::result::Result<Sampled, SampleFailure> {
    let script = format!("{GIT_LEASE_PROVENANCE_SCRIPT}\n{PROVENANCE_SAMPLE_SCRIPT}");
    let provenance = run_sampling_command(
        cwd,
        &[
            "sh",
            "-c",
            &script,
            "sh",
            &expected.canonical_path,
            &expected.git_common_dir,
        ],
        "lease provenance observation",
    )
    .await?;
    let provenance_holds = match provenance.status.code() {
        Some(0) => true,
        Some(10) | Some(12) => false,
        _ => {
            return Err(SampleFailure {
                reason: format!(
                    "lease provenance observation failed ({}): {}",
                    exit_label(&provenance),
                    stderr_tail(&provenance)
                ),
            });
        }
    };
    // The observation record is the last thing the script writes to stderr, exactly once;
    // `parse_stderr` is the one reader of that contract. A conclusion exit without a readable
    // observation is a sampling failure, not a verdict (fail-closed).
    let observation = ProvenanceSample::parse_stderr(&String::from_utf8_lossy(&provenance.stderr))
        .ok_or_else(|| SampleFailure {
            reason: "lease provenance observation printed no readable observation record"
                .to_string(),
        })?;

    let head = run_sampling_command(
        cwd,
        &["git", "rev-parse", "--verify", "HEAD^{commit}"],
        "git rev-parse",
    )
    .await?;
    if !head.status.success() {
        return Err(SampleFailure {
            reason: format!(
                "git rev-parse --verify HEAD^{{commit}} failed ({}): {}",
                exit_label(&head),
                stderr_tail(&head)
            ),
        });
    }
    let head = String::from_utf8(head.stdout)
        .map_err(|_| SampleFailure {
            reason: "git rev-parse printed non-UTF-8 output".to_string(),
        })?
        .trim()
        .to_string();
    if head.is_empty() || head.contains(char::is_whitespace) {
        return Err(SampleFailure {
            reason: format!("git rev-parse printed no single commit id: {head:?}"),
        });
    }

    let status = run_sampling_command(
        cwd,
        &[
            "git",
            "-c",
            "status.showUntrackedFiles=all",
            "-c",
            "core.quotepath=false",
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
        "git status",
    )
    .await?;
    if !status.status.success() {
        return Err(SampleFailure {
            reason: format!(
                "git status --porcelain=v1 failed ({}): {}",
                exit_label(&status),
                stderr_tail(&status)
            ),
        });
    }
    let dirty = String::from_utf8(status.stdout)
        .map_err(|_| SampleFailure {
            reason: "git status printed non-UTF-8 output".to_string(),
        })?
        .lines()
        .map(str::to_owned)
        .collect();
    Ok(Sampled {
        sample: Sample {
            head,
            dirty,
            provenance: observation,
        },
        provenance_holds,
    })
}

/// Which D3.0 checks failed: the script's conclusion, `head` against the candidate, and the
/// porcelain status. Empty means the sample matches the candidate.
pub fn reasons(sampled: &Sampled, expected: &Expected) -> Vec<MismatchReason> {
    let mut reasons = Vec::new();
    if !sampled.provenance_holds {
        reasons.push(MismatchReason::Provenance);
    }
    if sampled.sample.head != expected.commit_sha {
        reasons.push(MismatchReason::Head);
    }
    if !sampled.sample.dirty.is_empty() {
        reasons.push(MismatchReason::Dirty);
    }
    reasons
}

/// `log_tail` of a `gate-target-mismatch` verdict (5.1.7): one human line, then the target as
/// one JSON line.
fn mismatch_log_tail(line: &str, target: &VerifyTarget) -> String {
    let json = serde_json::to_string(target).unwrap_or_else(|_| "{}".to_string());
    format!("{line}\n{json}")
}

fn refused_verdict(
    status_detail: &str,
    log_tail: String,
    log_path: &Path,
    attempt: i64,
) -> GateVerdict {
    GateVerdict {
        passed: false,
        status_detail: Some(status_detail.to_string()),
        failing_step: None,
        exit_code: None,
        log_tail,
        log_path: log_path.display().to_string(),
        attempt,
    }
}

/// Append the refusal line to this attempt's log file so `log_path` names an existing file
/// (5.1.7); best effort — the verdict carries the line either way.
async fn append_log_line(log_path: &Path, line: &str) {
    if let Some(parent) = log_path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let mut text = line.to_string();
    text.push('\n');
    let result = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .await;
    match result {
        Ok(mut file) => {
            use tokio::io::AsyncWriteExt;
            let _ = file.write_all(text.as_bytes()).await;
        }
        Err(error) => {
            tracing::warn!(path = %log_path.display(), %error, "gate log line not written");
        }
    }
}

fn no_candidate_sentence(reason: &NoCandidateReason) -> String {
    let found = match reason {
        NoCandidateReason::DeliveryPending { delivery_id } => {
            format!("delivery {delivery_id} is pending")
        }
        NoCandidateReason::DeliveryFailed { delivery_id } => {
            format!("delivery {delivery_id} is failed")
        }
        NoCandidateReason::DeliveryAbandoned { delivery_id } => {
            format!("delivery {delivery_id} is abandoned")
        }
        NoCandidateReason::NoDeliveryRow => "no delivery row".to_string(),
    };
    format!("refused: no candidate to verify: {found}; gate admitted before settlement")
}

/// What `prepare_tx` froze and, when the target was refused, the verdict it writes in the same
/// transaction (P2 / P3).
pub(crate) struct PreparedTarget {
    pub target: FrozenTarget,
    pub refusal: Option<TaskGateResult>,
}

/// The prepare-time target check (D3, after the attempt bump): freeze the identity; on a
/// candidate-bound attempt refuse `gate.cwd` (`Conflict`, P4), refuse a missing candidate in the
/// transaction (P3), sample the checkout (a sampling failure is `Internal` → op Stuck, P5) and
/// refuse a mismatch in the transaction (P2); otherwise freeze `refused: false` (P1).
pub(crate) async fn prepare_target_tx(
    tx: &mut Tx<'_>,
    task: &Task,
    gate: &GateSpec,
    cwd: &str,
    attempt: i64,
    log_path: &Path,
) -> Result<PreparedTarget> {
    let (lease, delivery, candidate, abandoned) = match verify_target_identity(tx, task).await? {
        VerifyIdentity::Unbound { reason } => {
            return Ok(PreparedTarget {
                target: FrozenTarget::Unbound { reason },
                refusal: None,
            });
        }
        VerifyIdentity::Bound {
            lease,
            delivery,
            candidate,
            abandoned,
        } => (lease, delivery, candidate, abandoned),
    };
    if gate.cwd.is_some() {
        return Err(CalmError::Conflict(format!(
            "refused: agent task {} gate.cwd is not supported; the gate runs in the worker's \
             lease worktree — declare a follow-up with base:{{attempt}} and write a \
             sub-directory gate as `cd <subdir> && …` inside the step",
            task.key
        )));
    }
    let Some(candidate) = candidate else {
        let reason = no_candidate_reason(delivery.as_ref(), abandoned);
        let line = no_candidate_sentence(&reason);
        append_log_line(log_path, &line).await;
        let refusal = TaskGateResult {
            verdict: refused_verdict(GATE_INFRA, line, log_path, attempt),
            cwd: Some(cwd.to_string()),
            target: VerifyTarget::NoCandidate {
                reason: reason.clone(),
            },
        };
        return Ok(PreparedTarget {
            target: FrozenTarget::NoCandidate { reason },
            refusal: Some(refusal),
        });
    };
    let expected = Expected::for_candidate(&lease, &candidate)?;
    let sampled = sample(Path::new(cwd), &expected).await.map_err(|failure| {
        CalmError::Internal(format!(
            "gate-infra: verification target unsampled: {}",
            failure.reason
        ))
    })?;
    let reasons = reasons(&sampled, &expected);
    let refused = !reasons.is_empty();
    let refusal = refused.then(|| {
        let target = VerifyTarget::Candidate {
            candidate_id: candidate.candidate_id.clone(),
            commit_sha: candidate.commit_sha.clone(),
            lease_id: candidate.lease_id.clone(),
            evidence: VerifyTargetEvidence::Refused {
                cwd: cwd.to_string(),
                before: sampled.sample.clone(),
                reasons: reasons.clone(),
            },
        };
        let line = format!(
            "gate REFUSED: verification target mismatch ({}): expected candidate {} ({}) at {cwd}; found {}",
            render_reasons(&reasons),
            candidate.candidate_id,
            candidate.commit_sha,
            sampled.sample.head
        );
        (line, target)
    });
    let refusal = match refusal {
        Some((line, target)) => {
            append_log_line(log_path, &line).await;
            Some(TaskGateResult {
                verdict: refused_verdict(
                    GATE_TARGET_MISMATCH,
                    mismatch_log_tail(&line, &target),
                    log_path,
                    attempt,
                ),
                cwd: Some(cwd.to_string()),
                target,
            })
        }
        None => None,
    };
    Ok(PreparedTarget {
        target: FrozenTarget::Candidate {
            candidate_id: candidate.candidate_id,
            commit_sha: candidate.commit_sha,
            lease_id: candidate.lease_id,
            cwd: cwd.to_string(),
            canonical_path: expected.canonical_path,
            git_common_dir: expected.git_common_dir,
            before: sampled.sample,
            refused,
        },
        refusal,
    })
}

/// The one exit of the three completion paths (live observer, boot reattach, dead process with
/// an exit file): an `Unbound` freeze passes the verdict through (P7); a `Candidate` freeze is
/// sampled again — `reasons` empty keeps the verdict, non-empty discards every step result as
/// `gate-target-mismatch`, a sampling failure is `gate-infra` with `Unsampled { Finalize }` (P6).
pub(crate) async fn finalize(verdict: GateVerdict, frozen: &FrozenVerify) -> TaskGateResult {
    let cwd = Some(frozen.cwd.clone());
    match &frozen.target {
        FrozenTarget::Unbound { reason } => TaskGateResult {
            verdict,
            cwd,
            target: VerifyTarget::Unbound {
                reason: reason.wire(),
            },
        },
        // No process ran for these; a verdict here can only be a foreign completion. The frozen
        // identity is kept and the verdict is left as the caller built it.
        FrozenTarget::NoCandidate { reason } => TaskGateResult {
            verdict,
            cwd,
            target: VerifyTarget::NoCandidate {
                reason: reason.clone(),
            },
        },
        // A refused freeze spawned nothing, so no completion path can reach here; a verdict that
        // does is a foreign completion and is not trusted (fail-closed, `gate-infra`).
        FrozenTarget::Candidate {
            candidate_id,
            commit_sha,
            lease_id,
            cwd: gate_cwd,
            refused: true,
            ..
        } => {
            let reason = "the target was refused in prepare; no gate process ran".to_string();
            TaskGateResult {
                verdict: refused_verdict(
                    GATE_INFRA,
                    reason.clone(),
                    Path::new(&verdict.log_path),
                    verdict.attempt,
                ),
                cwd,
                target: VerifyTarget::Candidate {
                    candidate_id: candidate_id.clone(),
                    commit_sha: commit_sha.clone(),
                    lease_id: lease_id.clone(),
                    evidence: VerifyTargetEvidence::Unsampled {
                        phase: SamplePhase::Finalize {
                            cwd: gate_cwd.clone(),
                            reason,
                        },
                    },
                },
            }
        }
        FrozenTarget::Candidate {
            candidate_id,
            commit_sha,
            lease_id,
            cwd: gate_cwd,
            canonical_path,
            git_common_dir,
            before,
            refused: false,
        } => {
            let expected = Expected {
                canonical_path: canonical_path.clone(),
                git_common_dir: git_common_dir.clone(),
                commit_sha: commit_sha.clone(),
            };
            let candidate = |evidence: VerifyTargetEvidence| VerifyTarget::Candidate {
                candidate_id: candidate_id.clone(),
                commit_sha: commit_sha.clone(),
                lease_id: lease_id.clone(),
                evidence,
            };
            let log_path = Path::new(&verdict.log_path).to_path_buf();
            match sample(Path::new(gate_cwd), &expected).await {
                Ok(after) => {
                    let reasons = reasons(&after, &expected);
                    let target = candidate(VerifyTargetEvidence::Verified {
                        cwd: gate_cwd.clone(),
                        before: before.clone(),
                        after: after.sample.clone(),
                        reasons: reasons.clone(),
                    });
                    if reasons.is_empty() {
                        return TaskGateResult {
                            verdict,
                            cwd,
                            target,
                        };
                    }
                    let line = format!(
                        "gate RESULT DISCARDED: checkout changed during the gate ({}): HEAD {}→{}",
                        render_reasons(&reasons),
                        before.head,
                        after.sample.head
                    );
                    append_log_line(&log_path, &line).await;
                    TaskGateResult {
                        verdict: refused_verdict(
                            GATE_TARGET_MISMATCH,
                            mismatch_log_tail(&line, &target),
                            &log_path,
                            verdict.attempt,
                        ),
                        cwd,
                        target,
                    }
                }
                Err(SampleFailure { reason }) => {
                    let line = format!(
                        "gate-infra: verification target unsampled after the gate: {reason}"
                    );
                    append_log_line(&log_path, &line).await;
                    TaskGateResult {
                        verdict: refused_verdict(GATE_INFRA, line, &log_path, verdict.attempt),
                        cwd,
                        target: candidate(VerifyTargetEvidence::Unsampled {
                            phase: SamplePhase::Finalize {
                                cwd: gate_cwd.clone(),
                                reason,
                            },
                        }),
                    }
                }
            }
        }
    }
}

/// P8: the target of the compensation step's `gate-infra` verdict (spawn failed after the
/// freeze). A frozen `NoCandidate` is unreachable here (its spawn is a no-op) but expressible.
pub(crate) fn compensation_target(frozen: &FrozenTarget, reason: &str) -> VerifyTarget {
    frozen_unsampled(frozen, |cwd| SamplePhase::Compensation {
        cwd,
        reason: reason.to_string(),
    })
}

/// P9 / P9b: the target of a reconciled verdict from a frozen op.
fn reconciliation_target(frozen: &FrozenTarget, last_error: &str) -> VerifyTarget {
    frozen_unsampled(frozen, |cwd| SamplePhase::Reconciliation {
        cwd,
        last_error: last_error.to_string(),
    })
}

fn frozen_unsampled(
    frozen: &FrozenTarget,
    phase: impl FnOnce(String) -> SamplePhase,
) -> VerifyTarget {
    match frozen {
        FrozenTarget::Candidate {
            candidate_id,
            commit_sha,
            lease_id,
            cwd,
            ..
        } => VerifyTarget::Candidate {
            candidate_id: candidate_id.clone(),
            commit_sha: commit_sha.clone(),
            lease_id: lease_id.clone(),
            evidence: VerifyTargetEvidence::Unsampled {
                phase: phase(cwd.clone()),
            },
        },
        FrozenTarget::NoCandidate { reason } => VerifyTarget::NoCandidate {
            reason: reason.clone(),
        },
        FrozenTarget::Unbound { reason } => VerifyTarget::Unbound {
            reason: reason.wire(),
        },
    }
}

/// P10: the target derived from the rows at reconcile time, for an op that failed before it
/// froze anything (`prepare_tx` refused or a sampling command failed).
fn derived_target(identity: VerifyIdentity, reason: &str) -> VerifyTarget {
    match identity {
        VerifyIdentity::Unbound { reason } => VerifyTarget::Unbound {
            reason: reason.wire(),
        },
        VerifyIdentity::Bound {
            candidate: Some(candidate),
            ..
        } => VerifyTarget::Candidate {
            candidate_id: candidate.candidate_id,
            commit_sha: candidate.commit_sha,
            lease_id: candidate.lease_id,
            evidence: VerifyTargetEvidence::Unsampled {
                phase: SamplePhase::Prepare {
                    reason: reason.to_string(),
                },
            },
        },
        VerifyIdentity::Bound {
            delivery,
            abandoned,
            candidate: None,
            ..
        } => VerifyTarget::NoCandidate {
            reason: no_candidate_reason(delivery.as_ref(), abandoned),
        },
    }
}

/// The frozen `FrozenVerify` of the attempt's op, when the op has a `tx_output`.
async fn frozen_of_attempt(
    tx: &mut Tx<'_>,
    task_id: &str,
    attempt: i64,
) -> Result<Option<FrozenVerify>> {
    let tx_output: Option<Option<String>> = sqlx::query_scalar(
        "SELECT tx_output_json FROM operations WHERE kind = 'task-verify' AND idempotency_key = ?1",
    )
    .bind(gate_attempt_key(task_id, attempt))
    .fetch_optional(&mut **tx)
    .await?;
    let Some(Some(text)) = tx_output else {
        return Ok(None);
    };
    let output: TxOutput = match serde_json::from_str(&text) {
        Ok(output) => output,
        Err(error) => {
            tracing::warn!(task_id, attempt, %error, "task-verify tx_output unreadable; target derived from rows");
            return Ok(None);
        }
    };
    match FrozenVerify::from_output(&output) {
        Ok(frozen) => Ok(Some(frozen)),
        Err(error) => {
            tracing::warn!(task_id, attempt, %error, "task-verify freeze unreadable; target derived from rows");
            Ok(None)
        }
    }
}

/// The reconcile arm's verdict for a terminal task-verify op (`scheduler::reconcile_gate_outcome`):
/// a parseable success result stands as written; an unparseable one (P9b), a failed op (P9) and
/// a stuck op carry the frozen target as `Unsampled { Reconciliation }`; an op with no freeze
/// derives its target from the rows (P10).
pub(crate) async fn reconcile_result_tx(
    tx: &mut Tx<'_>,
    task: &Task,
    attempt: i64,
    log_path: &str,
    outcome: OperationOutcome,
) -> Result<TaskGateResult> {
    let (status_detail, last_error) = match outcome {
        OperationOutcome::Succeeded { result }
        | OperationOutcome::SucceededViaCollision { result, .. } => {
            match serde_json::from_value::<TaskGateResult>(result) {
                Ok(result) => return Ok(result),
                Err(error) => (GATE_INFRA, format!("gate op result unparseable: {error}")),
            }
        }
        OperationOutcome::Failed {
            last_error,
            last_error_class,
            ..
        } => {
            let status_detail = if last_error_class.as_deref() == Some("parked_deadline") {
                GATE_TIMEOUT
            } else {
                GATE_INFRA
            };
            (status_detail, last_error)
        }
        OperationOutcome::Stuck { reason, .. } => (GATE_INFRA, reason),
    };
    let frozen = frozen_of_attempt(tx, &task.id, attempt).await?;
    let (cwd, target) = match &frozen {
        Some(frozen) => (
            Some(frozen.cwd.clone()),
            reconciliation_target(&frozen.target, &last_error),
        ),
        None => (
            None,
            derived_target(verify_target_identity(tx, task).await?, &last_error),
        ),
    };
    Ok(TaskGateResult {
        verdict: GateVerdict {
            passed: false,
            status_detail: Some(status_detail.to_string()),
            failing_step: None,
            exit_code: None,
            log_tail: last_error,
            log_path: log_path.to_string(),
            attempt,
        },
        cwd,
        target,
    })
}

/// `serde_json` value of a P8 target for the compensation step's args.
pub(crate) fn target_json(target: &VerifyTarget) -> Value {
    serde_json::to_value(target).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_of(head: &str, dirty: &[&str], holds: bool) -> Sampled {
        Sampled {
            sample: Sample {
                head: head.into(),
                dirty: dirty.iter().map(|d| (*d).to_string()).collect(),
                provenance: ProvenanceSample {
                    realpath: "/leases/l-1".into(),
                    common_dir: "/repo/.git".into(),
                    registered: holds,
                },
            },
            provenance_holds: holds,
        }
    }

    fn expected() -> Expected {
        Expected {
            canonical_path: "/leases/l-1".into(),
            git_common_dir: "/repo/.git".into(),
            commit_sha: "a".repeat(40),
        }
    }

    /// `reasons` names exactly the failed checks, in the D3.0 order, and is empty on a match.
    #[test]
    fn reasons_name_each_failed_check() {
        assert_eq!(
            reasons(&sample_of(&"a".repeat(40), &[], true), &expected()),
            vec![]
        );
        assert_eq!(
            reasons(&sample_of(&"b".repeat(40), &[], true), &expected()),
            vec![MismatchReason::Head]
        );
        assert_eq!(
            reasons(&sample_of(&"a".repeat(40), &["?? x"], true), &expected()),
            vec![MismatchReason::Dirty]
        );
        assert_eq!(
            reasons(&sample_of(&"a".repeat(40), &[], false), &expected()),
            vec![MismatchReason::Provenance]
        );
        assert_eq!(
            reasons(&sample_of(&"b".repeat(40), &[" M f"], false), &expected()),
            vec![
                MismatchReason::Provenance,
                MismatchReason::Head,
                MismatchReason::Dirty
            ]
        );
    }

    /// A freeze without `target` (pre-slice-4) reads as `Unbound { LegacyFrozen }`; a verdict
    /// without `target` reads as `Unbound { LegacyVerdict }`, its `GateVerdict` fields intact.
    #[test]
    fn serde_defaults_read_legacy_shapes() {
        let frozen: FrozenVerify = serde_json::from_value(json!({
            "task_id": "t", "track_id": "trk", "area_id": "a", "key": "k", "attempt": 1,
            "cwd": "/w", "gate": {"steps": [{"name": "s", "cmd": "true"}]}
        }))
        .unwrap();
        assert_eq!(frozen.target, FrozenTarget::default());
        assert_eq!(
            frozen.target,
            FrozenTarget::Unbound {
                reason: FrozenUnbound::LegacyFrozen
            }
        );

        let legacy: TaskGateResult = serde_json::from_value(json!({
            "passed": false, "status_detail": "gate-red", "failing_step": "test",
            "exit_code": 101, "log_tail": "boom", "log_path": "/l", "attempt": 2, "cwd": "/w"
        }))
        .unwrap();
        assert_eq!(
            legacy.target,
            VerifyTarget::Unbound {
                reason: UnboundReason::LegacyVerdict
            }
        );
        assert_eq!(legacy.verdict.status_detail.as_deref(), Some("gate-red"));
        assert_eq!(legacy.verdict.failing_step.as_deref(), Some("test"));
        assert_eq!(legacy.verdict.exit_code, Some(101));
        assert_eq!(legacy.cwd.as_deref(), Some("/w"));
        assert_eq!(legacy.verdict.attempt, 2);
    }

    /// The persisted shape is the seven `GateVerdict` keys flattened plus `cwd` and `target`.
    #[test]
    fn task_gate_result_wire_shape() {
        let result = TaskGateResult {
            verdict: GateVerdict {
                passed: true,
                status_detail: None,
                failing_step: None,
                exit_code: Some(0),
                log_tail: "ok".into(),
                log_path: "/l".into(),
                attempt: 1,
            },
            cwd: Some("/w".into()),
            target: VerifyTarget::Unbound {
                reason: UnboundReason::Terminal,
            },
        };
        let wire = serde_json::to_value(&result).unwrap();
        let mut keys: Vec<_> = wire.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "attempt",
                "cwd",
                "exit_code",
                "failing_step",
                "log_path",
                "log_tail",
                "passed",
                "status_detail",
                "target"
            ]
        );
        assert_eq!(
            wire["target"],
            json!({"kind": "unbound", "reason": "terminal"})
        );
        let back: TaskGateResult = serde_json::from_value(wire).unwrap();
        assert_eq!(back.target, result.target);
        assert!(back.verdict.passed);
    }

    /// Every `FrozenTarget` arm maps to its P8 / P9 target; only `Candidate` is sampled-phase.
    #[test]
    fn frozen_target_maps_to_unsampled_phases() {
        let candidate = FrozenTarget::Candidate {
            candidate_id: "c".into(),
            commit_sha: "a".repeat(40),
            lease_id: "l".into(),
            cwd: "/w".into(),
            canonical_path: "/w".into(),
            git_common_dir: "/r/.git".into(),
            before: sample_of(&"a".repeat(40), &[], true).sample,
            refused: false,
        };
        assert_eq!(
            compensation_target(&candidate, "spawn failed"),
            VerifyTarget::Candidate {
                candidate_id: "c".into(),
                commit_sha: "a".repeat(40),
                lease_id: "l".into(),
                evidence: VerifyTargetEvidence::Unsampled {
                    phase: SamplePhase::Compensation {
                        cwd: "/w".into(),
                        reason: "spawn failed".into()
                    }
                }
            }
        );
        assert_eq!(
            reconciliation_target(&candidate, "parked_deadline"),
            VerifyTarget::Candidate {
                candidate_id: "c".into(),
                commit_sha: "a".repeat(40),
                lease_id: "l".into(),
                evidence: VerifyTargetEvidence::Unsampled {
                    phase: SamplePhase::Reconciliation {
                        cwd: "/w".into(),
                        last_error: "parked_deadline".into()
                    }
                }
            }
        );
        let no_candidate = FrozenTarget::NoCandidate {
            reason: NoCandidateReason::NoDeliveryRow,
        };
        assert_eq!(
            compensation_target(&no_candidate, "x"),
            VerifyTarget::NoCandidate {
                reason: NoCandidateReason::NoDeliveryRow
            }
        );
        assert!(no_candidate.spawn_is_noop());
        assert!(!candidate.spawn_is_noop());
        for (frozen, wire) in [
            (FrozenUnbound::LegacyLease, UnboundReason::LegacyLease),
            (FrozenUnbound::LegacyFrozen, UnboundReason::LegacyFrozen),
            (FrozenUnbound::Terminal, UnboundReason::Terminal),
        ] {
            assert_eq!(
                compensation_target(&FrozenTarget::Unbound { reason: frozen }, "x"),
                VerifyTarget::Unbound { reason: wire }
            );
        }
    }

    /// The mismatch `log_tail` is one line plus the target as one JSON line (5.1.7).
    #[test]
    fn mismatch_log_tail_carries_target_json() {
        let target = VerifyTarget::NoCandidate {
            reason: NoCandidateReason::NoDeliveryRow,
        };
        let tail = mismatch_log_tail("gate REFUSED: x", &target);
        let mut lines = tail.lines();
        assert_eq!(lines.next(), Some("gate REFUSED: x"));
        let json = lines.next().unwrap();
        assert_eq!(lines.next(), None);
        assert_eq!(serde_json::from_str::<VerifyTarget>(json).unwrap(), target);
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = crate::workspace_materialize::neige_git_command()
            .args(args)
            .current_dir(dir)
            .output()
            .expect("spawn git");
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// The sampler against a real linked worktree: a matching checkout samples clean and holds;
    /// a moved HEAD and an untracked file are the `head` / `dirty` reasons; a non-repository
    /// directory is a `SampleFailure`, not a mismatch. Also the one timing the U4 spike asked
    /// the implementation to report.
    #[tokio::test]
    async fn sampler_reads_a_linked_worktree() {
        let tmp = tempfile::Builder::new()
            .prefix("neige-gate-sample-")
            .tempdir()
            .unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@example.test"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("README.md"), "x\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-q", "-m", "initial"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        let worktree = tmp.path().join("wt");
        git(
            &repo,
            &["worktree", "add", "-q", worktree.to_str().unwrap(), "HEAD"],
        );
        let canonical = std::fs::canonicalize(&worktree).unwrap();
        let common_dir = std::fs::canonicalize(repo.join(".git")).unwrap();
        let expected = Expected {
            canonical_path: canonical.to_str().unwrap().to_string(),
            git_common_dir: common_dir.to_str().unwrap().to_string(),
            commit_sha: head.clone(),
        };

        let started = std::time::Instant::now();
        let sampled = sample(&worktree, &expected).await.unwrap();
        eprintln!("prepare-time sample took {:?}", started.elapsed());
        assert!(sampled.provenance_holds, "{sampled:?}");
        assert_eq!(sampled.sample.head, head);
        assert!(sampled.sample.dirty.is_empty(), "{sampled:?}");
        assert!(sampled.sample.provenance.registered);
        assert_eq!(sampled.sample.provenance.realpath, expected.canonical_path);
        assert_eq!(
            sampled.sample.provenance.common_dir,
            expected.git_common_dir
        );
        assert!(reasons(&sampled, &expected).is_empty());

        std::fs::write(worktree.join("extra.txt"), "").unwrap();
        git(&worktree, &["commit", "-q", "--allow-empty", "-m", "moved"]);
        let sampled = sample(&worktree, &expected).await.unwrap();
        assert_eq!(
            reasons(&sampled, &expected),
            vec![MismatchReason::Head, MismatchReason::Dirty]
        );
        assert_eq!(sampled.sample.dirty, vec!["?? extra.txt".to_string()]);

        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let failure = sample(&plain, &expected).await.unwrap_err();
        assert!(
            failure
                .reason
                .contains("lease provenance observation failed"),
            "{failure:?}"
        );
        let missing = tmp.path().join("missing");
        let failure = sample(&missing, &expected).await.unwrap_err();
        assert!(
            failure.reason.contains("could not be spawned"),
            "{failure:?}"
        );
    }
}
